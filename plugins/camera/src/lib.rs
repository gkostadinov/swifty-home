//! The IP camera plugin for Swifty Home.
//!
//! It answers one question about a camera: is it up and speaking RTSP? The
//! camera is one device:
//!
//! - class `camera` (the family may see it), no capabilities beyond reading;
//! - local id `camera`, hinted as the target `camera` so an existing install keeps it;
//! - default name "IP camera".
//!
//! The probe is one RTSP `OPTIONS` request without credentials. The camera's
//! login is stored, because a camera is not ready without it, but the probe never
//! sends it. Every operation is refused.

#![forbid(unsafe_code)]

pub mod config;
pub mod driver;
pub mod protocol;
pub mod setup;

use std::sync::Arc;

use serde_json::{json, Value};
use swifty_plugin_api::{
    ConfigField, ConfigProblem, DeviceClass, DeviceDecl, Driver, FieldKind, InstanceView, LiveContext, Manifest,
    NetUse, Op, Plugin, PluginError, Reach, Readiness, SecretSlot, SetupFlow, API_VERSION,
};

pub use config::CameraConfig;
pub use driver::CameraDriver;
pub use setup::CameraSetup;

/// The local id of the camera's one device, and its target hint.
pub const DEVICE: &str = "camera";

/// The name the camera gets until someone names it.
pub const DEFAULT_NAME: &str = "IP camera";

/// The secret slot of the camera's login.
pub const LOGIN_SLOT: &str = "login";

/// Why nothing can be sent to the camera.
pub const READ_ONLY: &str = "The camera is view-only here.";

const NO_LOGIN: &str = "The camera is disabled until its login is stored. Run the setup again.";

/// What the plugin says about itself.
pub static MANIFEST: Manifest = Manifest {
    id: "camera",
    name: "IP camera",
    version: env!("CARGO_PKG_VERSION"),
    api: API_VERSION,
    summary: "Whether an IP camera is up and answering in RTSP. Read-only: the camera is never moved or changed, \
              and the probe sends no credentials.",
    reach: Reach::Local,
    network: &[NetUse::Tcp { port: config::DEFAULT_PORT }],
    classes: &[DeviceClass::Camera],
    config_version: 1,
    config: &[
        ConfigField {
            key: "host",
            label: "Address",
            help: "The camera's IP address or host name, such as 192.0.2.30.",
            kind: FieldKind::Host,
            required: true,
            default: None,
        },
        ConfigField {
            key: "port",
            label: "RTSP port",
            help: "The camera's RTSP port.",
            kind: FieldKind::Port,
            required: false,
            default: Some("554"),
        },
    ],
    secrets: &[SecretSlot {
        slot: LOGIN_SLOT,
        label: "Login",
        help: "The camera's login, as user:secret. Stored for viewing; the status probe never sends it.",
        required: true,
    }],
    actions: &[],
    vocabulary: &[],
    nouns: &[],
    multi_instance: true,
};

/// The plugin.
pub struct Camera;

/// The plugin, as the hub's compiled-in list names it.
pub static PLUGIN: Camera = Camera;

impl Plugin for Camera {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn validate(&self, config: &Value) -> Result<(), ConfigProblem> {
        CameraConfig::parse(config).map(|_| ())
    }

    fn readiness(&self, instance: &InstanceView<'_>) -> Readiness {
        if let Err(problem) = CameraConfig::parse(instance.config) {
            return Readiness::Disabled(format!("The config is not valid: {problem}."));
        }
        if !instance.secrets_present.contains(LOGIN_SLOT) {
            return Readiness::NeedsSetup(NO_LOGIN.to_string());
        }
        Readiness::Ready
    }

    fn devices(&self, _instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
        vec![DeviceDecl {
            local_id: DEVICE.to_string(),
            target_hint: Some(DEVICE.to_string()),
            class: DeviceClass::Camera,
            name: DEFAULT_NAME.to_string(),
            capabilities: Vec::new(),
            attrs: json!({}),
        }]
    }

    fn refusal(&self, _instance: &InstanceView<'_>, _device: &str, _op: &Op) -> Option<String> {
        Some(READ_ONLY.to_string())
    }

    /// `Err` while the login is missing. The probe does not send it.
    fn connect(&self, ctx: LiveContext) -> Result<Arc<dyn Driver>, PluginError> {
        let config = CameraConfig::parse(&ctx.config)
            .map_err(|problem| PluginError(format!("The config is not valid: {problem}.")))?;
        ctx.secrets.require(LOGIN_SLOT)?;
        Ok(Arc::new(CameraDriver::new(ctx.grant, &config, ctx.timeouts)))
    }

    fn setup(&self) -> Option<Box<dyn SetupFlow>> {
        Some(Box::new(CameraSetup::new()))
    }
}
