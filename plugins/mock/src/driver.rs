//! The in-memory devices and the driver that talks to them. Nothing here opens
//! a socket. Behaviour can be set per device to stand in for silence, refusal
//! and devices that accept a command and then do nothing.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use swifty_plugin_api::{
    net, AgentPairing, CapKind, Capability, CommandAck, ConsoleGrant, DeviceClass, DeviceDecl, DeviceError,
    DeviceRecord, Directory, Driver, ExtraHost, Health, HealthStatus, HistoryRange, MachineHost, Op, Scalar,
    SeenMachine, State,
};

use crate::config::{default_capabilities, MockConfig, AC_FANS, AC_MODES, TV_INPUTS};
use crate::{op_refusal, Known, LEGACY_VMS, NOT_HERE};

/// How a device answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Behavior {
    /// Accepts the command and applies it.
    Normal,
    /// Answers with a timeout straight away. The state does not change.
    Silent,
    /// Never answers, so only the engine's own deadline can end the wait.
    Hang,
    /// Accepts the command but the state never changes.
    AcceptsButIgnores,
    /// Answers with a refusal.
    Rejects(String),
    /// Accepts the command, applies it after a delay in milliseconds.
    Slow(u64),
    /// Answers with something that cannot be read, as a device whose replies
    /// are cut short or spoiled: every command and read fails, and nothing changes.
    Garbles,
}

/// What a driver says of a device whose answer could not be read.
pub const UNREADABLE: &str = "The device sent an answer that could not be read.";

/// A state with every piece of text in it made safe to show and store, as
/// anything a device says must be: each string and key goes through
/// `net::device_text`.
fn cleaned(state: State) -> State {
    fn clean(value: Value) -> Value {
        match value {
            Value::String(text) => Value::String(net::device_text(&text)),
            Value::Array(items) => Value::Array(items.into_iter().map(clean).collect()),
            Value::Object(fields) => {
                Value::Object(fields.into_iter().map(|(key, value)| (net::device_text(&key), clean(value))).collect())
            }
            other => other,
        }
    }
    match serde_json::to_value(&state) {
        Ok(value) => serde_json::from_value(clean(value)).unwrap_or(state),
        Err(_) => state,
    }
}

#[derive(Default)]
struct Inner {
    states: HashMap<String, State>,
    behaviors: HashMap<String, Behavior>,
    commands: Vec<(String, Op)>,
    reads: Vec<String>,
    disabled: HashMap<String, String>,
    seen_machines: Vec<SeenMachine>,
    discoverable: Vec<(String, DeviceDecl)>,
}

/// The shared state behind every mock driver: one in-memory world of devices,
/// by local id, and what was asked of them.
#[derive(Default)]
pub struct MockDevices {
    inner: Mutex<Inner>,
}

impl MockDevices {
    /// A world holding today's house: the states of every device of
    /// [`crate::legacy_house`], plus a smart socket and an air conditioner.
    pub fn new() -> Arc<Self> {
        let devices = Self::default();
        {
            let mut inner = devices.lock();
            let bulbs = [(1, true, 60, 38), (2, true, 45, 30), (3, false, 70, 205), (4, true, 30, 14)];
            for (n, on, bri, hue) in bulbs {
                inner.states.insert(format!("bulb{n}"), State::Light { on, bri, hue, color: true });
            }
            for (vmid, _) in LEGACY_VMS {
                inner.states.insert(format!("vm{vmid}"), State::Vm { state: "running".into(), uptime_s: 1_000 });
            }
            let others = [
                ("hue", State::Bridge { reachable: true }),
                ("plug-bf0123456789abcdef", State::Plug { on: false }),
                (
                    "ac-0a1b2c3d-1111-2222-3333-444455556666",
                    State::Aircon {
                        on: false,
                        mode: Some("cool".into()),
                        target: Some(22),
                        fan: Some("auto".into()),
                        indoor: Some(24),
                        outdoor: Some(10),
                    },
                ),
                (
                    "sonos",
                    State::Speaker { vol: 24, mute: false, playing: true, source: "TV".into(), night: Some(false) },
                ),
                ("sub", State::Speaker { vol: 24, mute: false, playing: false, source: "Sub".into(), night: None }),
                ("tv", State::Tv { on: true, input: "HDMI 1".into() }),
                ("feeder", State::Feeder { last: Some("18:30".into()), portions: 3 }),
                ("litter", State::Litter { last: Some("20:12".into()), busy: false, cycles: 9 }),
                (
                    "printer",
                    State::Printer { state: "idle".into(), nozzle: 24.0, bed: 24.0, layer: 0, layers: 0, pct: 0.0 },
                ),
                ("host", State::Host { up: true, uptime_s: 90_000 }),
                ("router", State::Router { wireguard_up: true, uptime_s: 90_000 }),
                ("camera", State::Camera { streaming: true }),
                ("plex", State::Service { reachable: true }),
                ("collector", State::Telemetry { data: json!({ "pkg_temp_c": 64 }) }),
            ];
            for (target, state) in others {
                inner.states.insert(target.to_string(), state);
            }
        }
        Arc::new(devices)
    }

