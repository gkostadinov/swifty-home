//! The guided setup: the disclaimer, then the account, then which feeder and litter box.
//!
//! It replaces the old deploy step for Petkit: it logs in once, lists the feeders and litter
//! boxes on the account and shared to it, reads each chosen device once, and hands the
//! hub the config and the stored login. The sign-in secret the person types is turned
//! into its MD5 at once, which is all Petkit's login sends; the secret itself is neither
//! kept nor stored. Commands start switched off, so the readings can be checked first.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use swifty_plugin_api::{
    Choice, Driver, Field, FieldType, Scalar, Secret, SetupContext, SetupFlow, SetupInput, SetupResult, SetupStep,
};

use crate::config::{self, is_region, is_timezone, PetkitConfig, MAX_OFFSET_MINUTES, MAX_REGION, MAX_TIMEZONE};
use crate::driver::{Found, PetkitDriver};
use crate::protocol::{login_line, parse_credentials, FEEDER, FEEDER_TYPE, LITTER, LITTER_TYPE};
use crate::{LOGIN_SLOT, SECRET_LABEL};

/// What the person is told before anything else. Also in the README.
pub const DISCLAIMER: &str = "This is an unofficial integration. Petkit does not offer its cloud to other \
    software, so this plugin presents itself as Petkit's Android app and speaks its protocol, as the open-source \
    pypetkitapi client does. It may stop working whenever Petkit changes its servers, without warning, and \
    Petkit's terms do not clearly allow this use. Use a second Petkit account that the devices are shared to: an \
    account is signed in on one device at a time, so using your main one here signs the phone app out.";

const LIVE_NEEDED: &str =
    "Logging in to Petkit needs a hub that is live. Set live = true in swifty.toml, then set this up again.";
const NOT_FITTING: &str = "That answer does not fit this step.";
const MAX_ACCOUNT: usize = 128;
const MAX_SECRET: usize = 256;

enum Stage {
    Disclaimer,
    Account,
    Devices { driver: Arc<PetkitDriver>, settings: Map<String, Value>, login: Secret, found: Vec<(u64, String)> },
    Finished,
}

/// Petkit's setup flow.
pub struct PetkitSetup {
    stage: Stage,
    /// The stored config when an instance is set up again. Its settings are kept.
    existing: Map<String, Value>,
}

impl PetkitSetup {
    pub fn new() -> Self {
        Self { stage: Stage::Disclaimer, existing: Map::new() }
    }
}

impl Default for PetkitSetup {
    fn default() -> Self {
        Self::new()
    }
}

fn disclaimer() -> SetupStep {
    SetupStep::Form {
        title: "Petkit feeder and litter box: before you start".to_string(),
        text: DISCLAIMER.to_string(),
        fields: vec![Field {
            key: "accept".to_string(),
            label: "I understand, and want to go on".to_string(),
            help: "Nothing is sent anywhere until the next step.".to_string(),
            kind: FieldType::Bool,
            required: true,
            value: None,
        }],
    }
}

fn prefill(existing: &Map<String, Value>, key: &str) -> Option<Scalar> {
    match existing.get(key)? {
        Value::String(text) => Some(Scalar::Text(text.clone())),
        Value::Number(number) => number.as_i64().map(Scalar::Int),
        _ => None,
    }
}

fn account_form(existing: &Map<String, Value>) -> SetupStep {
    let field = |key: &str, label: &str, help: &str, kind: FieldType, required: bool| Field {
        key: key.to_string(),
        label: label.to_string(),
        help: help.to_string(),
        kind,
        required,
        value: prefill(existing, key),
    };
    SetupStep::Form {
        title: "The Petkit account".to_string(),
        text: "The account the devices are shared to, and where it is. The hub logs in to list the devices on it."
            .to_string(),
        fields: vec![
            field(
                "region",
                "Region",
                "Your country as the Petkit app has it, such as DE, or china.",
                FieldType::Text { max_len: MAX_REGION },
                true,
            ),
            field(
                "account",
                "Account",
                "The e-mail address or phone number the account signs in with.",
                FieldType::Text { max_len: MAX_ACCOUNT },
                true,
            ),
            Field {
                key: "sign_in".to_string(),
                label: SECRET_LABEL.to_string(),
                help: "The account's. Only its MD5 is sent to Petkit and stored on this hub, as Petkit's app does."
                    .to_string(),
                kind: FieldType::Secret,
                required: true,
                value: None,
            },
            Field {
                value: prefill(existing, "timezone").or(Some(Scalar::Text(config::DEFAULT_TIMEZONE.to_string()))),
                ..field(
                    "timezone",
                    "Time zone",
                    "An IANA name, such as Europe/Berlin. Petkit is told the account is there.",
                    FieldType::Text { max_len: MAX_TIMEZONE },
                    true,
                )
            },
            Field {
                value: prefill(existing, "utc_offset_minutes").or(Some(Scalar::Int(0))),
                ..field(
                    "utc_offset_minutes",
                    "Minutes east of UTC",
                    "For that zone, now: 60 for Berlin in winter. Petkit names days by it.",
                    FieldType::Int { min: -i64::from(MAX_OFFSET_MINUTES), max: i64::from(MAX_OFFSET_MINUTES) },
                    true,
                )
            },
        ],
    }
}

