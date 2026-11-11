//! Adding, setting up again and removing an integration:
//! the lifecycle actions through the action engine, the setup sessions they open,
//! and what a finished, failed or abandoned setup leaves behind.
//!
//! The setups run against the mock plugin, the Hue plugin with a mock bridge, and a
//! small plugin of this test's own (`gizmo`) whose flow waits, panics or hands back
//! something wrong on request, so every path of the engine is driven without a
//! network.

mod common;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{json, Value};
use swifty_hub::events::Event;
use swifty_plugin_api::{
    Capability, Choice, ConfigProblem, DeviceClass, DeviceDecl, Driver, Field, FieldType, InstanceView, LiveContext,
    Manifest, NetUse, Op, Plugin, PluginError, Reach, Readiness, Scalar, SecretSlot, SetupContext, SetupFlow,
    SetupInput, SetupResult, SetupStep, API_VERSION,
};
use swifty_plugin_testkit::devices::{hue_bridge_handler, HueLight, MockHttp, SharedBridge, HUE_KEY};
use webauthn_authenticator_rs::softpasskey::SoftPasskey;

// ----- a plugin of this test's own

/// The instances whose pretend button was pressed.
static PRESSED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

const GIZMO_TOKEN_SLOT: &str = "token";

static GIZMO_MANIFEST: Manifest = Manifest {
    id: "gizmo",
    name: "Gizmo",
    version: "0.1.0",
    api: API_VERSION,
    summary: "Pretend sockets that only exist in this test.",
    reach: Reach::Cloud { vendor: "Gizmo Inc.", official: false },
    network: &[NetUse::CloudHost { host: "api.example.org" }],
    classes: &[DeviceClass::Plug],
    config_version: 1,
    config: &[],
    secrets: &[SecretSlot { slot: GIZMO_TOKEN_SLOT, label: "Token", help: "Any text.", required: true }],
    actions: &[],
    vocabulary: &[],
    nouns: &[],
    multi_instance: true,
};

struct Gizmo;

static GIZMO: Gizmo = Gizmo;

fn socket(id: &str) -> DeviceDecl {
    DeviceDecl {
        local_id: id.to_string(),
        target_hint: None,
        class: DeviceClass::Plug,
        name: format!("Socket {id}"),
        capabilities: vec![Capability::Power],
        attrs: json!({ "address": "192.0.2.50" }),
    }
}

impl Plugin for Gizmo {
    fn manifest(&self) -> &'static Manifest {
        &GIZMO_MANIFEST
    }
    fn validate(&self, config: &Value) -> Result<(), ConfigProblem> {
        match config.get("name").and_then(Value::as_str) {
            Some(name) if !name.is_empty() && name != "refuse" => Ok(()),
            _ => Err(ConfigProblem { field: "name".into(), message: "must be a name the gizmo takes".into() }),
        }
    }
    fn readiness(&self, instance: &InstanceView<'_>) -> Readiness {
        if instance.secrets_present.contains(GIZMO_TOKEN_SLOT) {
            Readiness::Ready
        } else {
            Readiness::NeedsSetup("No token yet.".into())
        }
    }
    fn devices(&self, instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
        instance
            .adopted
            .iter()
            .map(|record| DeviceDecl {
                name: record.name.clone(),
                attrs: record.attrs.clone(),
                ..socket(&record.local_id)
            })
            .collect()
    }
    fn refusal(&self, instance: &InstanceView<'_>, device: &str, _op: &Op) -> Option<String> {
        (!instance.adopted.iter().any(|record| record.local_id == device)).then(|| "Not a gizmo here.".to_string())
    }
    fn connect(&self, _ctx: LiveContext) -> Result<Arc<dyn Driver>, PluginError> {
        Err(PluginError("The gizmo cloud is pretend.".into()))
    }
    fn setup(&self) -> Option<Box<dyn SetupFlow>> {
        Some(Box::<GizmoSetup>::default())
    }
}

#[derive(Default)]
struct GizmoSetup {
    name: String,
    typed: String,
}

impl GizmoSetup {
    fn form() -> SetupStep {
        let field = |key: &str, kind: FieldType, required: bool| Field {
            key: key.into(),
            label: key.into(),
            help: String::new(),
            kind,
            required,
            value: None,
        };
        SetupStep::Form {
            title: "Gizmo".into(),
            text: "A name, and the token.".into(),
            fields: vec![
                field("name", FieldType::Text { max_len: 32 }, true),
                field("token", FieldType::Secret, false),
            ],
        }
    }

    fn wait() -> SetupStep {
        SetupStep::Wait { title: "Press".into(), text: "Press the gizmo's button.".into(), poll_ms: 250, timeout_s: 30 }
    }

    fn done(&self, adopt: Vec<DeviceDecl>) -> SetupStep {
        let slot = if self.name == "rogue" { "other" } else { GIZMO_TOKEN_SLOT };
        let handed: Vec<(String, swifty_plugin_api::Secret)> =
            if self.typed.is_empty() { Vec::new() } else { vec![(slot.to_string(), self.typed.clone().into())] };
        SetupStep::Done(SetupResult {
            config: json!({ "name": self.name }),
            secrets: handed,
            adopt,
            summary: format!("Gizmo {} is set up.", self.name),
        })
    }
}

#[async_trait]
impl SetupFlow for GizmoSetup {
    async fn begin(&mut self, _ctx: &SetupContext, existing: Option<&Value>) -> SetupStep {
        self.name = existing.and_then(|config| config["name"].as_str()).unwrap_or_default().to_string();
        Self::form()
    }

