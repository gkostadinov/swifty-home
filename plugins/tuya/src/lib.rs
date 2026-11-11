//! Tuya smart sockets for Swifty Home: the local protocol 3.3 on TCP 6668.
//!
//! Each socket is a row of the instance config (id, address, port, name, USB data
//! point) and its local key is the secret slot `local_key/<id>`. The sockets are
//! switched on the local network only. Setup can look the keys up in a Tuya cloud
//! project once, or take them as entered.
//!
//! Local ids are today's targets, `plug-<id>` and `plug-<id>-usb`, and are hinted
//! unchanged, so stored rooms, favourites and audit rows keep working.

#![forbid(unsafe_code)]

pub mod cloud;
pub mod config;
mod driver;
mod protocol;
pub mod setup;

use std::sync::Arc;

use serde_json::{json, Value};
use swifty_plugin_api::{
    Capability, ConfigField, ConfigProblem, DeviceClass, DeviceDecl, Driver, FieldKind, InstanceView, LiveContext,
    Manifest, NetUse, Op, Plugin, PluginError, Reach, Readiness, SecretSlot, SetupFlow, API_VERSION,
};

pub use crate::driver::TuyaDriver;
pub use crate::setup::TuyaSetup;

/// The fields of one socket row.
const PLUG_FIELDS: &[ConfigField] = &[
    ConfigField {
        key: "id",
        label: "Device id",
        help: "The Tuya device id, 10 to 32 lower-case letters and digits.",
        kind: FieldKind::Text { max_len: 32 },
        required: true,
        default: None,
    },
    ConfigField {
        key: "address",
        label: "Address",
        help: "The socket's IPv4 address on the local network.",
        kind: FieldKind::Host,
        required: true,
        default: None,
    },
    ConfigField {
        key: "port",
        label: "Port",
        help: "The local protocol's port.",
        kind: FieldKind::Port,
        required: false,
        default: Some("6668"),
    },
    ConfigField {
        key: "name",
        label: "Name",
        help: "How it is shown. Without one it is \"Socket 1\", \"Socket 2\" and so on, in this order.",
        kind: FieldKind::Text { max_len: config::MAX_PLUG_NAME },
        required: false,
        default: None,
    },
    ConfigField {
        key: "usb_dp",
        label: "USB data point",
        help: "The data point of a USB port that switches on its own, usually 7. It becomes a switch of its own.",
        kind: FieldKind::Int { min: 2, max: 255 },
        required: false,
        default: None,
    },
];

/// Everything the hub and a person can know about this plugin before it runs.
pub static MANIFEST: Manifest = Manifest {
    id: "tuya",
    name: "Tuya smart sockets",
    version: env!("CARGO_PKG_VERSION"),
    api: API_VERSION,
    summary: "Switches Tuya-based smart sockets, and USB ports that switch on their own, with the local \
              protocol 3.3 on your network. Setup can look the local keys up in your Tuya cloud project once; \
              the sockets are never switched through the cloud.",
    reach: Reach::Local,
    network: &[
        NetUse::Tcp { port: config::DEFAULT_PORT },
        // Setup only, for the key lookup, in the data center the person picks.
        NetUse::CloudHost { host: "openapi.tuyaeu.com" },
        NetUse::CloudHost { host: "openapi-weaz.tuyaeu.com" },
        NetUse::CloudHost { host: "openapi.tuyaus.com" },
        NetUse::CloudHost { host: "openapi-ueaz.tuyaus.com" },
        NetUse::CloudHost { host: "openapi.tuyacn.com" },
        NetUse::CloudHost { host: "openapi.tuyain.com" },
    ],
    classes: &[DeviceClass::Plug],
    config_version: 1,
    config: &[ConfigField {
        key: "plugs",
        label: "Sockets",
        help: "One row per socket, 1 to 32. Each one's local key is stored as a secret, never here.",
        kind: FieldKind::Rows { fields: PLUG_FIELDS },
        required: true,
        default: None,
    }],
    secrets: &[SecretSlot {
        slot: "local_key/{id}",
        label: "Local key",
        help: "The socket's local key: 16 characters as Tuya shows them, stored in base64.",
        required: true,
    }],
    actions: &[],
    vocabulary: &[],
    nouns: &[("tuya", DeviceClass::Plug)],
    multi_instance: false,
};

