//! Klipper 3D printers through Moonraker, for Swifty Home.
//!
//! One instance is one printer (`printer`): start the job Klipper has loaded,
//! pause, resume, emergency stop, and its state, temperatures and progress read
//! back from Moonraker's HTTP API.

#![forbid(unsafe_code)]

pub mod config;
mod driver;
pub mod protocol;
mod setup;

use std::sync::Arc;

use serde_json::{json, Value};
use swifty_plugin_api::{
    Capability, ConfigField, ConfigProblem, DeviceClass, DeviceDecl, Driver, FieldKind, InstanceView, LiveContext,
    Manifest, NetUse, Op, Plugin, PluginError, Reach, Readiness, SecretSlot, SetupFlow, API_VERSION,
};

pub use config::{MoonrakerConfig, KEY_SLOT, PRINTER};
pub use driver::{MoonrakerDriver, NOT_A_PRINTER_OP};
pub use setup::MoonrakerSetup;

const CONFIG: &[ConfigField] = &[ConfigField {
    key: "url",
    label: "Moonraker address",
    help: "http:// and the printer's address with Moonraker's port, usually 7125, and nothing after it.",
    kind: FieldKind::Url { schemes: &["http", "https"] },
    required: true,
    default: None,
}];

const SECRETS: &[SecretSlot] = &[SecretSlot {
    slot: KEY_SLOT,
    label: "API key",
    help: "Only when Moonraker asks for one. It is sent as a header and never in a URL.",
    required: false,
}];

static MANIFEST: Manifest = Manifest {
    id: "moonraker",
    name: "Klipper (Moonraker)",
    version: env!("CARGO_PKG_VERSION"),
    api: API_VERSION,
    summary: "A Klipper 3D printer through Moonraker's HTTP API: start the loaded job, pause, resume, emergency \
              stop, and its state, temperatures and progress.",
    reach: Reach::Local,
    network: &[NetUse::Http { port: 7125 }],
    classes: &[DeviceClass::Printer],
    config_version: 1,
    config: CONFIG,
    secrets: SECRETS,
    actions: &[],
    vocabulary: &[("clipper", "klipper")],
    nouns: &[("klipper", DeviceClass::Printer), ("moonraker", DeviceClass::Printer)],
    multi_instance: true,
};

/// The printer, as a device.
fn printer_decl() -> DeviceDecl {
    DeviceDecl {
        local_id: PRINTER.to_string(),
        target_hint: Some(PRINTER.to_string()),
        class: DeviceClass::Printer,
        name: "3D printer".to_string(),
        capabilities: vec![Capability::PrintJob, Capability::EmergencyStop],
        attrs: json!({}),
    }
}

/// The plugin. Use [`PLUGIN`].
pub struct MoonrakerPlugin;

/// The Moonraker plugin, for the hub's list of compiled-in plugins.
pub static PLUGIN: MoonrakerPlugin = MoonrakerPlugin;

impl Plugin for MoonrakerPlugin {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn validate(&self, config: &Value) -> Result<(), ConfigProblem> {
        config::parse(config).map(|_| ())
    }

    fn readiness(&self, instance: &InstanceView<'_>) -> Readiness {
        match config::parse(instance.config) {
            Ok(_) => Readiness::Ready,
            Err(problem) => Readiness::Disabled(problem.to_string()),
        }
    }

    fn devices(&self, _instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
        vec![printer_decl()]
    }

    fn refusal(&self, _instance: &InstanceView<'_>, device: &str, op: &Op) -> Option<String> {
        driver::refusal(device, op)
    }

    fn connect(&self, ctx: LiveContext) -> Result<Arc<dyn Driver>, PluginError> {
        let config = config::parse(&ctx.config).map_err(|problem| PluginError(problem.to_string()))?;
        let key = ctx.secrets.get(KEY_SLOT).map(|value| value.expose());
        let driver = MoonrakerDriver::new(ctx.grant, &config, key, ctx.timeouts).map_err(PluginError)?;
        Ok(Arc::new(driver))
    }

    fn setup(&self) -> Option<Box<dyn SetupFlow>> {
        Some(Box::new(MoonrakerSetup::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn the_printer_is_the_one_device_and_takes_only_print_operations() {
        let config = json!({ "url": "http://192.0.2.30:7125" });
        let present = BTreeSet::new();
        let view = InstanceView { id: "moonraker", config: &config, secrets_present: &present, adopted: &[] };
        assert_eq!(PLUGIN.readiness(&view), Readiness::Ready);
        let devices = PLUGIN.devices(&view);
        assert_eq!(devices.len(), 1);
        assert_eq!((devices[0].local_id.as_str(), devices[0].target_hint.as_deref()), ("printer", Some("printer")));
        for op in [Op::PrintStart, Op::PrintPause, Op::PrintResume, Op::PrintEmergencyStop] {
            assert_eq!(PLUGIN.refusal(&view, "printer", &op), None, "{op:?}");
            assert!(PLUGIN.refusal(&view, "printer2", &op).is_some(), "{op:?}");
        }
        for op in [Op::VmStart, Op::LightPower { on: true }, Op::TvKey { key: "HOME".into() }] {
            assert_eq!(PLUGIN.refusal(&view, "printer", &op).as_deref(), Some(NOT_A_PRINTER_OP), "{op:?}");
        }
    }
}