fn kind_name(kind: &str) -> &'static str {
    if kind == FEEDER_TYPE {
        "Petkit D4 feeder"
    } else {
        "Petkit T4 litter box"
    }
}

fn choose(found: &[(u64, String)]) -> SetupStep {
    let mut first_of = Vec::new();
    let options = found
        .iter()
        .map(|(id, kind)| {
            // The first of each kind starts picked, since that is the one used without a choice.
            let selected = !first_of.contains(kind);
            first_of.push(kind.clone());
            Choice {
                id: id.to_string(),
                label: kind_name(kind).to_string(),
                detail: format!("Petkit device {id}"),
                selected,
            }
        })
        .collect();
    SetupStep::Choose {
        title: "Devices".to_string(),
        text: "Pick the feeder, the litter box, or both: one of each at most.".to_string(),
        options,
        multiple: true,
        min: 1,
    }
}

fn failed(reason: impl Into<String>) -> SetupStep {
    SetupStep::Failed { reason: reason.into(), retry: true }
}

fn text(values: &BTreeMap<String, Scalar>, key: &str) -> Option<String> {
    match values.get(key) {
        Some(Scalar::Text(text)) => Some(text.trim().to_string()),
        _ => None,
    }
}

impl PetkitSetup {
    async fn log_in(&mut self, ctx: &SetupContext, values: &BTreeMap<String, Scalar>) -> SetupStep {
        let region = text(values, "region").unwrap_or_default();
        let account = text(values, "account").unwrap_or_default();
        let timezone = text(values, "timezone").unwrap_or_else(|| config::DEFAULT_TIMEZONE.to_string());
        let offset = match values.get("utc_offset_minutes") {
            Some(Scalar::Int(minutes)) => i32::try_from(*minutes).ok(),
            Some(Scalar::Text(text)) => text.trim().parse().ok(),
            None => Some(0),
            _ => None,
        };
        let Some(sign_in) = values.get("sign_in").and_then(|value| match value {
            Scalar::Text(text) if !text.is_empty() => Some(text.clone()),
            _ => None,
        }) else {
            return failed("Enter the account's sign-in to go on.");
        };
        if !is_region(&region) {
            return failed("The region is your country as the Petkit app has it, such as DE, or china.");
        }
        if account.is_empty() || account.len() > MAX_ACCOUNT || account.chars().any(char::is_control) {
            return failed("Enter the account's e-mail address or phone number.");
        }
        if sign_in.len() > MAX_SECRET {
            return failed("That sign-in is longer than this plugin accepts.");
        }
        if !is_timezone(&timezone) {
            return failed("The time zone is an IANA name, such as Europe/Berlin.");
        }
        let Some(offset) = offset.filter(|minutes| (-MAX_OFFSET_MINUTES..=MAX_OFFSET_MINUTES).contains(minutes)) else {
            return failed("The offset is between -840 and 840 minutes.");
        };
        let mut settings = self.existing.clone();
        settings.remove("feeder_id");
        settings.remove("litter_id");
        settings.insert("region".to_string(), json!(region));
        settings.insert("timezone".to_string(), json!(timezone));
        settings.insert("utc_offset_minutes".to_string(), json!(offset));
        // A new instance starts with commands off. One set up again keeps its choice.
        let commands = settings.get("commands").and_then(Value::as_bool).unwrap_or(false);
        settings.insert("commands".to_string(), json!(commands));
        let petkit: PetkitConfig = match config::parse(&Value::Object(settings.clone())) {
            Ok(petkit) => petkit,
            Err(problem) => return failed(problem.to_string()),
        };
        let Some(grant) = ctx.grant else {
            return SetupStep::Failed { reason: LIVE_NEEDED.to_string(), retry: false };
        };
        let login = Secret::new(login_line(&account, &sign_in));
        let parsed = match parse_credentials(login.expose()) {
            Ok(parsed) => parsed,
            Err(reason) => return failed(reason),
        };
        let driver = match PetkitDriver::new(grant, &petkit, parsed, ctx.timeouts) {
            Ok(driver) => Arc::new(driver),
            Err(reason) => return failed(reason),
        };
        let found = match driver.devices_on_account().await {
            Ok(found) => found,
            Err(error) => return failed(error.to_string()),
        };
        if found.is_empty() {
            return failed(
                "No Petkit D4 feeder or T4 litter box is on this account. Share them to it in the Petkit app first.",
            );
        }
        let step = choose(&found);
        self.stage = Stage::Devices { driver, settings, login, found };
        step
    }

