//! The shape of the contract: every public type can be built, the operations,
//! states, setup steps and setup inputs survive a trip through JSON, and their
//! wire names are the ones the hub, the app and stored data rely on.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use swifty_plugin_api::net::{LiveGrant, Timeouts};
use swifty_plugin_api::*;

fn text(value: &str) -> String {
    value.to_string()
}

/// A credential whose value is a canary: it must not show up anywhere else.
fn credential(value: &str) -> Secret {
    Secret::new(value)
}

/// The same, through `From<String>`.
fn credential_from(value: String) -> Secret {
    value.into()
}

fn args(pairs: &[(&str, Scalar)]) -> BTreeMap<String, Scalar> {
    pairs.iter().map(|(name, value)| (name.to_string(), value.clone())).collect()
}

/// Serialises, reads it back, and serialises again. The two JSON documents must
/// be the same, and the value read back is handed over for any further check.
fn round_trip<T: Serialize + DeserializeOwned>(value: &T) -> (Value, T) {
    let json = serde_json::to_value(value).unwrap();
    let back: T = serde_json::from_value(json.clone()).unwrap_or_else(|e| panic!("{json} does not read back: {e}"));
    assert_eq!(serde_json::to_value(&back).unwrap(), json, "{json} changed on the way back");
    (json, back)
}

fn every_op() -> Vec<Op> {
    vec![
        Op::LightPower { on: true },
        Op::LightBrightness { percent: 40 },
        Op::LightHue { hue: 210 },
        Op::LightSet { percent: 60, hue: 30 },
        Op::PlugPower { on: false },
        Op::AirconPower { on: true },
        Op::AirconMode { mode: text("cool") },
        Op::AirconTemp { celsius: 22 },
        Op::AirconFan { fan: text("auto") },
        Op::Volume { level: 25 },
        Op::VolumeCap { max: 30 },
        Op::Mute { muted: true },
        Op::NightMode { on: true },
        Op::Play,
        Op::Pause,
        Op::TvPower { on: true },
        Op::TvInput { input: text("HDMI 1") },
        Op::TvKey { key: text("ok") },
        Op::PetFeed { grams: 10 },
        Op::LitterClean,
        Op::PrintStart,
        Op::PrintPause,
        Op::PrintResume,
        Op::PrintEmergencyStop,
        Op::VmStart,
        Op::VmShutdown,
        Op::VmReboot,
        Op::VmForceStop,
        Op::HostShutdown,
        Op::HostReboot,
        Op::WireguardRestart,
        Op::Custom {
            action: text("demo.pulse"),
            args: args(&[
                ("times", Scalar::Int(3)),
                ("loud", Scalar::Bool(false)),
                ("tone", Scalar::Text(text("low"))),
            ]),
        },
    ]
}

fn every_state() -> Vec<State> {
    vec![
        State::Light { on: true, bri: 40, hue: 210, color: true },
        State::Bridge { reachable: true },
        State::Plug { on: false },
        State::Aircon {
            on: true,
            mode: Some(text("cool")),
            target: Some(22),
            fan: None,
            indoor: Some(24),
            outdoor: Some(-3),
        },
        State::Speaker { vol: 25, mute: false, playing: true, source: text("tv"), night: Some(false) },
        State::Speaker { vol: 25, mute: false, playing: false, source: text("line-in"), night: None },
        State::Tv { on: true, input: text("HDMI 1") },
        State::Feeder { last: Some(text("08:00")), portions: 12 },
        State::Litter { last: None, busy: false, cycles: 7 },
        State::Printer { state: text("printing"), nozzle: 210.5, bed: 60.0, layer: 12, layers: 240, pct: 5.25 },
        State::Vm { state: text("running"), uptime_s: 3600 },
        State::Host { up: true, uptime_s: 86_400 },
        State::Router { wireguard_up: true, uptime_s: 1200 },
        State::Camera { streaming: false },
        State::Service { reachable: true },
        State::Telemetry { data: json!({ "cpu_usage": 12.5, "cores": [0.1, 0.2] }) },
        State::Custom { fields: args(&[("count", Scalar::Int(4)), ("open", Scalar::Bool(true))]) },
    ]
}

fn field(key: &str, kind: FieldType) -> Field {
    Field { key: text(key), label: text("A label"), help: text("Some help."), kind, required: true, value: None }
}

fn decl() -> DeviceDecl {
    DeviceDecl {
        local_id: text("lamp-1"),
        target_hint: None,
        class: DeviceClass::Light,
        name: text("Light 1"),
        capabilities: vec![Capability::Power, Capability::Brightness { min: 10 }, Capability::Color],
        attrs: json!({ "address": "192.0.2.21" }),
    }
}