    async fn advance(&mut self, ctx: &SetupContext, input: SetupInput) -> SetupStep {
        match input {
            SetupInput::Values { values } => {
                self.name = match values.get("name") {
                    Some(Scalar::Text(name)) => name.clone(),
                    _ => String::new(),
                };
                self.typed = match values.get("token") {
                    Some(Scalar::Text(text)) => text.clone(),
                    _ => String::new(),
                };
                match self.name.as_str() {
                    "panic" => panic!("the gizmo's setup fell over"),
                    "leak" => SetupStep::Failed { reason: format!("The cloud refused {}.", self.typed), retry: true },
                    "rogue" | "refuse" => self.done(Vec::new()),
                    _ => Self::wait(),
                }
            }
            SetupInput::Poll if PRESSED.lock().unwrap().contains(&ctx.instance_id) => SetupStep::Choose {
                title: "Sockets".into(),
                text: "Pick the sockets.".into(),
                options: ["plug-a", "plug-b"]
                    .iter()
                    .map(|id| Choice {
                        id: id.to_string(),
                        label: id.to_string(),
                        detail: String::new(),
                        selected: true,
                    })
                    .collect(),
                multiple: true,
                min: 0,
            },
            SetupInput::Poll => Self::wait(),
            SetupInput::Chosen { ids } => self.done(ids.iter().map(|id| socket(id)).collect()),
            SetupInput::Back => Self::form(),
        }
    }
}

/// The test house with the gizmo plugin in the build, and a signed-in owner.
async fn with_gizmo() -> (Harness, SoftPasskey, String) {
    let harness = Harness::with_plugins(vec![&GIZMO], |_| {});
    let (auth, cookie) = harness.sign_in().await;
    (harness, auth, cookie)
}

fn press(instance: &str) {
    PRESSED.lock().unwrap().insert(instance.to_string());
}

// ----- helpers

fn setup_path(id: &str) -> String {
    format!("/api/v1/setup/{id}")
}

/// Opens a setup with the full critical flow and returns its id and the answer.
async fn open(harness: &Harness, cookie: &str, auth: &mut SoftPasskey, params: Value) -> (String, Value) {
    let confirmed = harness.critical(cookie, auth, "plugin.add", "new", params).await;
    confirmed.assert_ok();
    assert_eq!(confirmed.phase(), "confirmed", "{}", confirmed.text);
    let setup = confirmed.body["setup"].clone();
    (setup["setup_id"].as_str().unwrap().to_string(), setup)
}

async fn send(harness: &Harness, cookie: &str, id: &str, input: Value) -> Resp {
    harness.post(&setup_path(id), cookie, input).await
}

fn values(pairs: Value) -> Value {
    json!({ "input": "values", "values": pairs })
}

fn chosen(ids: &[&str]) -> Value {
    json!({ "input": "chosen", "ids": ids })
}

fn step_kind(answer: &Value) -> &str {
    answer["step"]["step"].as_str().unwrap_or("")
}

fn instance_rows(harness: &Harness, id: &str) -> i64 {
    harness
        .app
        .store
        .with(|conn| conn.query_row("SELECT COUNT(*) FROM plugin_instances WHERE id = ?1", [id], |row| row.get(0)))
        .unwrap()
}