    /// A world with no devices in it yet. A driver gives each of its devices
    /// the usual state for its class when it first needs one.
    pub fn empty() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A VM that the next scan will report.
    pub fn add_seen_machine(&self, host: &str, vmid: u32, name: &str) {
        self.lock().seen_machines.push(SeenMachine { host: host.to_string(), vmid, name: name.to_string() });
    }

    /// Adds a device the mock can answer for, or replaces its state.
    pub fn add_state(&self, target: &str, state: State) {
        self.lock().states.insert(target.to_string(), state);
    }

    /// Replaces the collector's readings.
    pub fn set_collector(&self, data: Value) {
        self.add_state("collector", State::Telemetry { data });
    }

    /// How this device answers from now on.
    pub fn set_behavior(&self, target: &str, behavior: Behavior) {
        self.lock().behaviors.insert(target.to_string(), behavior);
    }

    /// Disables one instance from now on, with this reason. Its readiness, its
    /// refusals and its driver all say so, and nothing reaches its devices.
    pub fn set_disabled(&self, instance: &str, reason: &str) {
        self.lock().disabled.insert(instance.to_string(), reason.to_string());
    }

    /// Why this instance was disabled with [`MockDevices::set_disabled`], if it was.
    pub fn disabled(&self, instance: &str) -> Option<String> {
        self.lock().disabled.get(instance).cloned()
    }

    /// A device the next discovery of this instance will report.
    pub fn add_discoverable(&self, instance: &str, decl: DeviceDecl) {
        self.lock().discoverable.push((instance.to_string(), decl));
    }

    /// How this device answers.
    pub fn behavior(&self, target: &str) -> Behavior {
        self.lock().behaviors.get(target).cloned().unwrap_or(Behavior::Normal)
    }

    /// The device's state now.
    pub fn state(&self, target: &str) -> Option<State> {
        self.lock().states.get(target).cloned()
    }

    /// Every command a driver took, in order, with the device's local id.
    pub fn commands(&self) -> Vec<(String, Op)> {
        self.lock().commands.clone()
    }

    /// How many commands this device was sent.
    pub fn command_count(&self, target: &str) -> usize {
        self.lock().commands.iter().filter(|(t, _)| t == target).count()
    }

    /// How many commands were sent to any device.
    pub fn total_commands(&self) -> usize {
        self.lock().commands.len()
    }

    /// How many reads reached any device, history included.
    pub fn total_reads(&self) -> usize {
        self.lock().reads.len()
    }

    /// Gives the device the usual state for its class, unless it has one.
    fn ensure_state(&self, local_id: &str, known: &Known) {
        self.lock().states.entry(local_id.to_string()).or_insert_with(|| default_state(known));
    }