fn record() -> DeviceRecord {
    DeviceRecord {
        target: text("demo-lamp-1"),
        local_id: text("lamp-1"),
        class: DeviceClass::Light,
        name: text("Light 1"),
        attrs: json!({ "address": "192.0.2.21" }),
    }
}

fn setup_result() -> SetupResult {
    SetupResult {
        config: json!({ "address": "192.0.2.20" }),
        secrets: vec![(text("app_key"), credential("canary-setup-secret"))],
        adopt: vec![decl()],
        summary: text("Paired. 1 light added."),
    }
}

fn every_setup_step() -> Vec<SetupStep> {
    vec![
        SetupStep::Form {
            title: text("Where is it?"),
            text: text("Its address on your network."),
            fields: vec![
                field("name", FieldType::Text { max_len: 40 }),
                field("address", FieldType::Host),
                field("url", FieldType::Url),
                field("endpoint", FieldType::HostPort),
                field("port", FieldType::Port),
                field("count", FieldType::Int { min: 1, max: 9 }),
                field("enabled", FieldType::Bool),
                field("region", FieldType::Choice { options: vec![text("eu"), text("us")] }),
                field("mac", FieldType::Mac),
                field("token", FieldType::Secret),
                Field {
                    value: Some(Scalar::Text(text("-----BEGIN CERTIFICATE-----"))),
                    required: false,
                    ..field("ca", FieldType::Multiline { max_len: 8192 })
                },
            ],
        },
        SetupStep::Wait { title: text("Press the button"), text: text("On the bridge."), poll_ms: 1000, timeout_s: 30 },
        SetupStep::Choose {
            title: text("Which lights?"),
            text: text("Tick the ones to add."),
            options: vec![Choice {
                id: text("lamp-1"),
                label: text("Light 1"),
                detail: text("192.0.2.21"),
                selected: true,
            }],
            multiple: true,
            min: 1,
        },
        SetupStep::External {
            title: text("Make a token"),
            text: text("Create one with read access, then paste it here."),
            link: Some(text("https://example.org/tokens")),
            fields: vec![field("token", FieldType::Secret)],
        },
        SetupStep::Done(setup_result()),
        SetupStep::Failed { reason: text("Live mode is needed for this step."), retry: true },
    ]
}

fn every_setup_input() -> Vec<SetupInput> {
    vec![
        SetupInput::Values {
            values: args(&[("address", Scalar::Text(text("192.0.2.20"))), ("port", Scalar::Int(80))]),
        },
        SetupInput::Chosen { ids: vec![text("lamp-1"), text("lamp-2")] },
        SetupInput::Poll,
        SetupInput::Back,
    ]
}

static PARAMS: &[ParamDef] = &[
    ParamDef { name: "on", kind: ParamKind::Bool, required: true, help: "true for on, false for off." },
    ParamDef { name: "times", kind: ParamKind::Int { min: 1, max: 5 }, required: true, help: "How often." },
    ParamDef { name: "tone", kind: ParamKind::Enum(&["low", "high"]), required: false, help: "Which tone." },
    ParamDef { name: "label", kind: ParamKind::Label { max_len: 40 }, required: false, help: "A name." },
];

static ROW_FIELDS: &[ConfigField] = &[ConfigField {
    key: "id",
    label: "Id",
    help: "The unit's id.",
    kind: FieldKind::Text { max_len: 32 },
    required: true,
    default: None,
}];

static CONFIG_FIELDS: &[ConfigField] = &[
    ConfigField {
        key: "name",
        label: "Name",
        help: "A name.",
        kind: FieldKind::Text { max_len: 40 },
        required: false,
        default: None,
    },
    ConfigField {
        key: "address",
        label: "Address",
        help: "Its address.",
        kind: FieldKind::Host,
        required: true,
        default: None,
    },
    ConfigField {
        key: "url",
        label: "URL",
        help: "Where it answers.",
        kind: FieldKind::Url { schemes: &["http", "https"] },
        required: false,
        default: Some("http://192.0.2.20"),
    },
    ConfigField {
        key: "endpoint",
        label: "Endpoint",
        help: "Host and port.",
        kind: FieldKind::HostPort,
        required: false,
        default: None,
    },
    ConfigField {
        key: "port",
        label: "Port",
        help: "Its port.",
        kind: FieldKind::Port,
        required: false,
        default: Some("80"),
    },
    ConfigField {
        key: "count",
        label: "Count",
        help: "How many.",
        kind: FieldKind::Int { min: 1, max: 9 },
        required: false,
        default: Some("1"),
    },
    ConfigField {
        key: "enabled",
        label: "Enabled",
        help: "On or off.",
        kind: FieldKind::Bool,
        required: false,
        default: Some("true"),
    },
    ConfigField {
        key: "region",
        label: "Region",
        help: "The vendor's region.",
        kind: FieldKind::Choice { options: &["eu", "us"] },
        required: false,
        default: Some("eu"),
    },
    ConfigField {
        key: "mac",
        label: "MAC",
        help: "For Wake-on-LAN.",
        kind: FieldKind::Mac,
        required: false,
        default: None,
    },
    ConfigField {
        key: "units",
        label: "Units",
        help: "One row each.",
        kind: FieldKind::Rows { fields: ROW_FIELDS },
        required: false,
        default: None,
    },
];

