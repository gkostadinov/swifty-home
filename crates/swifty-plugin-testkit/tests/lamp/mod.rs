//! A tiny plugin for the test kit's own tests: lamps that speak JSON over UDP,
//! behind a four-byte `LAMP` header, with a key in every request. As written it
//! conforms. Each [`Bug`] breaks it the way a real plugin could, for one check.
//!
//! The device answers `{"cmd":"get","id":..}` with `{"on":..,"bri":..}`,
//! `{"cmd":"set","id":..,"on":..}` or `"bri":..` (10 to 100) with `{"ok":..}`,
//! `{"cmd":"flash","id":..,"times":..}` with `{"ok":true}`, and `{"cmd":"who"}`
//! with the lamps it has and their names.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use swifty_plugin_api::net::{self, LiveGrant, Timeouts};
use swifty_plugin_api::{
    Capability, CommandAck, ConfigField, ConfigProblem, CustomAction, DeviceClass, DeviceDecl, DeviceError, Directory,
    Driver, Expect, Field, FieldKind, FieldType, InstanceView, LiveContext, Manifest, NetUse, Op, ParamDef, ParamKind,
    Plugin, PluginError, Reach, Readiness, Scalar, Secret, SecretSlot, SetupContext, SetupFlow, SetupInput,
    SetupResult, SetupStep, State, Tier, API_VERSION,
};
use swifty_plugin_testkit::conformance::{Fixture, Rigged, Switches};
use swifty_plugin_testkit::devices::MockUdp;

/// The fixture's key. Check 9 looks for it everywhere it must not be.
pub const KEY: &str = "lamp-canary-5b7e19";

/// The lowest level a lamp dims to.
pub const FLOOR: u8 = 10;

const ROW_FIELDS: &[ConfigField] = &[
    ConfigField {
        key: "id",
        label: "Id",
        help: "The lamp's id.",
        kind: FieldKind::Text { max_len: 32 },
        required: true,
        default: None,
    },
    ConfigField {
        key: "name",
        label: "Name",
        help: "A name.",
        kind: FieldKind::Text { max_len: 64 },
        required: false,
        default: None,
    },
];

const FIELDS: &[ConfigField] = &[
    ConfigField {
        key: "host",
        label: "Host",
        help: "The lamp's address.",
        kind: FieldKind::Host,
        required: true,
        default: None,
    },
    ConfigField {
        key: "port",
        label: "Port",
        help: "Its UDP port.",
        kind: FieldKind::Port,
        required: false,
        default: Some("5000"),
    },
    ConfigField {
        key: "mode",
        label: "Mode",
        help: "How it answers.",
        kind: FieldKind::Choice { options: &["quick", "careful"] },
        required: false,
        default: None,
    },
    ConfigField {
        key: "lamps",
        label: "Lamps",
        help: "One row per lamp.",
        kind: FieldKind::Rows { fields: ROW_FIELDS },
        required: false,
        default: None,
    },
];