fn device_rows(harness: &Harness, instance: &str) -> Vec<String> {
    harness
        .app
        .store
        .with(|conn| {
            let mut statement = conn.prepare("SELECT target FROM devices WHERE instance_id = ?1 ORDER BY target")?;
            let rows = statement.query_map([instance], |row| row.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<String>>>()
        })
        .unwrap()
}

fn slots(harness: &Harness, instance: &str) -> Vec<String> {
    harness.app.registry.secret_store().slots(instance).unwrap().into_iter().collect()
}

/// The params of every audit row with this action and result.
fn audit_params(harness: &Harness, action: &str, result: &str) -> Vec<Value> {
    harness
        .app
        .store
        .with(|conn| {
            let mut statement =
                conn.prepare("SELECT params FROM audit_log WHERE action = ?1 AND result = ?2 ORDER BY seq")?;
            let rows = statement.query_map([action, result], |row| row.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<String>>>()
        })
        .unwrap()
        .iter()
        .map(|text| serde_json::from_str(text).unwrap())
        .collect()
}

/// Waits until the setup shows a step of this kind, asking every 50 ms for up to `seconds`.
async fn until_step(harness: &Harness, cookie: &str, id: &str, kind: &str, seconds: u64) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    loop {
        let shown = harness.get(&setup_path(id), cookie).await;
        shown.assert_ok();
        if step_kind(&shown.body) == kind {
            return shown.body;
        }
        assert!(tokio::time::Instant::now() < deadline, "still {} after {seconds} s", shown.text);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn setup_events(harness: &Harness, id: &str) -> usize {
    harness
        .app
        .events
        .since(0)
        .unwrap()
        .iter()
        .filter(|published| matches!(&published.event, Event::Setup { setup_id, .. } if setup_id == id))
        .count()
}

// ----- the step-up

#[tokio::test]
async fn adding_an_integration_needs_a_passkey_step_up_and_the_owner() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let params = json!({ "plugin": "mock", "instance": "demo" });

    let direct = harness.execute(&cookie, "plugin.add", "new", params.clone()).await;
    direct.assert_refused(StatusCode::FORBIDDEN, "step_up_required");

    let prepared = harness.prepare(&cookie, "plugin.add", "new", params.clone()).await;
    prepared.assert_ok();
    assert_eq!(prepared.body["needs"], json!("passkey"));
    assert_eq!(prepared.body["label"], json!("Add Mock devices"), "{}", prepared.text);
    let consequences = prepared.body["consequences"].to_string();
    assert!(consequences.contains("only to devices on your network"), "{consequences}");
    assert!(consequences.contains("Nothing is stored until the setup is finished"), "{consequences}");

    let unapproved = harness
        .post(
            "/api/v1/actions/confirm",
            &cookie,
            json!({
                "prepared_id": prepared.body["prepared_id"],
                "action": "plugin.add",
                "target": "new",
                "params": prepared.body["params"],
            }),
        )
        .await;
    unapproved.assert_refused(StatusCode::PRECONDITION_REQUIRED, "step_up_required");
    assert_eq!(harness.app.setup.open_count(&harness.app), 0, "no setup opened without the passkey");

    let (_family, family_cookie, _id) = harness.add_family(&cookie, &mut owner, "Alice").await;
    harness
        .prepare(&family_cookie, "plugin.add", "new", params.clone())
        .await
        .assert_refused(StatusCode::FORBIDDEN, "owner_only");

    let (id, setup) = open(&harness, &cookie, &mut owner, params).await;
    assert_eq!(setup["instance"], json!("demo"));
    assert_eq!(setup["purpose"], json!("add"));
    assert_eq!(step_kind(&setup), "choose");
    harness.get(&setup_path(&id), &family_cookie).await.assert_refused(StatusCode::FORBIDDEN, "owner_only");
    assert!(harness.audit_has("opened"));
}

// ----- the mock plugin, end to end

#[tokio::test]
async fn the_mock_setup_reaches_done_and_stores_the_instance_its_secret_and_its_devices() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let (id, setup) = open(&harness, &cookie, &mut owner, json!({ "plugin": "mock", "instance": "demo" })).await;
    assert_eq!(setup["plugin_name"], json!("Mock devices"));
    assert_eq!(setup["finished"], json!(false));
    assert!(setup["expires_at"].as_i64().unwrap() > harness.now());

    let form = send(&harness, &cookie, &id, chosen(&["lamp", "plug"])).await;
    form.assert_ok();
    assert_eq!(step_kind(&form.body), "form", "{}", form.text);
    assert_eq!(instance_rows(&harness, "demo"), 0, "nothing is stored before done");

    const CODE: &str = "demo-access-code-4711";
    let done = send(&harness, &cookie, &id, values(json!({ "access_code": CODE }))).await;
    done.assert_ok();
    assert_eq!(done.body["step"], json!({ "step": "done", "summary": "2 demo devices added." }), "{}", done.text);
    assert_eq!(done.body["finished"], json!(true));
    assert!(!done.text.contains(CODE));

    // The instance, its secret slot and its devices.
    assert_eq!(instance_rows(&harness, "demo"), 1);
    assert_eq!(slots(&harness, "demo"), ["access_code"]);
    assert_eq!(device_rows(&harness, "demo"), ["demo-lamp", "demo-plug"]);
    let devices = harness.get("/api/v1/devices", &cookie).await;
    let listed: Vec<&str> =
        devices.body["devices"].as_array().unwrap().iter().filter_map(|d| d["target"].as_str()).collect();
    assert!(listed.contains(&"demo-lamp") && listed.contains(&"demo-plug"), "{}", devices.text);
    let on = harness.execute(&cookie, "light.power", "demo-lamp", json!({ "on": true })).await;
    on.assert_ok();
    assert_eq!(on.phase(), "confirmed", "the new light works at once: {}", on.text);

    // The audit row names the slot and never the value.
    let rows = audit_params(&harness, "plugin.setup", "done");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["instance"], json!("demo"));
    assert_eq!(rows[0]["plugin"], json!("mock"));
    assert_eq!(rows[0]["secrets"], json!(["access_code"]));
    assert_eq!(rows[0]["devices"], json!(2));
    assert!(rows[0]["version"].is_string());
    for table in ["audit_log", "plugin_instances", "devices", "action_runs", "prepared_actions"] {
        assert!(!harness.dump(table).contains(CODE), "{table} holds the code");
    }
    // Finished: it takes no more input, and is still shown to its session.
    send(&harness, &cookie, &id, json!({ "input": "back" }))
        .await
        .assert_refused(StatusCode::CONFLICT, "setup_finished");
    let shown = harness.get(&setup_path(&id), &cookie).await;
    assert_eq!(step_kind(&shown.body), "done");
}

#[tokio::test]
async fn a_second_session_cannot_see_or_drive_the_setup() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let (id, _) = open(&harness, &cookie, &mut owner, json!({ "plugin": "mock", "instance": "demo" })).await;
    let other = harness.login(&mut owner).await;
    assert_ne!(other, cookie);

    harness.get(&setup_path(&id), &other).await.assert_refused(StatusCode::FORBIDDEN, "wrong_session");
    send(&harness, &other, &id, chosen(&["lamp"])).await.assert_refused(StatusCode::FORBIDDEN, "wrong_session");
    harness
        .send(Method::DELETE, &setup_path(&id), Some(&other), Some(ORIGIN), None)
        .await
        .assert_refused(StatusCode::FORBIDDEN, "wrong_session");
    assert!(harness.audit_count("refused: wrong_session") >= 3);

    // The session that opened it still can, and the other's attempts changed nothing.
    let shown = harness.get(&setup_path(&id), &cookie).await;
    assert_eq!(step_kind(&shown.body), "choose");
    send(&harness, &cookie, &id, chosen(&["lamp"])).await.assert_ok();
    harness.get(&setup_path("not-a-setup"), &cookie).await.assert_refused(StatusCode::NOT_FOUND, "unknown_setup");
}