static MANIFEST: Manifest = Manifest {
    id: "demo",
    name: "Demo",
    version: "0.1.0",
    api: API_VERSION,
    summary: "A plugin that only exists in this test.",
    reach: Reach::Cloud { vendor: "Example", official: false },
    network: &[
        NetUse::Http { port: 80 },
        NetUse::Https { port: 443 },
        NetUse::Ws { port: 3000 },
        NetUse::Tcp { port: 6668 },
        NetUse::Udp { port: 38899 },
        NetUse::UdpBroadcast { port: 38899 },
        NetUse::WakeOnLan,
        NetUse::CloudHost { host: "api.example.org" },
    ],
    classes: &[DeviceClass::Light, DeviceClass::Other],
    config_version: 1,
    config: CONFIG_FIELDS,
    secrets: &[SecretSlot { slot: "app_key", label: "App key", help: "From the vendor's app.", required: true }],
    actions: &[
        CustomAction {
            id: "demo.pulse",
            label: "Pulse",
            description: "Pulses the light.",
            consequences: &["The light flashes."],
            class: DeviceClass::Light,
            tier: Tier::Comfort,
            params: PARAMS,
            idempotent: false,
            confirm_timeout_ms: 3_000,
            expect: Expect::CounterIncreased { field: "pulses" },
        },
        CustomAction {
            id: "demo.mode",
            label: "Mode",
            description: "Sets a mode.",
            consequences: &[],
            class: DeviceClass::Other,
            tier: Tier::SafeDirection,
            params: &[],
            idempotent: true,
            confirm_timeout_ms: 3_000,
            expect: Expect::FieldEqualsArg { field: "mode", arg: "tone" },
        },
        CustomAction {
            id: "demo.lock",
            label: "Lock",
            description: "Locks it.",
            consequences: &[],
            class: DeviceClass::Other,
            tier: Tier::Critical,
            params: &[],
            idempotent: true,
            confirm_timeout_ms: 3_000,
            expect: Expect::FieldIs { field: "locked", value: true },
        },
        CustomAction {
            id: "demo.beep",
            label: "Beep",
            description: "Beeps.",
            consequences: &[],
            class: DeviceClass::Other,
            tier: Tier::Consequential,
            params: &[],
            idempotent: true,
            confirm_timeout_ms: 3_000,
            expect: Expect::Unreadable,
        },
    ],
    vocabulary: &[("dee mo", "demo")],
    nouns: &[("demo", DeviceClass::Light)],
    multi_instance: true,
};

// ----- a plugin that only exists here, to prove the traits can be used as objects

struct Lamps;

impl Directory for Lamps {
    fn get(&self, local_id: &str) -> Option<DeviceRecord> {
        (local_id == "lamp-1").then(record)
    }

    fn all(&self) -> Vec<DeviceRecord> {
        vec![record()]
    }
}

struct DemoDriver {
    directory: Arc<dyn Directory>,
}

#[async_trait]
impl Driver for DemoDriver {
    async fn command(&self, device: &str, _op: &Op) -> Result<CommandAck, DeviceError> {
        self.directory.get(device).map(|_| CommandAck).ok_or_else(DeviceError::unsupported)
    }

    async fn read_state(&self, _device: &str) -> Result<State, DeviceError> {
        Err(DeviceError::Timeout)
    }
}

struct DemoSetup;

#[async_trait]
impl SetupFlow for DemoSetup {
    async fn begin(&mut self, ctx: &SetupContext, _existing: Option<&Value>) -> SetupStep {
        match ctx.grant {
            None => SetupStep::Failed { reason: text("Live mode is needed."), retry: false },
            Some(_) => SetupStep::Done(setup_result()),
        }
    }

    async fn advance(&mut self, _ctx: &SetupContext, input: SetupInput) -> SetupStep {
        match input {
            SetupInput::Back => SetupStep::Failed { reason: text("Nothing to go back to."), retry: true },
            _ => SetupStep::Done(setup_result()),
        }
    }
}