pub static MANIFEST: Manifest = Manifest {
    id: "lamp",
    name: "Lamp",
    version: env!("CARGO_PKG_VERSION"),
    api: API_VERSION,
    summary: "A lamp that speaks UDP, for the test kit's own tests.",
    reach: Reach::Local,
    network: &[NetUse::Udp { port: 5000 }],
    classes: &[DeviceClass::Light],
    config_version: 1,
    config: FIELDS,
    secrets: &[SecretSlot { slot: "key", label: "Key", help: "On the box.", required: true }],
    actions: &[CustomAction {
        id: "lamp.flash",
        label: "Flash",
        description: "Flashes the lamp.",
        consequences: &[],
        class: DeviceClass::Light,
        tier: Tier::Comfort,
        params: &[ParamDef {
            name: "times",
            kind: ParamKind::Int { min: 1, max: 5 },
            required: true,
            help: "How often.",
        }],
        idempotent: false,
        confirm_timeout_ms: 1_000,
        expect: Expect::Unreadable,
    }],
    vocabulary: &[("lamb", "lamp")],
    nouns: &[("lumo", DeviceClass::Light)],
    multi_instance: true,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    id: String,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LampConfig {
    host: String,
    port: Option<u16>,
    mode: Option<String>,
    lamps: Option<Vec<Row>>,
}

impl LampConfig {
    fn parse(config: &Value) -> Option<Self> {
        serde_json::from_value(config.clone()).ok()
    }

    fn addr(&self) -> Option<SocketAddr> {
        let ip = self.host.parse().ok()?;
        Some(SocketAddr::new(ip, self.port.unwrap_or(5000)))
    }

    fn has(&self, id: &str) -> bool {
        self.lamps.iter().flatten().any(|row| row.id == id)
    }
}

/// The field a serde error names, as the typed config of a real plugin would report it.
fn problem(error: serde_json::Error) -> ConfigProblem {
    let text = error.to_string();
    let field = text.split('`').nth(1).unwrap_or("config").to_string();
    ConfigProblem { field, message: text }
}

/// Switches for what check 2 looks at.
#[derive(Clone, Copy, Default)]
pub struct Faults {
    pub accepts_anything: bool,
    pub host_optional: bool,
    pub migrate_changes: bool,
}

/// One way to break the plugin, each for one check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bug {
    None,
    /// 3: `refusal` sends a datagram to the lamp.
    SocketInRefusal,
    /// 4: `refusal` panics at the top of the catalog's brightness range.
    PanicsAtTheTopOfTheRange,
    /// 5: the dimming floor is only in the driver, so dry-run does not refuse it.
    FloorOnlyInTheDriver,
    /// 6: a command that met silence is sent again.
    Retries,
    /// 7: the driver waits four seconds whatever the timeouts say.
    IgnoresTimeouts,
    /// 8: `read_state` reports the opposite of what the lamp says.
    InvertsPower,
    /// 9: an error names the key.
    LeaksTheKey,
    /// 9: the key is logged.
    LogsTheKey,
    /// 10: a discovered name is passed on as the lamp sent it.
    RawDeviceText,
    /// 10: the setup flow shows a name as the lamp sent it.
    RawNameInSetup,
    /// 11: the setup flow makes its own grant when it has none.
    SetupWithoutGrant,
    /// 11: set up again with its key stored, the form shows something in the key's field.
    PrefillsTheStoredKey,
    /// 11: set up again with its key stored, the flow turns the person away.
    RefusesToSetUpAgain,
    /// 12: a reply shorter than the header panics.
    PanicsOnAShortReply,
}

pub struct Lamp {
    pub faults: Faults,
    pub bug: Bug,
}

impl Lamp {
    pub const fn good() -> Self {
        Self {
            faults: Faults { accepts_anything: false, host_optional: false, migrate_changes: false },
            bug: Bug::None,
        }
    }

    pub const fn with(faults: Faults) -> Self {
        Self { faults, bug: Bug::None }
    }

    pub const fn with_bug(bug: Bug) -> Self {
        Self { faults: Faults { accepts_anything: false, host_optional: false, migrate_changes: false }, bug }
    }
}

fn capabilities() -> Vec<Capability> {
    vec![Capability::Power, Capability::Brightness { min: FLOOR }, Capability::Custom { action: "lamp.flash".into() }]
}

impl Plugin for Lamp {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn validate(&self, config: &Value) -> Result<(), ConfigProblem> {
        if self.faults.accepts_anything {
            return Ok(());
        }
        let mut config = config.clone();
        if self.faults.host_optional && config.get("host").is_none() {
            config["host"] = json!("192.0.2.1");
        }
        serde_json::from_value::<LampConfig>(config).map(|_| ()).map_err(problem)
    }

    fn migrate(&self, _from_version: u32, mut config: Value) -> Result<Value, ConfigProblem> {
        if self.faults.migrate_changes {
            config["mode"] = json!("quick");
        }
        Ok(config)
    }

    fn readiness(&self, instance: &InstanceView<'_>) -> Readiness {
        if instance.secrets_present.contains("key") {
            Readiness::Ready
        } else {
            Readiness::NeedsSetup("Enter the key printed on the lamp's box.".to_string())
        }
    }

    fn devices(&self, instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
        let Some(config) = LampConfig::parse(instance.config) else { return Vec::new() };
        config
            .lamps
            .iter()
            .flatten()
            .map(|row| DeviceDecl {
                local_id: row.id.clone(),
                target_hint: None,
                class: DeviceClass::Light,
                name: row.name.clone().unwrap_or_else(|| "Lamp".to_string()),
                capabilities: capabilities(),
                attrs: json!({}),
            })
            .collect()
    }

