//! The Glimdock plugin for Swifty Home.
//!
//! A Glimdock node is a small HTTP service on a machine that collects that
//! machine's telemetry (CPU, memory, power, temperatures, disks, storage pools)
//! and pairs its own monitoring agents. This plugin offers one device per node:
//!
//! - class `telemetry`, capabilities `Metrics` and `AgentPairing`;
//! - local id `collector`, hinted as the target `collector` so an existing
//!   install keeps its target;
//! - default name "Glimdock node".
//!
//! It reads the node's snapshot (`GET <snapshot_path>`) for the charts and the
//! health page, and asks the node to start pairing an agent
//! (`POST <pairing_path>`) when the owner runs `agent.pair`. Nothing else is
//! ever sent: the node is read-only here. The README describes the HTTP
//! contract a node has to keep.

#![forbid(unsafe_code)]

pub mod config;
pub mod driver;
pub mod protocol;
pub mod setup;

use std::sync::Arc;

use serde_json::{json, Value};
use swifty_plugin_api::{
    Capability, ConfigField, ConfigProblem, DeviceClass, DeviceDecl, Driver, FieldKind, InstanceView, LiveContext,
    Manifest, NetUse, Op, Plugin, PluginError, Reach, Readiness, SecretSlot, SetupFlow, API_VERSION,
};

pub use config::GlimdockConfig;
pub use driver::GlimdockDriver;
pub use setup::GlimdockSetup;

/// The local id of a node's one device. Also its target hint, so the target an
/// existing install already uses, `collector`, is kept.
pub const DEVICE: &str = "collector";

/// The name a node's device gets until someone names it.
pub const DEFAULT_NAME: &str = "Glimdock node";

/// The secret slot of the node's reader token.
pub const TOKEN_SLOT: &str = "token";

/// Why nothing can be sent to a node.
pub const READ_ONLY: &str = "A Glimdock node is read-only here: it reports readings and takes no commands.";

const NOT_PAIRED: &str = "The node's access token is not stored yet. Run the setup again and paste it.";

const CONFIG_FIELDS: &[ConfigField] = &[
    ConfigField {
        key: "url",
        label: "Node URL",
        help: "The node's base URL: scheme, host and port only, such as http://192.0.2.10:8080.",
        kind: FieldKind::Url { schemes: &["http", "https"] },
        required: true,
        default: None,
    },
    ConfigField {
        key: "snapshot_path",
        label: "Snapshot path",
        help: "The path of the node's snapshot endpoint, such as /api/snapshot. A plain absolute path.",
        kind: FieldKind::Text { max_len: config::MAX_PATH },
        required: true,
        default: None,
    },
    ConfigField {
        key: "pairing_path",
        label: "Agent pairing path",
        help: "The path of the node's agent pairing endpoint. Without it, pairing an agent says it is not set up.",
        kind: FieldKind::Text { max_len: config::MAX_PATH },
        required: false,
        default: None,
    },
];

/// What the plugin says about itself.
pub static MANIFEST: Manifest = Manifest {
    id: "glimdock",
    name: "Glimdock",
    version: env!("CARGO_PKG_VERSION"),
    api: API_VERSION,
    summary: "Glimdock nodes: the telemetry snapshot behind the charts and the health page, and pairing of the \
              node's own monitoring agents. Read-only.",
    reach: Reach::Local,
    // The node's URL names its port; these are the schemes' own ports.
    network: &[NetUse::Http { port: 80 }, NetUse::Https { port: 443 }],
    classes: &[DeviceClass::Telemetry],
    config_version: 1,
    config: CONFIG_FIELDS,
    secrets: &[SecretSlot {
        slot: TOKEN_SLOT,
        label: "Access token",
        help: "The node's reader token. Sent only as `Authorization: Bearer` in a header.",
        required: true,
    }],
    actions: &[],
    vocabulary: &[("glim dock", "glimdock")],
    nouns: &[("glimdock", DeviceClass::Telemetry)],
    multi_instance: true,
};

/// The plugin.
pub struct Glimdock;

/// The plugin, as the hub's compiled-in list names it.
pub static PLUGIN: Glimdock = Glimdock;

impl Plugin for Glimdock {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn validate(&self, config: &Value) -> Result<(), ConfigProblem> {
        GlimdockConfig::parse(config).map(|_| ())
    }

    fn readiness(&self, instance: &InstanceView<'_>) -> Readiness {
        if let Err(problem) = GlimdockConfig::parse(instance.config) {
            return Readiness::Disabled(format!("The config is not valid: {problem}."));
        }
        if !instance.secrets_present.contains(TOKEN_SLOT) {
            return Readiness::NeedsSetup(NOT_PAIRED.to_string());
        }
        Readiness::Ready
    }

    fn devices(&self, _instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
        vec![DeviceDecl {
            local_id: DEVICE.to_string(),
            target_hint: Some(DEVICE.to_string()),
            class: DeviceClass::Telemetry,
            name: DEFAULT_NAME.to_string(),
            capabilities: vec![Capability::Metrics, Capability::AgentPairing],
            attrs: json!({}),
        }]
    }

    fn refusal(&self, _instance: &InstanceView<'_>, _device: &str, _op: &Op) -> Option<String> {
        Some(READ_ONLY.to_string())
    }

    fn connect(&self, ctx: LiveContext) -> Result<Arc<dyn Driver>, PluginError> {
        let config = GlimdockConfig::parse(&ctx.config)
            .map_err(|problem| PluginError(format!("The config is not valid: {problem}.")))?;
        let token = ctx.secrets.require(TOKEN_SLOT)?;
        let driver = GlimdockDriver::new(ctx.grant, &config, token, ctx.timeouts).map_err(PluginError)?;
        Ok(Arc::new(driver))
    }

    fn setup(&self) -> Option<Box<dyn SetupFlow>> {
        Some(Box::new(GlimdockSetup::new()))
    }
}