struct Demo;

impl Plugin for Demo {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn validate(&self, config: &Value) -> Result<(), ConfigProblem> {
        match config.get("address") {
            Some(Value::String(_)) => Ok(()),
            _ => Err(ConfigProblem { field: text("address"), message: text("An address is needed.") }),
        }
    }

    fn readiness(&self, instance: &InstanceView<'_>) -> Readiness {
        if instance.secrets_present.contains("app_key") {
            Readiness::Ready
        } else {
            Readiness::NeedsSetup(text("The app key is missing."))
        }
    }

    fn devices(&self, instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
        instance.adopted.iter().map(|_| decl()).collect()
    }

    fn refusal(&self, _instance: &InstanceView<'_>, device: &str, op: &Op) -> Option<String> {
        (!op.is_light_op()).then(|| format!("{device} is a light."))
    }

    fn connect(&self, ctx: LiveContext) -> Result<Arc<dyn Driver>, PluginError> {
        ctx.secrets.require("app_key")?;
        Ok(Arc::new(DemoDriver { directory: ctx.directory }))
    }
}

static PLUGIN: Demo = Demo;

// ----- the tests

#[test]
fn every_operation_survives_json_under_its_wire_name() {
    let ops = every_op();
    let mut names = BTreeSet::new();
    for op in &ops {
        let (json, back) = round_trip(op);
        assert_eq!(&back, op);
        names.insert(json["op"].as_str().unwrap().to_string());
    }
    let expected = [
        "light_power",
        "light_brightness",
        "light_hue",
        "light_set",
        "plug_power",
        "aircon_power",
        "aircon_mode",
        "aircon_temp",
        "aircon_fan",
        "volume",
        "volume_cap",
        "mute",
        "night_mode",
        "play",
        "pause",
        "tv_power",
        "tv_input",
        "tv_key",
        "pet_feed",
        "litter_clean",
        "print_start",
        "print_pause",
        "print_resume",
        "print_emergency_stop",
        "vm_start",
        "vm_shutdown",
        "vm_reboot",
        "vm_force_stop",
        "host_shutdown",
        "host_reboot",
        "wireguard_restart",
        "custom",
    ];
    assert_eq!(names, expected.iter().map(|n| n.to_string()).collect::<BTreeSet<_>>());
    assert_eq!(
        serde_json::to_value(Op::LightSet { percent: 60, hue: 30 }).unwrap(),
        json!({ "op": "light_set", "percent": 60, "hue": 30 })
    );
    assert_eq!(
        serde_json::to_value(&ops[31]).unwrap(),
        json!({ "op": "custom", "action": "demo.pulse", "args": { "loud": false, "times": 3, "tone": "low" } })
    );
}

#[test]
fn every_state_survives_json_under_its_wire_name() {
    let mut kinds = BTreeSet::new();
    for state in every_state() {
        let (json, back) = round_trip(&state);
        assert_eq!(back, state);
        kinds.insert(json["kind"].as_str().unwrap().to_string());
    }
    let expected = [
        "light",
        "bridge",
        "plug",
        "aircon",
        "speaker",
        "tv",
        "feeder",
        "litter",
        "printer",
        "vm",
        "host",
        "router",
        "camera",
        "service",
        "telemetry",
        "custom",
    ];
    assert_eq!(kinds, expected.iter().map(|n| n.to_string()).collect::<BTreeSet<_>>());
    // The renamed variants, and the speaker leaving out a night mode it does not have.
    assert_eq!(
        serde_json::to_value(State::Light { on: true, bri: 40, hue: 210, color: false }).unwrap(),
        json!({ "kind": "light", "on": true, "bri": 40, "hue": 210, "color": false })
    );
    assert_eq!(
        serde_json::to_value(State::Bridge { reachable: true }).unwrap(),
        json!({ "kind": "bridge", "reachable": true })
    );
    assert_eq!(
        serde_json::to_value(State::Service { reachable: false }).unwrap(),
        json!({ "kind": "service", "reachable": false })
    );
    let quiet: State =
        serde_json::from_value(json!({ "kind": "speaker", "vol": 5, "mute": false, "playing": false, "source": "tv" }))
            .unwrap();
    assert_eq!(quiet, State::Speaker { vol: 5, mute: false, playing: false, source: text("tv"), night: None });
    assert!(serde_json::to_value(&quiet).unwrap().get("night").is_none());
}

