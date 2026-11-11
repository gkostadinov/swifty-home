//! A catalog with no house in it: descriptions from device names and the owner's
//! notes, house scenes from the store, and the actions plugins declare, with the
//! hub's floors applied, kept to the owner, out of voice's and Swifty's reach, and
//! confirmed by the manifest's `Expect`.

mod common;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{json, Value};
use swifty_hub::actions::{self, Actor};
use swifty_hub::catalog::{self, ACK_PARAM};
use swifty_hub::policy::{Channel, Role};

use swifty_hub::voice::{self, Candidate, Parsed, Resolution};
use swifty_hub::{notes, scenes};
use swifty_plugin_api::{
    Capability, CommandAck, ConfigProblem, CustomAction, DeviceClass, DeviceDecl, DeviceError, Driver, Expect,
    InstanceView, LiveContext, Manifest, Op, ParamDef, ParamKind, Plugin, PluginError, Reach, Readiness, Scalar, State,
    Tier, API_VERSION,
};
use swifty_plugin_mock::{Behavior, CUSTOM_ACTION};
use webauthn_authenticator_rs::softpasskey::SoftPasskey;

// ----- a second plugin, for what the mock plugin does not declare

const COUNT: &[ParamDef] =
    &[ParamDef { name: "count", kind: ParamKind::Int { min: 0, max: 1_000 }, required: true, help: "The new count." }];
const FRESH: &[ParamDef] =
    &[ParamDef { name: "fresh", kind: ParamKind::Bool, required: true, help: "Whether it starts afresh." }];

const fn lab_action(id: &'static str, class: DeviceClass, idempotent: bool, expect: Expect) -> CustomAction {
    CustomAction {
        id,
        label: "Lab action",
        description: "Does one lab thing.",
        consequences: &["The lab notices."],
        class,
        tier: Tier::Comfort,
        params: &[],
        idempotent,
        confirm_timeout_ms: 3_000,
        expect,
    }
}

/// Asks for Comfort every time. A repeat of `lab.count` would count twice and
/// `lab.reset` acts on a VM, so the hub raises both.
static LAB: Manifest = Manifest {
    id: "lab",
    name: "Lab",
    version: "0.1.0",
    api: API_VERSION,
    summary: "A plugin that only exists in this test.",
    reach: Reach::Local,
    network: &[],
    classes: &[DeviceClass::Other, DeviceClass::Vm],
    config_version: 1,
    config: &[],
    secrets: &[],
    actions: &[
        CustomAction {
            params: COUNT,
            ..lab_action("lab.count", DeviceClass::Other, false, Expect::CounterIncreased { field: "count" })
        },
        CustomAction {
            params: FRESH,
            ..lab_action("lab.reset", DeviceClass::Vm, true, Expect::FieldIs { field: "fresh", value: true })
        },
        lab_action("lab.ping", DeviceClass::Other, true, Expect::Unreadable),
    ],
    vocabulary: &[],
    nouns: &[],
    multi_instance: false,
};

/// What the lab's devices hold, and what they were asked.
struct LabWorld {
    states: Mutex<BTreeMap<String, State>>,
    commands: Mutex<Vec<(String, Op)>>,
    reads: AtomicUsize,
}

impl LabWorld {
    fn new() -> Arc<Self> {
        let states = BTreeMap::from([
            (COUNTER.to_string(), State::Custom { fields: fields(&[("count", Scalar::Int(3))]) }),
            (LAB_VM.to_string(), State::Custom { fields: fields(&[("fresh", Scalar::Bool(false))]) }),
        ]);
        Arc::new(Self { states: Mutex::new(states), commands: Mutex::new(Vec::new()), reads: AtomicUsize::new(0) })
    }

    fn commands(&self) -> Vec<(String, Op)> {
        self.commands.lock().unwrap().clone()
    }

    fn command_count(&self, device: &str) -> usize {
        self.commands().iter().filter(|(sent_to, _)| sent_to == device).count()
    }

