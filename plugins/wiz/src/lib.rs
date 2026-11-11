//! WiZ smart bulbs for Swifty Home.
//!
//! Each bulb takes JSON over UDP port 38899 and needs no key or account. Bulbs
//! come from two places: the instance config names some by address (`bulb1`),
//! and [`Driver::discover`] broadcasts `getPilot` and offers every bulb that
//! answers as `wiz-<mac>`, which the hub may then adopt.
//!
//! Nothing in this crate opens a socket itself: every datagram goes through
//! `swifty-net`, which needs the grant only a live hub has.

#![forbid(unsafe_code)]

pub mod config;
pub mod driver;
pub mod protocol;
pub mod setup;

use std::sync::Arc;

use serde_json::Value;
use swifty_plugin_api::{
    ConfigField, ConfigProblem, DeviceClass, DeviceDecl, DeviceRecord, Driver, FieldKind, InstanceView, LiveContext,
    Manifest, NetUse, Op, Plugin, PluginError, Reach, Readiness, SetupFlow, API_VERSION,
};

use crate::config::WizConfig;
use crate::driver::{adopted_address, light_capabilities, WizDriver};
use crate::protocol::{is_discovered_id, DEFAULT_PORT};

/// The plugin, as the hub lists it.
pub static PLUGIN: Wiz = Wiz;

/// The WiZ plugin.
#[derive(Debug, Clone, Copy)]
pub struct Wiz;

const BULB_FIELDS: &[ConfigField] = &[
    ConfigField {
        key: "id",
        label: "Id",
        help: "A short id for the bulb, such as bulb1. It is also the name the hub asks for.",
        kind: FieldKind::Text { max_len: 64 },
        required: true,
        default: None,
    },
    ConfigField {
        key: "address",
        label: "Address",
        help: "The bulb's IP address. Add :port only if it does not listen on the usual port.",
        kind: FieldKind::Host,
        required: true,
        default: None,
    },
];

/// Everything the hub and a person can know about this plugin before it runs.
pub static MANIFEST: Manifest = Manifest {
    id: "wiz",
    name: "WiZ",
    version: env!("CARGO_PKG_VERSION"),
    api: API_VERSION,
    summary: "WiZ smart bulbs on the local network: power, brightness and colour over UDP, with no account and no key.",
    reach: Reach::Local,
    network: &[NetUse::Udp { port: DEFAULT_PORT }, NetUse::UdpBroadcast { port: DEFAULT_PORT }],
    classes: &[DeviceClass::Light],
    config_version: 1,
    config: &[
        ConfigField {
            key: "bulbs",
            label: "Bulbs by address",
            help: "Bulbs you name by address. Bulbs found by discovery do not need to be listed.",
            kind: FieldKind::Rows { fields: BULB_FIELDS },
            required: false,
            default: None,
        },
        ConfigField {
            key: "port",
            label: "Port",
            help: "The UDP port of a bulb whose address names none.",
            kind: FieldKind::Port,
            required: false,
            default: Some("38899"),
        },
        ConfigField {
            key: "broadcast",
            label: "Broadcast address",
            help: "Where discovery sends its question, as IPv4 address:port: the top address of the bulbs' \
                   network, or 255.255.255.255:38899. Discovery is off until this is set.",
            kind: FieldKind::HostPort,
            required: false,
            default: None,
        },
    ],
    secrets: &[],
    actions: &[],
    vocabulary: &[("whiz", "wiz")],
    nouns: &[("wiz", DeviceClass::Light)],
    multi_instance: false,
};

const INVALID: &str = "The WiZ configuration is not valid.";

/// Why this operation cannot go to this bulb, if it cannot. Pure: the config and
/// the adopted records are all it looks at.
pub fn refusal(config: &WizConfig, adopted: &[DeviceRecord], device: &str, op: &Op) -> Option<String> {
    if !matches!(op, Op::LightPower { .. } | Op::LightBrightness { .. } | Op::LightHue { .. } | Op::LightSet { .. }) {
        return Some("A WiZ bulb cannot do that.".to_string());
    }
    if is_discovered_id(device) {
        let known = adopted.iter().filter(|record| record.local_id == device).find_map(adopted_address);
        return known.is_none().then(|| format!("{device} is not a known light."));
    }
    config.address(device).is_none().then(|| format!("{device} has no address in this instance's bulbs."))
}

/// A generic name for a bulb from the config: `bulb3` is "WiZ light 3".
fn default_name(id: &str) -> String {
    let digits = id.trim_start_matches(|c: char| !c.is_ascii_digit());
    if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
        format!("WiZ light {digits}")
    } else {
        format!("WiZ light {id}")
    }
}

impl Plugin for Wiz {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn validate(&self, config: &Value) -> Result<(), ConfigProblem> {
        config::parse(config).map(|_| ())
    }

    fn readiness(&self, instance: &InstanceView<'_>) -> Readiness {
        match config::parse(instance.config) {
            Err(problem) => Readiness::Disabled(format!("{INVALID} {problem}")),
            Ok(config) if config.bulbs.is_empty() && config.broadcast.is_none() && instance.adopted.is_empty() => {
                Readiness::NeedsSetup("Name a bulb by its address, or set a broadcast address to find bulbs.".into())
            }
            Ok(_) => Readiness::Ready,
        }
    }