#[test]
fn every_setup_step_and_input_survives_json() {
    let mut steps = BTreeSet::new();
    for step in every_setup_step() {
        let (json, _) = round_trip(&step);
        steps.insert(json["step"].as_str().unwrap().to_string());
    }
    let expected = ["form", "wait", "choose", "external", "done", "failed"];
    assert_eq!(steps, expected.iter().map(|n| n.to_string()).collect::<BTreeSet<_>>());

    let mut inputs = BTreeSet::new();
    for input in every_setup_input() {
        let (json, _) = round_trip(&input);
        inputs.insert(json["input"].as_str().unwrap().to_string());
    }
    let expected = ["values", "chosen", "poll", "back"];
    assert_eq!(inputs, expected.iter().map(|n| n.to_string()).collect::<BTreeSet<_>>());

    // What the app sends, read as the hub reads it.
    let typed: SetupInput =
        serde_json::from_value(json!({ "input": "values", "values": { "port": 80, "tls": true } })).unwrap();
    let SetupInput::Values { values } = typed else { panic!("not values") };
    assert_eq!(values["port"], Scalar::Int(80));
    assert_eq!(values["tls"], Scalar::Bool(true));
}

#[test]
fn a_finished_setup_never_shows_its_secrets() {
    let done = SetupStep::Done(setup_result());
    let json = serde_json::to_string(&done).unwrap();
    assert!(!json.contains("canary-setup-secret"), "{json}");
    assert!(!json.contains("app_key"), "{json}");
    let debug = format!("{done:?}");
    assert!(!debug.contains("canary-setup-secret"), "{debug}");
    assert!(debug.contains("app_key"), "the slot is named, its value is not: {debug}");
    let (_, back) = round_trip(&done);
    let SetupStep::Done(back) = back else { panic!("not done") };
    assert!(back.secrets.is_empty(), "secrets never travel as JSON");
}

#[test]
fn secrets_are_redacted_and_required_by_slot() {
    let canary = credential("canary-secret-value");
    assert_eq!(canary.expose(), "canary-secret-value");
    assert_eq!(format!("{canary:?}"), "<redacted>");
    assert_eq!(format!("{canary}"), "<redacted>");
    assert_eq!(credential_from(text("x")).expose(), "x");

    let secrets = Secrets::of([("app_key", "canary-secret-value")]);
    assert_eq!(secrets.get("app_key").unwrap().expose(), "canary-secret-value");
    assert!(secrets.get("other").is_none());
    let missing = secrets.require("other").unwrap_err();
    assert!(missing.to_string().contains("other"));
    assert!(!format!("{secrets:?}").contains("canary-secret-value"));
    assert!(Secrets::default().get("app_key").is_none());
}

#[test]
fn classes_capabilities_and_audiences() {
    let classes = [
        DeviceClass::Light,
        DeviceClass::Bridge,
        DeviceClass::Plug,
        DeviceClass::Aircon,
        DeviceClass::Speaker,
        DeviceClass::Tv,
        DeviceClass::Feeder,
        DeviceClass::Litter,
        DeviceClass::Printer,
        DeviceClass::Vm,
        DeviceClass::Host,
        DeviceClass::Router,
        DeviceClass::Camera,
        DeviceClass::Service,
        DeviceClass::Telemetry,
        DeviceClass::Other,
    ];
    let family: Vec<DeviceClass> =
        classes.iter().copied().filter(|class| class.default_audience() == Audience::Family).collect();
    assert_eq!(
        family,
        [
            DeviceClass::Light,
            DeviceClass::Bridge,
            DeviceClass::Speaker,
            DeviceClass::Tv,
            DeviceClass::Feeder,
            DeviceClass::Litter,
            DeviceClass::Camera
        ]
    );
    let approval: Vec<DeviceClass> = classes.iter().copied().filter(|class| class.needs_approval()).collect();
    assert_eq!(approval, [DeviceClass::Vm, DeviceClass::Host, DeviceClass::Router]);
    for class in classes {
        assert_eq!(round_trip(&class).1, class);
    }
    assert_eq!(serde_json::to_value(DeviceClass::Aircon).unwrap(), json!("aircon"));
    assert_eq!(round_trip(&Audience::Owner).1, Audience::Owner);

    let capabilities = [
        (Capability::Power, CapKind::Power),
        (Capability::Brightness { min: 10 }, CapKind::Brightness),
        (Capability::Color, CapKind::Color),
        (Capability::Volume, CapKind::Volume),
        (Capability::Mute, CapKind::Mute),
        (Capability::NightMode, CapKind::NightMode),
        (Capability::Transport, CapKind::Transport),
        (Capability::Inputs { inputs: vec![text("HDMI 1")] }, CapKind::Inputs),
        (Capability::Keys { keys: vec![text("ok")] }, CapKind::Keys),
        (Capability::Mode { modes: vec![text("cool")] }, CapKind::Mode),
        (Capability::Temperature { min: 17, max: 30 }, CapKind::Temperature),
        (Capability::Fan { speeds: vec![text("auto")] }, CapKind::Fan),
        (Capability::Dispense { grams: vec![5, 10] }, CapKind::Dispense),
        (Capability::Clean, CapKind::Clean),
        (Capability::PrintJob, CapKind::PrintJob),
        (Capability::EmergencyStop, CapKind::EmergencyStop),
        (Capability::Lifecycle, CapKind::Lifecycle),
        (Capability::NodePower, CapKind::NodePower),
        (Capability::Tunnel, CapKind::Tunnel),
        (Capability::Metrics, CapKind::Metrics),
        (Capability::History, CapKind::History),
        (Capability::Console, CapKind::Console),
        (Capability::AgentPairing, CapKind::AgentPairing),
        (Capability::Custom { action: text("demo.pulse") }, CapKind::Custom),
    ];
    let mut kinds = BTreeSet::new();
    for (capability, kind) in capabilities {
        assert_eq!(capability.kind(), kind);
        assert_eq!(round_trip(&capability).1, capability);
        assert_eq!(round_trip(&kind).1, kind);
        assert!(kinds.insert(serde_json::to_string(&kind).unwrap()), "{kind:?} twice");
    }
    assert_eq!(
        serde_json::to_value(Capability::Brightness { min: 10 }).unwrap(),
        json!({ "cap": "brightness", "min": 10 })
    );
}

