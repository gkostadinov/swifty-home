//! The mock plugin: in-memory devices of every class.
//!
//! It is the test double of the hub (one world of devices shared by every mock
//! instance, with behaviours that stand in for silence, refusal and devices
//! that ignore a command), the demo home for `swifty init --demo`, and the
//! smallest complete plugin to read when writing one.
//!
//! - [`PLUGIN`] is the plugin. Each instance gets a fresh world of its own when
//!   it connects. [`MockPlugin::bound`] makes one whose instances all share a
//!   world a test holds.
//! - [`MockDevices`] is that world, with the helpers the tests use.
//! - [`legacy_house`] is today's house as mock instances, and [`demo_home`] the
//!   demo home.
//!
//! It opens no socket, reads no file and needs no live grant beyond the one the
//! hub hands every driver.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod config;
mod driver;
mod house;
mod setup;

use std::sync::{Arc, Mutex, Weak};

use serde_json::json;
use swifty_plugin_api::{
    CapKind, Capability, ConfigField, ConfigProblem, CustomAction, DeviceClass, DeviceDecl, DeviceRecord, Driver,
    Expect, FieldKind, InstanceView, LiveContext, Manifest, Op, ParamDef, ParamKind, Plugin, PluginError, Reach,
    Readiness, Scalar, SecretSlot, SetupFlow, Tier, API_VERSION,
};

pub use config::{default_capabilities, default_name, CapabilityRow, DeviceRow, MockConfig};
pub use driver::{adopted_capabilities, default_state, Behavior, MockDevices, MockDriver, UNREADABLE};
pub use house::{
    demo_home, demo_rows, legacy_house, legacy_house_with_vms, House, HouseDevice, HouseInstance, HouseNote,
    HouseScene, Origin, SceneOverride, Status, DEMO_DEVICES, LEGACY_VMS, PLUGIN_ID, PRIMARY_HOST,
};
pub use setup::MockSetup;

/// The one custom action: sets a gadget's level.
pub const CUSTOM_ACTION: &str = "mock.set_level";
/// The highest level [`CUSTOM_ACTION`] takes.
pub const MAX_LEVEL: i64 = 10;
/// The secret slot of the optional access code.
pub const ACCESS_CODE: &str = "access_code";

/// Why an instance that needs its access code is not ready.
pub const CODE_MISSING: &str = "The access code is not set. Run the setup again and enter one.";
/// The refusal of a device that is not part of the instance.
pub const NOT_HERE: &str = "This device is not part of this mock instance.";
/// The refusal of an operation a device's capabilities do not cover.
pub const CANNOT: &str = "This device cannot do that.";