    fn devices(&self, instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
        let Ok(config) = config::parse(instance.config) else { return Vec::new() };
        let mut devices: Vec<DeviceDecl> = config
            .bulbs
            .iter()
            .filter_map(|bulb| {
                let address = config.address(&bulb.id)?;
                Some(DeviceDecl {
                    local_id: bulb.id.clone(),
                    target_hint: Some(bulb.id.clone()),
                    class: DeviceClass::Light,
                    name: default_name(&bulb.id),
                    capabilities: light_capabilities(),
                    attrs: serde_json::json!({ "address": address.to_string() }),
                })
            })
            .collect();
        for record in instance.adopted.iter().filter(|record| adopted_address(record).is_some()) {
            if devices.iter().any(|device| device.local_id == record.local_id) {
                continue;
            }
            devices.push(DeviceDecl {
                local_id: record.local_id.clone(),
                target_hint: Some(record.local_id.clone()),
                class: DeviceClass::Light,
                name: record.name.clone(),
                capabilities: light_capabilities(),
                attrs: record.attrs.clone(),
            });
        }
        devices
    }

    fn refusal(&self, instance: &InstanceView<'_>, device: &str, op: &Op) -> Option<String> {
        match config::parse(instance.config) {
            Ok(config) => refusal(&config, instance.adopted, device, op),
            Err(_) => Some(INVALID.to_string()),
        }
    }

    fn connect(&self, ctx: LiveContext) -> Result<Arc<dyn Driver>, PluginError> {
        let config = config::parse(&ctx.config).map_err(|problem| PluginError(format!("{INVALID} {problem}")))?;
        Ok(Arc::new(WizDriver::new(ctx.grant, config, ctx.timeouts.read, ctx.directory)))
    }

    fn setup(&self) -> Option<Box<dyn SetupFlow>> {
        Some(Box::new(setup::WizSetup::default()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;
    use swifty_plugin_api::CapKind;

    use super::*;

    fn record(local_id: &str, attrs: Value) -> DeviceRecord {
        DeviceRecord {
            target: local_id.to_string(),
            local_id: local_id.to_string(),
            class: DeviceClass::Light,
            name: "Hall".to_string(),
            attrs,
        }
    }

    fn view<'a>(config: &'a Value, adopted: &'a [DeviceRecord], none: &'a BTreeSet<String>) -> InstanceView<'a> {
        InstanceView { id: "wiz", config, secrets_present: none, adopted }
    }

    #[test]
    fn configured_and_adopted_bulbs_keep_todays_ids() {
        let config = json!({ "bulbs": [{ "id": "bulb1", "address": "192.0.2.21" }, { "id": "bulb2", "address": "192.0.2.22" }] });
        let adopted = [
            record("wiz-a8bb50aabbcc", json!({ "mac": "a8bb50aabbcc", "address": "192.0.2.30:38899" })),
            record("wiz-a8bb50aabbcd", json!({ "mac": "a8bb50aabbcd" })),
        ];
        let none = BTreeSet::new();
        let devices = PLUGIN.devices(&view(&config, &adopted, &none));
        let ids: Vec<(&str, Option<&str>, &str)> =
            devices.iter().map(|d| (d.local_id.as_str(), d.target_hint.as_deref(), d.name.as_str())).collect();
        assert_eq!(
            ids,
            [
                ("bulb1", Some("bulb1"), "WiZ light 1"),
                ("bulb2", Some("bulb2"), "WiZ light 2"),
                ("wiz-a8bb50aabbcc", Some("wiz-a8bb50aabbcc"), "Hall"),
            ],
            "a record with no address is not offered"
        );
        for device in &devices {
            assert_eq!(device.class, DeviceClass::Light);
            let kinds: Vec<CapKind> = device.capabilities.iter().map(|c| c.kind()).collect();
            assert_eq!(kinds, [CapKind::Power, CapKind::Brightness, CapKind::Color]);
        }
    }

    #[test]
    fn refusals_are_pure_and_agree_with_the_driver() {
        let config = json!({ "bulbs": [{ "id": "bulb1", "address": "192.0.2.21" }] });
        let adopted = [record("wiz-a8bb50aabbcc", json!({ "address": "192.0.2.30" }))];
        let none = BTreeSet::new();
        let view = view(&config, &adopted, &none);
        let on = Op::LightPower { on: true };
        assert_eq!(PLUGIN.refusal(&view, "bulb1", &on), None);
        assert_eq!(PLUGIN.refusal(&view, "wiz-a8bb50aabbcc", &on), None);
        assert!(PLUGIN.refusal(&view, "bulb2", &on).is_some());
        assert!(PLUGIN.refusal(&view, "wiz-a8bb50aabbcd", &on).is_some(), "not adopted");
        assert!(PLUGIN.refusal(&view, "bulb1", &Op::VmStart).is_some());
        assert!(PLUGIN.refusal(&view, "bulb1", &Op::LightSet { percent: 40, hue: 30 }).is_none());
    }

    #[test]
    fn readiness_follows_the_config() {
        let none = BTreeSet::new();
        let empty = json!({});
        assert!(matches!(PLUGIN.readiness(&view(&empty, &[], &none)), Readiness::NeedsSetup(_)));
        let broadcast = json!({ "broadcast": "192.0.2.255:38899" });
        assert_eq!(PLUGIN.readiness(&view(&broadcast, &[], &none)), Readiness::Ready);
        let bad = json!({ "broadcast": "nowhere" });
        assert!(matches!(PLUGIN.readiness(&view(&bad, &[], &none)), Readiness::Disabled(_)));
        assert_eq!(
            PLUGIN.refusal(&view(&bad, &[], &none), "bulb1", &Op::LightPower { on: true }).as_deref(),
            Some(INVALID)
        );
        assert!(PLUGIN.devices(&view(&bad, &[], &none)).is_empty());
    }

    #[test]
    fn a_generic_name_comes_from_the_id() {
        assert_eq!(default_name("bulb3"), "WiZ light 3");
        assert_eq!(default_name("desk"), "WiZ light desk");
        assert_eq!(default_name("a1b"), "WiZ light a1b");
    }
}