    async fn finish(&mut self, ids: &[String]) -> SetupStep {
        let Stage::Devices { driver, settings, login, found } = &self.stage else { return failed(NOT_FITTING) };
        let chosen: Vec<&(u64, String)> = found.iter().filter(|(id, _)| ids.contains(&id.to_string())).collect();
        let of_kind =
            |kind: &str| chosen.iter().filter(|(_, found)| found == kind).map(|(id, _)| *id).collect::<Vec<_>>();
        let (feeders, litters) = (of_kind(FEEDER_TYPE), of_kind(LITTER_TYPE));
        if chosen.is_empty() {
            return failed("Pick the feeder, the litter box, or both.");
        }
        if feeders.len() > 1 || litters.len() > 1 {
            return failed("Pick one feeder and one litter box at most.");
        }
        let pinned = Found { feeder: feeders.first().copied(), litter: litters.first().copied() };
        let mut config = settings.clone();
        for (key, id) in [("feeder_id", pinned.feeder), ("litter_id", pinned.litter)] {
            if let Some(id) = id {
                config.insert(key.to_string(), json!(id));
            }
        }
        let config = Value::Object(config);
        if let Err(problem) = config::parse(&config) {
            return failed(problem.to_string());
        }
        // Every flow ends with one read of each chosen device.
        driver.pin(pinned).await;
        let reads = [(FEEDER, pinned.feeder, "The feeder"), (LITTER, pinned.litter, "The litter box")];
        for (local_id, _, name) in reads.iter().filter(|(_, id, _)| id.is_some()) {
            if let Err(error) = driver.read_state(local_id).await {
                return failed(format!("{name} could not be read: {error}"));
            }
        }
        let added = match (pinned.feeder, pinned.litter) {
            (Some(_), Some(_)) => "The feeder and the litter box were read",
            (Some(_), None) => "The feeder was read",
            _ => "The litter box was read",
        };
        let result = SetupResult {
            config,
            secrets: vec![(LOGIN_SLOT.to_string(), login.clone())],
            adopt: Vec::new(),
            summary: format!(
                "Logged in to Petkit. {added}. Feeding and cleaning stay off until commands is turned on."
            ),
        };
        self.stage = Stage::Finished;
        SetupStep::Done(result)
    }
}

#[async_trait]
impl SetupFlow for PetkitSetup {
    async fn begin(&mut self, _ctx: &SetupContext, existing: Option<&Value>) -> SetupStep {
        self.existing = existing.and_then(Value::as_object).cloned().unwrap_or_default();
        self.stage = Stage::Disclaimer;
        disclaimer()
    }

    async fn advance(&mut self, ctx: &SetupContext, input: SetupInput) -> SetupStep {
        match (&self.stage, input) {
            (Stage::Disclaimer | Stage::Account, SetupInput::Back) => {
                self.stage = Stage::Disclaimer;
                disclaimer()
            }
            (Stage::Devices { .. }, SetupInput::Back) => {
                self.stage = Stage::Account;
                account_form(&self.existing)
            }
            (Stage::Disclaimer, SetupInput::Values { values }) => {
                if values.get("accept") == Some(&Scalar::Bool(true)) {
                    self.stage = Stage::Account;
                    account_form(&self.existing)
                } else {
                    failed("The setup goes on only once the notice is accepted.")
                }
            }
            (Stage::Account, SetupInput::Values { values }) => self.log_in(ctx, &values).await,
            (Stage::Devices { .. }, SetupInput::Chosen { ids }) => self.finish(&ids).await,
            (Stage::Finished, _) => SetupStep::Failed { reason: "This setup has finished.".to_string(), retry: false },
            _ => failed(NOT_FITTING),
        }
    }
}
