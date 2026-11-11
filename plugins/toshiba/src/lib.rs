//! Toshiba air conditioners on Wi-Fi, through Toshiba's Home AC Control cloud.
//!
//! The Wi-Fi adapter in these units talks only to Toshiba's cloud and offers nothing
//! on the local network, so this plugin has to leave the house. Toshiba does not
//! document that cloud for other software: this is an unofficial integration that
//! speaks the protocol of Toshiba's own app, and it can break whenever Toshiba changes
//! it. The manifest says so (`Reach::Cloud { official: false }`), and the setup starts
//! with that notice.
//!
//! - [`config`]: the instance config and its checks.
//! - [`identity`]: how the plugin presents itself to the cloud, overridable in config.
//! - [`protocol`]: the unit's state, the command message, the tokens. Pure.
//! - `driver`: signing in, reading and commanding.
//! - `setup`: the guided setup.

#![forbid(unsafe_code)]

pub mod config;
mod driver;
pub mod identity;
pub mod protocol;
mod setup;

use std::sync::Arc;

use serde_json::{json, Value};
use swifty_plugin_api::{
    Capability, ConfigField, ConfigProblem, DeviceClass, DeviceDecl, Driver, FieldKind, InstanceView, LiveContext,
    Manifest, NetUse, Op, Plugin, PluginError, Reach, Readiness, SecretSlot, SetupFlow, API_VERSION,
};

pub use driver::{ToshibaDriver, NO_SIGN_IN};
pub use setup::{ToshibaSetup, DISCLAIMER};

use config::ToshibaConfig;
use protocol::{FAN_CODES, MODE_CODES, TARGET_MAX, TARGET_MIN};

/// The secret slot that holds the account's sign-in: one line, base64 of the JSON the
/// cloud takes.
pub const LOGIN_SLOT: &str = "login";

/// The label of the field where the person types the account's sign-in secret: the
/// word Toshiba's own app uses. The hub's source scan allows it in this plugin's field
/// labels and nowhere else.
pub(crate) const SECRET_LABEL: &str = "Password";

const NOT_SIGNED_IN: &str =
    "The air conditioners are disabled until the Toshiba sign-in is stored. Run this integration's setup.";

const UNIT_FIELDS: &[ConfigField] = &[
    ConfigField {
        key: "id",
        label: "Unit id",
        help: "The Id Toshiba's cloud lists for the unit: 8 to 64 letters, digits and hyphens.",
        kind: FieldKind::Text { max_len: config::MAX_AIRCON_ID },
        required: true,
        default: None,
    },
    ConfigField {
        key: "unique_id",
        label: "Unique id",
        help: "The DeviceUniqueId Toshiba's cloud lists for the unit. Commands are addressed to it.",
        kind: FieldKind::Text { max_len: config::MAX_AIRCON_UNIQUE_ID },
        required: true,
        default: None,
    },
    ConfigField {
        key: "name",
        label: "Name",
        help: "What the app calls it. Without one it is \"Air conditioner\", numbered when there are several.",
        kind: FieldKind::Text { max_len: config::MAX_NAME },
        required: false,
        default: None,
    },
];

const FIELDS: &[ConfigField] = &[
    ConfigField {
        key: "units",
        label: "Air conditioners",
        help: "One row per unit on the account, up to 8. The setup fills these in.",
        kind: FieldKind::Rows { fields: UNIT_FIELDS },
        required: true,
        default: None,
    },
    ConfigField {
        key: "client_suffix",
        label: "Client id suffix",
        help: "Hex digits joined to the account's e-mail to name this hub at Toshiba's cloud. Keep the value an \
               existing install has: the cloud knows its client by it.",
        kind: FieldKind::Text { max_len: config::MAX_CLIENT_SUFFIX },
        required: false,
        default: Some(identity::CLIENT_SUFFIX),
    },
    ConfigField {
        key: "user_agent",
        label: "User agent",
        help: "What the hub says it is. Toshiba's cloud turns away clients that do not look like a browser; change \
               this only if it starts turning this one away.",
        kind: FieldKind::Text { max_len: config::MAX_USER_AGENT },
        required: false,
        default: Some(identity::USER_AGENT),
    },
    ConfigField {
        key: "api_url",
        label: "API address",
        help: "Toshiba's REST API. Only a test changes this.",
        kind: FieldKind::Url { schemes: &["https", "http"] },
        required: false,
        default: Some(config::DEFAULT_API_URL),
    },
    ConfigField {
        key: "hub_url",
        label: "Command hub address",
        help: "The hub that takes commands. A token for any other hub is refused. Only a test changes this.",
        kind: FieldKind::Url { schemes: &["https", "http"] },
        required: false,
        default: Some(config::DEFAULT_HUB_URL),
    },
    ConfigField {
        key: "timeout_ms",
        label: "Reply timeout (ms)",
        help: "How long one request to the cloud may take to answer. The hub's own read timeout is the most it can be.",
        kind: FieldKind::Int { min: 1, max: config::MAX_TIMEOUT_MS as i64 },
        required: false,
        default: Some("10000"),
    },
];

