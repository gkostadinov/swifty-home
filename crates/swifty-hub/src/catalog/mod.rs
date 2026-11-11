//! The table of named actions, and the actions plugins add to it. Nothing outside
//! the catalog can be asked of a device: every entry has an id, a risk tier, its
//! allowed targets, a parameter schema with ranges, and a plain-language account
//! of what will happen.
//!
//! What a device is comes from the [`DeviceIndex`]: its class and capabilities.
//! Nothing here decides it from the shape of a target's name, and nothing here
//! knows one particular house: names come from the index, the owner's notes on a
//! device come from the store, and so do the house scenes' levels.
//!
//! - [`actions`]: the table of standard actions;
//! - [`custom`]: the actions plugins declare, with the hub's floors applied;
//! - [`plan`]: what an action does, step by step;
//! - [`describe`]: what the console shows before an action runs.

mod actions;
mod custom;
mod describe;
mod plan;

use serde_json::{json, Map, Value};
use swifty_plugin_api::{CapKind, Capability, DeviceClass, Expect, ParamDef, ParamKind, Tier};

use crate::runtime::index::{DeviceIndex, IndexedDevice};

pub use actions::{ACK_PARAM, AC_FANS, AC_MODES, CATALOG, METRICS, TV_INPUTS, TV_KEYS};
pub use custom::{floor, register_plugins, registered, Registered, CONFIRM_TIMEOUT_RANGE_MS};
pub use describe::{
    console_offline_warning, describe, node_power_warning, takes_console_offline, Description, Facts,
    NODE_REBOOT_WARNING, NODE_SHUTDOWN_WARNING,
};
pub use plan::{
    applies_a_scene, every_light, expand_target, movie_in_room, movie_tv, plan, reaches_every_light, within_reach,
    Context, Step,
};

/// What an action may be aimed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Targets {
    /// Words the hub owns: `all_bulbs`, the house scenes.
    Fixed(&'static [&'static str]),
    /// An active device of this class.
    Class(DeviceClass),
    /// An active device of this class that has this capability. For a custom
    /// action (`CapKind::Custom`) the device must name that very action.
    Capable(DeviceClass, CapKind),
    /// Any device or group the caller may see.
    Readable,
    /// A device waiting for approval.
    Pending,
    /// A house scene. No standard action names it: the scene actions list the
    /// scenes each of them applies.
    Scene,
    /// A stored room.
    Room,
    /// A family member, by user id.
    User,
    /// An existing passkey, by its public id.
    Credential,
    /// The literal target `new`.
    NewCredential,
    /// A further Proxmox host that was added, by its id.
    FurtherHost,
    /// A stored plugin instance, by id.
    Instance,
    /// The literal target `new`, for a plugin instance that does not exist yet.
    NewInstance,
}

/// The groups a read may name. Each stands for several devices.
pub const GROUPS: &[&str] = &["all_bulbs", "all_vms"];

