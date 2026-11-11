//! {{name}} for Swifty Home.
//!
//! Written by `swifty plugin new` from the plugin template. As written, it builds
//! and passes its tests against a stand-in device in `tests/`. To make it yours:
//!
//! - `protocol.rs`: what the device speaks. Pure functions with unit tests.
//! - `config.rs`: what a person fills in for one instance, and its checks.
//! - `driver.rs`: sends the bytes, through `swifty_plugin_api::net` only.
//! - `setup.rs`: the guided setup, the same in the app and in the terminal.
//!
//! DEVELOPING.md has the steps and the rules.

#![forbid(unsafe_code)]

pub mod config;
mod driver;
pub mod protocol;
mod setup;

use std::sync::Arc;

use serde_json::{json, Value};
{{#local}}
use swifty_plugin_api::{
    ConfigField, ConfigProblem, DeviceClass, DeviceDecl, Driver, FieldKind, InstanceView, LiveContext, Manifest,
    NetUse, Op, Plugin, PluginError, Reach, Readiness, SetupFlow, API_VERSION,
};
{{/local}}
{{#cloud}}
use swifty_plugin_api::{
    ConfigField, ConfigProblem, DeviceClass, DeviceDecl, Driver, FieldKind, InstanceView, LiveContext, Manifest,
    NetUse, Op, Plugin, PluginError, Reach, Readiness, SecretSlot, SetupFlow, API_VERSION,
};
{{/cloud}}

pub use crate::driver::{{Type}}Driver;
pub use crate::setup::{{Type}}Setup;

/// The fields of one device row.
const DEVICE_FIELDS: &[ConfigField] = &[
    ConfigField {
        key: "id",
        label: "Device id",
        help: "How the device knows itself: 1 to 64 lower-case letters, digits, - and _.",
        kind: FieldKind::Text { max_len: config::MAX_ID },
        required: true,
        default: None,
    },
{{#local}}
    ConfigField {
        key: "address",
        label: "Address",
        help: "The device's IP address on the local network.",
        kind: FieldKind::Host,
        required: true,
        default: None,
    },
    ConfigField {
        key: "port",
        label: "Port",
        help: "The port the device listens on.",
        kind: FieldKind::Port,
        required: false,
        default: Some("{{port}}"),
    },
{{/local}}
    ConfigField {
        key: "class",
        label: "Kind",
        help: "What kind of device it is.",
        kind: FieldKind::Choice {
            options: &[
                // The words of protocol::CLASSES, in the same order.
                {{class_options}}
            ],
        },
        required: false,
        default: Some("{{first_class}}"),
    },
    ConfigField {
        key: "name",
        label: "Name",
        help: "How it is shown. Without one it gets a name such as \"{{first_thing}} 1\".",
        kind: FieldKind::Text { max_len: config::MAX_NAME },
        required: false,
        default: None,
    },
];

/// Everything the hub and a person can know about this plugin before it runs.
pub static MANIFEST: Manifest = Manifest {
    id: "{{id}}",
    name: "{{name}}",
    version: env!("CARGO_PKG_VERSION"),
    api: API_VERSION,
{{#local}}
    summary: "Controls {{name}} devices on the local network.",
    reach: Reach::Local,
    network: &[NetUse::Udp { port: config::DEFAULT_PORT }],
{{/local}}
{{#cloud}}
    summary: "Controls {{name}} devices through the vendor's servers.",
    // Set `official` to true only when the vendor documents this API for third parties.
    reach: Reach::Cloud { vendor: "{{name}}", official: false },
    network: &[NetUse::CloudHost { host: config::API_HOST }],
{{/cloud}}
    classes: &[
        // Each kind of device it offers.
        {{classes}}
    ],
    config_version: 1,
{{#local}}
    config: &[ConfigField {
        key: "devices",
        label: "Devices",
        help: "One row per device.",
        kind: FieldKind::Rows { fields: DEVICE_FIELDS },
        required: true,
        default: None,
    }],
{{/local}}
{{#cloud}}
    config: &[
        ConfigField {
            key: "api_url",
            label: "Service address",
            help: "The address of the vendor's API.",
            kind: FieldKind::Url { schemes: &["https"] },
            required: false,
            default: Some(config::DEFAULT_API_URL),
        },
        ConfigField {
            key: "devices",
            label: "Devices",
            help: "One row per device.",
            kind: FieldKind::Rows { fields: DEVICE_FIELDS },
            required: true,
            default: None,
        },
    ],
{{/cloud}}
{{#local}}
    secrets: &[],
{{/local}}
{{#cloud}}
    secrets: &[SecretSlot {
        slot: config::TOKEN_SLOT,
        label: "Access token",
        help: "The token the vendor's app or developer page gives you.",
        required: true,
    }],
{{/cloud}}
    actions: &[],
    // Words a speech recogniser mishears, and brand words that name a class, such as
    // ("acme", DeviceClass::Plug). Lower-case words only; the conformance suite checks them.
    vocabulary: &[],
    nouns: &[],
    multi_instance: true,
};

/// The plugin, as the hub's list of compiled-in plugins names it.
pub static PLUGIN: {{Type}}Plugin = {{Type}}Plugin;

/// {{name}}.
pub struct {{Type}}Plugin;

impl Plugin for {{Type}}Plugin {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn validate(&self, config: &Value) -> Result<(), ConfigProblem> {
        config::parse(config).map(|_| ())
    }

    fn readiness(&self, instance: &InstanceView<'_>) -> Readiness {
        match config::parse(instance.config) {
            Err(problem) => Readiness::Disabled(format!("The configuration is not valid: {problem}")),
{{#cloud}}
            Ok(_) if !instance.secrets_present.contains(config::TOKEN_SLOT) => {
                Readiness::NeedsSetup("The access token is not set. Run the setup again and enter it.".to_string())
            }
{{/cloud}}
            Ok(_) => Readiness::Ready,
        }
    }

    fn devices(&self, instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
        let Ok(parsed) = config::parse(instance.config) else { return Vec::new() };
        parsed
            .devices
            .iter()
            .enumerate()
            .map(|(row, device)| DeviceDecl {
                local_id: device.id.clone(),
                target_hint: None,
                class: device.class(),
                name: device.name.clone().unwrap_or_else(|| protocol::default_name(device.class(), row)),
                capabilities: protocol::capabilities(device.class()),
{{#local}}
                attrs: json!({ "address": device.address, "port": device.port }),
{{/local}}
{{#cloud}}
                attrs: json!({ "id": device.id }),
{{/cloud}}
            })
            .collect()
    }

    fn refusal(&self, instance: &InstanceView<'_>, device: &str, op: &Op) -> Option<String> {
        let Ok(parsed) = config::parse(instance.config) else {
            return Some("The configuration is not valid.".to_string());
        };
        let Some(row) = parsed.device(device) else {
            return Some(format!("{device} is not a device of this instance."));
        };
{{#cloud}}
        if !instance.secrets_present.contains(config::TOKEN_SLOT) {
            return Some("The access token is not set.".to_string());
        }
{{/cloud}}
        protocol::encode(row.class(), op).err().map(str::to_string)
    }

    fn connect(&self, ctx: LiveContext) -> Result<Arc<dyn Driver>, PluginError> {
        let parsed = config::parse(&ctx.config)
            .map_err(|problem| PluginError(format!("The configuration is not valid: {problem}")))?;
{{#local}}
        Ok(Arc::new({{Type}}Driver::new(ctx.grant, ctx.timeouts, &parsed)))
{{/local}}
{{#cloud}}
        let token = ctx.secrets.require(config::TOKEN_SLOT)?.clone();
        Ok(Arc::new({{Type}}Driver::new(ctx.grant, ctx.timeouts, &parsed, token)?))
{{/cloud}}
    }

    fn setup(&self) -> Option<Box<dyn SetupFlow>> {
        Some(Box::new({{Type}}Setup::default()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn view<'a>(config: &'a Value, present: &'a BTreeSet<String>) -> InstanceView<'a> {
        InstanceView { id: "{{id}}", config, secrets_present: present, adopted: &[] }
    }

    #[test]
    fn every_row_is_a_device_with_a_name() {
{{#local}}
        let config = json!({ "devices": [
            { "id": "first", "address": "192.0.2.10" },
            { "id": "second", "address": "192.0.2.11", "name": "Hall" },
        ] });
{{/local}}
{{#cloud}}
        let config = json!({ "devices": [{ "id": "first" }, { "id": "second", "name": "Hall" }] });
{{/cloud}}
        let present = BTreeSet::new();
        let devices = PLUGIN.devices(&view(&config, &present));
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].name, "{{first_thing}} 1");
        assert_eq!(devices[1].name, "Hall");
        assert_eq!(devices[0].class, DeviceClass::{{first_variant}});
        assert!(PLUGIN.devices(&view(&json!({}), &present)).is_empty());
    }

    #[test]
    fn refusals_are_pure_and_name_the_reason() {
{{#local}}
        let config = json!({ "devices": [{ "id": "first", "address": "192.0.2.10" }] });
        let present = BTreeSet::new();
{{/local}}
{{#cloud}}
        let config = json!({ "devices": [{ "id": "first" }] });
        let present: BTreeSet<String> = [config::TOKEN_SLOT.to_string()].into();
        let missing = BTreeSet::new();
        assert!(PLUGIN.refusal(&view(&config, &missing), "first", &Op::VmStart).is_some());
        assert!(matches!(PLUGIN.readiness(&view(&config, &missing)), Readiness::NeedsSetup(_)));
{{/cloud}}
        assert_eq!(PLUGIN.readiness(&view(&config, &present)), Readiness::Ready);
        assert!(PLUGIN.refusal(&view(&config, &present), "nobody", &Op::VmStart).unwrap().contains("nobody"));
        assert!(PLUGIN.refusal(&view(&config, &present), "first", &Op::VmStart).is_some());
        assert!(matches!(PLUGIN.readiness(&view(&json!({}), &present)), Readiness::Disabled(_)));
    }
}