/// Every class, in the order of the contract.
pub const CLASSES: &[DeviceClass] = &[
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

const CLASS_NAMES: &[&str] = &[
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
    "other",
];

const CAP_NAMES: &[&str] = &[
    "power",
    "brightness",
    "color",
    "volume",
    "mute",
    "night_mode",
    "transport",
    "inputs",
    "keys",
    "mode",
    "temperature",
    "fan",
    "dispense",
    "clean",
    "print_job",
    "emergency_stop",
    "lifecycle",
    "node_power",
    "tunnel",
    "metrics",
    "history",
    "console",
    "agent_pairing",
    "custom",
];

const CAPABILITY_FIELDS: &[ConfigField] = &[
    ConfigField {
        key: "cap",
        label: "Capability",
        help: "Which capability.",
        kind: FieldKind::Choice { options: CAP_NAMES },
        required: true,
        default: None,
    },
    ConfigField {
        key: "min",
        label: "Lowest",
        help: "For brightness, the lowest level in percent (default 10). For temperature, the lowest target in °C (default 17).",
        kind: FieldKind::Int { min: 0, max: 255 },
        required: false,
        default: None,
    },
    ConfigField {
        key: "max",
        label: "Highest",
        help: "For temperature, the highest target in °C (default 30).",
        kind: FieldKind::Int { min: 0, max: 255 },
        required: false,
        default: None,
    },
    ConfigField {
        key: "options",
        label: "Values",
        help: "For inputs, keys, mode, fan and dispense: the accepted values, separated by commas. Portions are in grams.",
        kind: FieldKind::Text { max_len: 2048 },
        required: false,
        default: None,
    },
    ConfigField {
        key: "action",
        label: "Action",
        help: "For custom: the action id, mock.set_level.",
        kind: FieldKind::Text { max_len: 64 },
        required: false,
        default: None,
    },
];

const DEVICE_FIELDS: &[ConfigField] = &[
    ConfigField {
        key: "id",
        label: "Id",
        help: "The device's id within this instance: lower-case letters, digits, - and _.",
        kind: FieldKind::Text { max_len: 64 },
        required: true,
        default: None,
    },
    ConfigField {
        key: "class",
        label: "Class",
        help: "What kind of device it pretends to be.",
        kind: FieldKind::Choice { options: CLASS_NAMES },
        required: true,
        default: None,
    },
    ConfigField {
        key: "name",
        label: "Name",
        help: "Its name. Left empty, a generic name for its class.",
        kind: FieldKind::Text { max_len: 64 },
        required: false,
        default: None,
    },
    ConfigField {
        key: "target",
        label: "Target",
        help: "The id to ask the hub for. Left empty, the hub picks <instance>-<id>.",
        kind: FieldKind::Text { max_len: 64 },
        required: false,
        default: None,
    },
    ConfigField {
        key: "capabilities",
        label: "Capabilities",
        help: "What it can do. Left out, the usual set for its class. An empty list makes it read-only.",
        kind: FieldKind::Rows { fields: CAPABILITY_FIELDS },
        required: false,
        default: None,
    },
];

const CONFIG_FIELDS: &[ConfigField] = &[
    ConfigField {
        key: "devices",
        label: "Devices",
        help: "One row per pretend device.",
        kind: FieldKind::Rows { fields: DEVICE_FIELDS },
        required: true,
        default: None,
    },
    ConfigField {
        key: "disabled",
        label: "Disabled because",
        help: "A sentence. When set, the instance is listed as disabled with it.",
        kind: FieldKind::Text { max_len: 200 },
        required: false,
        default: None,
    },
    ConfigField {
        key: "require_code",
        label: "Require the access code",
        help: "When on, the instance needs its access code before it is ready.",
        kind: FieldKind::Bool,
        required: false,
        default: Some("false"),
    },
];

const LEVEL: ParamDef = ParamDef {
    name: "level",
    kind: ParamKind::Int { min: 0, max: MAX_LEVEL },
    required: true,
    help: "The level, from 0 to 10.",
};

/// What the mock plugin is.
pub static MANIFEST: Manifest = Manifest {
    id: PLUGIN_ID,
    name: "Mock devices",
    version: env!("CARGO_PKG_VERSION"),
    api: API_VERSION,
    summary: "Pretend devices of every class that live in the hub's memory. For trying Swifty Home out, and for tests.",
    reach: Reach::Local,
    network: &[],
    classes: CLASSES,
    config_version: 1,
    config: CONFIG_FIELDS,
    secrets: &[SecretSlot {
        slot: ACCESS_CODE,
        label: "Access code",
        help: "Optional. A stand-in credential, to see how the hub keeps one. The mock never shows it.",
        required: false,
    }],
    actions: &[CustomAction {
        id: CUSTOM_ACTION,
        label: "Set a gadget's level",
        description: "Sets the level of a pretend gadget, a device of a class the hub has no shape for.",
        consequences: &["Nothing real changes: the gadget lives in memory."],
        class: DeviceClass::Other,
        tier: Tier::Comfort,
        params: &[LEVEL],
        idempotent: true,
        confirm_timeout_ms: 3_000,
        expect: Expect::FieldEqualsArg { field: "level", arg: "level" },
    }],
    vocabulary: &[],
    nouns: &[("gadget", DeviceClass::Other)],
    multi_instance: true,
};

/// A device as the mock knows it: its class and what it can do.
#[derive(Debug, Clone, PartialEq)]
pub struct Known {
    /// Its class.
    pub class: DeviceClass,
    /// Its capabilities.
    pub capabilities: Vec<Capability>,
}

impl Known {
    /// A device named by a config row.
    pub fn from_row(row: &DeviceRow) -> Self {
        Self { class: row.class, capabilities: row.capability_list() }
    }

    /// An adopted device.
    pub fn from_record(record: &DeviceRecord) -> Self {
        Self { class: record.class, capabilities: adopted_capabilities(record) }
    }
}

/// The device with this local id: a config row first, then an adopted record.
fn known_device(config: &MockConfig, local_id: &str, adopted: Option<&DeviceRecord>) -> Option<Known> {
    match config.devices.iter().find(|row| row.id == local_id) {
        Some(row) => Some(Known::from_row(row)),
        None => adopted.filter(|record| record.local_id == local_id).map(Known::from_record),
    }
}

/// Why a device with these capabilities will not do `op`, if it will not. Pure.
pub fn op_refusal(known: &Known, op: &Op) -> Option<String> {
    let find = |kind: CapKind| known.capabilities.iter().find(|capability| capability.kind() == kind);
    let need = |kind: CapKind| find(kind).is_none().then(|| CANNOT.to_string());
    let one_of = |kind: CapKind, value: &str, what: &str| {
        let words = match find(kind)? {
            Capability::Inputs { inputs } => inputs,
            Capability::Keys { keys } => keys,
            Capability::Mode { modes } => modes,
            Capability::Fan { speeds } => speeds,
            _ => return None,
        };
        (!words.iter().any(|word| word == value)).then(|| format!("This device has no such {what}."))
    };
    let brightness = |percent: u8| match find(CapKind::Brightness) {
        None => Some(CANNOT.to_string()),
        Some(Capability::Brightness { min }) if percent < *min => {
            Some(format!("This light dims to {min} % at the lowest."))
        }
        Some(_) if percent > 100 => Some("Brightness goes up to 100 %.".to_string()),
        Some(_) => None,
    };
    let color = |hue: u16| match find(CapKind::Color) {
        None => Some(CANNOT.to_string()),
        Some(_) if hue > 359 => Some("A colour is a hue angle from 0 to 359.".to_string()),
        Some(_) => None,
    };
    let volume =
        |level: u8| need(CapKind::Volume).or_else(|| (level > 100).then(|| "Volume goes from 0 to 100.".to_string()));
    match op {
        Op::LightPower { .. } | Op::PlugPower { .. } | Op::AirconPower { .. } | Op::TvPower { .. } => {
            need(CapKind::Power)
        }
        Op::LightBrightness { percent } => brightness(*percent),
        Op::LightHue { hue } => color(*hue),
        Op::LightSet { percent, hue } => brightness(*percent).or_else(|| color(*hue)),
        Op::AirconMode { mode } => need(CapKind::Mode).or_else(|| one_of(CapKind::Mode, mode, "mode")),
        Op::AirconFan { fan } => need(CapKind::Fan).or_else(|| one_of(CapKind::Fan, fan, "fan speed")),
        Op::AirconTemp { celsius } => match find(CapKind::Temperature) {
            Some(Capability::Temperature { min, max }) if !(*min..=*max).contains(celsius) => {
                Some(format!("This device takes {min} to {max} °C."))
            }
            Some(_) => None,
            None => Some(CANNOT.to_string()),
        },
        Op::Volume { level } => volume(*level),
        Op::VolumeCap { max } => volume(*max),
        Op::Mute { .. } => need(CapKind::Mute),
        Op::NightMode { .. } => need(CapKind::NightMode),
        Op::Play | Op::Pause => need(CapKind::Transport),
        Op::TvInput { input } => need(CapKind::Inputs).or_else(|| one_of(CapKind::Inputs, input, "input")),
        Op::TvKey { key } => need(CapKind::Keys).or_else(|| one_of(CapKind::Keys, key, "key")),
        Op::PetFeed { grams } => match find(CapKind::Dispense) {
            Some(Capability::Dispense { grams: portions }) if !portions.contains(grams) => {
                let list = portions.iter().map(u8::to_string).collect::<Vec<_>>().join(", ");
                Some(format!("This feeder dispenses {list} g at a time."))
            }
            Some(_) => None,
            None => Some(CANNOT.to_string()),
        },
        Op::LitterClean => need(CapKind::Clean),
        Op::PrintStart | Op::PrintPause | Op::PrintResume => need(CapKind::PrintJob),
        Op::PrintEmergencyStop => need(CapKind::EmergencyStop),
        Op::VmStart | Op::VmShutdown | Op::VmReboot | Op::VmForceStop => need(CapKind::Lifecycle),
        Op::HostShutdown | Op::HostReboot => need(CapKind::NodePower),
        Op::WireguardRestart => need(CapKind::Tunnel),
        Op::Custom { action, args } => {
            let offered = known
                .capabilities
                .iter()
                .any(|capability| matches!(capability, Capability::Custom { action: offered } if offered == action));
            if !offered || action != CUSTOM_ACTION {
                return Some(CANNOT.to_string());
            }
            match (args.get("level"), args.len()) {
                (Some(Scalar::Int(level)), 1) if (0..=MAX_LEVEL).contains(level) => None,
                _ => Some(format!("{CUSTOM_ACTION} takes one level, a whole number from 0 to {MAX_LEVEL}.")),
            }
        }
        _ => Some(CANNOT.to_string()),
    }
}

/// The mock plugin. [`PLUGIN`] gives each instance a world of its own;
/// [`MockPlugin::bound`] shares one world between all its instances.
pub struct MockPlugin {
    world: Option<Arc<MockDevices>>,
}

/// The mock plugin, for the hub's list of plugins.
pub static PLUGIN: MockPlugin = MockPlugin { world: None };

/// The worlds [`PLUGIN`] made, by instance, newest last. Weak: a world goes
/// with the last driver that drives it.
static WORLDS: Mutex<Vec<(String, Weak<MockDevices>)>> = Mutex::new(Vec::new());

impl MockPlugin {
    /// A mock plugin whose instances all drive this world, so a test can set
    /// behaviours and look at what was sent. It also answers readiness and
    /// refusals from the world's [`MockDevices::set_disabled`]. Each call leaks
    /// one small value, which is fine in tests and nowhere else.
    pub fn bound(world: &Arc<MockDevices>) -> &'static MockPlugin {
        Box::leak(Box::new(MockPlugin { world: Some(world.clone()) }))
    }

    /// The world this plugin is bound to, if it is.
    pub fn world(&self) -> Option<&Arc<MockDevices>> {
        self.world.as_ref()
    }

    /// The world [`PLUGIN`] made when it last connected `instance`, while that
    /// instance's driver is alive, so a test can look at what its devices were
    /// asked and set their behaviours. A bound plugin's instances drive the
    /// world it was given instead.
    pub fn world_of(instance: &str) -> Option<Arc<MockDevices>> {
        let worlds = WORLDS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        worlds.iter().rev().find(|(id, _)| id == instance).and_then(|(_, world)| world.upgrade())
    }

    fn disabled(&self, instance: &str, config: &MockConfig) -> Option<String> {
        self.world.as_ref().and_then(|world| world.disabled(instance)).or_else(|| config.disabled.clone())
    }
}