impl Targets {
    /// The name `/api/v1/catalog` gives this kind of target.
    pub fn kind_name(self) -> &'static str {
        match self {
            Targets::Fixed(_) => "fixed",
            Targets::Class(_) => "class",
            Targets::Capable(..) => "capable",
            Targets::Readable => "readable",
            Targets::Pending => "pending",
            Targets::Scene => "scene",
            Targets::Room => "room",
            Targets::User => "user",
            Targets::Credential => "credential",
            Targets::NewCredential => "new_credential",
            Targets::FurtherHost => "further_host",
            Targets::Instance => "instance",
            Targets::NewInstance => "new_instance",
        }
    }

    /// The class a target must have, where there is one.
    pub fn class(self) -> Option<DeviceClass> {
        match self {
            Targets::Class(class) | Targets::Capable(class, _) => Some(class),
            _ => None,
        }
    }

    /// The capability a target must have, where there is one.
    pub fn requires(self) -> Option<CapKind> {
        match self {
            Targets::Capable(_, cap) => Some(cap),
            _ => None,
        }
    }

    /// Whether the targets are devices, so the index is what knows them.
    pub fn names_devices(self) -> bool {
        matches!(self, Targets::Class(_) | Targets::Capable(..) | Targets::Readable | Targets::Pending)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Executor {
    /// Reads state and changes nothing.
    Read,
    /// Sends typed operations to device adapters.
    Device,
    /// Opens a registration window for a new passkey.
    PasskeyRegister,
    /// Revokes a passkey and ends its sessions.
    PasskeyRevoke,
    /// Creates a single-use invitation for a family member.
    UserInvite,
    /// Removes a family member and everything that lets them sign in.
    UserRemove,
    /// Puts a pending VM on the approved list.
    MachineApprove,
    /// Takes a machine off the list.
    MachineRemove,
    /// Adds a further Proxmox host.
    ProxmoxAddHost,
    /// Forgets a further Proxmox host and the machines listed on it.
    ProxmoxRemoveHost,
    /// Asks Proxmox for a VNC proxy and issues the single-use ticket for it.
    ConsoleOpen,
    /// Starts the collector's own agent pairing.
    AgentPair,
    /// Opens a setup session for a new instance of a plugin (`plugin_actions`).
    PluginAdd,
    /// Opens a setup session for a stored instance, from its stored config.
    PluginReconfigure,
    /// Removes an instance with its devices, their places, and its secrets.
    PluginRemove,
}

#[derive(Debug, Clone, Copy)]
pub struct ActionDef {
    pub id: &'static str,
    /// Ids this action was known by before. Accepted on input, never written.
    pub aliases: &'static [&'static str],
    pub label: &'static str,
    pub tier: Tier,
    pub targets: Targets,
    pub params: &'static [ParamDef],
    /// What will happen.
    pub description: &'static str,
    /// What follows from it.
    pub consequences: &'static [&'static str],
    /// False means a repeat would do the thing twice, so it is never retried.
    pub idempotent: bool,
    pub confirm_timeout_ms: u64,
    pub executor: Executor,
    /// How a custom action is confirmed. `None` for standard actions.
    pub expect: Option<&'static Expect>,
}

/// The action with this id: the standard table first, then the custom actions. An
/// id it was known by before finds it too, so `bulb.power` finds `light.power`.
pub fn find(id: &str) -> Option<&'static ActionDef> {
    CATALOG
        .iter()
        .find(|def| def.id == id)
        .or_else(|| CATALOG.iter().find(|def| def.aliases.contains(&id)))
        .or_else(|| custom::find(id))
}

/// Every action: the standard table, then the custom actions in the order they
/// were added.
pub fn all() -> Vec<&'static ActionDef> {
    CATALOG.iter().chain(registered().into_iter().map(|known| known.def)).collect()
}