    fn apply(&self, target: &str, op: &Op) {
        use State as S;
        let mut inner = self.lock();
        let Some(state) = inner.states.get_mut(target) else {
            return;
        };
        match (op, state) {
            (Op::LightPower { on: v }, S::Light { on, .. }) => *on = *v,
            (Op::LightBrightness { percent }, S::Light { on, bri, .. }) => {
                *on = true;
                *bri = *percent;
            }
            (Op::LightHue { hue: v }, S::Light { on, hue, color, .. }) => {
                *on = true;
                *hue = *v;
                *color = true;
            }
            (Op::LightSet { percent, hue: v }, S::Light { on, bri, hue, color }) => {
                *on = true;
                *bri = *percent;
                *hue = *v;
                *color = true;
            }
            (Op::PlugPower { on: v }, S::Plug { on }) => *on = *v,
            (Op::AirconPower { on: v }, S::Aircon { on, .. }) => *on = *v,
            (Op::AirconMode { mode: v }, S::Aircon { mode, .. }) => *mode = Some(v.clone()),
            (Op::AirconTemp { celsius }, S::Aircon { target, .. }) => *target = Some(i16::from(*celsius)),
            (Op::AirconFan { fan: v }, S::Aircon { fan, .. }) => *fan = Some(v.clone()),
            (Op::Volume { level }, S::Speaker { vol, .. }) => *vol = *level,
            (Op::VolumeCap { max }, S::Speaker { vol, .. }) => *vol = (*vol).min(*max),
            (Op::Mute { muted }, S::Speaker { mute, .. }) => *mute = *muted,
            (Op::NightMode { on }, S::Speaker { night, .. }) => *night = Some(*on),
            (Op::Play, S::Speaker { playing, .. }) => *playing = true,
            (Op::Pause, S::Speaker { playing, .. }) => *playing = false,
            (Op::TvPower { on: v }, S::Tv { on, .. }) => *on = *v,
            (Op::TvInput { input: v }, S::Tv { on, input }) => {
                *on = true;
                *input = v.clone();
            }
            (Op::PetFeed { .. }, S::Feeder { portions, .. }) => *portions += 1,
            (Op::LitterClean, S::Litter { cycles, .. }) => *cycles += 1,
            (Op::PrintStart | Op::PrintResume, S::Printer { state, .. }) => *state = "printing".into(),
            (Op::PrintPause, S::Printer { state, .. }) => *state = "paused".into(),
            (Op::PrintEmergencyStop, S::Printer { state, .. }) => *state = "stopped".into(),
            (Op::VmStart, S::Vm { state, uptime_s }) => {
                *state = "running".into();
                *uptime_s = 1;
            }
            (Op::VmShutdown | Op::VmForceStop, S::Vm { state, uptime_s }) => {
                *state = "stopped".into();
                *uptime_s = 0;
            }
            (Op::VmReboot, S::Vm { state, uptime_s }) => {
                *state = "running".into();
                *uptime_s = 1;
            }
            (Op::HostShutdown, S::Host { up, .. }) => *up = false,
            (Op::HostReboot, S::Host { up, uptime_s }) => {
                *up = true;
                *uptime_s = 1;
            }
            (Op::WireguardRestart, S::Router { wireguard_up, uptime_s }) => {
                *wireguard_up = true;
                *uptime_s = 1;
            }
            (Op::Custom { action, args }, S::Custom { fields }) if action == crate::CUSTOM_ACTION => {
                if let Some(level) = args.get("level") {
                    fields.insert("level".to_string(), level.clone());
                }
            }
            _ => {}
        }
    }
}

