//! The guided setup: the disclaimer, then the account, then the units to add.
//!
//! It replaces the old deploy step and its helper script: it signs in once, lists the units
//! on the account, reads each chosen unit once, and hands the hub the config and the
//! stored sign-in. The sign-in secret the person types is only ever held as a
//! [`Secret`], goes to Toshiba's cloud and into the secret slot, and is never shown.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use swifty_plugin_api::{
    Choice, Field, FieldType, Scalar, Secret, SetupContext, SetupFlow, SetupInput, SetupResult, SetupStep,
};

use crate::config::{self, AirconConfig, ToshibaConfig, MAX_AIRCONS};
use crate::driver::{Cloud, Unit};
use crate::protocol::{encode_credentials, ListedUnit};
use crate::{default_name, LOGIN_SLOT, SECRET_LABEL};

/// What the person is told before anything else. Also in the README.
pub const DISCLAIMER: &str = "This is an unofficial integration. Toshiba does not offer its cloud to other \
    software, so this plugin speaks the protocol of Toshiba's own app, as the open-source toshiba-ac library \
    does. It may stop working whenever Toshiba changes its cloud, without warning, and Toshiba may not \
    allow this use. Your account's sign-in is sent only to Toshiba's cloud and stored on this hub.";

const LIVE_NEEDED: &str =
    "Signing in to Toshiba's cloud needs a hub that is live. Set live = true in swifty.toml, then set this up again.";
const NOT_FITTING: &str = "That answer does not fit this step.";
const MAX_ACCOUNT: usize = 254;
const MAX_SECRET: usize = 256;

enum Stage {
    Disclaimer,
    Account,
    Units { cloud: Arc<Cloud>, login: Secret, found: Vec<ListedUnit> },
    Finished,
}

/// Toshiba's setup flow.
pub struct ToshibaSetup {
    stage: Stage,
    /// The stored config when an instance is set up again. Its URLs and client identity
    /// are kept, so an install keeps the client id the cloud knows.
    existing: Map<String, Value>,
}

impl ToshibaSetup {
    pub fn new() -> Self {
        Self { stage: Stage::Disclaimer, existing: Map::new() }
    }

    /// Everything but the units: from the stored config where it has a value, and the
    /// defaults elsewhere.
    fn settings(&self) -> ToshibaConfig {
        let mut base = self.existing.clone();
        base.insert("units".to_string(), json!([]));
        serde_json::from_value(Value::Object(base))
            .or_else(|_| serde_json::from_value(json!({ "units": [] })))
            .expect("the defaults make a config")
    }
}

impl Default for ToshibaSetup {
    fn default() -> Self {
        Self::new()
    }
}