    fn total_reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

/// The lab plugin. Its devices are named in its config: `{ "devices": [{ "id",
/// "class", "name", "actions" }] }`, each offering the custom actions it lists.
struct Lab {
    world: Arc<LabWorld>,
}

fn lab_devices(config: &Value) -> Vec<DeviceDecl> {
    let rows = config["devices"].as_array().cloned().unwrap_or_default();
    rows.iter()
        .map(|row| DeviceDecl {
            local_id: row["id"].as_str().unwrap().to_string(),
            target_hint: row["id"].as_str().map(str::to_string),
            class: serde_json::from_value(row["class"].clone()).unwrap(),
            name: row["name"].as_str().unwrap().to_string(),
            capabilities: row["actions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|action| Capability::Custom { action: action.as_str().unwrap().to_string() })
                .collect(),
            attrs: json!({}),
        })
        .collect()
}

impl Plugin for Lab {
    fn manifest(&self) -> &'static Manifest {
        &LAB
    }
    fn validate(&self, _config: &Value) -> Result<(), ConfigProblem> {
        Ok(())
    }
    fn readiness(&self, _instance: &InstanceView<'_>) -> Readiness {
        Readiness::Ready
    }
    fn devices(&self, instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
        lab_devices(instance.config)
    }
    fn refusal(&self, instance: &InstanceView<'_>, device: &str, _op: &Op) -> Option<String> {
        let known = lab_devices(instance.config).iter().any(|decl| decl.local_id == device);
        (!known).then(|| "This device is not part of the lab.".to_string())
    }
    fn connect(&self, _ctx: LiveContext) -> Result<Arc<dyn Driver>, PluginError> {
        Ok(Arc::new(LabDriver { world: self.world.clone() }))
    }
}

/// Takes a custom action's arguments as the fields of the same name, as the mock does.
struct LabDriver {
    world: Arc<LabWorld>,
}

#[async_trait]
impl Driver for LabDriver {
    async fn command(&self, device: &str, op: &Op) -> Result<CommandAck, DeviceError> {
        self.world.commands.lock().unwrap().push((device.to_string(), op.clone()));
        if let (Op::Custom { args, .. }, Some(State::Custom { fields })) =
            (op, self.world.states.lock().unwrap().get_mut(device))
        {
            fields.extend(args.clone());
        }
        Ok(CommandAck)
    }

