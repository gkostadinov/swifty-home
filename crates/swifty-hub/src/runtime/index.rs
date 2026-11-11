//! The device index: every device there is, by target, with what it is and who may
//! see it. The registry finds the instance that carries a target here, so nothing
//! outside it needs to know which plugin that is.
//!
//! [`build`] makes it from the stored devices (the `devices` table, the authority on
//! targets) and what each loaded instance's plugin declares for them (capabilities).
//! [`App::sync_directory`](crate::app::App::sync_directory) rebuilds it after anything
//! that changes the devices, and the registry swaps it in whole.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use swifty_plugin_api::{Audience, CapKind, Capability, DeviceClass, DeviceDecl};

use super::instance::{Instance, StoredDevice};
use crate::policy::SCENE_FAVOURITES;

/// One device, as everything outside the registry sees it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IndexedDevice {
    /// The public id the API, rooms, favourites and the audit log use.
    pub target: String,
    /// The configured instance that carries it.
    pub instance: String,
    /// The plugin that instance belongs to.
    pub plugin: &'static str,
    /// Its id within the instance: what the driver is asked about.
    pub local_id: String,
    pub class: DeviceClass,
    pub name: String,
    pub capabilities: Vec<Capability>,
    pub audience: Audience,
    /// Waiting for the owner's approval: listed, and inert until then.
    pub pending: bool,
}

impl IndexedDevice {
    /// Whether it has a capability of this kind.
    pub fn has(&self, cap: CapKind) -> bool {
        self.capabilities.iter().any(|capability| capability.kind() == cap)
    }
}

/// Every device there is, by target. Rebuilt after any change and swapped in whole.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeviceIndex {
    devices: BTreeMap<String, IndexedDevice>,
}

impl DeviceIndex {
    /// An index of these devices. If two claim the same target, the first keeps it.
    pub fn new(devices: impl IntoIterator<Item = IndexedDevice>) -> Self {
        let mut map = BTreeMap::new();
        for device in devices {
            map.entry(device.target.clone()).or_insert(device);
        }
        Self { devices: map }
    }

    pub fn get(&self, target: &str) -> Option<&IndexedDevice> {
        self.devices.get(target)
    }

    pub fn class_of(&self, target: &str) -> Option<DeviceClass> {
        self.get(target).map(|device| device.class)
    }

    /// Whether this device has a capability of this kind.
    pub fn has(&self, target: &str, cap: CapKind) -> bool {
        self.get(target).is_some_and(|device| device.has(cap))
    }

    /// Every active device of this class, in target order.
    pub fn of_class(&self, class: DeviceClass) -> impl Iterator<Item = &IndexedDevice> {
        self.active().filter(move |device| device.class == class)
    }

    /// Every device that is not waiting for approval, in target order.
    pub fn active(&self) -> impl Iterator<Item = &IndexedDevice> {
        self.devices.values().filter(|device| !device.pending)
    }

    /// Every device, pending ones included, in target order.
    pub fn all(&self) -> impl Iterator<Item = &IndexedDevice> {
        self.devices.values()
    }

    /// Whether a family member may see this device.
    pub fn family_may_see(&self, target: &str) -> bool {
        self.get(target).is_some_and(|device| device.audience == Audience::Family)
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }
}

// ----- targets

/// Words the hub uses itself as targets. A device can never be called one of them.
const HUB_WORDS: &[&str] = &["all_bulbs", "all_vms", "new", "-"];

/// `[a-z0-9][a-z0-9_-]{0,63}`, not a word the hub uses itself (`all_bulbs`,
/// `all_vms`, `new`, a scene id, `-`), and not `taken`.
pub fn is_free_target(taken: &dyn Fn(&str) -> bool, text: &str) -> bool {
    let bytes = text.as_bytes();
    let shaped = (1..=64).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'));
    shaped && !HUB_WORDS.contains(&text) && !SCENE_FAVOURITES.contains(&text) && !taken(text)
}