#[tokio::test]
async fn an_abandoned_setup_leaves_nothing_behind() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let (id, _) = open(&harness, &cookie, &mut owner, json!({ "plugin": "mock", "instance": "demo" })).await;
    send(&harness, &cookie, &id, chosen(&["lamp", "tv"])).await.assert_ok();

    let abandoned = harness.send(Method::DELETE, &setup_path(&id), Some(&cookie), Some(ORIGIN), None).await;
    abandoned.assert_ok();
    assert_eq!(abandoned.body["abandoned"], json!(true));
    harness.get(&setup_path(&id), &cookie).await.assert_refused(StatusCode::NOT_FOUND, "unknown_setup");
    assert_eq!(instance_rows(&harness, "demo"), 0);
    assert!(device_rows(&harness, "demo").is_empty());
    assert!(slots(&harness, "demo").is_empty());
    assert!(harness.audit_has("abandoned"));
    assert_eq!(harness.app.setup.open_count(&harness.app), 0);

    // The same id can be set up at once.
    let (again, _) = open(&harness, &cookie, &mut owner, json!({ "plugin": "mock", "instance": "demo" })).await;
    assert_ne!(again, id);
}

#[tokio::test]
async fn every_input_is_checked_against_its_step_before_the_plugin_sees_it() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let (id, _) = open(&harness, &cookie, &mut owner, json!({ "plugin": "mock", "instance": "demo" })).await;

    for (input, code) in [
        (json!({ "input": "poll" }), "invalid_input"),
        (values(json!({ "access_code": "x" })), "invalid_input"),
        (chosen(&["lamp", "toaster"]), "invalid_input"),
        (chosen(&[]), "invalid_input"),
        (json!({ "input": "chosen", "ids": ["lamp"], "extra": true }), "invalid_input"),
        (json!({ "input": "teleport" }), "invalid_input"),
    ] {
        send(&harness, &cookie, &id, input.clone()).await.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, code);
    }
    let form = send(&harness, &cookie, &id, chosen(&["lamp"])).await;
    assert_eq!(step_kind(&form.body), "form");
    let unknown = send(&harness, &cookie, &id, values(json!({ "colour": "red" }))).await;
    unknown.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_input");
    assert_eq!(unknown.body["error"]["detail"]["field"], json!("colour"));
    let too_long = send(&harness, &cookie, &id, values(json!({ "access_code": "a\nb" }))).await;
    too_long.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_input");
    assert!(!too_long.text.contains("a\\nb"), "a secret is never echoed: {}", too_long.text);
    // Back is always possible, and the flow goes on from there.
    let back = send(&harness, &cookie, &id, json!({ "input": "back" })).await;
    assert_eq!(step_kind(&back.body), "choose");
}

// ----- choosing what to add

#[tokio::test]
async fn the_new_integration_gets_a_free_id_and_a_single_one_cannot_be_added_twice() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    // The test house has an instance called `hue` already (of the mock).
    let (_id, setup) = open(&harness, &cookie, &mut owner, json!({ "plugin": "hue" })).await;
    assert_eq!(setup["instance"], json!("hue2"));
    let (_id, setup) = open(&harness, &cookie, &mut owner, json!({ "plugin": "hue" })).await;
    assert_eq!(setup["instance"], json!("hue3"), "an id an open setup holds is taken too");

    for (params, status, code) in [
        (json!({ "plugin": "toaster" }), StatusCode::NOT_FOUND, "unknown_plugin"),
        (json!({ "plugin": "mock", "instance": "new" }), StatusCode::UNPROCESSABLE_ENTITY, "invalid_params"),
        (json!({ "plugin": "mock", "instance": "Demo" }), StatusCode::UNPROCESSABLE_ENTITY, "invalid_params"),
        (json!({ "plugin": "mock", "instance": "sonos" }), StatusCode::CONFLICT, "instance_exists"),
        (json!({ "plugin": "hue", "instance": "hue2" }), StatusCode::CONFLICT, "setup_open"),
        (json!({ "plugin": "mock", "extra": 1 }), StatusCode::UNPROCESSABLE_ENTITY, "invalid_params"),
    ] {
        harness.prepare(&cookie, "plugin.add", "new", params.clone()).await.assert_refused(status, code);
    }

    // WiZ takes one instance; once there is one, it is set up again instead.
    harness.add_instance("lights", "wiz", json!({ "broadcast": "192.0.2.255:38899" }));
    let refused = harness.prepare(&cookie, "plugin.add", "new", json!({ "plugin": "wiz" })).await;
    refused.assert_refused(StatusCode::CONFLICT, "instance_exists");
    assert!(refused.text.contains("lights"), "{}", refused.text);
}

#[tokio::test]
async fn at_most_four_setups_are_open_and_one_left_alone_is_forgotten() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let mut ids = Vec::new();
    for instance in ["one", "two", "three", "four"] {
        ids.push(open(&harness, &cookie, &mut owner, json!({ "plugin": "mock", "instance": instance })).await.0);
    }
    let fifth = harness.prepare(&cookie, "plugin.add", "new", json!({ "plugin": "mock", "instance": "five" })).await;
    fifth.assert_refused(StatusCode::CONFLICT, "setup_busy");

    // A finished one does not count.
    send(&harness, &cookie, &ids[0], chosen(&["lamp"])).await.assert_ok();
    send(&harness, &cookie, &ids[0], values(json!({}))).await.assert_ok();
    open(&harness, &cookie, &mut owner, json!({ "plugin": "mock", "instance": "five" })).await;

    // Fifteen minutes alone and they are gone; nothing they held was stored.
    harness.advance(15 * MINUTE_MS + 1);
    harness.get(&setup_path(&ids[1]), &cookie).await.assert_refused(StatusCode::NOT_FOUND, "unknown_setup");
    assert_eq!(harness.app.setup.open_count(&harness.app), 0);
    assert_eq!(instance_rows(&harness, "two"), 0);
}