    fn refusal(&self, instance: &InstanceView<'_>, device: &str, op: &Op) -> Option<String> {
        let config = LampConfig::parse(instance.config)?;
        if self.bug == Bug::SocketInRefusal {
            if let Some(addr) = config.addr() {
                let socket = std::net::UdpSocket::bind("127.0.0.1:0").ok()?;
                let _ = socket.send_to(b"LAMP?", addr);
            }
        }
        if !config.has(device) {
            return Some(format!("{device} is not a lamp of this instance."));
        }
        match op {
            Op::LightPower { .. } => None,
            Op::LightBrightness { percent } => {
                if self.bug == Bug::PanicsAtTheTopOfTheRange {
                    // A table of 100 levels, indexed by a percentage that goes up to 100.
                    let levels = [0u8; 100];
                    let _ = levels[usize::from(*percent)];
                }
                (*percent < FLOOR && self.bug != Bug::FloorOnlyInTheDriver)
                    .then(|| format!("A lamp dims to {FLOOR}% at the lowest."))
            }
            Op::Custom { action, args } if action == "lamp.flash" => match args.get("times") {
                Some(Scalar::Int(1..=5)) => None,
                _ => Some("A lamp flashes one to five times.".to_string()),
            },
            _ => Some("A lamp can switch, dim and flash, and has no colour.".to_string()),
        }
    }

    fn connect(&self, ctx: LiveContext) -> Result<Arc<dyn Driver>, PluginError> {
        let config = LampConfig::parse(&ctx.config).ok_or_else(|| PluginError("The config is not valid.".into()))?;
        let addr = config.addr().ok_or_else(|| PluginError("The lamp's address is not valid.".into()))?;
        let key = ctx.secrets.require("key")?.clone();
        Ok(Arc::new(LampDriver {
            addr,
            key,
            grant: ctx.grant,
            timeouts: ctx.timeouts,
            directory: ctx.directory,
            bug: self.bug,
        }))
    }

    fn setup(&self) -> Option<Box<dyn SetupFlow>> {
        Some(Box::new(LampSetup { bug: self.bug, found: None }))
    }
}

struct LampDriver {
    addr: SocketAddr,
    key: Secret,
    grant: LiveGrant,
    timeouts: Timeouts,
    directory: Arc<dyn Directory>,
    bug: Bug,
}

/// One request and its reply, through the socket helpers.
async fn exchange(
    grant: LiveGrant,
    addr: SocketAddr,
    request: &Value,
    wait: Duration,
    bug: Bug,
) -> Result<Value, DeviceError> {
    let reply = net::udp_exchange(grant, addr, request.to_string().as_bytes(), wait).await?;
    if bug == Bug::PanicsOnAShortReply {
        assert_eq!(&reply[..4], b"LAMP");
        return Ok(serde_json::from_slice(&reply[4..]).unwrap());
    }
    let body =
        reply.strip_prefix(b"LAMP").ok_or_else(|| DeviceError::Rejected("the lamp's reply has no header".into()))?;
    serde_json::from_slice(body).map_err(|_| DeviceError::Rejected("the lamp's reply is not JSON".into()))
}

impl LampDriver {
    fn wait(&self) -> Duration {
        if self.bug == Bug::IgnoresTimeouts {
            Duration::from_secs(4)
        } else {
            self.timeouts.read
        }
    }

    async fn ask(&self, mut request: Value) -> Result<Value, DeviceError> {
        request["key"] = json!(self.key.expose());
        if self.bug == Bug::LogsTheKey {
            tracing::debug!(request = %request, "asking the lamp");
        }
        let result = exchange(self.grant, self.addr, &request, self.wait(), self.bug).await;
        match result {
            Err(error) if error.is_silence() && self.bug == Bug::LeaksTheKey => {
                Err(DeviceError::Unreachable(format!("no answer to the key {}", self.key.expose())))
            }
            other => other,
        }
    }

    fn known(&self, device: &str) -> Result<(), DeviceError> {
        match self.directory.get(device) {
            Some(_) => Ok(()),
            None => Err(DeviceError::Disabled("This lamp is not known.".into())),
        }
    }
}

#[async_trait]
impl Driver for LampDriver {
    async fn command(&self, device: &str, op: &Op) -> Result<CommandAck, DeviceError> {
        self.known(device)?;
        let request = match op {
            Op::LightPower { on } => json!({ "cmd": "set", "id": device, "on": on }),
            Op::LightBrightness { percent } if *percent < FLOOR => {
                return Err(DeviceError::Disabled(format!("A lamp dims to {FLOOR}% at the lowest.")))
            }
            Op::LightBrightness { percent } => json!({ "cmd": "set", "id": device, "bri": percent }),
            Op::Custom { action, args } if action == "lamp.flash" => match args.get("times") {
                Some(Scalar::Int(times)) => json!({ "cmd": "flash", "id": device, "times": times }),
                _ => return Err(DeviceError::Rejected("a flash needs a number of times".into())),
            },
            _ => return Err(DeviceError::unsupported()),
        };
        let mut reply = self.ask(request.clone()).await;
        if self.bug == Bug::Retries && matches!(reply, Err(DeviceError::Timeout)) {
            reply = self.ask(request).await;
        }
        match reply?.get("ok").and_then(Value::as_bool) {
            Some(true) => Ok(CommandAck),
            _ => Err(DeviceError::Rejected("the lamp did not accept the command".into())),
        }
    }

