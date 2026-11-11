//! LG webOS TVs: the SSAP websocket, pairing by a prompt on the TV, and
//! Wake-on-LAN to turn the TV on.
//!
//! The TV is paired once, by the setup flow: it registers without a key, the
//! person accepts the prompt on the TV, and the key the TV sends back is stored
//! in the secret slot `client_key`. The driver only ever presents that key and
//! never starts pairing. On `wss` the TV's self-signed certificate is accepted
//! only by its SHA-256, pinned in the config.

#![forbid(unsafe_code)]

mod config;
mod driver;
mod protocol;
mod setup;

use std::sync::Arc;

use serde_json::json;
use swifty_plugin_api::{
    Capability, ConfigField, ConfigProblem, DeviceClass, DeviceDecl, Driver, FieldKind, InstanceView, LiveContext,
    Manifest, NetUse, Op, Plugin, PluginError, Reach, Readiness, SecretSlot, SetupFlow, API_VERSION,
};

pub use config::{parse_mac, WebosConfig};
pub use driver::{WebosDriver, KEY_REFUSED};
pub use protocol::{BUTTONS, INPUTS, PERMISSIONS};
pub use setup::{fingerprint, WebosSetup, TRUST};

/// The TV's local id, which is also today's target.
pub const TV_ID: &str = "tv";
/// The secret slot that holds the key the TV handed out when it was paired.
pub const CLIENT_KEY: &str = "client_key";
/// Why the TV is listed but cannot be used yet.
pub const NOT_PAIRED: &str =
    "The TV is not paired yet. Run its setup and accept the prompt on the TV. Nothing else starts pairing.";

pub(crate) const CANNOT: &str = "The TV cannot do that.";

pub(crate) fn not_this_tv(device: &str) -> String {
    format!("{device} is not the TV of this instance.")
}

const FIELDS: &[ConfigField] = &[
    ConfigField {
        key: "url",
        label: "TV address",
        help: "ws://<address>:3000, or wss://<address>:3001 with the certificate's SHA-256 below. \
               Newer TVs only offer wss.",
        kind: FieldKind::Url { schemes: &["ws", "wss"] },
        required: true,
        default: None,
    },
    ConfigField {
        key: "cert_sha256",
        label: "Certificate SHA-256",
        help: "For wss only, and required there: the SHA-256 of the TV's certificate in hex. \
               Only that certificate is ever accepted.",
        kind: FieldKind::Text { max_len: 64 },
        required: false,
        default: None,
    },
    ConfigField {
        key: "wol_mac",
        label: "MAC address",
        help: "The TV's MAC address, for Wake-on-LAN. Without it the TV can be turned off but not on.",
        kind: FieldKind::Mac,
        required: false,
        default: None,
    },
    ConfigField {
        key: "wol_broadcast",
        label: "Wake-on-LAN broadcast",
        help: "Where the wake packet goes, as IP:port. 255.255.255.255:9 when empty.",
        kind: FieldKind::HostPort,
        required: false,
        default: None,
    },
];

const SECRETS: &[SecretSlot] = &[SecretSlot {
    slot: CLIENT_KEY,
    label: "Client key",
    help: "Handed out by the TV when its pairing prompt is accepted. The setup stores it; nobody types it.",
    required: true,
}];

/// What this plugin is, before it runs.
pub static MANIFEST: Manifest = Manifest {
    id: "webos",
    name: "LG webOS TV",
    version: env!("CARGO_PKG_VERSION"),
    api: API_VERSION,
    summary: "An LG TV with webOS on the local network: power, inputs and the remote's buttons. \
              Paired once with a prompt on the TV; turned on with Wake-on-LAN.",
    reach: Reach::Local,
    network: &[NetUse::Ws { port: 3000 }, NetUse::Ws { port: 3001 }, NetUse::WakeOnLan],
    classes: &[DeviceClass::Tv],
    config_version: 1,
    config: FIELDS,
    secrets: SECRETS,
    actions: &[],
    vocabulary: &[("lgtv", "lg tv")],
    nouns: &[("lg", DeviceClass::Tv)],
    multi_instance: true,
};

/// The plugin.
pub struct WebosPlugin;

/// The plugin, as the hub's compiled-in list names it.
pub static PLUGIN: WebosPlugin = WebosPlugin;

/// Why this cannot go to the TV, if it cannot. Pure.
pub fn refusal(config: Option<&WebosConfig>, op: &Op) -> Option<String> {
    match op {
        Op::TvPower { on: true } if !config.is_some_and(|c| c.wol_mac.is_some()) => {
            Some("The TV cannot be woken over SSAP. Set wol_mac to use Wake-on-LAN.".to_string())
        }
        Op::TvKey { key } if protocol::button_name(key).is_none() => {
            Some(format!("{key} is not a button on the remote."))
        }
        Op::TvInput { input } if protocol::input_app(input).is_none() => Some(format!("{input} is not a known input.")),
        Op::TvPower { .. } | Op::TvInput { .. } | Op::TvKey { .. } => None,
        _ => Some(CANNOT.to_string()),
    }
}

fn capabilities() -> Vec<Capability> {
    vec![
        Capability::Power,
        Capability::Inputs { inputs: INPUTS.iter().map(|(name, _)| name.to_string()).collect() },
        Capability::Keys { keys: BUTTONS.iter().map(|(key, _)| key.to_string()).collect() },
    ]
}

impl Plugin for WebosPlugin {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn validate(&self, config: &serde_json::Value) -> Result<(), ConfigProblem> {
        WebosConfig::parse(config).map(|_| ())
    }

    fn readiness(&self, instance: &InstanceView<'_>) -> Readiness {
        match WebosConfig::parse(instance.config) {
            Err(problem) => Readiness::Disabled(problem.to_string()),
            Ok(_) if !instance.secrets_present.contains(CLIENT_KEY) => Readiness::NeedsSetup(NOT_PAIRED.to_string()),
            Ok(_) => Readiness::Ready,
        }
    }

    fn devices(&self, instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
        let Ok(config) = WebosConfig::parse(instance.config) else { return Vec::new() };
        vec![DeviceDecl {
            local_id: TV_ID.to_string(),
            target_hint: Some(TV_ID.to_string()),
            class: DeviceClass::Tv,
            name: "TV".to_string(),
            capabilities: capabilities(),
            attrs: json!({ "url": config.url }),
        }]
    }

    fn refusal(&self, instance: &InstanceView<'_>, device: &str, op: &Op) -> Option<String> {
        if device != TV_ID {
            return Some(not_this_tv(device));
        }
        match WebosConfig::parse(instance.config) {
            Ok(config) => refusal(Some(&config), op),
            Err(problem) => Some(problem.to_string()),
        }
    }

    fn connect(&self, ctx: LiveContext) -> Result<Arc<dyn Driver>, PluginError> {
        let config = WebosConfig::parse(&ctx.config).map_err(|problem| PluginError(problem.to_string()))?;
        let key = ctx.secrets.get(CLIENT_KEY).cloned().ok_or_else(|| PluginError(NOT_PAIRED.to_string()))?;
        let driver = WebosDriver::new(ctx.grant, &config, key, ctx.timeouts).map_err(PluginError)?;
        Ok(Arc::new(driver))
    }

    fn setup(&self) -> Option<Box<dyn SetupFlow>> {
        Some(Box::new(WebosSetup::default()))
    }
}