// ----- the Hue plugin and its bridge

struct PairingBridge {
    http: MockHttp,
    button: Arc<AtomicBool>,
}

/// A bridge with two lights, which hands out [`HUE_KEY`] once its button is pressed.
async fn pairing_bridge() -> PairingBridge {
    let button = Arc::new(AtomicBool::new(false));
    let bridge = SharedBridge::default();
    {
        let mut shared = bridge.lock().unwrap();
        shared.lights.insert("1".to_string(), HueLight::colour("Lightstrip"));
        shared.lights.insert("2".to_string(), HueLight::white("Hall"));
    }
    let (pressed, lights) = (button.clone(), hue_bridge_handler(bridge));
    let http = MockHttp::start(move |seen| {
        if seen.method == "POST" && seen.path == "/api" {
            return if pressed.load(Ordering::SeqCst) {
                (200, json!([{ "success": { "username": HUE_KEY } }]).to_string())
            } else {
                let early =
                    json!([{ "error": { "type": 101, "address": "", "description": "link button not pressed" } }]);
                (200, early.to_string())
            };
        }
        lights(seen)
    })
    .await;
    PairingBridge { http, button }
}

#[tokio::test]
async fn the_hue_setup_pairs_with_the_bridge_and_its_lights_work_at_once() {
    let bridge = pairing_bridge().await;
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let (id, setup) = open(&harness, &cookie, &mut owner, json!({ "plugin": "hue", "instance": "bridge" })).await;
    assert_eq!(step_kind(&setup), "form");

    let wait = send(&harness, &cookie, &id, values(json!({ "url": bridge.http.url }))).await;
    wait.assert_ok();
    assert_eq!(step_kind(&wait.body), "wait", "{}", wait.text);
    let early = send(&harness, &cookie, &id, json!({ "input": "poll" })).await;
    assert_eq!(step_kind(&early.body), "wait", "the button is not pressed yet: {}", early.text);

    bridge.button.store(true, Ordering::SeqCst);
    let choose = send(&harness, &cookie, &id, json!({ "input": "poll" })).await;
    assert_eq!(step_kind(&choose.body), "choose", "{}", choose.text);
    assert_eq!(choose.body["step"]["options"].as_array().unwrap().len(), 2);

    let done = send(&harness, &cookie, &id, chosen(&["hue-1"])).await;
    assert_eq!(done.body["step"], json!({ "step": "done", "summary": "Paired. 1 light added." }), "{}", done.text);
    assert!(!done.text.contains(HUE_KEY));

    assert_eq!(slots(&harness, "bridge"), ["app_key"]);
    let instance = harness.get("/api/v1/instances/bridge", &cookie).await;
    instance.assert_ok();
    assert_eq!(instance.body["readiness"], json!({ "status": "ready" }), "{}", instance.text);
    assert_eq!(instance.body["config"], json!({ "url": bridge.http.url }));
    let targets: Vec<&str> =
        instance.body["devices"].as_array().unwrap().iter().filter_map(|d| d["target"].as_str()).collect();
    assert_eq!(targets.len(), 2, "the bridge and the light: {}", instance.text);
    let light = targets.iter().find(|target| target.ends_with("hue-1")).unwrap();

    // The instance was connected with its key: the light reads through the bridge.
    let state = harness.get(&format!("/api/v1/state/{light}"), &cookie).await;
    assert_eq!(state.body["available"], json!(true), "{}", state.text);
    let on = harness.execute(&cookie, "light.power", light, json!({ "on": true })).await;
    assert_eq!(on.phase(), "confirmed", "{}", on.text);

    let rows = audit_params(&harness, "plugin.setup", "done");
    assert_eq!(rows[0]["secrets"], json!(["app_key"]));
    assert_eq!(rows[0]["devices"], json!(2));
    assert!(!harness.dump("audit_log").contains(HUE_KEY));
}

#[tokio::test]
async fn the_hub_polls_a_waiting_step_and_tells_the_session_when_it_moves_on() {
    let bridge = pairing_bridge().await;
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let (id, _) = open(&harness, &cookie, &mut owner, json!({ "plugin": "hue", "instance": "bridge" })).await;
    send(&harness, &cookie, &id, values(json!({ "url": bridge.http.url }))).await.assert_ok();
    bridge.button.store(true, Ordering::SeqCst);
    // Nobody sends a poll: the hub asks the bridge every two seconds by itself.
    until_step(&harness, &cookie, &id, "choose", 10).await;
    assert_eq!(setup_events(&harness, &id), 1, "the owning session is told once");
}

