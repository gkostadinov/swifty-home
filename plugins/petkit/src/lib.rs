//! The Petkit D4 feeder and T4 litter box, through Petkit's cloud.
//!
//! Neither device has a local interface, so this plugin speaks the HTTPS interface that
//! Petkit's own Android app uses. Petkit does not document it for other software: this
//! is an unofficial integration, it can change without notice, and Petkit's terms do not
//! clearly allow it. The manifest says so (`Reach::Cloud { official: false }`), the setup
//! starts with that notice, and it is meant to run on a second account that the devices
//! are shared to. Feeding and cleaning start switched off (`commands = false`).
//!
//! - [`config`]: the instance config and its checks.
//! - [`identity`]: how the plugin presents itself to Petkit, overridable in config.
//! - [`protocol`]: dates, the login, the replies and what they say. Pure.
//! - [`driver`]: the login, the readings and the two commands.
//! - `setup`: the guided setup.

#![forbid(unsafe_code)]

pub mod config;
pub mod driver;
pub mod identity;
pub mod protocol;
mod setup;

use std::sync::Arc;

use serde_json::{json, Value};
use swifty_plugin_api::{
    Capability, ConfigField, ConfigProblem, DeviceClass, DeviceDecl, Driver, FieldKind, InstanceView, LiveContext,
    Manifest, NetUse, Op, Plugin, PluginError, Reach, Readiness, SecretSlot, SetupFlow, API_VERSION,
};

pub use driver::PetkitDriver;
pub use setup::{PetkitSetup, DISCLAIMER};

use config::PetkitConfig;
use protocol::{parse_credentials, FEEDER, FEED_GRAMS, LITTER};

/// The secret slot that holds the login: one line, the account, a colon, and the MD5 of
/// its sign-in secret in hex.
pub const LOGIN_SLOT: &str = "login";

/// The label of the field where the person types the account's sign-in secret: the
/// word Petkit's own app uses. The hub's source scan allows it in this plugin's field
/// labels and nowhere else.
pub(crate) const SECRET_LABEL: &str = "Password";

pub const COMMANDS_OFF: &str = "Petkit commands are switched off. Turn on commands in this integration's settings \
                                once the readings match the real devices.";
const NOT_LOGGED_IN: &str =
    "The feeder and litter box are disabled until the Petkit login is stored. Run this integration's setup.";

const FIELDS: &[ConfigField] = &[
    ConfigField {
        key: "region",
        label: "Region",
        help: "Your country as the Petkit app has it, such as DE, or china.",
        kind: FieldKind::Text { max_len: config::MAX_REGION },
        required: true,
        default: None,
    },
    ConfigField {
        key: "timezone",
        label: "Time zone",
        help: "An IANA name, sent to Petkit as the time zone of this account.",
        kind: FieldKind::Text { max_len: config::MAX_TIMEZONE },
        required: false,
        default: Some(config::DEFAULT_TIMEZONE),
    },
    ConfigField {
        key: "utc_offset_minutes",
        label: "Minutes east of UTC",
        help: "For that zone, now. The date of a feed and the time shown for the last one use it. It does not \
               follow daylight saving time by itself.",
        kind: FieldKind::Int { min: -(config::MAX_OFFSET_MINUTES as i64), max: config::MAX_OFFSET_MINUTES as i64 },
        required: false,
        default: Some("0"),
    },
    ConfigField {
        key: "commands",
        label: "Commands",
        help: "Feeding and cleaning are refused until this is on, so the readings can be checked against the real \
               devices first.",
        kind: FieldKind::Bool,
        required: false,
        default: Some("false"),
    },
    ConfigField {
        key: "feeder_id",
        label: "Feeder id",
        help: "Only needed when the account has more than one feeder. The setup fills it in.",
        kind: FieldKind::Int { min: 1, max: i64::MAX },
        required: false,
        default: None,
    },
    ConfigField {
        key: "litter_id",
        label: "Litter box id",
        help: "Only needed when the account has more than one litter box. The setup fills it in.",
        kind: FieldKind::Int { min: 1, max: i64::MAX },
        required: false,
        default: None,
    },
    ConfigField {
        key: "poll_secs",
        label: "Reading kept for (s)",
        help: "How often a reading is fetched from Petkit when nothing was just commanded.",
        kind: FieldKind::Int { min: config::MIN_POLL_SECS as i64, max: config::MAX_POLL_SECS as i64 },
        required: false,
        default: Some("60"),
    },
    ConfigField {
        key: "passport_url",
        label: "Passport address",
        help: "Where Petkit lists its regional servers. Only a test changes this.",
        kind: FieldKind::Url { schemes: &["https", "http"] },
        required: false,
        default: Some(config::DEFAULT_PASSPORT_URL),
    },
    ConfigField {
        key: "app_version",
        label: "App version",
        help: "The version of Petkit's app the hub says it is. Change it only if Petkit starts refusing this one.",
        kind: FieldKind::Text { max_len: config::MAX_IDENTITY },
        required: false,
        default: Some(identity::APP_VERSION),
    },
    ConfigField {
        key: "user_agent",
        label: "User agent",
        help: "The app's HTTP client, as the hub presents it.",
        kind: FieldKind::Text { max_len: config::MAX_USER_AGENT },
        required: false,
        default: Some(identity::USER_AGENT),
    },
    ConfigField {
        key: "os_version",
        label: "Android version",
        help: "Of the phone the hub says the app runs on.",
        kind: FieldKind::Text { max_len: config::MAX_IDENTITY },
        required: false,
        default: Some(identity::OS_VERSION),
    },
    ConfigField {
        key: "phone_model",
        label: "Phone model",
        help: "Of the phone the hub says the app runs on.",
        kind: FieldKind::Text { max_len: config::MAX_IDENTITY },
        required: false,
        default: Some(identity::PHONE_MODEL),
    },
    ConfigField {
        key: "phone_brand",
        label: "Phone brand",
        help: "Of the phone the hub says the app runs on.",
        kind: FieldKind::Text { max_len: config::MAX_IDENTITY },
        required: false,
        default: Some(identity::PHONE_BRAND),
    },
];