/// The target a newly declared device gets: the plugin's hint if it is free, else
/// `<instance>-<local id>`, else that with the first free number after it. The hub
/// stores it once and never computes it again.
pub fn assign_target(taken: &dyn Fn(&str) -> bool, instance: &str, decl: &DeviceDecl) -> String {
    if let Some(hint) = decl.target_hint.as_deref().filter(|hint| is_free_target(taken, hint)) {
        return hint.to_string();
    }
    let base = format!("{instance}-{}", decl.local_id);
    if is_free_target(taken, &base) {
        return base;
    }
    (2u32..).map(|n| format!("{base}-{n}")).find(|t| is_free_target(taken, t)).expect("a free name exists")
}

// ----- building it

/// The index of these stored devices. A device is indexed when the instance it is
/// stored for is loaded (its plugin is in this build); what it can do is what its
/// plugin declares for its local id (for one waiting for approval, what it would
/// declare once approved), and nothing when the plugin declares nothing for it (it
/// is listed, and no action admits it).
pub fn build(instances: &[Arc<Instance>], devices: &[StoredDevice]) -> DeviceIndex {
    let by_id: BTreeMap<&str, &Arc<Instance>> =
        instances.iter().map(|instance| (instance.spec.id.as_str(), instance)).collect();
    DeviceIndex::new(devices.iter().filter_map(|stored| {
        let Some(instance) = by_id.get(stored.instance.as_str()) else {
            tracing::debug!(device = %stored.target, instance = %stored.instance, "a device of an instance that is not loaded");
            return None;
        };
        let declared = if stored.pending { &instance.declared_pending } else { &instance.declared };
        let capabilities = declared
            .iter()
            .find(|decl| decl.local_id == stored.local_id)
            .map(|decl| decl.capabilities.clone())
            .unwrap_or_default();
        Some(IndexedDevice {
            target: stored.target.clone(),
            instance: stored.instance.clone(),
            plugin: instance.plugin.manifest().id,
            local_id: stored.local_id.clone(),
            class: stored.class,
            name: stored.name.clone(),
            capabilities,
            audience: stored.class.default_audience(),
            pending: stored.pending,
        })
    }))
}

#[cfg(test)]
pub(crate) use fixture::{legacy, legacy_seeded, DeviceEntry};

/// Today's house for the hub's own unit tests, which cannot store devices: the
/// twenty devices the mock plugin's legacy house has, each under an instance
/// named after what used to carry it, with that plugin's classes and capabilities.
#[cfg(test)]
pub(crate) mod fixture {
    use swifty_plugin_api::MIN_LIGHT_DIMMING;

    use super::*;
    use crate::catalog::{AC_FANS, AC_MODES, TV_INPUTS, TV_KEYS};
    use crate::inventory::Machine;

    /// One device of the fixture's fixed list.
    #[derive(Debug, Clone, Copy)]
    pub struct DeviceInfo {
        pub target: &'static str,
        pub name: &'static str,
        /// The instance that carries it.
        pub instance: &'static str,
    }

    /// A device to put in the fixture: its target, name and instance.
    #[derive(Debug, Clone)]
    pub struct DeviceEntry {
        pub target: String,
        pub name: String,
        pub instance: &'static str,
    }

    /// The devices of the legacy house, with generic names.
    pub static DEVICES: &[DeviceInfo] = &[
        DeviceInfo { target: "bulb1", name: "Bulb 1", instance: "wiz" },
        DeviceInfo { target: "bulb2", name: "Bulb 2", instance: "wiz" },
        DeviceInfo { target: "bulb3", name: "Bulb 3", instance: "wiz" },
        DeviceInfo { target: "bulb4", name: "Bulb 4", instance: "wiz" },
        DeviceInfo { target: "hue", name: "Light bridge", instance: "hue" },
        DeviceInfo { target: "sonos", name: "Sound bar", instance: "sonos" },
        DeviceInfo { target: "sub", name: "Subwoofer", instance: "sonos" },
        DeviceInfo { target: "tv", name: "TV", instance: "webos" },
        DeviceInfo { target: "feeder", name: "Feeder", instance: "petkit" },
        DeviceInfo { target: "litter", name: "Litter box", instance: "petkit" },
        DeviceInfo { target: "printer", name: "3D printer", instance: "moonraker" },
        DeviceInfo { target: "vm100", name: "VM 100", instance: "proxmox" },
        DeviceInfo { target: "vm101", name: "VM 101", instance: "proxmox" },
        DeviceInfo { target: "vm102", name: "VM 102", instance: "proxmox" },
        DeviceInfo { target: "vm103", name: "VM 103", instance: "proxmox" },
        DeviceInfo { target: "host", name: "Host", instance: "proxmox" },
        DeviceInfo { target: "router", name: "Router", instance: "router" },
        DeviceInfo { target: "camera", name: "Camera", instance: "camera" },
        DeviceInfo { target: "plex", name: "Media server", instance: "plex" },
        DeviceInfo { target: "collector", name: "Collector", instance: "glimdock" },
    ];