#[tokio::test]
async fn a_try_again_after_the_hubs_timeout_pairs_when_the_button_is_pressed_then() {
    let bridge = pairing_bridge().await;
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let (id, _) = open(&harness, &cookie, &mut owner, json!({ "plugin": "hue", "instance": "bridge" })).await;
    send(&harness, &cookie, &id, values(json!({ "url": bridge.http.url }))).await.assert_ok();
    // The person waits, and the bridge is asked more often than the plugin's wait holds polls at its own pace
    // (the hub asks every two seconds for ninety).
    for number in 1..=60 {
        let early = send(&harness, &cookie, &id, json!({ "input": "poll" })).await;
        assert_eq!(step_kind(&early.body), "wait", "poll {number}: {}", early.text);
    }
    // The hub's clock runs out: it ends the wait by itself, and offers the wait again.
    harness.advance(91_000);
    let failed = until_step(&harness, &cookie, &id, "failed", 5).await;
    assert_eq!(failed["step"]["retry"], json!(true));
    assert!(failed["step"]["reason"].as_str().unwrap().contains("in time"), "{failed}");
    assert_eq!(failed["retry"]["step"], json!("wait"));

    // Try again: the same wait, and the press that comes now pairs the bridge.
    let again = send(&harness, &cookie, &id, json!({ "input": "poll" })).await;
    assert_eq!(step_kind(&again.body), "wait", "the first Try again waits again: {}", again.text);
    bridge.button.store(true, Ordering::SeqCst);
    let choose = send(&harness, &cookie, &id, json!({ "input": "poll" })).await;
    assert_eq!(step_kind(&choose.body), "choose", "{}", choose.text);
    let done = send(&harness, &cookie, &id, chosen(&["hue-1"])).await;
    assert_eq!(done.body["step"]["summary"], json!("Paired. 1 light added."), "{}", done.text);
    assert_eq!(slots(&harness, "bridge"), ["app_key"]);
}

#[tokio::test]
async fn a_hue_setup_abandoned_after_pairing_keeps_nothing_and_asks_the_bridge_no_more() {
    let bridge = pairing_bridge().await;
    bridge.button.store(true, Ordering::SeqCst);
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let (id, _) = open(&harness, &cookie, &mut owner, json!({ "plugin": "hue", "instance": "bridge" })).await;
    send(&harness, &cookie, &id, values(json!({ "url": bridge.http.url }))).await.assert_ok();
    let choose = send(&harness, &cookie, &id, json!({ "input": "poll" })).await;
    assert_eq!(step_kind(&choose.body), "choose", "paired: the key is in the flow only: {}", choose.text);

    let abandoned = harness.send(Method::DELETE, &setup_path(&id), Some(&cookie), Some(ORIGIN), None).await;
    assert_eq!(abandoned.body["abandoned"], json!(true));
    assert_eq!(instance_rows(&harness, "bridge"), 0);
    assert!(slots(&harness, "bridge").is_empty(), "the key the bridge gave went with the setup");
    assert!(device_rows(&harness, "bridge").is_empty());
    assert!(!harness.dump("audit_log").contains(HUE_KEY));

    // Abandoned while it waits for the button, its poll stops too.
    bridge.button.store(false, Ordering::SeqCst);
    let (id, _) = open(&harness, &cookie, &mut owner, json!({ "plugin": "hue", "instance": "bridge" })).await;
    let wait = send(&harness, &cookie, &id, values(json!({ "url": bridge.http.url }))).await;
    assert_eq!(step_kind(&wait.body), "wait");
    let asked = bridge.http.traffic();
    harness.send(Method::DELETE, &setup_path(&id), Some(&cookie), Some(ORIGIN), None).await.assert_ok();
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert_eq!(bridge.http.traffic(), asked, "nothing polls an abandoned setup");
}

#[tokio::test]
async fn in_dry_run_a_form_only_setup_completes_and_a_pairing_one_fails_without_a_packet() {
    let bridge = pairing_bridge().await;
    bridge.button.store(true, Ordering::SeqCst);
    let (harness, mut owner, cookie) = Harness::signed_in_with(|config| config.live = false).await;

    let (id, _) = open(&harness, &cookie, &mut owner, json!({ "plugin": "mock", "instance": "demo" })).await;
    send(&harness, &cookie, &id, chosen(&["lamp"])).await.assert_ok();
    let done = send(&harness, &cookie, &id, values(json!({}))).await;
    assert_eq!(step_kind(&done.body), "done", "{}", done.text);
    assert_eq!(device_rows(&harness, "demo"), ["demo-lamp"]);

    let (id, _) = open(&harness, &cookie, &mut owner, json!({ "plugin": "hue", "instance": "bridge" })).await;
    let failed = send(&harness, &cookie, &id, values(json!({ "url": bridge.http.url }))).await;
    failed.assert_ok();
    assert_eq!(failed.body["step"]["step"], json!("failed"), "{}", failed.text);
    assert_eq!(failed.body["step"]["retry"], json!(false));
    assert!(failed.body["step"]["reason"].as_str().unwrap().contains("needs live mode"), "{}", failed.text);
    assert_eq!(failed.body["finished"], json!(true));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(bridge.http.traffic(), 0, "dry-run sends nothing");
    assert_eq!(instance_rows(&harness, "bridge"), 0);
    assert!(harness.audit_has("failed"));
}

// ----- setting up again and removing

async fn demo_with_code(harness: &Harness, cookie: &str, owner: &mut SoftPasskey, code: &str) {
    let (id, _) = open(harness, cookie, owner, json!({ "plugin": "mock", "instance": "demo" })).await;
    send(harness, cookie, &id, chosen(&["lamp", "plug"])).await.assert_ok();
    let done = send(harness, cookie, &id, values(json!({ "access_code": code }))).await;
    assert_eq!(step_kind(&done.body), "done", "{}", done.text);
}