    async fn read_state(&self, device: &str) -> Result<State, DeviceError> {
        self.known(device)?;
        let reply = self.ask(json!({ "cmd": "get", "id": device })).await?;
        let on = reply.get("on").and_then(Value::as_bool);
        let bri = reply.get("bri").and_then(Value::as_u64).filter(|bri| *bri <= 100);
        match (on, bri) {
            (Some(on), Some(bri)) => {
                let on = if self.bug == Bug::InvertsPower { !on } else { on };
                Ok(State::Light { on, bri: bri as u8, hue: 0, color: false })
            }
            _ => Err(DeviceError::Rejected("the lamp's state could not be read".into())),
        }
    }

    async fn discover(&self, window: Duration) -> Result<Vec<DeviceDecl>, DeviceError> {
        let reply = self.ask(json!({ "cmd": "who" })).await;
        let reply = reply?;
        let _ = window;
        let lamps = reply.get("lamps").and_then(Value::as_array).cloned().unwrap_or_default();
        Ok(lamps
            .iter()
            .filter_map(|lamp| {
                let id = lamp.get("id")?.as_str()?;
                let name = lamp.get("name")?.as_str()?;
                let name = if self.bug == Bug::RawDeviceText { name.to_string() } else { net::device_text(name) };
                let local_id = net::device_text(id).to_ascii_lowercase();
                Some(DeviceDecl {
                    local_id,
                    target_hint: None,
                    class: DeviceClass::Light,
                    name,
                    capabilities: capabilities(),
                    attrs: json!({}),
                })
            })
            .collect())
    }
}

/// Form (address and key), then one read of the lamp, then Done.
struct LampSetup {
    bug: Bug,
    found: Option<(String, u16)>,
}

fn text(values: &BTreeMap<String, Scalar>, key: &str) -> Option<String> {
    match values.get(key) {
        Some(Scalar::Text(text)) => Some(text.clone()),
        Some(Scalar::Int(number)) => Some(number.to_string()),
        _ => None,
    }
}

#[async_trait]
impl SetupFlow for LampSetup {
    async fn begin(&mut self, ctx: &SetupContext, _existing: Option<&Value>) -> SetupStep {
        let stored = ctx.secrets_present.contains("key");
        if stored && self.bug == Bug::RefusesToSetUpAgain {
            return SetupStep::Failed { reason: "This lamp is already set up.".to_string(), retry: false };
        }
        let shown = (stored && self.bug == Bug::PrefillsTheStoredKey).then(|| Scalar::Text("stored".to_string()));
        let field = |key: &str, label: &str, kind: FieldType, required: bool, value: Option<Scalar>| Field {
            key: key.to_string(),
            label: label.to_string(),
            help: String::new(),
            kind,
            required,
            value,
        };
        SetupStep::Form {
            title: "The lamp".to_string(),
            text: "Where the lamp is, and the key on its box.".to_string(),
            fields: vec![
                field("host", "Address", FieldType::Host, true, None),
                field("port", "Port", FieldType::Port, false, Some(Scalar::Int(5000))),
                field("key", "Key", FieldType::Secret, true, shown),
            ],
        }
    }

    async fn advance(&mut self, ctx: &SetupContext, input: SetupInput) -> SetupStep {
        let SetupInput::Values { values } = input else {
            return SetupStep::Failed { reason: "Fill in the form.".to_string(), retry: true };
        };
        let (Some(host), Some(key)) = (text(&values, "host"), text(&values, "key")) else {
            return SetupStep::Failed { reason: "The address and the key are needed.".to_string(), retry: true };
        };
        let port = text(&values, "port").and_then(|port| port.parse().ok()).unwrap_or(5000u16);
        let Some(ip) = host.parse().ok() else {
            return SetupStep::Failed { reason: "The address is not an IP address.".to_string(), retry: true };
        };
        let grant = match (ctx.grant, self.bug) {
            (Some(grant), _) => grant,
            (None, Bug::SetupWithoutGrant) => LiveGrant::when_live(true).expect("a grant"),
            (None, _) => {
                return SetupStep::Failed { reason: "Live mode is needed to reach the lamp.".to_string(), retry: false }
            }
        };
        let request = json!({ "cmd": "who", "key": key });
        let Ok(reply) = exchange(grant, SocketAddr::new(ip, port), &request, ctx.timeouts.read, Bug::None).await else {
            return SetupStep::Failed { reason: "The lamp did not answer.".to_string(), retry: true };
        };
        self.found = Some((host.clone(), port));
        let lamps: Vec<Value> = reply
            .get("lamps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|lamp| {
                let id = lamp.get("id")?.as_str()?;
                let sent = lamp.get("name")?.as_str()?;
                let name = if self.bug == Bug::RawNameInSetup { sent.to_string() } else { net::device_text(sent) };
                Some(json!({ "id": id, "name": name }))
            })
            .collect();
        let count = lamps.len();
        SetupStep::Done(SetupResult {
            config: json!({ "host": host, "port": port, "lamps": lamps }),
            secrets: vec![("key".to_string(), key.into())],
            adopt: Vec::new(),
            summary: format!("Found {count} lamp(s)."),
        })
    }
}