/// A stored id: letters and digits, at most 64 of them.
pub fn is_plain_id(text: &str) -> bool {
    !text.is_empty() && text.len() <= 64 && text.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Why a request does not fit its catalog entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid(pub String);

impl ActionDef {
    /// Whether a plugin declared this action.
    pub fn is_custom(&self) -> bool {
        self.expect.is_some()
    }

    /// Whether this device is of the kind this action is for, waiting or not.
    fn fits(&self, device: &IndexedDevice) -> bool {
        match self.targets {
            Targets::Class(class) => device.class == class,
            Targets::Capable(class, CapKind::Custom) => {
                // A custom action reaches only a device of its own plugin that names it.
                let plugin = self.id.split_once('.').map(|(plugin, _)| plugin);
                device.class == class
                    && plugin == Some(device.plugin)
                    && device
                        .capabilities
                        .iter()
                        .any(|capability| matches!(capability, Capability::Custom { action } if action == self.id))
            }
            Targets::Capable(class, cap) => device.class == class && device.has(cap),
            Targets::Readable => true,
            _ => false,
        }
    }

    /// Whether this action may be aimed at `target`. A device target must be in the
    /// index, active, of the action's class and, where the action names one, have the
    /// capability. Rooms, people, passkeys, further hosts and plugin instances are only
    /// checked for the shape of their id here: the store is asked whether they exist.
    pub fn admits(&self, index: &DeviceIndex, target: &str) -> Result<(), Invalid> {
        let active = || index.get(target).filter(|device| !device.pending);
        let admitted = match self.targets {
            Targets::Fixed(list) => list.contains(&target),
            Targets::Class(_) | Targets::Capable(..) => active().is_some_and(|device| self.fits(device)),
            Targets::Readable => GROUPS.contains(&target) || active().is_some(),
            Targets::Pending => index.get(target).is_some_and(|device| device.pending),
            Targets::Scene => crate::policy::SCENE_FAVOURITES.contains(&target),
            Targets::NewCredential | Targets::NewInstance if target == "new" => true,
            Targets::NewCredential | Targets::NewInstance => {
                return Err(Invalid("the target must be \"new\"".to_string()));
            }
            // Passkey ids are checked where they are revoked.
            Targets::Credential => true,
            Targets::Room | Targets::User => is_plain_id(target),
            Targets::FurtherHost => crate::inventory::is_host_id(target),
            // An instance may carry no device at all (a WiZ instance before its first
            // bulb, one of a plugin this build lacks), so the store is what knows it.
            Targets::Instance => crate::secrets::is_instance_id(target),
        };
        if admitted {
            Ok(())
        } else {
            Err(Invalid(format!("{target} is not a target of {}", self.id)))
        }
    }

    /// Whether `target` is a device waiting for approval that this action would
    /// admit once it is approved.
    pub fn waits_for_approval(&self, index: &DeviceIndex, target: &str) -> bool {
        let Some(device) = index.get(target).filter(|device| device.pending) else {
            return false;
        };
        matches!(self.targets, Targets::Class(_) | Targets::Capable(..) | Targets::Readable) && self.fits(device)
    }

    /// The targets the index admits right now, for the kinds of target that are
    /// devices or words the hub owns. Rooms, people, passkeys, further hosts and
    /// plugin instances are in the store, so this leaves them empty.
    pub fn admitted(&self, index: &DeviceIndex) -> Vec<String> {
        let words = |list: &[&str]| list.iter().map(|word| word.to_string()).collect();
        let devices = |keep: &dyn Fn(&str) -> bool| {
            index.all().map(|device| device.target.as_str()).filter(|target| keep(target)).map(str::to_string).collect()
        };
        match self.targets {
            Targets::Fixed(list) => words(list),
            Targets::Readable => {
                let mut targets: Vec<String> = words(GROUPS);
                targets.extend(index.active().map(|device| device.target.clone()));
                targets
            }
            Targets::Scene => words(crate::policy::SCENE_FAVOURITES),
            Targets::NewCredential | Targets::NewInstance => words(&["new"]),
            Targets::Class(_) | Targets::Capable(..) | Targets::Pending => {
                devices(&|target| self.admits(index, target).is_ok())
            }
            Targets::Room | Targets::User | Targets::Credential | Targets::FurtherHost | Targets::Instance => {
                Vec::new()
            }
        }
    }

    /// Checks `params` against the schema and returns them with nothing extra.
    /// `null` is accepted as an empty object.
    pub fn validate_params(&self, params: &Value) -> Result<Value, Invalid> {
        let empty = Map::new();
        let given = match params {
            Value::Null => &empty,
            Value::Object(map) => map,
            _ => return Err(Invalid("params must be an object".to_string())),
        };
        for key in given.keys() {
            if !self.params.iter().any(|p| p.name == key) {
                return Err(Invalid(format!("unknown parameter {key}")));
            }
        }
        let mut clean = Map::new();
        for def in self.params {
            match given.get(def.name) {
                None | Some(Value::Null) if def.required => {
                    return Err(Invalid(format!("{} is required", def.name)));
                }
                None | Some(Value::Null) => {}
                Some(value) => {
                    check_param(def, value)?;
                    clean.insert(def.name.to_string(), value.clone());
                }
            }
        }
        Ok(Value::Object(clean))
    }

    /// The entry as `/api/v1/catalog` lists it. `targets` is what the caller may aim
    /// it at right now.
    pub fn to_json(&self, targets: &[String]) -> Value {
        let params: Vec<Value> = self
            .params
            .iter()
            .map(|p| {
                let schema = match p.kind {
                    ParamKind::Bool => json!({ "type": "boolean" }),
                    ParamKind::Int { min, max } => json!({ "type": "integer", "min": min, "max": max }),
                    ParamKind::Enum(values) => json!({ "type": "enum", "values": values }),
                    ParamKind::Label { max_len } => json!({ "type": "label", "max_len": max_len }),
                };
                json!({ "name": p.name, "required": p.required, "help": p.help, "schema": schema })
            })
            .collect();
        json!({
            "id": self.id,
            "aliases": self.aliases,
            "label": self.label,
            "tier": self.tier.wire(),
            "tier_name": self.tier.name(),
            "flow": crate::policy::flow(self.tier).name(),
            "target_kind": self.targets.kind_name(),
            "class": self.targets.class(),
            "requires": self.targets.requires(),
            "targets": targets,
            "params": params,
            "description": self.description,
            "consequences": self.consequences,
            "idempotent": self.idempotent,
        })
    }
}

fn check_param(def: &ParamDef, value: &Value) -> Result<(), Invalid> {
    let name = def.name;
    match def.kind {
        ParamKind::Bool => {
            if !value.is_boolean() {
                return Err(Invalid(format!("{name} must be true or false")));
            }
        }
        ParamKind::Int { min, max } => {
            let number = value.as_i64().ok_or_else(|| Invalid(format!("{name} must be a whole number")))?;
            if number < min || number > max {
                return Err(Invalid(format!("{name} must be between {min} and {max}")));
            }
        }
        ParamKind::Enum(values) => {
            let text = value.as_str().ok_or_else(|| Invalid(format!("{name} must be text")))?;
            if !values.contains(&text) {
                return Err(Invalid(format!("{name} must be one of {values:?}")));
            }
        }
        ParamKind::Label { max_len } => {
            let text = value.as_str().ok_or_else(|| Invalid(format!("{name} must be text")))?;
            validate_label(name, text, max_len)?;
        }
    }
    Ok(())
}

pub fn validate_label(name: &str, text: &str, max_len: usize) -> Result<(), Invalid> {
    let count = text.chars().count();
    if text.trim().is_empty() || count > max_len {
        return Err(Invalid(format!("{name} must be 1 to {max_len} characters")));
    }
    if text.trim() != text || text.chars().any(char::is_control) {
        return Err(Invalid(format!("{name} must not have control characters or surrounding spaces")));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::inventory::Machine;
    use crate::runtime::index::fixture::DEVICES;
    use crate::runtime::index::{self, legacy_seeded, DeviceEntry};
    use crate::scenes::{Scene, SceneOverride};
    use swifty_plugin_api::Op;

    pub(crate) const WIZ_LIGHT: &str = "wiz-a8bb50aabbcc";
    pub(crate) const HUE_LIGHT: &str = "hue-1";
    const SOCKET: &str = "plug-bf0123456789abcdef";
    const AIRCON: &str = "ac-0a1b2c3d-1111-2222-3333-444455556666";
    /// A machine on a further host that happens to be called `ac`.
    const MACHINE_ON_AC: &str = "ac-vm12345678";

    /// The seeded house, plus a light of each kind added after discovery, a socket
    /// with its USB port, an air conditioner, a machine on a further host and a VM
    /// that waits for approval.
    pub(crate) fn house() -> DeviceIndex {
        let mut entries: Vec<DeviceEntry> = DEVICES
            .iter()
            .map(|d| DeviceEntry { target: d.target.to_string(), name: d.name.to_string(), instance: d.instance })
            .collect();
        let usb = format!("{SOCKET}-usb");
        for (target, instance) in [
            (WIZ_LIGHT, "wiz"),
            (HUE_LIGHT, "hue"),
            (SOCKET, "tuya"),
            (usb.as_str(), "tuya"),
            (AIRCON, "toshiba"),
            (MACHINE_ON_AC, "proxmox"),
        ] {
            entries.push(DeviceEntry { target: target.to_string(), name: format!("The {target}"), instance });
        }
        let pending = Machine {
            target: "vm150".to_string(),
            host: "primary".to_string(),
            vmid: 150,
            name: "new".to_string(),
            status: "pending".to_string(),
            first_seen: 0,
            approved_at: None,
            cpus: None,
        };
        index::legacy(&entries, &[pending])
    }

    /// The house scenes with a level of their own for some bulbs, as the site file
    /// gives them: the scene's level is the first bulb's.
    pub(crate) fn scenes() -> Vec<Scene> {
        let level = |bulb: u8, percent, hue| SceneOverride { target: format!("bulb{bulb}"), percent, hue };
        crate::scenes::house_scenes()
            .into_iter()
            .map(|scene| {
                let overrides = match scene.id.as_str() {
                    "evening" => vec![level(2, 35, 30), level(3, 0, 0), level(4, 25, 14)],
                    "focus" => vec![level(4, 70, 52)],
                    "movie" => vec![level(2, 8, 14), level(3, 0, 0), level(4, 0, 0)],
                    _ => Vec::new(),
                };
                Scene { overrides, ..scene }
            })
            .collect()
    }

    pub(crate) fn scene(id: &str) -> Scene {
        scenes().into_iter().find(|scene| scene.id == id).unwrap()
    }

    /// Plans `def` on `target` the way the engine does: every light the index has
    /// (with `added` last, in that order), and the stored scene the target names.
    pub(crate) fn plan_in(
        index: &DeviceIndex,
        def: &ActionDef,
        target: &str,
        params: &Value,
        added: &[String],
    ) -> Result<Vec<Step>, Invalid> {
        let lights = if reaches_every_light(def) { every_light(index, added) } else { Vec::new() };
        let stored = scenes().into_iter().find(|scene| scene.id == target);
        plan(def, target, params, &Context { index, lights: &lights, scene: stored.as_ref() })
    }

    pub(crate) fn valid_params(def: &ActionDef) -> Value {
        let mut map = Map::new();
        for p in def.params.iter().filter(|p| p.required) {
            let value = match p.kind {
                ParamKind::Bool => json!(true),
                ParamKind::Int { min, .. } => json!(min),
                ParamKind::Enum(values) => json!(values[0]),
                ParamKind::Label { .. } => json!("A name"),
            };
            map.insert(p.name.to_string(), value);
        }
        Value::Object(map)
    }

    #[test]
    fn ids_are_unique_and_every_entry_explains_itself() {
        let mut seen = std::collections::HashSet::new();
        for def in CATALOG {
            assert!(seen.insert(def.id), "duplicate id {}", def.id);
            assert!(!def.description.is_empty(), "{} has no description", def.id);
            assert!(!def.label.is_empty(), "{} has no label", def.id);
            assert!(!def.is_custom(), "{} is standard", def.id);
        }
    }

    #[test]
    fn every_device_action_plans_for_every_allowed_target() {
        let index = house();
        for def in CATALOG.iter().filter(|d| d.executor == Executor::Device) {
            if def.targets == Targets::Room {
                continue;
            }
            let targets = def.admitted(&index);
            assert!(!targets.is_empty(), "{} has no targets", def.id);
            for target in &targets {
                let params = def.validate_params(&valid_params(def)).unwrap();
                let steps = plan_in(&index, def, target, &params, &[]).unwrap_or_else(|e| panic!("{}: {e:?}", def.id));
                assert!(!steps.is_empty());
                for step in steps {
                    let carried_by = index.get(&step.target).map(|device| device.instance.as_str());
                    assert!(carried_by.is_some(), "{} plans for {}, which is no device", def.id, step.target);
                    if let Some(info) = DEVICES.iter().find(|device| device.target == step.target) {
                        assert_eq!(carried_by, Some(info.instance), "{}", step.target);
                    }
                }
            }
        }
    }

    #[test]
    fn spec_tiers_are_as_specified() {
        let tier = |id| find(id).unwrap().tier;
        for id in [
            "light.power",
            "light.brightness",
            "plug.power",
            "speaker.volume",
            "speaker.mute",
            "speaker.night",
            "tv.key",
            "scene.apply",
        ] {
            assert_eq!(tier(id), Tier::Comfort, "{id}");
        }
        for id in ["pets.feed", "vm.start", "print.start", "tv.power"] {
            assert_eq!(tier(id), Tier::Consequential, "{id}");
        }
        for id in [
            "vm.stop",
            "vm.restart",
            "vm.force_stop",
            "host.shutdown",
            "host.reboot",
            "router.wireguard_restart",
            "passkey.register",
            "passkey.revoke",
            "proxmox.add_host",
            "proxmox.remove_host",
        ] {
            assert_eq!(tier(id), Tier::Critical, "{id}");
        }
        assert_eq!(tier("state.read"), Tier::Read);
        assert_eq!(tier("print.estop"), Tier::SafeDirection);
        // Movie turns the TV on, so it cannot sit below TV power.
        assert_eq!(tier("scene.movie"), Tier::Consequential);
    }

    #[test]
    fn non_idempotent_actions_are_marked() {
        for id in ["pets.feed", "pets.clean", "print.start"] {
            assert!(!find(id).unwrap().idempotent, "{id}");
        }
    }

    #[test]
    fn parameter_ranges_are_enforced() {
        let cases = [
            ("light.brightness", json!({"percent": 0})),
            ("light.brightness", json!({"percent": 101})),
            ("light.brightness", json!({"percent": 50.5})),
            ("light.brightness", json!({"percent": "50"})),
            ("light.brightness", json!({})),
            ("light.brightness", json!({"percent": 50, "extra": 1})),
            ("light.hue", json!({"hue": 360})),
            ("light.hue", json!({"hue": -1})),
            ("speaker.volume", json!({"level": 101})),
            ("speaker.volume", json!({"level": -1})),
            ("pets.feed", json!({"grams": 4})),
            ("pets.feed", json!({"grams": 51})),
            ("tv.input", json!({"input": "HDMI 5"})),
            ("tv.key", json!({"key": "power"})),
            ("tv.key", json!({"key": "UP"})),
            ("tv.key", json!({"key": ""})),
            ("tv.key", json!({"key": "up\nname:POWER"})),
            ("tv.key", json!({"key": 1})),
            ("tv.key", json!({})),
            ("speaker.night", json!({"on": "yes"})),
            ("speaker.night", json!({})),
            ("speaker.mute", json!({"muted": "yes"})),
            ("passkey.register", json!({"name": ""})),
            ("passkey.register", json!({"name": "bad\nname"})),
            ("passkey.register", json!({"name": "x".repeat(65)})),
            ("light.power", json!([true])),
        ];
        for (id, params) in cases {
            assert!(find(id).unwrap().validate_params(&params).is_err(), "{id} {params}");
        }
        let ok = find("light.brightness").unwrap().validate_params(&json!({"percent": 100})).unwrap();
        assert_eq!(ok, json!({"percent": 100}));
    }

    #[test]
    fn targets_are_limited_to_the_table() {
        let index = legacy_seeded();
        let def = find("vm.stop").unwrap();
        assert!(def.admits(&index, "vm101").is_ok());
        assert!(def.admits(&index, "host").is_err());
        let def = find("tv.power").unwrap();
        assert!(def.admits(&index, "tv").is_ok());
        assert!(def.admits(&index, "vm101").is_err());
    }

    #[test]
    fn the_light_actions_ask_the_index_what_a_light_is() {
        let index = house();
        let power = find("light.power").unwrap();
        for light in ["bulb1", WIZ_LIGHT, HUE_LIGHT] {
            assert!(power.admits(&index, light).is_ok(), "{light}");
        }
        assert_eq!(index.get(HUE_LIGHT).unwrap().instance, "hue");
        assert_eq!(index.get(WIZ_LIGHT).unwrap().instance, "wiz");
        assert!(power.admits(&index, "hue").is_err(), "the bridge is not a light");
        assert_eq!(index.class_of("hue"), Some(DeviceClass::Bridge));
        for unknown in ["wiz-a8bb50aabbcd", "hue-2", "bulb5"] {
            assert!(power.admits(&index, unknown).is_err(), "{unknown} only looks like a light");
        }
        assert!(find("speaker.volume").unwrap().admits(&index, "sonos").is_ok());
        assert!(find("speaker.volume").unwrap().admits(&index, "sub").is_err(), "the subwoofer is read-only");
        assert!(find("state.read").unwrap().admits(&index, "sub").is_ok());
    }

    #[test]
    fn a_machine_waiting_for_approval_is_admitted_by_nothing_but_approval() {
        let index = house();
        for id in ["vm.start", "vm.stop", "console.open", "machine.remove", "state.read"] {
            let def = find(id).unwrap();
            assert!(def.admits(&index, "vm150").is_err(), "{id}");
            assert!(def.waits_for_approval(&index, "vm150"), "{id}");
            assert!(!def.waits_for_approval(&index, "vm101"), "{id}");
        }
        let approve = find("machine.approve").unwrap();
        assert!(approve.admits(&index, "vm150").is_ok());
        assert!(approve.admits(&index, "vm101").is_err(), "already approved");
        assert!(!find("tv.power").unwrap().waits_for_approval(&index, "vm150"), "not a target of that action at all");
        assert_eq!(approve.admitted(&index), ["vm150"]);
        assert!(!find("vm.start").unwrap().admitted(&index).contains(&"vm150".to_string()));
    }

    #[test]
    fn the_admitted_targets_are_what_the_index_has_now() {
        let index = house();
        let listed = |id: &str| find(id).unwrap().admitted(&index);
        assert_eq!(listed("light.power"), ["bulb1", "bulb2", "bulb3", "bulb4", HUE_LIGHT, WIZ_LIGHT]);
        assert_eq!(listed("speaker.volume"), ["sonos"]);
        assert_eq!(listed("plug.power"), [SOCKET.to_string(), format!("{SOCKET}-usb")]);
        assert_eq!(listed("ac.fan"), [AIRCON]);
        assert_eq!(listed("vm.start"), [MACHINE_ON_AC, "vm100", "vm101", "vm102", "vm103"]);
        assert_eq!(listed("host.reboot"), ["host"]);
        assert_eq!(listed("lights.power"), ["all_bulbs"]);
        assert_eq!(listed("passkey.register"), ["new"]);
        assert!(listed("room.power").is_empty(), "rooms are in the store");
        let read = listed("state.read");
        assert!(read.starts_with(&["all_bulbs".to_string(), "all_vms".to_string()]));
        assert!(read.contains(&AIRCON.to_string()) && read.contains(&"sub".to_string()));
        assert!(!read.contains(&"vm150".to_string()), "a pending machine cannot be read");

        let def = find("speaker.volume").unwrap();
        let json = def.to_json(&listed("speaker.volume"));
        assert_eq!(json["target_kind"], json!("capable"));
        assert_eq!(json["class"], json!("speaker"));
        assert_eq!(json["requires"], json!("volume"));
        assert_eq!(json["targets"], json!(["sonos"]));
        let read = find("state.read").unwrap().to_json(&[]);
        assert_eq!((read["target_kind"].clone(), read["class"].clone()), (json!("readable"), Value::Null));
        let remove = find("machine.remove").unwrap().to_json(&[]);
        assert_eq!((remove["class"].clone(), remove["requires"].clone()), (json!("vm"), Value::Null));
    }

    #[test]
    fn the_actions_for_every_light_reach_the_added_ones_too() {
        let index = house();
        let added = [WIZ_LIGHT.to_string(), HUE_LIGHT.to_string()];
        let on = json!({ "on": true });
        let steps = plan_in(&index, find("lights.power").unwrap(), "all_bulbs", &on, &added).unwrap();
        let targets: Vec<&str> = steps.iter().map(|s| s.target.as_str()).collect();
        assert_eq!(targets, ["bulb1", "bulb2", "bulb3", "bulb4", WIZ_LIGHT, HUE_LIGHT]);
        assert_eq!(index.get(&steps[5].target).unwrap().instance, "hue");

        let settings = |id: &str, target: &str| plan_in(&index, find(id).unwrap(), target, &json!({}), &added).unwrap();
        assert_eq!(settings("scene.apply", "evening")[5].op, Op::LightSet { percent: 40, hue: 34 });
        assert_eq!(settings("scene.apply", "focus")[4].op, Op::LightSet { percent: 90, hue: 52 });
        assert_eq!(settings("scene.apply", "all_off")[5].op, Op::LightPower { on: false });
        let movie = settings("scene.movie", "movie");
        assert_eq!(movie[4].op, Op::LightSet { percent: 10, hue: 14 });
        assert_eq!(movie[6].target, "tv", "the lights come first");

        let one = plan_in(&index, find("light.power").unwrap(), "bulb1", &on, &added).unwrap();
        assert_eq!(one.len(), 1, "one light stays one light");

        // Every light in the index: its own first, then the added ones in the order the store added them.
        assert_eq!(every_light(&index, &added), ["bulb1", "bulb2", "bulb3", "bulb4", WIZ_LIGHT, HUE_LIGHT]);
        assert_eq!(every_light(&index, &[]), ["bulb1", "bulb2", "bulb3", "bulb4", HUE_LIGHT, WIZ_LIGHT]);
        assert_eq!(
            every_light(&legacy_seeded(), &added),
            ["bulb1", "bulb2", "bulb3", "bulb4"],
            "only lights the index has"
        );
    }

    #[test]
    fn an_air_conditioner_target_is_exactly_a_lower_case_id() {
        let index = house();
        assert_eq!(index.get(AIRCON).unwrap().instance, "toshiba");
        for bad in [
            "ac",
            "ac-",
            "ac-short",
            "ac-0A1B2C3D-1111-2222-3333-444455556666",
            "ac-0a1b2c3d",
            "ac-0a1b2c3d/x",
            " ac-0a1b2c3d-1111-2222-3333-444455556666",
            "ac-0a1b2c3d-1111-2222-3333-444455556666\n",
            "ac_0a1b2c3d-1111-2222-3333-444455556666",
        ] {
            assert!(find("ac.power").unwrap().admits(&index, bad).is_err(), "{bad:?}");
            assert!(find("state.read").unwrap().admits(&index, bad).is_err(), "{bad:?}");
        }
        let machine = index.get(MACHINE_ON_AC).unwrap();
        assert_eq!(
            (machine.instance.as_str(), machine.class),
            ("proxmox", DeviceClass::Vm),
            "a machine on a further host called ac"
        );
        assert!(find("ac.power").unwrap().admits(&index, MACHINE_ON_AC).is_err());
        assert!(find("vm.start").unwrap().admits(&index, MACHINE_ON_AC).is_ok());
        for (action, good, bad) in [
            ("ac.power", json!({ "on": true }), json!({ "on": "yes" })),
            ("ac.mode", json!({ "mode": "dry" }), json!({ "mode": "boil" })),
            ("ac.temperature", json!({ "celsius": 17 }), json!({ "celsius": 16 })),
            ("ac.fan", json!({ "fan": "medium_low" }), json!({ "fan": "turbo" })),
        ] {
            let def = find(action).unwrap();
            assert!(def.admits(&index, AIRCON).is_ok(), "{action}");
            assert!(def.admits(&index, "bulb1").is_err(), "{action}");
            assert!(def.validate_params(&good).is_ok(), "{action}");
            assert!(def.validate_params(&bad).is_err(), "{action}");
            let planned = plan_in(&index, def, AIRCON, &good, &[]).unwrap();
            assert_eq!(planned[0].target, AIRCON, "{action}");
            assert_eq!(index.get(&planned[0].target).unwrap().instance, "toshiba");
        }
        assert!(find("state.read").unwrap().admits(&index, AIRCON).is_ok());
    }

    #[test]
    fn a_socket_target_is_exactly_a_lower_case_id() {
        let index = house();
        let usb = format!("{SOCKET}-usb");
        for good in [SOCKET, usb.as_str()] {
            assert_eq!(index.get(good).unwrap().instance, "tuya", "{good}");
            assert!(find("plug.power").unwrap().admits(&index, good).is_ok(), "{good}");
        }
        for bad in [
            "plug",
            "plug-",
            "plug--usb",
            "plug-bf0123456789abcdef-usb-usb",
            "plug-short",
            "plug-0123456789",
            "plug-BF0123456789ABCDEF",
            "plug-bf0123456789abcde/",
            "plug-bf0123 456789abc",
            " plug-bf0123456789abcdef",
            "plug-bf0123456789abcdef\n",
            "plug_bf0123456789abcdef",
        ] {
            assert!(find("plug.power").unwrap().admits(&index, bad).is_err(), "{bad:?}");
        }
        let def = find("plug.power").unwrap();
        assert!(def.admits(&index, "bulb1").is_err());
        assert!(def.validate_params(&json!({ "on": "yes" })).is_err());
        assert!(def.validate_params(&json!({})).is_err());
    }

    #[test]
    fn node_power_always_warns() {
        let index = house();
        let facts = Facts { index: &index, runs_on: Some("vm102"), notes: &[], scene: None };
        let shutdown = find("host.shutdown").unwrap();
        let text = describe(shutdown, "host", &json!({}), &facts);
        let warning = text.warning.unwrap();
        assert!(warning.contains("offline") && warning.contains("cannot be undone"), "{warning}");
        let reboot = find("host.reboot").unwrap();
        assert!(describe(reboot, "host", &json!({}), &facts).warning.unwrap().contains("offline"));
        let quiet = Facts { runs_on: None, ..facts };
        assert!(describe(reboot, "host", &json!({}), &quiet).warning.unwrap().contains("offline"), "always");
    }

    #[test]
    fn console_vm_stop_warns() {
        let index = house();
        let def = find("vm.stop").unwrap();
        assert!(takes_console_offline(def, "vm102", Some("vm102")));
        assert!(!takes_console_offline(def, "vm101", Some("vm102")));
        assert!(!takes_console_offline(find("vm.start").unwrap(), "vm102", Some("vm102")));
        assert!(!takes_console_offline(def, "vm102", None), "a hub that runs on none of its devices");
        let facts = Facts { index: &index, runs_on: Some("vm102"), notes: &[], scene: None };
        let text = describe(def, "vm102", &json!({}), &facts);
        assert!(text.warning.unwrap().contains("offline"));
    }

    #[test]
    fn the_static_table_has_generic_names() {
        let index = legacy_seeded();
        assert_eq!(index.get("vm101").unwrap().name, "VM 101");
        assert_eq!(index.get("host").unwrap().name, "Host");
        for device in DEVICES {
            assert!(device.name.chars().all(|c| c.is_ascii_alphanumeric() || c == ' '), "{}", device.name);
        }
    }
}