#[test]
fn confirmation_is_the_hubs_and_reads_the_state() {
    assert_eq!(MIN_LIGHT_DIMMING, 10);
    let light = |on, bri, color| State::Light { on, bri, hue: 30, color };
    assert!(Op::LightPower { on: true }.is_confirmed_by(&light(true, 50, false), None));
    assert!(Op::LightBrightness { percent: 5 }.is_confirmed_by(&light(true, 10, false), None), "the lowest level");
    assert!(!Op::LightHue { hue: 30 }.is_confirmed_by(&light(true, 50, false), None), "white mode shows no colour");
    assert!(Op::LightSet { percent: 0, hue: 0 }.is_confirmed_by(&light(false, 0, false), None));
    assert!(!Op::LightPower { on: true }.is_confirmed_by(&State::Plug { on: true }, None), "another class");

    let feeder = |portions| State::Feeder { last: None, portions };
    assert!(Op::PetFeed { grams: 10 }.needs_baseline());
    assert!(!Op::PetFeed { grams: 10 }.is_confirmed_by(&feeder(3), None), "no baseline, no confirmation");
    assert!(Op::PetFeed { grams: 10 }.is_confirmed_by(&feeder(3), Some(&feeder(2))));

    assert!(Op::LightSet { percent: 1, hue: 1 }.is_light_op());
    assert!(!Op::PlugPower { on: true }.is_light_op());
    assert!(Op::AirconFan { fan: text("auto") }.is_aircon_op());
    assert!(Op::TvKey { key: text("ok") }.cannot_be_read_back());
    assert!(Op::HostReboot.takes_the_device_away());
    assert!(!Op::VmReboot.takes_the_device_away());

    let custom = Op::Custom { action: text("demo.pulse"), args: BTreeMap::new() };
    let fields = State::Custom { fields: args(&[("pulses", Scalar::Int(2))]) };
    assert!(!custom.is_confirmed_by(&fields, Some(&fields)), "a custom action is confirmed by its Expect");
    assert!(!custom.is_light_op() && !custom.needs_baseline() && !custom.cannot_be_read_back());
}

#[test]
fn custom_actions_confirm_with_their_expectation() {
    let state = |pairs: &[(&str, Scalar)]| State::Custom { fields: args(pairs) };
    let tone = args(&[("tone", Scalar::Text(text("low")))]);

    let equals = Expect::FieldEqualsArg { field: "mode", arg: "tone" };
    assert!(equals.is_met(&tone, &state(&[("mode", Scalar::Text(text("low")))]), None));
    assert!(!equals.is_met(&tone, &state(&[("mode", Scalar::Text(text("high")))]), None));
    assert!(!equals.is_met(&BTreeMap::new(), &state(&[("mode", Scalar::Text(text("low")))]), None));
    assert!(!equals.needs_baseline());

    let is = Expect::FieldIs { field: "locked", value: true };
    assert!(is.is_met(&BTreeMap::new(), &state(&[("locked", Scalar::Bool(true))]), None));
    assert!(!is.is_met(&BTreeMap::new(), &state(&[("locked", Scalar::Bool(false))]), None));
    assert!(!is.is_met(&BTreeMap::new(), &State::Plug { on: true }, None), "only a custom state can show it");

    let counter = Expect::CounterIncreased { field: "pulses" };
    assert!(counter.needs_baseline());
    let before = state(&[("pulses", Scalar::Int(2))]);
    assert!(counter.is_met(&BTreeMap::new(), &state(&[("pulses", Scalar::Int(3))]), Some(&before)));
    assert!(!counter.is_met(&BTreeMap::new(), &state(&[("pulses", Scalar::Int(2))]), Some(&before)));
    assert!(!counter.is_met(&BTreeMap::new(), &state(&[("pulses", Scalar::Int(3))]), None));

    assert!(!Expect::Unreadable.is_met(&BTreeMap::new(), &state(&[]), None));
    assert_eq!(
        serde_json::to_value(equals).unwrap(),
        json!({ "expect": "field_equals_arg", "field": "mode", "arg": "tone" })
    );
}