/// The state a device of this class starts in when nothing set one.
pub fn default_state(known: &Known) -> State {
    let has = |kind: CapKind| known.capabilities.iter().any(|capability| capability.kind() == kind);
    let first = |kind: CapKind, fallback: &str| {
        known
            .capabilities
            .iter()
            .find_map(|capability| match capability {
                Capability::Inputs { inputs: words } if kind == CapKind::Inputs => words.first().cloned(),
                Capability::Mode { modes: words } if kind == CapKind::Mode => words.first().cloned(),
                Capability::Fan { speeds: words } if kind == CapKind::Fan => words.first().cloned(),
                _ => None,
            })
            .unwrap_or_else(|| fallback.to_string())
    };
    use DeviceClass::*;
    match known.class {
        Light => State::Light { on: false, bri: 100, hue: 30, color: false },
        Bridge => State::Bridge { reachable: true },
        Plug => State::Plug { on: false },
        Aircon => State::Aircon {
            on: false,
            mode: Some(first(CapKind::Mode, AC_MODES[0])),
            target: Some(21),
            fan: Some(first(CapKind::Fan, AC_FANS[0])),
            indoor: Some(21),
            outdoor: Some(12),
        },
        Speaker => State::Speaker {
            vol: 20,
            mute: false,
            playing: false,
            source: "Idle".to_string(),
            night: has(CapKind::NightMode).then_some(false),
        },
        Tv => State::Tv { on: false, input: first(CapKind::Inputs, TV_INPUTS[0]) },
        Feeder => State::Feeder { last: None, portions: 0 },
        Litter => State::Litter { last: None, busy: false, cycles: 0 },
        Printer => State::Printer { state: "idle".into(), nozzle: 21.0, bed: 21.0, layer: 0, layers: 0, pct: 0.0 },
        Vm => State::Vm { state: "running".into(), uptime_s: 1_000 },
        Host => State::Host { up: true, uptime_s: 90_000 },
        Router => State::Router { wireguard_up: true, uptime_s: 90_000 },
        Camera => State::Camera { streaming: true },
        Service => State::Service { reachable: true },
        Telemetry => State::Telemetry { data: json!({}) },
        _ => State::Custom { fields: BTreeMap::from([("level".to_string(), Scalar::Int(0))]) },
    }
}

/// What a device of an adopted record can do: the list stored in its
/// `capabilities` attribute when there is one, otherwise its class's usual set.
pub fn adopted_capabilities(record: &DeviceRecord) -> Vec<Capability> {
    record
        .attrs
        .get("capabilities")
        .and_then(|list| serde_json::from_value(list.clone()).ok())
        .unwrap_or_else(|| default_capabilities(record.class))
}

/// The live side of one mock instance.
pub struct MockDriver {
    instance: String,
    config: MockConfig,
    directory: Arc<dyn Directory>,
    world: Arc<MockDevices>,
}

impl MockDriver {
    /// A driver for one instance, over this world. Every device it knows gets
    /// a state if it has none yet.
    pub fn new(instance: &str, config: MockConfig, directory: Arc<dyn Directory>, world: Arc<MockDevices>) -> Self {
        let driver = Self { instance: instance.to_string(), config, directory, world };
        for (local_id, known) in driver.everything() {
            driver.world.ensure_state(&local_id, &known);
        }
        driver
    }

    /// The device with this local id, if it belongs to this instance.
    fn known(&self, local_id: &str) -> Option<Known> {
        crate::known_device(&self.config, local_id, self.directory.get(local_id).as_ref())
    }

    /// Every device of this instance, by local id.
    fn everything(&self) -> Vec<(String, Known)> {
        let mut all: Vec<(String, Known)> =
            self.config.devices.iter().map(|row| (row.id.clone(), Known::from_row(row))).collect();
        for record in self.directory.all() {
            if !all.iter().any(|(id, _)| *id == record.local_id) {
                all.push((record.local_id.clone(), Known::from_record(&record)));
            }
        }
        all
    }

    fn disabled(&self) -> Option<String> {
        self.world.disabled(&self.instance).or_else(|| self.config.disabled.clone())
    }
}