    async fn read_state(&self, device: &str) -> Result<State, DeviceError> {
        self.world.reads.fetch_add(1, Ordering::SeqCst);
        self.world.states.lock().unwrap().get(device).cloned().ok_or_else(|| DeviceError::Unreachable("gone".into()))
    }
}

/// The mock plugin's device of class `other`.
const GADGET: &str = "gadget";
/// The lab's counter, which also answers `lab.ping`.
const COUNTER: &str = "counter";
/// The lab's VM.
const LAB_VM: &str = "lab-vm";

fn fields(pairs: &[(&str, Scalar)]) -> BTreeMap<String, Scalar> {
    pairs.iter().map(|(name, value)| (name.to_string(), value.clone())).collect()
}

/// The mock house with the lab plugin in the build: a `gadgets` instance of the
/// mock plugin with the gadget, and a `lab` instance with the counter and the VM.
/// The plugins' actions join the catalog when the hub starts.
async fn custom_harness() -> (Harness, SoftPasskey, String, Arc<LabWorld>) {
    let world = LabWorld::new();
    let lab: &'static Lab = Box::leak(Box::new(Lab { world: world.clone() }));
    let harness = Harness::with_plugins(vec![lab], |_| {});
    let gadget = json!({ "id": GADGET, "class": "other", "name": "Test gadget", "target": GADGET });
    harness.add_instance("gadgets", "mock", json!({ "devices": [gadget] }));
    let devices = json!([
        { "id": COUNTER, "class": "other", "name": "Test counter", "actions": ["lab.count", "lab.ping"] },
        { "id": LAB_VM, "class": "vm", "name": "Test lab-vm", "actions": ["lab.reset"] },
    ]);
    harness.add_instance("lab", "lab", json!({ "devices": devices }));
    let (auth, cookie) = harness.sign_in().await;
    (harness, auth, cookie, world)
}

fn custom_op(action: &str, args: &[(&str, Scalar)]) -> Op {
    Op::Custom { action: action.to_string(), args: fields(args) }
}

fn catalog_entry(catalog: &Resp, id: &str) -> Option<Value> {
    catalog.body["actions"].as_array().unwrap().iter().find(|action| action["id"] == json!(id)).cloned()
}

fn owner() -> Actor {
    Actor {
        session_ref: "test-session".to_string(),
        credential_id: "test-credential".to_string(),
        device: "test".to_string(),
        user_id: "test-owner".to_string(),
        role: Role::Owner,
    }
}

// ----- custom actions

#[tokio::test]
async fn a_custom_action_runs_for_the_owner_and_is_confirmed_only_by_its_expect() {
    let (harness, _auth, cookie, _lab) = custom_harness().await;

    let done = harness.execute(&cookie, CUSTOM_ACTION, GADGET, json!({ "level": 7 })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(
        harness.devices.commands(),
        [(GADGET.to_string(), custom_op(CUSTOM_ACTION, &[("level", Scalar::Int(7))]))]
    );
    assert_eq!(harness.devices.state(GADGET), Some(State::Custom { fields: fields(&[("level", Scalar::Int(7))]) }));
    assert!(harness.audit_has("confirmed"));

    // The device takes the command and shows nothing: the manifest's rule is not met.
    harness.devices.set_behavior(GADGET, Behavior::AcceptsButIgnores);
    let ignored = harness.execute(&cookie, CUSTOM_ACTION, GADGET, json!({ "level": 2 })).await;
    assert_eq!(ignored.phase(), "no_response", "{}", ignored.text);

    // The parameters are the manifest's, and only a device that names the action is a target.
    for params in [json!({ "level": 11 }), json!({}), json!({ "level": 1, "colour": "red" })] {
        harness
            .execute(&cookie, CUSTOM_ACTION, GADGET, params)
            .await
            .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_params");
    }
    for target in [COUNTER, "bulb1", "tv", "nothing"] {
        harness
            .execute(&cookie, CUSTOM_ACTION, target, json!({ "level": 1 }))
            .await
            .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
    }
    assert_eq!(harness.devices.command_count(GADGET), 2);
}

#[tokio::test]
async fn a_counter_is_read_before_the_command_and_must_go_up() {
    let (harness, _auth, cookie, lab) = custom_harness().await;
    // It could count twice, so the hub made it a tier 2 action.
    harness
        .execute(&cookie, "lab.count", COUNTER, json!({ "count": 4 }))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "confirmation_required");
    let reads_before = lab.total_reads();
    let prepared = harness.prepare(&cookie, "lab.count", COUNTER, json!({ "count": 4 })).await;
    assert_eq!(prepared.body["needs"], json!("tap"), "{}", prepared.text);
    let done = harness.confirm_tap(&cookie, &prepared).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert!(lab.total_reads() >= reads_before + 2, "a reading first, then the reading that confirmed it");

    // The same count again does not go up, so nothing shows it worked.
    harness.advance(60_000);
    let prepared = harness.prepare(&cookie, "lab.count", COUNTER, json!({ "count": 4 })).await;
    let done = harness.confirm_tap(&cookie, &prepared).await;
    assert_eq!(done.phase(), "no_response", "{}", done.text);
    assert_eq!(lab.command_count(COUNTER), 2, "never sent twice on its own");
}

#[tokio::test]
async fn a_custom_action_on_a_vm_needs_a_passkey() {
    let (harness, mut auth, cookie, lab) = custom_harness().await;
    let params = json!({ "fresh": true });
    harness
        .execute(&cookie, "lab.reset", LAB_VM, params.clone())
        .await
        .assert_refused(StatusCode::FORBIDDEN, "step_up_required");
    let prepared = harness.prepare(&cookie, "lab.reset", LAB_VM, params.clone()).await;
    assert_eq!(prepared.body["needs"], json!("passkey"), "{}", prepared.text);
    assert_eq!(prepared.body["tier"], json!(3));
    assert_eq!(prepared.body["target_name"], json!("Test lab-vm"));
    assert_eq!(lab.commands().len(), 0);
    let done = harness.critical(&cookie, &mut auth, "lab.reset", LAB_VM, params).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(done.body["step_up"], json!("passkey"));
}

#[tokio::test]
async fn an_action_nothing_can_read_back_ends_sent_like_a_key_press() {
    let (harness, _auth, cookie, lab) = custom_harness().await;
    let done = harness.execute(&cookie, "lab.ping", COUNTER, json!({})).await;
    done.assert_ok();
    assert_eq!(done.phase(), "sent", "{}", done.text);
    assert!(done.body["run_id"].is_null(), "{}", done.text);
    assert_eq!(lab.commands(), [(COUNTER.to_string(), custom_op("lab.ping", &[]))]);
    assert!(harness.audit_has("commanded") && harness.audit_has("sent"), "{:?}", harness.audit_results());
}

#[tokio::test]
async fn a_custom_action_is_the_owners_alone_and_listed_with_the_tier_the_hub_gave_it() {
    let (harness, mut owner, cookie, lab) = custom_harness().await;
    let (_family, family_cookie, _id) = harness.add_family(&cookie, &mut owner, "Alice").await;

    harness
        .execute(&family_cookie, CUSTOM_ACTION, GADGET, json!({ "level": 1 }))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "owner_only");
    let theirs = harness.get("/api/v1/catalog", &family_cookie).await;
    for id in [CUSTOM_ACTION, "lab.count", "lab.reset", "lab.ping"] {
        assert!(catalog_entry(&theirs, id).is_none(), "{id} is offered to family");
    }

