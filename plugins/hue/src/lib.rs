//! Philips Hue for Swifty Home, through the bridge's local v1 REST API.
//!
//! The device `hue` is the bridge itself and only reports that it can be
//! reached. Its lights are `hue-<n>`: [`Driver::discover`] lists them, the hub
//! adopts the ones the owner picks, and they are then set one at a time.
//!
//! The bridge hands out an application key while its link button is pressed.
//! The setup flow asks for it in a `Wait` step and the hub keeps it in the
//! secret slot `app_key`. New pairings call this hub `swifty_home#hub`.
//!
//! Nothing in this crate opens a socket itself: every request goes through
//! `swifty-net`, which needs the grant only a live hub has.

#![forbid(unsafe_code)]

pub mod config;
pub mod driver;
pub mod protocol;
pub mod setup;

use std::sync::Arc;

use serde_json::Value;
use swifty_plugin_api::{
    ConfigField, ConfigProblem, DeviceClass, DeviceDecl, Driver, FieldKind, InstanceView, LiveContext, Manifest,
    NetUse, Op, Plugin, PluginError, Reach, Readiness, SecretSlot, SetupFlow, API_VERSION,
};

use crate::config::APP_KEY;
use crate::driver::{light_capabilities, record_colour, HueDriver};
use crate::protocol::{is_light_id, BRIDGE_ID};

/// The plugin, as the hub lists it.
pub static PLUGIN: Hue = Hue;

/// The Hue plugin.
#[derive(Debug, Clone, Copy)]
pub struct Hue;

/// Everything the hub and a person can know about this plugin before it runs.
pub static MANIFEST: Manifest = Manifest {
    id: "hue",
    name: "Philips Hue",
    version: env!("CARGO_PKG_VERSION"),
    api: API_VERSION,
    summary: "Lights on a Philips Hue Bridge, through the bridge's local API: power, brightness and colour. \
              Paired with the bridge's link button; the Hue cloud is never used.",
    reach: Reach::Local,
    network: &[NetUse::Http { port: 80 }, NetUse::Https { port: 443 }],
    classes: &[DeviceClass::Bridge, DeviceClass::Light],
    config_version: 1,
    config: &[ConfigField {
        key: "url",
        label: "Bridge address",
        help: "The bridge's address on your network, such as http://192.0.2.10: a scheme, a host and a port, \
               and no path.",
        kind: FieldKind::Url { schemes: &["http", "https"] },
        required: true,
        default: None,
    }],
    secrets: &[SecretSlot {
        slot: APP_KEY,
        label: "Application key",
        help: "The key the bridge gives when its link button is pressed. Setup asks for it; nobody types it.",
        required: true,
    }],
    actions: &[],
    vocabulary: &[],
    // No noun: "hue" is the voice grammar's word for a colour ("set the hue of
    // bulb 2 to blue"), so it cannot also name the lights.
    nouns: &[],
    multi_instance: true,
};

const INVALID: &str = "The Hue configuration is not valid.";
const NOT_PAIRED: &str = "The bridge is not paired yet. Set this instance up again and press the bridge's link button.";

impl Plugin for Hue {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn validate(&self, config: &Value) -> Result<(), ConfigProblem> {
        config::parse(config).map(|_| ())
    }

    fn readiness(&self, instance: &InstanceView<'_>) -> Readiness {
        if let Err(problem) = config::parse(instance.config) {
            return Readiness::Disabled(format!("{INVALID} {problem}"));
        }
        if !instance.secrets_present.contains(APP_KEY) {
            return Readiness::NeedsSetup(NOT_PAIRED.to_string());
        }
        Readiness::Ready
    }

    fn devices(&self, instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
        if config::parse(instance.config).is_err() {
            return Vec::new();
        }
        let bridge = DeviceDecl {
            local_id: BRIDGE_ID.to_string(),
            target_hint: Some(BRIDGE_ID.to_string()),
            class: DeviceClass::Bridge,
            name: "Hue Bridge".to_string(),
            capabilities: Vec::new(),
            attrs: Value::Object(Default::default()),
        };
        let lights = instance.adopted.iter().filter(|record| is_light_id(&record.local_id)).map(|record| DeviceDecl {
            local_id: record.local_id.clone(),
            target_hint: Some(record.local_id.clone()),
            class: DeviceClass::Light,
            name: record.name.clone(),
            capabilities: light_capabilities(record_colour(record) != Some(false)),
            attrs: record.attrs.clone(),
        });
        std::iter::once(bridge).chain(lights).collect()
    }