impl Plugin for MockPlugin {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn validate(&self, config: &serde_json::Value) -> Result<(), ConfigProblem> {
        config::parse(config).map(|_| ())
    }

    fn readiness(&self, instance: &InstanceView<'_>) -> Readiness {
        let config = match config::parse(instance.config) {
            Ok(config) => config,
            Err(problem) => return Readiness::Disabled(format!("The config does not validate: {problem}")),
        };
        if let Some(reason) = self.disabled(instance.id, &config) {
            return Readiness::Disabled(reason);
        }
        if config.require_code && !instance.secrets_present.contains(ACCESS_CODE) {
            return Readiness::NeedsSetup(CODE_MISSING.to_string());
        }
        Readiness::Ready
    }

    fn devices(&self, instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
        let Ok(config) = config::parse(instance.config) else {
            return Vec::new();
        };
        let mut decls: Vec<DeviceDecl> = config
            .devices
            .iter()
            .map(|row| DeviceDecl {
                local_id: row.id.clone(),
                target_hint: row.target.clone(),
                class: row.class,
                name: row.display_name(),
                capabilities: row.capability_list(),
                attrs: json!({}),
            })
            .collect();
        for record in instance.adopted {
            if !decls.iter().any(|decl| decl.local_id == record.local_id) {
                decls.push(DeviceDecl {
                    local_id: record.local_id.clone(),
                    target_hint: Some(record.target.clone()),
                    class: record.class,
                    name: record.name.clone(),
                    capabilities: adopted_capabilities(record),
                    attrs: record.attrs.clone(),
                });
            }
        }
        decls
    }