/// What Petkit's plugin is.
pub static MANIFEST: Manifest = Manifest {
    id: "petkit",
    name: "Petkit feeder and litter box",
    version: env!("CARGO_PKG_VERSION"),
    api: API_VERSION,
    summary: "The Petkit D4 feeder and T4 litter box, through Petkit's cloud. Unofficial: it presents itself as \
              Petkit's app and can break when Petkit changes its servers.",
    reach: Reach::Cloud { vendor: "Petkit", official: false },
    network: &[
        NetUse::CloudHost { host: "passport.petkt.com" },
        NetUse::CloudHost { host: "api.petkt.com" },
        NetUse::CloudHost { host: "api.eu-pet.com" },
        NetUse::CloudHost { host: "api.petktasia.com" },
        NetUse::CloudHost { host: "api.petkit.cn" },
    ],
    classes: &[DeviceClass::Feeder, DeviceClass::Litter],
    config_version: 1,
    config: FIELDS,
    secrets: &[SecretSlot {
        slot: LOGIN_SLOT,
        label: "Account login",
        help: "The account, a colon, and the MD5 of its sign-in secret in hex, on one line. The setup writes it.",
        required: true,
    }],
    actions: &[],
    vocabulary: &[("pet feeder", "petkit feeder")],
    nouns: &[("petkit", DeviceClass::Feeder)],
    multi_instance: false,
};

/// Why this cannot go to this device, if it cannot. Reads nothing and opens nothing.
pub fn refusal(config: Option<&PetkitConfig>, target: &str, op: &Op) -> Option<String> {
    let reason = match (target, op) {
        (FEEDER, Op::PetFeed { grams }) if !FEED_GRAMS.contains(grams) => {
            Some("The D4 feeds 10, 20, 30, 40 or 50 g at a time.".to_string())
        }
        (FEEDER, Op::PetFeed { .. }) | (LITTER, Op::LitterClean) => None,
        (FEEDER | LITTER, _) => Some("This Petkit device cannot do that.".to_string()),
        _ => Some(format!("{target} is not a Petkit device.")),
    };
    reason.or_else(|| config.filter(|petkit| !petkit.commands).map(|_| COMMANDS_OFF.to_string()))
}

/// The plugin. Exported as [`PLUGIN`].
pub struct PetkitPlugin;

/// Petkit's plugin, for the hub's list of compiled-in plugins.
pub static PLUGIN: PetkitPlugin = PetkitPlugin;