#[tokio::test]
async fn setting_an_integration_up_again_starts_from_its_config_and_keeps_its_secret() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    demo_with_code(&harness, &cookie, &mut owner, "first-code-1234").await;

    let prepared = harness.prepare(&cookie, "plugin.reconfigure", "demo", json!({})).await;
    prepared.assert_ok();
    assert_eq!(prepared.body["label"], json!("Set up demo again"));
    let confirmed = harness.critical(&cookie, &mut owner, "plugin.reconfigure", "demo", json!({})).await;
    assert_eq!(confirmed.phase(), "confirmed", "{}", confirmed.text);
    let setup = &confirmed.body["setup"];
    assert_eq!(setup["purpose"], json!("reconfigure"));
    let id = setup["setup_id"].as_str().unwrap();
    let picked: Vec<&str> = setup["step"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|option| option["selected"] == json!(true))
        .filter_map(|option| option["id"].as_str())
        .collect();
    assert_eq!(picked, ["lamp", "plug"], "it starts from what is stored");

    let form = send(&harness, &cookie, id, chosen(&["lamp", "speaker"])).await;
    assert!(form.text.contains("keep the one that is stored"), "{}", form.text);
    let done = send(&harness, &cookie, id, values(json!({ "access_code": "" }))).await;
    assert_eq!(step_kind(&done.body), "done", "{}", done.text);
    assert_eq!(device_rows(&harness, "demo"), ["demo-lamp", "demo-speaker"], "the dropped socket went");
    assert_eq!(slots(&harness, "demo"), ["access_code"], "an empty secret field keeps the stored one");
    let rows = audit_params(&harness, "plugin.setup", "done");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1]["secrets"], json!([]), "nothing was written the second time");

    for (target, status, code) in [
        ("nothing", StatusCode::UNPROCESSABLE_ENTITY, "invalid_target"),
        ("Not An Id", StatusCode::UNPROCESSABLE_ENTITY, "invalid_target"),
    ] {
        harness.prepare(&cookie, "plugin.reconfigure", target, json!({})).await.assert_refused(status, code);
    }
}

#[tokio::test]
async fn removing_an_integration_takes_its_devices_places_favourites_and_secrets_and_keeps_the_audit() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    demo_with_code(&harness, &cookie, &mut owner, "demo-code-9876").await;
    let room = harness.post("/api/v1/rooms", &cookie, json!({ "name": "Den" })).await;
    let room_id = room.body["room"]["id"].as_str().unwrap().to_string();
    harness
        .send(
            Method::PUT,
            &format!("/api/v1/rooms/{room_id}/devices"),
            Some(&cookie),
            Some(ORIGIN),
            Some(json!({ "targets": ["demo-lamp", "bulb1"] })),
        )
        .await
        .assert_ok();
    harness
        .send(
            Method::PUT,
            "/api/v1/me/favourites",
            Some(&cookie),
            Some(ORIGIN),
            Some(json!({ "targets": ["demo-plug", "bulb1"] })),
        )
        .await
        .assert_ok();
    harness
        .send(
            Method::PUT,
            "/api/v1/devices/demo-lamp/name",
            Some(&cookie),
            Some(ORIGIN),
            Some(json!({ "name": "Reading lamp" })),
        )
        .await
        .assert_ok();

    let prepared = harness.prepare(&cookie, "plugin.remove", "demo", json!({})).await;
    prepared.assert_ok();
    let consequences: Vec<&str> =
        prepared.body["consequences"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert_eq!(consequences[0], "Its 2 devices go: Reading lamp and Kettle plug.", "{consequences:?}");
    assert_eq!(consequences[1], "With them go 1 place in a room and 1 favourite.");
    assert_eq!(consequences[2], "Its stored secrets go: access_code.");
    assert_eq!(prepared.body["affected"], json!(["Reading lamp", "Kettle plug"]));
    let audit_before = harness.audit_results().len();

    let removed = harness.critical(&cookie, &mut owner, "plugin.remove", "demo", json!({})).await;
    assert_eq!(removed.phase(), "confirmed", "{}", removed.text);
    assert!(removed.body["detail"].as_str().unwrap().contains("2 devices"), "{}", removed.text);

    assert_eq!(instance_rows(&harness, "demo"), 0);
    assert!(device_rows(&harness, "demo").is_empty());
    assert!(slots(&harness, "demo").is_empty());
    let rooms = harness.get("/api/v1/rooms", &cookie).await;
    let den = rooms.body["rooms"].as_array().unwrap().iter().find(|room| room["name"] == json!("Den")).unwrap();
    let placed: Vec<&str> = den["devices"].as_array().unwrap().iter().filter_map(|d| d["target"].as_str()).collect();
    assert_eq!(placed, ["bulb1"], "{}", rooms.text);
    let me = harness.get("/api/v1/me", &cookie).await;
    assert_eq!(me.body["preferences"]["favourites"], json!(["bulb1"]));
    assert!(!harness.dump("device_names").contains("Reading lamp"));
    let devices = harness.get("/api/v1/devices", &cookie).await;
    assert!(!devices.text.contains("demo-"), "{}", devices.text);

    // Every audit row stays, and one more says what went, by slot.
    assert!(harness.audit_results().len() > audit_before);
    assert_eq!(audit_params(&harness, "plugin.setup", "done").len(), 1, "the setup's row is still there");
    let rows = audit_params(&harness, "plugin.remove", "done");
    assert_eq!(rows[0]["secrets"], json!(["access_code"]));
    assert_eq!(rows[0]["devices"], json!(2));
    assert!(!harness.dump("audit_log").contains("demo-code-9876"));
    harness
        .prepare(&cookie, "plugin.remove", "demo", json!({}))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
}