/// The plugin, as the hub's list of compiled-in plugins names it.
pub static PLUGIN: TuyaPlugin = TuyaPlugin;

/// Tuya smart sockets.
pub struct TuyaPlugin;

/// The default name of the socket in row `row` (from 0).
fn default_name(row: usize) -> String {
    format!("Socket {}", row + 1)
}

impl Plugin for TuyaPlugin {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn validate(&self, config: &Value) -> Result<(), ConfigProblem> {
        config::parse(config).map(|_| ())
    }

    /// Ready when at least one socket has its key. A socket without one is refused
    /// on its own, as a bad key file disabled only that socket before.
    fn readiness(&self, instance: &InstanceView<'_>) -> Readiness {
        match config::parse(instance.config) {
            Err(problem) => Readiness::Disabled(format!("The socket config is not valid: {problem}")),
            Ok(tuya) if tuya.plugs.iter().any(|plug| instance.secrets_present.contains(&plug.key_slot())) => {
                Readiness::Ready
            }
            Ok(_) => Readiness::NeedsSetup(
                "No socket has its local key yet. Set the sockets up again to look the keys up or enter them."
                    .to_string(),
            ),
        }
    }

    fn devices(&self, instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
        let Ok(tuya) = config::parse(instance.config) else { return Vec::new() };
        let mut out = Vec::new();
        for (row, plug) in tuya.plugs.iter().enumerate() {
            let name = plug.name.clone().unwrap_or_else(|| default_name(row));
            let attrs = json!({ "id": plug.id, "address": plug.address, "port": plug.port });
            out.push(DeviceDecl {
                local_id: plug.target(),
                target_hint: Some(plug.target()),
                class: DeviceClass::Plug,
                name: name.clone(),
                capabilities: vec![Capability::Power],
                attrs: attrs.clone(),
            });
            if let (Some(usb), Some(point)) = (plug.usb_target(), plug.usb_dp) {
                let mut attrs = attrs;
                attrs["data_point"] = json!(point);
                out.push(DeviceDecl {
                    local_id: usb.clone(),
                    target_hint: Some(usb),
                    class: DeviceClass::Plug,
                    name: format!("{name} USB"),
                    capabilities: vec![Capability::Power],
                    attrs,
                });
            }
        }
        out
    }

    fn refusal(&self, instance: &InstanceView<'_>, device: &str, op: &Op) -> Option<String> {
        if !matches!(op, Op::PlugPower { .. }) {
            return Some("A socket cannot do that.".to_string());
        }
        let tuya = config::parse(instance.config).ok();
        let plug = tuya.as_ref().and_then(|tuya| {
            tuya.plugs.iter().find(|plug| plug.target() == device || plug.usb_target().as_deref() == Some(device))
        });
        let Some(plug) = plug else {
            return Some(format!("{device} is not a known socket."));
        };
        (!instance.secrets_present.contains(&plug.key_slot()))
            .then(|| format!("The local key for {} is not set.", plug.target()))
    }

    fn connect(&self, ctx: LiveContext) -> Result<Arc<dyn Driver>, PluginError> {
        let tuya = config::parse(&ctx.config)
            .map_err(|problem| PluginError(format!("The socket config is not valid: {problem}")))?;
        Ok(Arc::new(TuyaDriver::new(ctx.grant, &tuya, &ctx.secrets, ctx.timeouts)))
    }

    fn setup(&self) -> Option<Box<dyn SetupFlow>> {
        Some(Box::new(TuyaSetup::new()))
    }
}
