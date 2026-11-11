//! Whole homes made of mock instances: today's house for the hub's tests, and
//! the demo home for `swifty init --demo`.
//!
//! These are plain data in the shapes of the hub's `plugin_instances`,
//! `devices`, `scenes` and `device_notes` tables. The hub converts them into its
//! own types; this crate does not depend on the hub.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use swifty_plugin_api::{Capability, DeviceClass, DeviceRecord};

use crate::config::{DeviceRow, MockConfig};
use crate::MANIFEST;

/// The plugin id every instance here uses.
pub const PLUGIN_ID: &str = "mock";

/// The four VMs of [`legacy_house`]: VM id and name. Generic on purpose; a
/// test that needs other VMs passes its own list to [`legacy_house_with_vms`].
pub const LEGACY_VMS: [(u32, &str); 4] = [(200, "alpha"), (201, "bravo"), (202, "charlie"), (203, "delta")];

/// Where the VMs of [`legacy_house`] run: the name of the primary Proxmox
/// endpoint, as the stored machines have it.
pub const PRIMARY_HOST: &str = "primary";

/// A home: instances, the devices stored for them, scenes and notes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct House {
    /// One row of `plugin_instances` each.
    pub instances: Vec<HouseInstance>,
    /// One row of `devices` each.
    pub devices: Vec<HouseDevice>,
    /// One row of `scenes` each.
    pub scenes: Vec<HouseScene>,
    /// One row of `device_notes` each.
    pub notes: Vec<HouseNote>,
}

impl House {
    /// The instance with this id.
    pub fn instance(&self, id: &str) -> Option<&HouseInstance> {
        self.instances.iter().find(|instance| instance.id == id)
    }

    /// The device with this target.
    pub fn device(&self, target: &str) -> Option<&HouseDevice> {
        self.devices.iter().find(|device| device.target == target)
    }

    /// The adopted, active devices of one instance, as a plugin sees them.
    pub fn adopted(&self, instance: &str) -> Vec<DeviceRecord> {
        self.devices
            .iter()
            .filter(|device| {
                device.instance == instance && device.origin == Origin::Adopted && device.status == Status::Active
            })
            .map(HouseDevice::record)
            .collect()
    }
}

/// A plugin instance: the hub's `InstanceSpec`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HouseInstance {
    /// The instance id.
    pub id: String,
    /// The plugin it is an instance of.
    pub plugin: String,
    /// Its config. Passes the plugin's `validate`.
    pub config: Value,
    /// The plugin's config version the config was written for.
    pub config_version: u32,
    /// Whether it is loaded.
    pub enabled: bool,
}

/// Where a stored device came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Named by the instance's config.
    Config,
    /// Discovered and then adopted (lights, approved VMs).
    Adopted,
}

/// Whether a stored device can be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// It can.
    Active,
    /// It waits for approval.
    Pending,
}

/// A row of the hub's `devices` table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HouseDevice {
    /// The instance it belongs to.
    pub instance: String,
    /// Its target.
    pub target: String,
    /// The id the plugin knows it by.
    pub local_id: String,
    /// What kind of thing it is.
    pub class: DeviceClass,
    /// Its name.
    pub name: String,
    /// What the plugin stored to find it again.
    pub attrs: Value,
    /// Where it came from.
    pub origin: Origin,
    /// Whether it can be used.
    pub status: Status,
}

impl HouseDevice {
    /// The record a plugin gets back for it.
    pub fn record(&self) -> DeviceRecord {
        DeviceRecord {
            target: self.target.clone(),
            local_id: self.local_id.clone(),
            class: self.class,
            name: self.name.clone(),
            attrs: self.attrs.clone(),
        }
    }
}

/// A row of the hub's `scenes` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HouseScene {
    /// The scene id, which is also its target.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Where it is listed.
    pub position: u32,
    /// The level of every light without an override. 0 means off.
    pub percent: u8,
    /// The colour of every light without an override.
    pub hue: u16,
    /// Lights with a level of their own.
    pub overrides: Vec<SceneOverride>,
    /// What else the scene does: Movie's `tv_input` and `volume_cap`.
    pub extras: Value,
}

/// One light's own level in a scene.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneOverride {
    /// The light.
    pub target: String,
    /// Its level. 0 means off.
    pub percent: u8,
    /// Its colour.
    pub hue: u16,
}

/// A row of the hub's `device_notes` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HouseNote {
    /// The device.
    pub target: String,
    /// Free lines about it, such as what depends on it.
    pub notes: Vec<String>,
}

fn instance(id: &str, config: &MockConfig) -> HouseInstance {
    HouseInstance {
        id: id.to_string(),
        plugin: PLUGIN_ID.to_string(),
        config: serde_json::to_value(config).unwrap_or_default(),
        config_version: MANIFEST.config_version,
        enabled: true,
    }
}

fn config_device(instance: &str, row: &DeviceRow) -> HouseDevice {
    HouseDevice {
        instance: instance.to_string(),
        target: row.target.clone().unwrap_or_else(|| row.id.clone()),
        local_id: row.id.clone(),
        class: row.class,
        name: row.display_name(),
        attrs: json!({}),
        origin: Origin::Config,
        status: Status::Active,
    }
}

fn scene(id: &str, name: &str, position: u32, (percent, hue): (u8, u16), overrides: &[(u8, u8, u16)]) -> HouseScene {
    HouseScene {
        id: id.to_string(),
        name: name.to_string(),
        position,
        percent,
        hue,
        overrides: overrides
            .iter()
            .map(|(bulb, percent, hue)| SceneOverride { target: format!("bulb{bulb}"), percent: *percent, hue: *hue })
            .collect(),
        extras: json!({}),
    }
}