    fn refusal(&self, instance: &InstanceView<'_>, device: &str, op: &Op) -> Option<String> {
        let config = match config::parse(instance.config) {
            Ok(config) => config,
            Err(problem) => return Some(format!("The config does not validate: {problem}")),
        };
        if let Some(reason) = self.disabled(instance.id, &config) {
            return Some(reason);
        }
        let adopted = instance.adopted.iter().find(|record| record.local_id == device);
        match known_device(&config, device, adopted) {
            None => Some(NOT_HERE.to_string()),
            Some(known) => op_refusal(&known, op),
        }
    }

    fn connect(&self, ctx: LiveContext) -> Result<Arc<dyn Driver>, PluginError> {
        let config = config::parse(&ctx.config)
            .map_err(|problem| PluginError(format!("The config does not validate: {problem}")))?;
        if config.require_code {
            ctx.secrets.require(ACCESS_CODE)?;
        }
        let world = match &self.world {
            Some(world) => world.clone(),
            None => {
                let world = MockDevices::empty();
                let mut worlds = WORLDS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                worlds.retain(|(_, made)| made.strong_count() > 0);
                worlds.push((ctx.instance_id.clone(), Arc::downgrade(&world)));
                world
            }
        };
        Ok(Arc::new(MockDriver::new(&ctx.instance_id, config, ctx.directory, world)))
    }

    fn setup(&self) -> Option<Box<dyn SetupFlow>> {
        Some(Box::new(MockSetup::default()))
    }
}