#[test]
fn the_manifest_is_static_data_that_serialises() {
    assert_eq!(API_VERSION, 1);
    let json = serde_json::to_value(PLUGIN.manifest()).unwrap();
    assert_eq!(json["id"], json!("demo"));
    assert_eq!(json["api"], json!(1));
    assert_eq!(json["reach"], json!({ "reach": "cloud", "vendor": "Example", "official": false }));
    assert_eq!(json["network"][6], json!({ "use": "wake_on_lan" }));
    assert_eq!(json["network"][7], json!({ "use": "cloud_host", "host": "api.example.org" }));
    assert_eq!(json["config"][1]["kind"], json!({ "type": "host" }));
    assert_eq!(json["config"][9]["kind"]["fields"][0]["key"], json!("id"));
    assert_eq!(json["secrets"][0]["slot"], json!("app_key"));
    assert_eq!(json["actions"][0]["tier"], json!("comfort"));
    assert_eq!(json["actions"][0]["expect"], json!({ "expect": "counter_increased", "field": "pulses" }));
    assert_eq!(json["actions"][0]["params"][1]["kind"], json!({ "int": { "min": 1, "max": 5 } }));
    assert_eq!(json["vocabulary"][0], json!(["dee mo", "demo"]));
    assert_eq!(json["nouns"][0], json!(["demo", "light"]));
    assert_eq!(serde_json::to_value(Reach::Local).unwrap(), json!({ "reach": "local" }));
    for tier in [Tier::Read, Tier::Comfort, Tier::Consequential, Tier::Critical, Tier::SafeDirection] {
        assert_eq!(serde_json::to_value(tier).unwrap(), json!(tier.name()));
        assert_eq!(tier.code().len(), 1);
    }
    assert_eq!(Tier::SafeDirection.wire(), json!("S"));
    assert_eq!(Tier::Critical.wire(), json!(3));
}

#[test]
fn errors_say_what_happened_and_travel_as_data() {
    let errors = [
        DeviceError::Timeout,
        DeviceError::Unreachable(text("the connection failed")),
        DeviceError::Rejected(text("busy")),
        DeviceError::Disabled(text("off")),
    ];
    for error in &errors {
        assert_eq!(&round_trip(error).1, error);
    }
    assert_eq!(errors[0].to_string(), "no reply from the device");
    assert_eq!(errors[2].to_string(), "device refused: busy");
    assert!(errors[0].is_silence() && errors[1].is_silence());
    assert!(!errors[2].is_silence() && !errors[3].is_silence());
    assert_eq!(DeviceError::unsupported(), DeviceError::Disabled(text("This device cannot do that.")));

    let problem = ConfigProblem { field: text("address"), message: text("An address is needed.") };
    assert_eq!(problem.to_string(), "address: An address is needed.");
    assert_eq!(
        serde_json::to_value(&problem).unwrap(),
        json!({ "field": "address", "message": "An address is needed." })
    );
    assert_eq!(PluginError(text("Not paired.")).to_string(), "Not paired.");
}

#[test]
fn machines_charts_and_consoles() {
    for range in [HistoryRange::Hour, HistoryRange::Day, HistoryRange::Week] {
        assert_eq!(HistoryRange::parse(range.name()), Some(range));
        assert_eq!(serde_json::to_value(range).unwrap(), json!(range.name()));
        assert_eq!(round_trip(&range).1, range);
        assert!(range.bucket_ms() < range.span_ms());
    }
    assert_eq!(HistoryRange::parse("1y"), None);

    let seen = SeenMachine { host: text("node-a"), vmid: 104, name: text("new-vm") };
    assert_eq!(round_trip(&seen).1, seen);
    let host = ExtraHost {
        id: text("node-b"),
        address: "192.0.2.30:8006".parse().unwrap(),
        node: text("node-b"),
        token_file: PathBuf::from("secrets/node-b.token"),
        ca_file: PathBuf::from("secrets/node-b.pem"),
    };
    assert_eq!(round_trip(&host).1, host);
    let grant = ConsoleGrant { target: text("vm104"), port: 5900, ticket: credential("canary-ticket") };
    assert!(!format!("{grant:?}").contains("canary-ticket"));
    assert_eq!(grant.clone().ticket.expose(), "canary-ticket");
}