fn disclaimer() -> SetupStep {
    SetupStep::Form {
        title: "Toshiba air conditioners: before you start".to_string(),
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

fn account_form() -> SetupStep {
    SetupStep::Form {
        title: "Your Toshiba account".to_string(),
        text: "The account you use in Toshiba's Home AC Control app. The hub signs in to list the air \
               conditioners on it."
            .to_string(),
        fields: vec![
            Field {
                key: "account".to_string(),
                label: "E-mail".to_string(),
                help: "The e-mail address you sign in to the app with.".to_string(),
                kind: FieldType::Text { max_len: MAX_ACCOUNT },
                required: true,
                value: None,
            },
            Field {
                key: "sign_in".to_string(),
                label: SECRET_LABEL.to_string(),
                help: "The one you sign in to the app with. It is stored on this hub and sent only to Toshiba."
                    .to_string(),
                kind: FieldType::Secret,
                required: true,
                value: None,
            },
        ],
    }
}

fn choose(found: &[ListedUnit]) -> SetupStep {
    let several = found.len() > 1;
    let options = found
        .iter()
        .enumerate()
        .map(|(at, unit)| Choice {
            id: unit.id.clone(),
            label: unit.name.clone().unwrap_or_else(|| default_name(at, several)),
            detail: format!("Toshiba unit {}", unit.id),
            selected: at < MAX_AIRCONS,
        })
        .collect();
    SetupStep::Choose {
        title: "Air conditioners".to_string(),
        text: format!("Pick the air conditioners to add, up to {MAX_AIRCONS}."),
        options,
        multiple: true,
        min: 1,
    }
}

fn failed(reason: impl Into<String>) -> SetupStep {
    SetupStep::Failed { reason: reason.into(), retry: true }
}

fn text(values: &std::collections::BTreeMap<String, Scalar>, key: &str) -> Option<String> {
    match values.get(key) {
        Some(Scalar::Text(text)) => Some(text.clone()),
        _ => None,
    }
}

/// The config of a finished setup: what the stored config had, the chosen units, and the
/// client suffix written out. The suffix names this hub at the cloud, so a later release
/// with another default must not change it. The user agent and the URLs stay at their
/// defaults unless they were set, so that a release can move them on.
fn config_json(existing: &Map<String, Value>, settings: &ToshibaConfig, units: &[AirconConfig]) -> Value {
    let rows: Vec<Value> = units
        .iter()
        .map(|unit| {
            let mut row = json!({ "id": unit.id, "unique_id": unit.unique_id });
            if let Some(name) = &unit.name {
                row["name"] = json!(name);
            }
            row
        })
        .collect();
    let mut config = existing.clone();
    config.insert("client_suffix".to_string(), json!(settings.client_suffix));
    config.insert("units".to_string(), Value::Array(rows));
    Value::Object(config)
}

impl ToshibaSetup {
    async fn sign_in(&mut self, ctx: &SetupContext, values: &std::collections::BTreeMap<String, Scalar>) -> SetupStep {
        let account = text(values, "account").map(|account| account.trim().to_string()).unwrap_or_default();
        let Some(sign_in) = text(values, "sign_in").filter(|secret| !secret.is_empty()) else {
            return failed("Enter the account's sign-in to go on.");
        };
        if account.is_empty() || account.len() > MAX_ACCOUNT || account.chars().any(char::is_control) {
            return failed("Enter the e-mail address of the account.");
        }
        if sign_in.len() > MAX_SECRET {
            return failed("That sign-in is longer than this plugin accepts.");
        }
        let Some(grant) = ctx.grant else {
            return SetupStep::Failed { reason: LIVE_NEEDED.to_string(), retry: false };
        };
        let login = Secret::new(encode_credentials(&account, &sign_in));
        let cloud = match Cloud::new(grant, &self.settings(), login.expose(), ctx.timeouts) {
            Ok(cloud) => Arc::new(cloud),
            Err(reason) => return failed(reason),
        };
        let found = match cloud.units_on_account().await {
            Ok(found) => found,
            Err(error) => return failed(error.to_string()),
        };
        if found.is_empty() {
            return failed("The account lists no air conditioners that this plugin can use.");
        }
        let step = choose(&found);
        self.stage = Stage::Units { cloud, login, found };
        step
    }

    async fn finish(&mut self, ids: &[String]) -> SetupStep {
        let Stage::Units { cloud, login, found } = &self.stage else { return failed(NOT_FITTING) };
        let several = found.len() > 1;
        let chosen: Vec<(usize, &ListedUnit)> = found
            .iter()
            .enumerate()
            .filter(|(_, unit)| ids.iter().any(|id| id.eq_ignore_ascii_case(&unit.id)))
            .collect();
        if chosen.is_empty() {
            return failed("Pick at least one air conditioner.");
        }
        if chosen.len() > MAX_AIRCONS {
            return failed(format!("Pick at most {MAX_AIRCONS} air conditioners."));
        }
        let units: Vec<AirconConfig> = chosen
            .iter()
            .map(|(_, unit)| AirconConfig {
                id: unit.id.clone(),
                unique_id: unit.unique_id.clone(),
                name: unit.name.clone(),
            })
            .collect();
        let settings = self.settings();
        let config = config_json(&self.existing, &settings, &units);
        if let Err(problem) = config::parse(&config) {
            return failed(problem.to_string());
        }
        // Every flow ends with one read of each chosen device.
        for ((at, listed), unit) in chosen.iter().zip(&units) {
            if let Err(error) = Unit::new(unit).reading(cloud).await {
                let name = listed.name.clone().unwrap_or_else(|| default_name(*at, several));
                return failed(format!("{name} could not be read: {error}"));
            }
        }
        let count = units.len();
        let result = SetupResult {
            config,
            secrets: vec![(LOGIN_SLOT.to_string(), login.clone())],
            adopt: Vec::new(),
            summary: if count == 1 {
                "Signed in to Toshiba's cloud. 1 air conditioner added.".to_string()
            } else {
                format!("Signed in to Toshiba's cloud. {count} air conditioners added.")
            },
        };
        self.stage = Stage::Finished;
        SetupStep::Done(result)
    }
}

#[async_trait]
impl SetupFlow for ToshibaSetup {
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
            (Stage::Units { .. }, SetupInput::Back) => {
                self.stage = Stage::Account;
                account_form()
            }
            (Stage::Disclaimer, SetupInput::Values { values }) => {
                if values.get("accept") == Some(&Scalar::Bool(true)) {
                    self.stage = Stage::Account;
                    account_form()
                } else {
                    failed("The setup goes on only once the notice is accepted.")
                }
            }
            (Stage::Account, SetupInput::Values { values }) => self.sign_in(ctx, &values).await,
            (Stage::Units { .. }, SetupInput::Chosen { ids }) => self.finish(&ids).await,
            (Stage::Finished, _) => SetupStep::Failed { reason: "This setup has finished.".to_string(), retry: false },
            _ => failed(NOT_FITTING),
        }
    }
}