    let mine = harness.get("/api/v1/catalog", &cookie).await;
    for (id, tier, target) in [(CUSTOM_ACTION, 1, GADGET), ("lab.count", 2, COUNTER), ("lab.reset", 3, LAB_VM)] {
        let entry = catalog_entry(&mine, id).unwrap_or_else(|| panic!("{id} is not listed: {}", mine.text));
        assert_eq!(entry["tier"], json!(tier), "{id}");
        assert_eq!(entry["targets"], json!([target]), "{id}");
        assert_eq!(entry["requires"], json!("custom"), "{id}");
    }
    let standard = mine.body["actions"].as_array().unwrap().iter().position(|a| a["id"] == json!("agent.pair"));
    let custom = mine.body["actions"].as_array().unwrap().iter().position(|a| a["id"] == json!(CUSTOM_ACTION));
    assert!(standard < custom, "the standard table comes first");
    assert_eq!(harness.devices.total_commands() + lab.commands().len(), 0);
}

#[tokio::test]
async fn voice_and_swifty_never_offer_or_run_a_custom_action() {
    let (harness, _auth, _cookie, lab) = custom_harness().await;

    // Nothing anyone says becomes one.
    let lexicon = voice::lexicon_for(&harness.app).unwrap();
    for text in [
        "set the level of test gadget to 5",
        "test gadget level five",
        "set level seven",
        "mock set level",
        "turn on test gadget",
        "count test counter",
        "ping test counter",
        "reset test lab vm",
        "start the gadget",
    ] {
        let candidates: Vec<&str> = match voice::parse(text, &lexicon).resolution {
            Resolution::Action(candidate) => vec![candidate.action],
            Resolution::Choices { options, .. } => options.iter().map(|option| option.action).collect(),
            _ => Vec::new(),
        };
        for action in candidates {
            assert!(!catalog::find(action).is_some_and(|def| def.is_custom()), "{text:?} became {action}");
        }
    }

    // Neither can run one when asked straight out.
    let params = json!({ "level": 3 });
    let refused =
        actions::execute_on_channel(&harness.app, &owner(), CUSTOM_ACTION, GADGET, &params, Channel::Voice, None)
            .await
            .unwrap_err();
    assert_eq!((refused.status, refused.code), (StatusCode::FORBIDDEN, "not_available_by_voice"));
    let proposed = Parsed {
        normalized: String::new(),
        resolution: Resolution::Action(Candidate {
            action: CUSTOM_ACTION,
            target: GADGET.to_string(),
            params: params.clone(),
            label: "Set the level".to_string(),
        }),
        confidence: 1.0,
    };
    let reply = voice::dispatch(&harness.app, &owner(), proposed, Channel::Assistant).await.unwrap();
    assert_eq!((reply["kind"].clone(), reply["code"].clone()), (json!("refused"), json!("not_available_by_voice")));
    let prepared = actions::prepare_on_channel(
        &harness.app,
        &owner(),
        "lab.count",
        COUNTER,
        &json!({ "count": 9 }),
        Channel::Voice,
    );
    assert_eq!(prepared.unwrap_err().code, "not_available_by_voice");
    assert_eq!(harness.devices.total_commands() + lab.commands().len(), 0);
    assert_eq!(harness.audit_count("refused: not_available_by_voice"), 3);
}