#[async_trait]
impl Driver for MockDriver {
    async fn command(&self, device: &str, op: &Op) -> Result<CommandAck, DeviceError> {
        if let Some(reason) = self.disabled() {
            return Err(DeviceError::Disabled(reason));
        }
        let known = self.known(device).ok_or_else(|| DeviceError::Rejected(NOT_HERE.to_string()))?;
        if let Some(reason) = op_refusal(&known, op) {
            return Err(DeviceError::Rejected(reason));
        }
        self.world.ensure_state(device, &known);
        self.world.lock().commands.push((device.to_string(), op.clone()));
        match self.world.behavior(device) {
            Behavior::Normal => {
                self.world.apply(device, op);
                Ok(CommandAck)
            }
            Behavior::Silent => Err(DeviceError::Timeout),
            Behavior::Hang => {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Err(DeviceError::Timeout)
            }
            Behavior::AcceptsButIgnores => Ok(CommandAck),
            Behavior::Rejects(reason) => Err(DeviceError::Rejected(reason)),
            Behavior::Garbles => Err(DeviceError::Rejected(UNREADABLE.to_string())),
            Behavior::Slow(ms) => {
                let world = self.world.clone();
                let device = device.to_string();
                let op = op.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                    world.apply(&device, &op);
                });
                Ok(CommandAck)
            }
        }
    }

    async fn read_state(&self, device: &str) -> Result<State, DeviceError> {
        if let Some(reason) = self.disabled() {
            return Err(DeviceError::Disabled(reason));
        }
        // A device that is not this instance's is not there to be asked.
        let known = self.known(device).ok_or_else(|| DeviceError::Unreachable(NOT_HERE.to_string()))?;
        self.world.lock().reads.push(device.to_string());
        match self.world.behavior(device) {
            Behavior::Silent => return Err(DeviceError::Timeout),
            Behavior::Garbles => return Err(DeviceError::Rejected(UNREADABLE.to_string())),
            _ => {}
        }
        self.world.ensure_state(device, &known);
        self.world.state(device).map(cleaned).ok_or_else(|| DeviceError::Unreachable(NOT_HERE.to_string()))
    }

    async fn discover(&self, _window: Duration) -> Result<Vec<DeviceDecl>, DeviceError> {
        if let Some(reason) = self.disabled() {
            return Err(DeviceError::Disabled(reason));
        }
        let found = self.world.lock().discoverable.clone();
        Ok(found
            .into_iter()
            .filter(|(instance, _)| *instance == self.instance)
            .map(|(_, mut decl)| {
                decl.name = net::device_text(&decl.name);
                // Kept with the record when it is adopted, so the device keeps
                // these capabilities after a restart.
                if let Value::Object(attrs) = &mut decl.attrs {
                    attrs.insert("capabilities".to_string(), json!(decl.capabilities));
                } else {
                    decl.attrs = json!({ "capabilities": decl.capabilities });
                }
                decl
            })
            .collect())
    }

    async fn health(&self) -> Health {
        match self.disabled() {
            Some(reason) => Health { status: HealthStatus::Down, detail: Some(reason) },
            None => Health { status: HealthStatus::Ok, detail: None },
        }
    }

    fn machines(&self) -> Option<&dyn MachineHost> {
        let hosts =
            self.everything().iter().any(|(_, known)| matches!(known.class, DeviceClass::Vm | DeviceClass::Host));
        hosts.then_some(self as &dyn MachineHost)
    }

    fn agents(&self) -> Option<&dyn AgentPairing> {
        let pairs = self
            .everything()
            .iter()
            .any(|(_, known)| known.capabilities.iter().any(|capability| capability.kind() == CapKind::AgentPairing));
        pairs.then_some(self as &dyn AgentPairing)
    }
}

#[async_trait]
impl MachineHost for MockDriver {
    async fn list_machines(&self) -> Result<Vec<SeenMachine>, DeviceError> {
        Ok(self.world.lock().seen_machines.clone())
    }

    async fn history(&self, device: &str, _range: HistoryRange) -> Result<Vec<Value>, DeviceError> {
        if self.known(device).is_none() {
            return Err(DeviceError::Unreachable(NOT_HERE.to_string()));
        }
        self.world.lock().reads.push(format!("rrd:{device}"));
        Ok(vec![json!({ "time": 1, "cpu": 0.25 })])
    }

    async fn open_console(&self, _device: &str) -> Result<ConsoleGrant, DeviceError> {
        Err(DeviceError::unsupported())
    }

    async fn connect_console(&self, _grant: &ConsoleGrant) -> Result<net::ConsoleLink, DeviceError> {
        Err(DeviceError::unsupported())
    }

    fn add_host(&self, _spec: &ExtraHost) -> Result<(), String> {
        Err("This device cannot do that.".to_string())
    }

    fn remove_host(&self, _id: &str) {}
}

#[async_trait]
impl AgentPairing for MockDriver {
    async fn pair_agent(&self, name: &str) -> Result<Value, DeviceError> {
        Ok(json!({ "name": name, "code": "MOCK-CODE" }))
    }
}