    const AC_TEMPERATURE: (u8, u8) = (17, 30);
    const HUE_MIN_BRIGHTNESS: u8 = 1;
    const FEED_GRAMS: [u8; 5] = [10, 20, 30, 40, 50];

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|word| word.to_string()).collect()
    }

    fn light(min: u8) -> Vec<Capability> {
        vec![Capability::Power, Capability::Brightness { min }, Capability::Color]
    }

    /// What a device of this instance is and can do, as its first-party plugin says.
    fn shape(instance: &str, target: &str) -> Option<(DeviceClass, Vec<Capability>)> {
        use Capability as C;
        Some(match instance {
            "wiz" => (DeviceClass::Light, light(MIN_LIGHT_DIMMING)),
            "hue" if target == "hue" => (DeviceClass::Bridge, Vec::new()),
            "hue" => (DeviceClass::Light, light(HUE_MIN_BRIGHTNESS)),
            "sonos" if target == "sub" => (DeviceClass::Speaker, Vec::new()),
            "sonos" => (DeviceClass::Speaker, vec![C::Volume, C::Mute, C::NightMode, C::Transport]),
            "webos" => (
                DeviceClass::Tv,
                vec![C::Power, C::Inputs { inputs: words(TV_INPUTS) }, C::Keys { keys: words(TV_KEYS) }],
            ),
            "moonraker" => (DeviceClass::Printer, vec![C::PrintJob, C::EmergencyStop]),
            "plex" => (DeviceClass::Service, Vec::new()),
            "camera" => (DeviceClass::Camera, Vec::new()),
            "petkit" if target == "litter" => (DeviceClass::Litter, vec![C::Clean]),
            "petkit" => (DeviceClass::Feeder, vec![C::Dispense { grams: FEED_GRAMS.to_vec() }]),
            "proxmox" if target == "host" => (DeviceClass::Host, vec![C::NodePower, C::History]),
            "proxmox" => (DeviceClass::Vm, vec![C::Lifecycle, C::Console, C::History]),
            "glimdock" => (DeviceClass::Telemetry, vec![C::Metrics, C::AgentPairing]),
            "router" => (DeviceClass::Router, vec![C::Tunnel]),
            "tuya" => (DeviceClass::Plug, vec![C::Power]),
            "toshiba" => {
                let (min, max) = AC_TEMPERATURE;
                (
                    DeviceClass::Aircon,
                    vec![
                        C::Power,
                        C::Mode { modes: words(AC_MODES) },
                        C::Temperature { min, max },
                        C::Fan { speeds: words(AC_FANS) },
                    ],
                )
            }
            _ => return None,
        })
    }

    /// The plugin of each instance. The router has none of its own; the mock stands in.
    fn plugin_of(instance: &str) -> Option<&'static str> {
        const PLUGINS: &[&str] = &[
            "wiz",
            "hue",
            "sonos",
            "webos",
            "moonraker",
            "plex",
            "camera",
            "petkit",
            "proxmox",
            "glimdock",
            "tuya",
            "toshiba",
        ];
        match instance {
            "router" => Some("mock"),
            other => PLUGINS.iter().copied().find(|plugin| *plugin == other),
        }
    }

    fn device(instance: &str, target: &str, name: &str, pending: bool) -> Option<IndexedDevice> {
        let (class, capabilities) = shape(instance, target)?;
        let plugin = plugin_of(instance)?;
        Some(IndexedDevice {
            target: target.to_string(),
            instance: instance.to_string(),
            plugin,
            local_id: target.to_string(),
            class,
            name: name.to_string(),
            capabilities,
            audience: class.default_audience(),
            pending,
        })
    }

    /// The index of these devices, each under its instance, plus the machines that
    /// wait for approval. A device of an unknown instance is left out.
    pub fn legacy(entries: &[DeviceEntry], machines: &[Machine]) -> DeviceIndex {
        let listed = entries.iter().filter_map(|entry| device(entry.instance, &entry.target, &entry.name, false));
        let pending = machines.iter().filter(|machine| machine.status == "pending").filter_map(|machine| {
            device("proxmox", &machine.target, &format!("{} · VM {}", machine.name, machine.vmid), true)
        });
        DeviceIndex::new(listed.chain(pending))
    }

    /// The legacy house: every device in the list, its four VMs approved.
    pub fn legacy_seeded() -> DeviceIndex {
        let entries: Vec<DeviceEntry> = DEVICES
            .iter()
            .map(|device| DeviceEntry {
                target: device.target.to_string(),
                name: device.name.to_string(),
                instance: device.instance,
            })
            .collect();
        legacy(&entries, &[])
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;
    use swifty_plugin_api::{
        ConfigProblem, DeviceRecord, Driver, InstanceView, LiveContext, Manifest, Op, Plugin, PluginError, Reach,
        Readiness, API_VERSION,
    };

    use super::fixture::{DeviceEntry, DEVICES};
    use super::*;
    use crate::catalog::{self, ActionDef};
    use crate::inventory::Machine;
    use crate::runtime::instance::{InstanceDirectory, InstanceSpec, Origin};
    use swifty_plugin_api::ParamKind;

    fn entry(target: &str, instance: &'static str) -> DeviceEntry {
        DeviceEntry { target: target.to_string(), name: format!("The {target}"), instance }
    }

    fn machine(target: &str, vmid: u32, status: &str) -> Machine {
        Machine {
            target: target.to_string(),
            host: "primary".to_string(),
            vmid,
            name: "box".to_string(),
            status: status.to_string(),
            first_seen: 0,
            approved_at: None,
            cpus: None,
        }
    }

    #[test]
    fn the_fixture_house_has_twenty_active_devices() {
        let index = legacy_seeded();
        assert_eq!(index.len(), 20);
        assert_eq!(index.active().count(), 20);
        for device in index.all() {
            assert_eq!(device.local_id, device.target);
            assert_eq!(device.audience, device.class.default_audience(), "{}", device.target);
        }
        assert_eq!(index.get("collector").unwrap().plugin, "glimdock");
        assert_eq!(index.get("router").unwrap().plugin, "mock");
        assert_eq!(DEVICES.len(), 20);
    }

    #[test]
    fn lights_are_for_the_family_and_machines_for_the_owner() {
        let index = legacy(&[entry("wiz-a8bb50aabbcc", "wiz"), entry("hue-3", "hue"), entry("vm104", "proxmox")], &[]);
        assert!(index.family_may_see("wiz-a8bb50aabbcc") && index.family_may_see("hue-3"));
        assert!(!index.family_may_see("vm104"));
        assert!(!index.family_may_see("ghost"), "nothing unknown is shown");
    }

    #[test]
    fn a_pending_machine_is_listed_and_inert() {
        let index =
            legacy(&[entry("plug-bf01", "tuya"), entry("ghost", "nothing")], &[machine("vm150", 150, "pending")]);
        assert_eq!(index.class_of("plug-bf01"), Some(DeviceClass::Plug));
        assert_eq!(index.get("ghost"), None, "an unknown instance is left out");
        let pending = index.get("vm150").unwrap();
        assert!(pending.pending);
        assert_eq!(pending.name, "box · VM 150");
        assert!(index.active().all(|device| device.target != "vm150"));
        assert!(index.of_class(DeviceClass::Vm).all(|device| !device.pending));
    }

    #[test]
    fn capabilities_follow_what_each_device_accepts() {
        let index = legacy_seeded();
        assert!(index.has("bulb1", CapKind::Brightness) && index.has("bulb1", CapKind::Color));
        assert!(!index.has("hue", CapKind::Power), "the bridge takes no commands");
        assert!(index.has("sonos", CapKind::Volume));
        assert!(!index.has("sub", CapKind::Volume), "the subwoofer is read-only");
        assert!(index.has("tv", CapKind::Inputs) && index.has("tv", CapKind::Keys));
        assert!(index.has("feeder", CapKind::Dispense) && index.has("litter", CapKind::Clean));
        assert!(index.has("vm101", CapKind::Lifecycle) && index.has("vm101", CapKind::Console));
        assert!(index.has("host", CapKind::NodePower) && !index.has("host", CapKind::Lifecycle));
        assert!(index.has("router", CapKind::Tunnel));
        assert!(index.has("collector", CapKind::Metrics));
        assert_eq!(index.get("plex").unwrap().capabilities, Vec::new());
        assert_eq!(index.get("ghost"), None);
        assert!(!index.has("ghost", CapKind::Power));
    }

    fn int_range(def: &ActionDef, name: &str) -> Option<(i64, i64)> {
        def.params.iter().find(|param| param.name == name).and_then(|param| match param.kind {
            ParamKind::Int { min, max } => Some((min, max)),
            _ => None,
        })
    }

    #[test]
    fn the_fixtures_values_are_the_catalogs_own() {
        let ac = catalog::find("ac.temperature").unwrap();
        assert_eq!(int_range(ac, "celsius"), Some((17, 30)));
        let feed = catalog::find("pets.feed").unwrap();
        let (low, high) = int_range(feed, "grams").unwrap();
        assert!((low..=high).contains(&10) && (low..=high).contains(&50));
        let brightness = catalog::find("light.brightness").unwrap();
        assert_eq!(int_range(brightness, "percent").map(|(low, _)| low), Some(1));
    }

    fn decl(local_id: &str, hint: Option<&str>) -> DeviceDecl {
        DeviceDecl {
            local_id: local_id.to_string(),
            target_hint: hint.map(str::to_string),
            class: DeviceClass::Light,
            name: "A light".to_string(),
            capabilities: Vec::new(),
            attrs: serde_json::Value::Null,
        }
    }

    #[test]
    fn a_target_is_a_short_lower_case_word_the_hub_does_not_use() {
        let free = |text: &str| is_free_target(&|_| false, text);
        for good in ["bulb1", "hue-3", "plug-bf01_usb", "0", &"a".repeat(64)] {
            assert!(free(good), "{good}");
        }
        for bad in [
            "",
            "-x",
            "_x",
            "Bulb1",
            "bulb 1",
            "bulb/1",
            "bulb.1",
            "all_bulbs",
            "all_vms",
            "new",
            "-",
            "evening",
            "movie",
        ] {
            assert!(!free(bad), "{bad}");
        }
        assert!(!free(&"a".repeat(65)));
        assert!(!is_free_target(&|text| text == "tv", "tv"));
    }

    #[test]
    fn a_device_gets_its_hint_or_a_name_from_its_instance() {
        let taken = |text: &str| ["hue", "hue-3", "hue2-hue-3", "hue2-hue-3-2"].contains(&text);
        assert_eq!(assign_target(&taken, "hue", &decl("hue-4", Some("hue-4"))), "hue-4");
        assert_eq!(assign_target(&taken, "hue2", &decl("hue-3", Some("hue-3"))), "hue2-hue-3-3");
        assert_eq!(assign_target(&taken, "hue2", &decl("hue-5", Some("hue-5"))), "hue-5");
        assert_eq!(assign_target(&taken, "acme", &decl("blind1", None)), "acme-blind1");
        assert_eq!(assign_target(&taken, "acme", &decl("x", Some("all_bulbs"))), "acme-x");
    }

    // ----- the builder, with a plugin of its own

    static LAMPS: Manifest = Manifest {
        id: "lamps",
        name: "Lamps",
        version: "0",
        api: API_VERSION,
        summary: "Lamps.",
        reach: Reach::Local,
        network: &[],
        classes: &[DeviceClass::Light],
        config_version: 1,
        config: &[],
        secrets: &[],
        actions: &[],
        vocabulary: &[],
        nouns: &[],
        multi_instance: true,
    };

    /// Declares `lamp` from its config, and every adopted record whose local id
    /// starts with `found-`.
    struct Lamps;

    impl Plugin for Lamps {
        fn manifest(&self) -> &'static Manifest {
            &LAMPS
        }
        fn validate(&self, _config: &serde_json::Value) -> Result<(), ConfigProblem> {
            Ok(())
        }
        fn readiness(&self, _instance: &InstanceView<'_>) -> Readiness {
            Readiness::Ready
        }
        fn devices(&self, instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
            let lamp = DeviceDecl { capabilities: vec![Capability::Power], ..decl("lamp", Some("lamp")) };
            let found = instance.adopted.iter().filter(|record| record.local_id.starts_with("found-")).map(|record| {
                DeviceDecl { capabilities: vec![Capability::Power, Capability::Color], ..decl(&record.local_id, None) }
            });
            std::iter::once(lamp).chain(found).collect()
        }
        fn refusal(&self, _instance: &InstanceView<'_>, _device: &str, _op: &Op) -> Option<String> {
            None
        }
        fn connect(&self, _ctx: LiveContext) -> Result<std::sync::Arc<dyn Driver>, PluginError> {
            Err(PluginError("no".to_string()))
        }
    }

    static PLUGIN: Lamps = Lamps;

    fn stored(target: &str, instance: &str, local_id: &str, origin: Origin, pending: bool) -> StoredDevice {
        StoredDevice {
            target: target.to_string(),
            instance: instance.to_string(),
            local_id: local_id.to_string(),
            class: DeviceClass::Light,
            name: format!("The {target}"),
            attrs: json!({}),
            origin,
            pending,
        }
    }

    fn loaded(id: &str, devices: &[StoredDevice]) -> Arc<Instance> {
        let adopted: Vec<DeviceRecord> = devices
            .iter()
            .filter(|device| device.instance == id && device.origin == Origin::Adopted && !device.pending)
            .map(StoredDevice::record)
            .collect();
        let spec = InstanceSpec {
            id: id.to_string(),
            plugin: "lamps".to_string(),
            config: json!({}),
            config_version: 1,
            enabled: true,
            updated_at: 0,
        };
        let pending: Vec<DeviceRecord> =
            devices.iter().filter(|device| device.instance == id && device.pending).map(StoredDevice::record).collect();
        Arc::new(Instance::new(
            spec,
            &PLUGIN,
            BTreeSet::new(),
            adopted,
            pending,
            Arc::new(InstanceDirectory::default()),
        ))
    }

    #[test]
    fn the_index_holds_the_stored_targets_with_what_the_plugin_declares() {
        let devices = vec![
            stored("lamp", "one", "lamp", Origin::Config, false),
            stored("two-lamp", "two", "lamp", Origin::Config, false),
            stored("porch", "one", "found-1", Origin::Adopted, false),
            stored("odd", "one", "stale", Origin::Adopted, false),
            stored("waiting", "one", "found-2", Origin::Adopted, true),
            stored("elsewhere", "gone", "lamp", Origin::Config, false),
        ];
        let instances = vec![loaded("one", &devices), loaded("two", &devices)];
        let index = build(&instances, &devices);

        let lamp = index.get("lamp").unwrap();
        assert_eq!((lamp.instance.as_str(), lamp.plugin, lamp.local_id.as_str()), ("one", "lamps", "lamp"));
        assert_eq!(lamp.capabilities, [Capability::Power]);
        assert_eq!(lamp.name, "The lamp");
        assert_eq!(lamp.audience, Audience::Family);
        assert_eq!(index.get("two-lamp").unwrap().instance, "two", "the same local id under another instance");
        assert!(index.has("porch", CapKind::Color), "an adopted device the plugin recognises");
        assert_eq!(index.get("odd").unwrap().capabilities, Vec::new(), "one it does not is listed and can do nothing");
        assert!(index.get("waiting").unwrap().pending);
        let waiting = &index.get("waiting").unwrap().capabilities;
        assert_eq!(waiting, &[Capability::Power, Capability::Color], "what it will do once approved");
        assert_eq!(index.get("elsewhere"), None, "an instance that is not loaded");
        assert_eq!(index.len(), 5);
    }
}