// ----- descriptions from names and notes

#[tokio::test]
async fn a_note_appears_in_the_confirm_description() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let house = legacy_house();
    let note = |target: &str| house.notes.iter().find(|note| note.target == target).unwrap().notes.clone();
    let first_vm = format!("vm{}", TEST_VMS[0].0);

    let prepared = harness.prepare(&cookie, "vm.stop", &first_vm, json!({})).await;
    prepared.assert_ok();
    let consequences: Vec<String> = serde_json::from_value(prepared.body["consequences"].clone()).unwrap();
    for line in note(&first_vm) {
        assert!(consequences.contains(&line), "{line:?} not in {consequences:?}");
    }
    let name = format!("{} · VM {}", TEST_VMS[0].1, TEST_VMS[0].0);
    assert_eq!(prepared.body["target_name"], json!(name));
    assert_eq!(prepared.body["label"], json!(format!("Stop {name}")));

    let host = harness.prepare(&cookie, "host.reboot", "host", json!({})).await;
    let consequences: Vec<String> = serde_json::from_value(host.body["consequences"].clone()).unwrap();
    assert!(consequences.starts_with(&note("host")), "{consequences:?}");
    assert_eq!(host.body["target_name"], json!("Host"));

    // A note the owner writes later is there the next time.
    harness.app.store.with(|conn| notes::set(conn, "vm101", &["Backups land here.".to_string()])).unwrap();
    let prepared = harness.prepare(&cookie, "vm.restart", "vm101", json!({})).await;
    assert!(prepared.text.contains("Backups land here."), "{}", prepared.text);
    // A device with no notes has only what the action itself says.
    let prepared = harness.prepare(&cookie, "vm.start", "vm103", json!({})).await;
    let consequences: Vec<String> = serde_json::from_value(prepared.body["consequences"].clone()).unwrap();
    assert_eq!(consequences, [format!("{} · VM 103 starts.", TEST_VMS[3].1)]);
}

#[tokio::test]
async fn a_renamed_device_is_described_by_its_new_name() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    harness
        .send(
            Method::PUT,
            "/api/v1/devices/vm102/name",
            Some(&cookie),
            Some(ORIGIN),
            Some(json!({ "name": "Build box" })),
        )
        .await
        .assert_ok();
    let prepared = harness.prepare(&cookie, "vm.restart", "vm102", json!({ ACK_PARAM: true })).await;
    assert_eq!(prepared.body["target_name"], json!("Build box"), "{}", prepared.text);
    assert_eq!(prepared.body["label"], json!("Restart Build box"));
    let fed = harness.prepare(&cookie, "pets.feed", "feeder", json!({ "grams": 20 })).await;
    assert_eq!(fed.body["what"], json!("Feeder dispenses 20 g now."), "{}", fed.text);
}