#[tokio::test]
async fn a_plugin_is_used_through_its_traits() {
    let plugin: &'static dyn Plugin = &PLUGIN;
    assert_eq!(plugin.manifest().id, "demo");
    assert!(plugin.validate(&json!({ "address": "192.0.2.20" })).is_ok());
    assert_eq!(plugin.validate(&json!({})).unwrap_err().field, "address");
    assert_eq!(plugin.migrate(0, json!({ "a": 1 })).unwrap(), json!({ "a": 1 }), "migrate keeps by default");
    assert!(plugin.setup().is_none(), "no setup by default");

    let config = json!({ "address": "192.0.2.20" });
    let adopted = [record()];
    let none = BTreeSet::new();
    let some: BTreeSet<String> = [text("app_key")].into();
    let view = InstanceView { id: "demo", config: &config, secrets_present: &none, adopted: &adopted };
    assert_eq!(plugin.readiness(&view), Readiness::NeedsSetup(text("The app key is missing.")));
    assert_eq!(plugin.devices(&view), vec![decl()]);
    assert!(plugin.refusal(&view, "lamp-1", &Op::LightPower { on: true }).is_none());
    assert!(plugin.refusal(&view, "lamp-1", &Op::Play).is_some());
    let ready = InstanceView { secrets_present: &some, ..view };
    assert_eq!(plugin.readiness(&ready), Readiness::Ready);
    for readiness in [Readiness::Ready, Readiness::NeedsSetup(text("Pair it.")), Readiness::Disabled(text("Off."))] {
        assert_eq!(round_trip(&readiness).1, readiness);
    }
    assert_eq!(
        serde_json::to_value(Readiness::Disabled(text("Off."))).unwrap(),
        json!({ "status": "disabled", "reason": "Off." })
    );

    let timeouts = Timeouts { connect: Duration::from_millis(500), read: Duration::from_millis(500) };
    let context = |secrets| LiveContext {
        instance_id: text("demo"),
        config: config.clone(),
        secrets,
        grant: LiveGrant::when_live(true).expect("a live hub has a grant"),
        timeouts,
        directory: Arc::new(Lamps),
    };
    assert!(LiveGrant::when_live(false).is_none(), "a dry-run hub has none");
    assert!(plugin.connect(context(Secrets::default())).is_err(), "a missing secret stops the instance");
    let driver = plugin.connect(context(Secrets::of([("app_key", "canary-secret-value")]))).unwrap();
    assert_eq!(driver.command("lamp-1", &Op::LightPower { on: true }).await, Ok(CommandAck));
    assert_eq!(driver.command("lamp-9", &Op::LightPower { on: true }).await, Err(DeviceError::unsupported()));
    assert_eq!(driver.read_state("lamp-1").await, Err(DeviceError::Timeout));
    assert!(driver.poll_refusal().is_none());
    assert_eq!(driver.discover(Duration::from_millis(10)).await, Err(DeviceError::unsupported()));
    assert_eq!(driver.health().await, Health::unknown());
    assert!(driver.machines().is_none() && driver.agents().is_none());
    assert_eq!(Lamps.all(), vec![record()]);

    let health = Health { status: HealthStatus::Degraded, detail: Some(text("One of two answers.")) };
    assert_eq!(round_trip(&health).1, health);
    for status in [HealthStatus::Ok, HealthStatus::Degraded, HealthStatus::Down, HealthStatus::Unknown] {
        assert_eq!(round_trip(&status).1, status);
    }

    let mut flow: Box<dyn SetupFlow> = Box::new(DemoSetup);
    let dry = SetupContext { instance_id: text("demo"), grant: None, timeouts, secrets_present: BTreeSet::new() };
    assert!(matches!(flow.begin(&dry, None).await, SetupStep::Failed { retry: false, .. }));
    let live = SetupContext { grant: LiveGrant::when_live(true), ..dry };
    assert!(matches!(flow.begin(&live, Some(&config)).await, SetupStep::Done(_)));
    assert!(matches!(flow.advance(&live, SetupInput::Poll).await, SetupStep::Done(_)));
    assert!(matches!(flow.advance(&live, SetupInput::Back).await, SetupStep::Failed { retry: true, .. }));
}