/// The lamp itself, on loopback.
pub async fn start_device(switches: Switches) -> MockUdp {
    let lamps: Arc<Mutex<BTreeMap<String, (bool, u8)>>> =
        Arc::new(Mutex::new(BTreeMap::from([("one".to_string(), (false, 60)), ("two".to_string(), (true, 30))])));
    MockUdp::start(move |datagram| {
        let request: Value = serde_json::from_slice(datagram).ok()?;
        let answer = if request.get("key").and_then(Value::as_str) != Some(KEY) {
            json!({ "ok": false })
        } else {
            let mut lamps = lamps.lock().unwrap();
            let id = request.get("id").and_then(Value::as_str).unwrap_or("");
            match request.get("cmd").and_then(Value::as_str) {
                Some("who") => json!({ "lamps": lamps.keys().map(|id| json!({
                    "id": id,
                    // A datagram carries 2 kB at most, so a long name is cut to fit.
                    "name": switches.device_text(if id == "one" { "Desk" } else { "Shelf" }).chars().take(600).collect::<String>(),
                })).collect::<Vec<_>>() }),
                Some("get") => match lamps.get(id) {
                    Some((on, bri)) => json!({ "on": on, "bri": bri }),
                    None => json!({ "ok": false }),
                },
                Some("set") => match lamps.get_mut(id) {
                    Some(lamp) => {
                        if let Some(on) = request.get("on").and_then(Value::as_bool) {
                            lamp.0 = on;
                            json!({ "ok": true })
                        } else {
                            match request.get("bri").and_then(Value::as_u64) {
                                Some(bri @ 10..=100) => {
                                    *lamp = (true, bri as u8);
                                    json!({ "ok": true })
                                }
                                _ => json!({ "ok": false }),
                            }
                        }
                    }
                    None => json!({ "ok": false }),
                },
                Some("flash") if lamps.contains_key(id) => json!({ "ok": true }),
                _ => json!({ "ok": false }),
            }
        };
        let mut reply = b"LAMP".to_vec();
        reply.extend_from_slice(answer.to_string().as_bytes());
        Some(switches.garble(reply))
    })
    .await
}

/// A fixture that drives every check against a lamp that can be rigged.
pub async fn fixture() -> Fixture {
    fixture_with(true).await
}

/// The same, with the lamp wrapped in [`Rigged`] or bare.
pub async fn fixture_with(rigged: bool) -> Fixture {
    let switches = Switches::default();
    let device = start_device(switches.clone()).await;
    let port = device.addr.port();
    let config = json!({ "host": "127.0.0.1", "port": port, "lamps": [{ "id": "one", "name": "Desk" }] });
    let values = BTreeMap::from([
        ("host".to_string(), Scalar::Text("127.0.0.1".to_string())),
        ("port".to_string(), Scalar::Int(i64::from(port))),
        ("key".to_string(), Scalar::Text(KEY.to_string())),
    ]);
    Fixture {
        config,
        secrets: vec![("key".to_string(), KEY.to_string())],
        adopted: Vec::new(),
        device: if rigged { Box::new(Rigged::new(device, switches)) } else { Box::new(device) },
        ops: vec![
            ("one".to_string(), Op::LightPower { on: true }),
            ("one".to_string(), Op::LightBrightness { percent: 40 }),
            (
                "one".to_string(),
                Op::Custom {
                    action: "lamp.flash".to_string(),
                    args: BTreeMap::from([("times".to_string(), Scalar::Int(2))]),
                },
            ),
        ],
        setup: vec![SetupInput::Values { values }],
    }
}

/// The slots a fixture fills, for a pure call in a test.
pub fn slots() -> BTreeSet<String> {
    BTreeSet::from(["key".to_string()])
}