// ----- house scenes from the store

#[tokio::test]
async fn a_house_scene_does_what_its_stored_row_says() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let level = |target: &str| match harness.devices.state(target) {
        Some(State::Light { on, bri, hue, .. }) => (on, bri, hue),
        other => panic!("{target}: {other:?}"),
    };
    let done = harness.execute(&cookie, "scene.apply", "evening", json!({})).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(level("bulb2"), (true, 35, 30), "the legacy house's own level for bulb 2");
    assert!(!level("bulb3").0, "bulb 3 is off in the evening");

    // The owner changes the scene: one level for every light now.
    harness.app.store.with(|conn| scenes::set_levels(conn, "evening", 60, 120, &[])).unwrap();
    let done = harness.execute(&cookie, "scene.apply", "evening", json!({})).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    for bulb in ["bulb1", "bulb2", "bulb3", "bulb4"] {
        assert_eq!(level(bulb), (true, 60, 120), "{bulb}");
    }

    // Movie's input and volume cap are the stored ones too.
    let mut movie = harness.app.store.with(|conn| scenes::get(conn, "movie")).unwrap().unwrap();
    movie.extras = json!({ "tv_input": "HDMI 2", "volume_cap": 15 });
    harness.app.store.with(|conn| scenes::save(conn, &movie)).unwrap();
    let prepared = harness.prepare(&cookie, "scene.movie", "movie", json!({})).await;
    assert!(prepared.body["what"].as_str().unwrap().contains("HDMI 2"), "{}", prepared.text);
    let done = harness.confirm_tap(&cookie, &prepared).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert!(harness.devices.commands().contains(&("tv".to_string(), Op::TvInput { input: "HDMI 2".to_string() })));
    assert!(harness.devices.commands().contains(&("sonos".to_string(), Op::VolumeCap { max: 15 })));

    // A scene that is no longer stored is not run.
    harness.app.store.with(|conn| conn.execute("DELETE FROM scenes WHERE id = 'focus'", [])).unwrap();
    harness
        .execute(&cookie, "scene.apply", "focus", json!({}))
        .await
        .assert_refused(StatusCode::NOT_FOUND, "unknown_scene");
}

#[tokio::test]
async fn movie_without_a_tv_sets_the_lights_and_caps_the_speaker() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    // The house's TV leaves its instance's config.
    harness.set_instance_config("webos", json!({ "devices": [] }));
    assert!(harness.app.registry.index().get("tv").is_none());
    let prepared = harness.prepare(&cookie, "scene.movie", "movie", json!({})).await;
    let done = harness.confirm_tap(&cookie, &prepared).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    let targets: Vec<&str> =
        done.body["steps"].as_array().unwrap().iter().map(|s| s["target"].as_str().unwrap()).collect();
    assert_eq!(targets, ["bulb1", "bulb2", "bulb3", "bulb4", "sonos"]);
    assert_eq!(harness.devices.command_count("tv"), 0);
}

// ----- no house in the code