/// What Toshiba's plugin is.
pub static MANIFEST: Manifest = Manifest {
    id: "toshiba",
    name: "Toshiba air conditioners",
    version: env!("CARGO_PKG_VERSION"),
    api: API_VERSION,
    summary: "Toshiba air conditioners on Wi-Fi, through Toshiba's Home AC Control cloud. Unofficial: it speaks the \
              protocol of Toshiba's own app and can break when Toshiba changes it.",
    reach: Reach::Cloud { vendor: "Toshiba", official: false },
    network: &[
        NetUse::CloudHost { host: "mobileapi.toshibahomeaccontrols.com" },
        NetUse::CloudHost { host: "toshibasmaciothubprod.azure-devices.net" },
    ],
    classes: &[DeviceClass::Aircon],
    config_version: 1,
    config: FIELDS,
    secrets: &[SecretSlot {
        slot: LOGIN_SLOT,
        label: "Account sign-in",
        help: "The account's e-mail and sign-in secret, as one line of base64 JSON. The setup writes it.",
        required: true,
    }],
    actions: &[],
    vocabulary: &[("air con", "aircon")],
    nouns: &[("toshiba", DeviceClass::Aircon)],
    multi_instance: true,
};

/// The plain name of the unit at `position`, when it has none of its own.
pub(crate) fn default_name(position: usize, several: bool) -> String {
    if several {
        format!("Air conditioner {}", position + 1)
    } else {
        "Air conditioner".to_string()
    }
}

/// Why this cannot go to this unit, if it cannot. Reads no credential and opens nothing.
pub fn refusal(config: Option<&ToshibaConfig>, target: &str, op: &Op) -> Option<String> {
    if !op.is_aircon_op() {
        return Some("An air conditioner cannot do that.".to_string());
    }
    let known = config.is_some_and(|toshiba| toshiba.units.iter().any(|unit| unit.target() == target));
    (!known).then(|| format!("{target} is not a known air conditioner."))
}

fn capabilities() -> Vec<Capability> {
    vec![
        Capability::Power,
        Capability::Mode { modes: MODE_CODES.iter().map(|(name, _)| name.to_string()).collect() },
        Capability::Temperature { min: TARGET_MIN, max: TARGET_MAX },
        Capability::Fan { speeds: FAN_CODES.iter().map(|(name, _)| name.to_string()).collect() },
    ]
}

/// The plugin. Exported as [`PLUGIN`].
pub struct ToshibaPlugin;

/// Toshiba's plugin, for the hub's list of compiled-in plugins.
pub static PLUGIN: ToshibaPlugin = ToshibaPlugin;

impl Plugin for ToshibaPlugin {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn validate(&self, config: &Value) -> Result<(), ConfigProblem> {
        config::parse(config).map(|_| ())
    }

    fn readiness(&self, instance: &InstanceView<'_>) -> Readiness {
        match config::parse(instance.config) {
            Err(problem) => Readiness::Disabled(format!("The Toshiba settings are not valid: {problem}")),
            Ok(_) if !instance.secrets_present.contains(LOGIN_SLOT) => Readiness::NeedsSetup(NOT_SIGNED_IN.to_string()),
            Ok(_) => Readiness::Ready,
        }
    }

    fn devices(&self, instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
        let Ok(toshiba) = config::parse(instance.config) else { return Vec::new() };
        let several = toshiba.units.len() > 1;
        toshiba
            .units
            .iter()
            .enumerate()
            .map(|(position, unit)| DeviceDecl {
                local_id: unit.target(),
                target_hint: Some(unit.target()),
                class: DeviceClass::Aircon,
                name: unit.name.clone().unwrap_or_else(|| default_name(position, several)),
                capabilities: capabilities(),
                attrs: json!({ "id": unit.id }),
            })
            .collect()
    }

    fn refusal(&self, instance: &InstanceView<'_>, device: &str, op: &Op) -> Option<String> {
        refusal(config::parse(instance.config).ok().as_ref(), device, op)
    }

    fn connect(&self, ctx: LiveContext) -> Result<Arc<dyn Driver>, PluginError> {
        let toshiba = config::parse(&ctx.config).map_err(|problem| PluginError(problem.to_string()))?;
        let login = ctx.secrets.get(LOGIN_SLOT).ok_or_else(|| PluginError(NO_SIGN_IN.to_string()))?;
        let driver = ToshibaDriver::new(ctx.grant, &toshiba, login.expose(), ctx.timeouts).map_err(PluginError)?;
        Ok(Arc::new(driver))
    }

    fn setup(&self) -> Option<Box<dyn SetupFlow>> {
        Some(Box::new(ToshibaSetup::new()))
    }
}