#[tokio::test]
async fn the_integration_the_hub_runs_on_cannot_be_removed() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    // The test hub runs on vm102, a machine of the `proxmox` instance.
    let refused = harness.prepare(&cookie, "plugin.remove", "proxmox", json!({})).await;
    refused.assert_refused(StatusCode::CONFLICT, "console_machine");
    let _ = &mut owner;
    assert_eq!(instance_rows(&harness, "proxmox"), 1);
}

// ----- what the engine does with what a plugin answers

#[tokio::test]
async fn a_waiting_step_moves_on_by_itself_and_a_finished_setup_adopts_what_was_chosen() {
    let (harness, mut owner, cookie) = with_gizmo().await;
    let (id, setup) = open(&harness, &cookie, &mut owner, json!({ "plugin": "gizmo", "instance": "giz" })).await;
    assert_eq!(step_kind(&setup), "form");
    let wait = send(&harness, &cookie, &id, values(json!({ "name": "porch", "token": "giz-token-5555" }))).await;
    assert_eq!(step_kind(&wait.body), "wait", "{}", wait.text);
    press("giz");
    let choose = until_step(&harness, &cookie, &id, "choose", 5).await;
    assert_eq!(choose["step"]["options"].as_array().unwrap().len(), 2);
    assert_eq!(setup_events(&harness, &id), 1);

    let done = send(&harness, &cookie, &id, chosen(&["plug-b"])).await;
    assert_eq!(done.body["step"]["summary"], json!("Gizmo porch is set up."), "{}", done.text);
    assert_eq!(device_rows(&harness, "giz"), ["giz-plug-b"]);
    let adopted: (String, String, String) = harness
        .app
        .store
        .with(|conn| {
            conn.query_row("SELECT origin, status, adopted_by FROM devices WHERE target = 'giz-plug-b'", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
        })
        .unwrap();
    assert_eq!((adopted.0.as_str(), adopted.1.as_str()), ("adopted", "active"));
    assert!(!adopted.2.is_empty());
    assert_eq!(slots(&harness, "giz"), [GIZMO_TOKEN_SLOT]);
}

#[tokio::test]
async fn a_wait_that_runs_out_offers_the_wait_again() {
    let (harness, mut owner, cookie) = with_gizmo().await;
    let (id, _) = open(&harness, &cookie, &mut owner, json!({ "plugin": "gizmo", "instance": "late" })).await;
    send(&harness, &cookie, &id, values(json!({ "name": "porch" }))).await.assert_ok();
    harness.advance(31_000);
    let failed = until_step(&harness, &cookie, &id, "failed", 5).await;
    assert_eq!(failed["step"]["retry"], json!(true));
    assert!(failed["step"]["reason"].as_str().unwrap().contains("in time"));
    assert_eq!(failed["retry"]["step"], json!("wait"), "the wait is offered again");
    assert_eq!(failed["finished"], json!(false));
    assert_eq!(setup_events(&harness, &id), 1);

    let again = send(&harness, &cookie, &id, json!({ "input": "poll" })).await;
    assert_eq!(step_kind(&again.body), "wait", "{}", again.text);
    press("late");
    until_step(&harness, &cookie, &id, "choose", 5).await;
}

#[tokio::test]
async fn a_plugin_that_fails_hands_back_something_wrong_or_leaks_ends_only_its_setup() {
    let (harness, mut owner, cookie) = with_gizmo().await;

    // A panic in the flow ends that setup and nothing else.
    let (id, _) = open(&harness, &cookie, &mut owner, json!({ "plugin": "gizmo", "instance": "boom" })).await;
    let failed = send(&harness, &cookie, &id, values(json!({ "name": "panic" }))).await;
    failed.assert_ok();
    assert_eq!(
        failed.body["step"]["reason"],
        json!("The plugin's setup failed. Nothing was stored; start the setup again.")
    );
    assert_eq!(failed.body["finished"], json!(true));
    harness.get("/api/v1/devices", &cookie).await.assert_ok();

    // A slot the plugin does not declare, or settings its own checks refuse: nothing is stored.
    for (instance, name, said) in [("rogue", "rogue", "does not declare"), ("strict", "refuse", "refused the settings")]
    {
        let (id, _) = open(&harness, &cookie, &mut owner, json!({ "plugin": "gizmo", "instance": instance })).await;
        let refused = send(&harness, &cookie, &id, values(json!({ "name": name, "token": "rogue-token-1" }))).await;
        assert_eq!(refused.body["step"]["step"], json!("failed"), "{}", refused.text);
        assert!(refused.body["step"]["reason"].as_str().unwrap().contains(said), "{}", refused.text);
        assert_eq!(instance_rows(&harness, instance), 0);
        assert!(slots(&harness, instance).is_empty());
    }

    // A secret a plugin puts into its text is never shown; the step can be tried again.
    let (id, _) = open(&harness, &cookie, &mut owner, json!({ "plugin": "gizmo", "instance": "leaky" })).await;
    let leaked = send(&harness, &cookie, &id, values(json!({ "name": "leak", "token": "leaky-token-2222" }))).await;
    assert_eq!(leaked.body["step"]["reason"], json!("The cloud refused <redacted>."), "{}", leaked.text);
    assert!(!leaked.text.contains("leaky-token-2222"));
    assert_eq!(leaked.body["retry"]["step"], json!("form"), "the form is offered again");
    let wait = send(&harness, &cookie, &id, values(json!({ "name": "porch", "token": "leaky-token-2222" }))).await;
    assert_eq!(step_kind(&wait.body), "wait", "{}", wait.text);
}