/// SHA-256 of words and word pairs that name one particular house: its host,
/// machines, printer, gear, share, other projects and the CI runners. Kept as
/// hashes, because a list of the words would be the very thing it guards against.
const SITE_WORD_HASHES: &[&str] = &[
    "934a11e602682e862b4b4d1f881ae8da1e097e3f6964bd780e5bb1fb798a6fdd",
    "6ff9770cf6986977f8ace21d8d41c7f3e503a547ab54e28c392fda3ef8a375f7",
    "751abe89e28f7eaf9f4115705bc9f6a5cc695258ec70d59d7ac9475ee345eb5b",
    "1e59e58928d28e7e77bfb39494f7b49eb1374387086215a5934b21e0bdbe2497",
    "ea7cc4f3381cbb63b00d366f3b84f485c8d9b5155af063f08d357124984f35eb",
    "3e2c9e4840a6855396c6406a861b9b22aad6400e19864b105fcde43791f8ae85",
    "dde7e12aa0621bda49525bba98858d5908387df852f06f435c03f4e8e45fcde5",
    "e0ec026a6108385788ac74b5fd4557ca178e0f48f5c6f7b1d50fa34cbf48d5dd",
    "e7dffb6afb4f1f7ece1720977a57c58de643709b9c387b0a536299d5d7505910",
    "c9cefeb408b8ad318961cbfa2d3b3ae64e332979e773e6e8be5275356a0bfa0f",
    "be8082907c36a5f538e320532446f5f44d7352113fb885584fc917dad72b6f01",
    "0a3fbd78a6169998cf2e5a4dccb6f99656d4036e6567c2cf88c4a0c0fa93fa31",
    "b95a0222eb9f84b88cbe9928bb1f4a5c1ec89aa182076ffacd2378da4f23ac8f",
    "8c6e8e8b9a0eb98305639756502e98756b16f2ee31ce90c2df072ca2e354ec08",
    "7804a56a5c7636cc05814736f44139e32920810d3bd51aa099a5df932e754ce9",
    "a0c0485eff9c817deee7190edf264622b1677fc992920657411b36a6aed48f83",
    "340d600392818df2413382dc7d8325c360d83ea49a262d31760348484bbc10b5",
    "82ba7e1f6f4171d3648bb4f785ca31992efe32df6e024acb5bccd297feb2f263",
    "3d74d988e6ff0854c5925d5f2db5c7629cd29bf7da1ba1cb6b243da0d3f337fb",
    "beebceee49c6bd9bea9c0adcec34f48a668823f2960d9634ce6ea3988596bfdc",
    "d1c3778f53dcb2eb4fbe4c78980d06fec781613fbb5c4b154a098022cd7b1e6e",
    "48676410c5766d8a3dd1ade1d89c3c31b7c2ed38705fd20cc3adf197890d27a3",
    "22b74ae7a23605500b1aa2eddefe404d89712b026a0f4bcaacca140548c7338f",
    "c3045967bf04fc0fed47790274ef176352b391f295b6ca1b94c896d762e12d4c",
    "7f7568634f23e9fe4347bfd5917d8aef5ab21d629f7f972659d6c92baf54a39c",
];

#[test]
fn the_catalog_names_no_house() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../swifty-hub/src");
    let mut files: Vec<_> =
        std::fs::read_dir(src.join("catalog")).unwrap().map(|entry| entry.unwrap().path()).collect();
    assert!(files.len() >= 5, "{files:?}");
    // The engine and the rooms, which word what happened and plan the room scenes.
    files.extend([src.join("actions.rs"), src.join("rooms.rs")]);
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap().to_lowercase();
        for (number, line) in text.lines().enumerate() {
            let words: Vec<&str> =
                line.split(|c: char| !c.is_ascii_alphanumeric()).filter(|word| !word.is_empty()).collect();
            let pairs = words.windows(2).map(|pair| pair.join(" "));
            for word in words.iter().map(|word| word.to_string()).chain(pairs) {
                let hash = swifty_hub::crypto::sha256_hex(word.as_bytes());
                assert!(
                    !SITE_WORD_HASHES.contains(&hash.as_str()),
                    "{}:{} names something of one particular house",
                    file.display(),
                    number + 1
                );
            }
        }
    }
    // And the device table names no gear: every name is plain words.
    for device in legacy_house().devices {
        assert!(device.name.chars().all(|c| c.is_ascii_alphanumeric() || c == ' '), "{}", device.name);
    }
}