/// Today's house, with [`LEGACY_VMS`] as its four VMs. See [`legacy_house_with_vms`].
pub fn legacy_house() -> House {
    legacy_house_with_vms(&LEGACY_VMS)
}

/// Today's house as mock instances: one instance per old adapter kind, named
/// after it (`wiz`, `hue`, `sonos`, `webos`, `moonraker`, `plex`, `camera`,
/// `petkit`, `proxmox`, `collector`, `router`, `tuya`, `toshiba`), the same
/// targets, classes and capabilities, these VMs adopted under `proxmox`, and
/// the four house scenes with their per-bulb levels. `tuya` and `toshiba` start
/// with no devices, as the house does without sockets or air conditioners
/// configured. Names are generic.
pub fn legacy_house_with_vms(vms: &[(u32, &str)]) -> House {
    use Capability as C;
    use DeviceClass::*;
    let rows: Vec<(&str, Vec<DeviceRow>)> = vec![
        ("wiz", (1..=4).map(|n| DeviceRow::new(&format!("bulb{n}"), Light).named(&format!("Bulb {n}"))).collect()),
        ("hue", vec![DeviceRow::new("hue", Bridge).named("Light bridge")]),
        (
            "sonos",
            vec![
                DeviceRow::new("sonos", Speaker).named("Sound bar"),
                DeviceRow::new("sub", Speaker).named("Subwoofer").with(&[]),
            ],
        ),
        ("webos", vec![DeviceRow::new("tv", Tv).named("TV")]),
        ("moonraker", vec![DeviceRow::new("printer", Printer).named("3D printer")]),
        ("plex", vec![DeviceRow::new("plex", Service).named("Media server")]),
        ("camera", vec![DeviceRow::new("camera", Camera).named("Camera")]),
        (
            "petkit",
            vec![
                DeviceRow::new("feeder", Feeder).named("Feeder"),
                DeviceRow::new("litter", Litter).named("Litter box"),
            ],
        ),
        ("proxmox", vec![DeviceRow::new("host", Host).named("Host")]),
        (
            "collector",
            vec![DeviceRow::new("collector", Telemetry).named("Collector").with(&[C::Metrics, C::AgentPairing])],
        ),
        ("router", vec![DeviceRow::new("router", Router).named("Router")]),
        ("tuya", Vec::new()),
        ("toshiba", Vec::new()),
    ];
    let mut house = House { instances: Vec::new(), devices: Vec::new(), scenes: Vec::new(), notes: Vec::new() };
    for (id, devices) in rows {
        let config = MockConfig {
            devices: devices.into_iter().map(DeviceRow::hinted).collect(),
            disabled: None,
            require_code: false,
        };
        house.devices.extend(config.devices.iter().map(|row| config_device(id, row)));
        house.instances.push(instance(id, &config));
    }
    house.devices.extend(vms.iter().map(|(vmid, name)| HouseDevice {
        instance: "proxmox".to_string(),
        target: format!("vm{vmid}"),
        local_id: format!("vm{vmid}"),
        class: Vm,
        name: name.to_string(),
        attrs: json!({ "host": PRIMARY_HOST, "vmid": vmid }),
        origin: Origin::Adopted,
        status: Status::Active,
    }));

    // (bulb, level, hue) where a bulb differs from the first, which sets the scene's level.
    let movie = scene("movie", "Movie", 3, (10, 14), &[(2, 8, 14), (3, 0, 0), (4, 0, 0)]);
    house.scenes = vec![
        scene("evening", "Evening", 0, (40, 34), &[(2, 35, 30), (3, 0, 0), (4, 25, 14)]),
        scene("focus", "Focus", 1, (90, 52), &[(4, 70, 52)]),
        scene("all_off", "All off", 2, (0, 0), &[]),
        HouseScene { extras: json!({ "tv_input": "HDMI 1", "volume_cap": 30 }), ..movie },
    ];

    house.notes.push(HouseNote {
        target: "host".to_string(),
        notes: vec!["Every VM on it stops while it is down.".to_string()],
    });
    if let Some((vmid, _)) = vms.first() {
        house.notes.push(HouseNote {
            target: format!("vm{vmid}"),
            notes: vec!["The shared folders go offline while this is down.".to_string()],
        });
    }
    house
}

/// The devices of the demo home: local id, class and name. The setup flow
/// offers the same list.
pub const DEMO_DEVICES: &[(&str, DeviceClass, &str)] = &[
    ("lamp", DeviceClass::Light, "Floor lamp"),
    ("ceiling", DeviceClass::Light, "Ceiling light"),
    ("plug", DeviceClass::Plug, "Kettle plug"),
    ("speaker", DeviceClass::Speaker, "Speaker"),
    ("tv", DeviceClass::Tv, "TV"),
    ("aircon", DeviceClass::Aircon, "Air conditioner"),
    ("feeder", DeviceClass::Feeder, "Cat feeder"),
    ("gadget", DeviceClass::Other, "Gadget"),
];

/// The demo home's devices as config rows, with no target hints: the hub
/// gives them `mock-<id>` and so on.
pub fn demo_rows() -> Vec<DeviceRow> {
    DEMO_DEVICES.iter().map(|(id, class, name)| DeviceRow::new(id, *class).named(name)).collect()
}

/// The demo home for `swifty init --demo`: one instance of the mock plugin,
/// with the id `mock`, and its devices. Nothing is stored for them yet; the
/// hub names the devices when it loads the instance. Scenes and notes are left
/// to the hub's defaults.
pub fn demo_home() -> House {
    let config = MockConfig { devices: demo_rows(), disabled: None, require_code: false };
    House { instances: vec![instance(PLUGIN_ID, &config)], devices: Vec::new(), scenes: Vec::new(), notes: Vec::new() }
}
