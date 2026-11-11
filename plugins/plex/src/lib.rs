//! The Plex plugin for Swifty Home.
//!
//! It answers one question about a Plex Media Server: is it up, and does it
//! take the token? The server is one device:
//!
//! - class `service`, no capabilities beyond reading;
//! - local id `plex`, hinted as the target `plex` so an existing install keeps it;
//! - default name "Plex server".
//!
//! Nothing is ever played, changed or listed. Every operation is refused.

#![forbid(unsafe_code)]

pub mod config;
pub mod driver;
pub mod setup;

use std::sync::Arc;

use serde_json::{json, Value};
use swifty_plugin_api::{
    ConfigField, ConfigProblem, DeviceClass, DeviceDecl, Driver, FieldKind, InstanceView, LiveContext, Manifest,
    NetUse, Op, Plugin, PluginError, Reach, Readiness, SecretSlot, SetupFlow, API_VERSION,
};

pub use config::PlexConfig;
pub use driver::PlexDriver;
pub use setup::PlexSetup;

/// The local id of the server's one device, and its target hint.
pub const DEVICE: &str = "plex";

/// The name the server gets until someone names it.
pub const DEFAULT_NAME: &str = "Plex server";

/// The secret slot of the server's token.
pub const TOKEN_SLOT: &str = "token";

/// Why nothing can be sent to the server.
pub const READ_ONLY: &str = "Plex is read-only here.";

const NO_TOKEN: &str = "The Plex token is not stored yet. Run the setup again and paste it.";

/// What the plugin says about itself.
pub static MANIFEST: Manifest = Manifest {
    id: "plex",
    name: "Plex",
    version: env!("CARGO_PKG_VERSION"),
    api: API_VERSION,
    summary: "Whether a Plex Media Server is up and accepts its token. Read-only: nothing is played or changed.",
    reach: Reach::Local,
    // Plex serves HTTP and HTTPS on the same port. The URL names the one to use.
    network: &[NetUse::Http { port: 32400 }, NetUse::Https { port: 32400 }],
    classes: &[DeviceClass::Service],
    config_version: 1,
    config: &[ConfigField {
        key: "url",
        label: "Server URL",
        help: "The server's base URL: scheme, host and port only, such as http://192.0.2.20:32400.",
        kind: FieldKind::Url { schemes: &["http", "https"] },
        required: true,
        default: None,
    }],
    secrets: &[SecretSlot {
        slot: TOKEN_SLOT,
        label: "Token",
        help: "The server's X-Plex-Token. Sent only in the X-Plex-Token header, never in a URL.",
        required: true,
    }],
    actions: &[],
    vocabulary: &[],
    nouns: &[("plex", DeviceClass::Service)],
    multi_instance: true,
};

/// The plugin.
pub struct Plex;

/// The plugin, as the hub's compiled-in list names it.
pub static PLUGIN: Plex = Plex;

impl Plugin for Plex {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn validate(&self, config: &Value) -> Result<(), ConfigProblem> {
        PlexConfig::parse(config).map(|_| ())
    }

    fn readiness(&self, instance: &InstanceView<'_>) -> Readiness {
        if let Err(problem) = PlexConfig::parse(instance.config) {
            return Readiness::Disabled(format!("The config is not valid: {problem}."));
        }
        if !instance.secrets_present.contains(TOKEN_SLOT) {
            return Readiness::NeedsSetup(NO_TOKEN.to_string());
        }
        Readiness::Ready
    }

    fn devices(&self, _instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
        vec![DeviceDecl {
            local_id: DEVICE.to_string(),
            target_hint: Some(DEVICE.to_string()),
            class: DeviceClass::Service,
            name: DEFAULT_NAME.to_string(),
            capabilities: Vec::new(),
            attrs: json!({}),
        }]
    }

    fn refusal(&self, _instance: &InstanceView<'_>, _device: &str, _op: &Op) -> Option<String> {
        Some(READ_ONLY.to_string())
    }

    fn connect(&self, ctx: LiveContext) -> Result<Arc<dyn Driver>, PluginError> {
        let config = PlexConfig::parse(&ctx.config)
            .map_err(|problem| PluginError(format!("The config is not valid: {problem}.")))?;
        let token = ctx.secrets.require(TOKEN_SLOT)?;
        let driver = PlexDriver::new(ctx.grant, &config, token, ctx.timeouts).map_err(PluginError)?;
        Ok(Arc::new(driver))
    }

    fn setup(&self) -> Option<Box<dyn SetupFlow>> {
        Some(Box::new(PlexSetup::new()))
    }
}