    fn refusal(&self, instance: &InstanceView<'_>, device: &str, op: &Op) -> Option<String> {
        if config::parse(instance.config).is_err() {
            return Some(INVALID.to_string());
        }
        let record = instance.adopted.iter().find(|record| record.local_id == device);
        driver::refusal(device, op, record)
    }

    fn connect(&self, ctx: LiveContext) -> Result<Arc<dyn Driver>, PluginError> {
        let config = config::parse(&ctx.config).map_err(|problem| PluginError(format!("{INVALID} {problem}")))?;
        let key = ctx.secrets.get(APP_KEY).cloned().ok_or_else(|| PluginError(NOT_PAIRED.to_string()))?;
        let driver = HueDriver::new(ctx.grant, &config.url, key, ctx.timeouts, ctx.directory).map_err(PluginError)?;
        Ok(Arc::new(driver))
    }

    fn setup(&self) -> Option<Box<dyn SetupFlow>> {
        Some(Box::new(setup::HueSetup::default()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;
    use swifty_plugin_api::{CapKind, DeviceRecord};

    use super::*;

    fn record(local_id: &str, attrs: Value) -> DeviceRecord {
        DeviceRecord {
            target: local_id.to_string(),
            local_id: local_id.to_string(),
            class: DeviceClass::Light,
            name: "Lightstrip".to_string(),
            attrs,
        }
    }

    #[test]
    fn the_bridge_and_its_adopted_lights_keep_todays_ids() {
        let config = json!({ "url": "http://192.0.2.10" });
        let adopted = [record("hue-1", json!({ "colour": true })), record("hue-2", json!({ "colour": false }))];
        let paired: BTreeSet<String> = [APP_KEY.to_string()].into();
        let view = InstanceView { id: "hue", config: &config, secrets_present: &paired, adopted: &adopted };
        let devices = PLUGIN.devices(&view);
        let ids: Vec<(&str, DeviceClass)> = devices.iter().map(|d| (d.local_id.as_str(), d.class)).collect();
        assert_eq!(ids, [("hue", DeviceClass::Bridge), ("hue-1", DeviceClass::Light), ("hue-2", DeviceClass::Light)]);
        assert!(devices.iter().all(|d| d.target_hint.as_deref() == Some(d.local_id.as_str())));
        assert!(devices[0].capabilities.is_empty(), "the bridge takes no commands");
        let kinds = |i: usize| devices[i].capabilities.iter().map(|c| c.kind()).collect::<Vec<_>>();
        assert_eq!(kinds(1), [CapKind::Power, CapKind::Brightness, CapKind::Color]);
        assert_eq!(kinds(2), [CapKind::Power, CapKind::Brightness]);
        assert_eq!(PLUGIN.readiness(&view), Readiness::Ready);
    }

    #[test]
    fn without_a_key_the_bridge_needs_setup() {
        let config = json!({ "url": "http://192.0.2.10" });
        let none = BTreeSet::new();
        let view = InstanceView { id: "hue", config: &config, secrets_present: &none, adopted: &[] };
        assert!(matches!(PLUGIN.readiness(&view), Readiness::NeedsSetup(_)));
        let bad = json!({ "url": "http://192.0.2.10/api" });
        let view = InstanceView { id: "hue", config: &bad, secrets_present: &none, adopted: &[] };
        assert!(matches!(PLUGIN.readiness(&view), Readiness::Disabled(_)));
        assert!(PLUGIN.devices(&view).is_empty());
    }

    #[test]
    fn refusals_are_pure() {
        let config = json!({ "url": "http://192.0.2.10" });
        let adopted = [record("hue-1", json!({})), record("hue-2", json!({ "colour": false }))];
        let none = BTreeSet::new();
        let view = InstanceView { id: "hue", config: &config, secrets_present: &none, adopted: &adopted };
        let on = Op::LightPower { on: true };
        assert!(PLUGIN.refusal(&view, "hue", &on).is_some(), "the bridge");
        assert_eq!(PLUGIN.refusal(&view, "hue-1", &on), None);
        assert!(PLUGIN.refusal(&view, "hue-3", &on).is_some(), "not adopted");
        assert!(PLUGIN.refusal(&view, "hue-1", &Op::Play).is_some());
        assert!(PLUGIN.refusal(&view, "hue-2", &Op::LightHue { hue: 10 }).is_some(), "no colour");
        assert_eq!(PLUGIN.refusal(&view, "hue-2", &Op::LightSet { percent: 0, hue: 10 }), None, "that is off");
        assert_eq!(PLUGIN.refusal(&view, "hue-2", &Op::LightBrightness { percent: 40 }), None);
    }
}