impl Plugin for PetkitPlugin {
    fn manifest(&self) -> &'static Manifest {
        &MANIFEST
    }

    fn validate(&self, config: &Value) -> Result<(), ConfigProblem> {
        config::parse(config).map(|_| ())
    }

    fn readiness(&self, instance: &InstanceView<'_>) -> Readiness {
        match config::parse(instance.config) {
            Err(problem) => Readiness::Disabled(format!("The Petkit settings are not valid: {problem}")),
            Ok(_) if !instance.secrets_present.contains(LOGIN_SLOT) => Readiness::NeedsSetup(NOT_LOGGED_IN.to_string()),
            Ok(_) => Readiness::Ready,
        }
    }

    fn devices(&self, instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
        if config::parse(instance.config).is_err() {
            return Vec::new();
        }
        vec![
            DeviceDecl {
                local_id: FEEDER.to_string(),
                target_hint: Some(FEEDER.to_string()),
                class: DeviceClass::Feeder,
                name: "Petkit D4 feeder".to_string(),
                capabilities: vec![Capability::Dispense { grams: FEED_GRAMS.to_vec() }],
                attrs: json!({}),
            },
            DeviceDecl {
                local_id: LITTER.to_string(),
                target_hint: Some(LITTER.to_string()),
                class: DeviceClass::Litter,
                name: "Petkit T4 litter box".to_string(),
                capabilities: vec![Capability::Clean],
                attrs: json!({}),
            },
        ]
    }

    fn refusal(&self, instance: &InstanceView<'_>, device: &str, op: &Op) -> Option<String> {
        refusal(config::parse(instance.config).ok().as_ref(), device, op)
    }

    fn connect(&self, ctx: LiveContext) -> Result<Arc<dyn Driver>, PluginError> {
        let petkit = config::parse(&ctx.config).map_err(|problem| PluginError(problem.to_string()))?;
        let text = ctx
            .secrets
            .get(LOGIN_SLOT)
            .ok_or_else(|| PluginError("The Petkit login is not stored. Run this integration's setup.".to_string()))?;
        let login = parse_credentials(text.expose()).map_err(PluginError)?;
        let driver = PetkitDriver::new(ctx.grant, &petkit, login, ctx.timeouts).map_err(PluginError)?;
        Ok(Arc::new(driver))
    }

    fn setup(&self) -> Option<Box<dyn SetupFlow>> {
        Some(Box::new(PetkitSetup::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(commands: bool) -> PetkitConfig {
        config::parse(&json!({
            "region": "DE",
            "timezone": "Europe/Paris",
            "utc_offset_minutes": 120,
            "commands": commands,
            "passport_url": "https://passport.petkt.com/",
            "poll_secs": 60,
        }))
        .unwrap()
    }

    #[test]
    fn the_d4_feeds_only_the_amounts_petkit_accepts() {
        let petkit = config(true);
        for grams in FEED_GRAMS {
            assert_eq!(refusal(Some(&petkit), FEEDER, &Op::PetFeed { grams }), None, "{grams}");
        }
        for grams in [0u8, 5, 15, 25, 51, 100] {
            let reason = refusal(Some(&petkit), FEEDER, &Op::PetFeed { grams }).unwrap();
            assert!(reason.contains("10, 20, 30, 40 or 50"), "{grams}: {reason}");
        }
        assert_eq!(refusal(Some(&petkit), LITTER, &Op::LitterClean), None);
        assert!(refusal(Some(&petkit), FEEDER, &Op::LitterClean).is_some());
        assert!(refusal(Some(&petkit), LITTER, &Op::PetFeed { grams: 10 }).is_some());
        assert!(refusal(Some(&petkit), "bulb1", &Op::LitterClean).is_some());
        assert!(refusal(Some(&petkit), FEEDER, &Op::Play).is_some());
    }

    #[test]
    fn commands_stay_off_until_the_config_says_so() {
        let off = config(false);
        for (target, op) in [(FEEDER, Op::PetFeed { grams: 10 }), (LITTER, Op::LitterClean)] {
            assert_eq!(refusal(Some(&off), target, &op).as_deref(), Some(COMMANDS_OFF));
            assert_eq!(refusal(Some(&config(true)), target, &op), None);
        }
    }
}
