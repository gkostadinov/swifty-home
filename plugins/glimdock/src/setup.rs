//! The guided setup: one form (node URL, snapshot path, token, pairing path),
//! then one test read of the snapshot when the hub is live, then done.
//!
//! The pairing endpoint is never called here: pairing starts something on the
//! node, and that is an action the owner asks for on its own.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use swifty_plugin_api::{
    Driver, Field, FieldType, Scalar, SetupContext, SetupFlow, SetupInput, SetupResult, SetupStep, State,
};

use crate::config::{GlimdockConfig, MAX_PATH};
use crate::driver::GlimdockDriver;
use crate::{DEVICE, TOKEN_SLOT};

/// The keys of the form that end up in the config, in order.
const CONFIG_KEYS: [&str; 3] = ["url", "snapshot_path", "pairing_path"];

/// The longest token the form accepts.
const MAX_TOKEN: usize = 4096;

/// The setup flow of a Glimdock node.
#[derive(Debug, Default)]
pub struct GlimdockSetup {
    /// What was last entered or stored, to fill the form in again. Never the token.
    draft: BTreeMap<String, String>,
}

impl GlimdockSetup {
    /// A flow with an empty form.
    pub fn new() -> Self {
        Self::default()
    }

    fn form(&self, ctx: &SetupContext) -> SetupStep {
        let prefill = |key: &str| self.draft.get(key).filter(|v| !v.is_empty()).map(|v| Scalar::Text(v.clone()));
        let stored = ctx.secrets_present.contains(TOKEN_SLOT);
        let token_help = if stored {
            "The node's reader token. Leave it empty to keep the one already stored."
        } else {
            "The node's reader token. It is sent only as a bearer token in a header, never in a URL."
        };
        SetupStep::Form {
            title: "Connect a Glimdock node".to_string(),
            text: "The hub reads the node's telemetry snapshot over HTTP, and can ask the node to pair one of \
                   its agents. It never sends the node anything else."
                .to_string(),
            fields: vec![
                Field {
                    key: "url".to_string(),
                    label: "Node URL".to_string(),
                    help: "Scheme, host and port only, such as http://192.0.2.10:8080.".to_string(),
                    kind: FieldType::Url,
                    required: true,
                    value: prefill("url"),
                },
                Field {
                    key: "snapshot_path".to_string(),
                    label: "Snapshot path".to_string(),
                    help: "The path of the node's snapshot endpoint, such as /api/snapshot.".to_string(),
                    kind: FieldType::Text { max_len: MAX_PATH },
                    required: true,
                    value: prefill("snapshot_path"),
                },
                Field {
                    key: TOKEN_SLOT.to_string(),
                    label: "Access token".to_string(),
                    help: token_help.to_string(),
                    kind: FieldType::Secret,
                    required: !stored,
                    value: None,
                },
                Field {
                    key: "pairing_path".to_string(),
                    label: "Agent pairing path".to_string(),
                    help: "The path of the node's agent pairing endpoint. Leave it empty if this node does not \
                           pair agents."
                        .to_string(),
                    kind: FieldType::Text { max_len: MAX_PATH },
                    required: false,
                    value: prefill("pairing_path"),
                },
            ],
        }
    }

    async fn submit(&mut self, ctx: &SetupContext, values: BTreeMap<String, Scalar>) -> SetupStep {
        let mut text = BTreeMap::new();
        for (key, value) in &values {
            match value {
                Scalar::Text(value) => {
                    text.insert(key.as_str(), value.trim().to_string());
                }
                _ => return failed(&format!("{key} must be text.")),
            }
        }
        for key in CONFIG_KEYS {
            self.draft.insert(key.to_string(), text.get(key).cloned().unwrap_or_default());
        }

        let mut config = Map::new();
        for key in CONFIG_KEYS {
            if let Some(value) = text.get(key).filter(|value| !value.is_empty()) {
                config.insert(key.to_string(), json!(value));
            }
        }
        let config = match GlimdockConfig::parse(&Value::Object(config)) {
            Ok(config) => config,
            Err(problem) => return failed(&format!("{problem}.")),
        };

        let token = text.get(TOKEN_SLOT).filter(|token| !token.is_empty()).cloned();
        if token.as_ref().is_some_and(|token| token.len() > MAX_TOKEN || token.chars().any(char::is_control)) {
            return failed("The access token is too long or holds control characters.");
        }
        if token.is_none() && !ctx.secrets_present.contains(TOKEN_SLOT) {
            return failed("The access token is needed.");
        }

        let summary = match (ctx.grant, &token) {
            (None, _) => "Saved. The hub is in dry-run, so the node was not contacted.".to_string(),
            (Some(_), None) => "Saved with the token already stored. The node is read when the instance loads.".into(),
            (Some(grant), Some(token)) => {
                let driver = match GlimdockDriver::new(grant, &config, &token.clone().into(), ctx.timeouts) {
                    Ok(driver) => driver,
                    Err(reason) => return failed(&format!("The node cannot be used: {reason}.")),
                };
                match driver.read_state(DEVICE).await {
                    Ok(State::Telemetry { .. }) => "Connected. The node's snapshot was read.".to_string(),
                    Ok(_) => return failed("The node answered with something that is not a snapshot."),
                    Err(error) => return failed(&format!("The node could not be read: {error}.")),
                }
            }
        };
        SetupStep::Done(SetupResult {
            config: config.to_value(),
            secrets: token.map(|token| (TOKEN_SLOT.to_string(), token.into())).into_iter().collect(),
            adopt: Vec::new(),
            summary,
        })
    }
}

fn failed(reason: &str) -> SetupStep {
    SetupStep::Failed { reason: reason.to_string(), retry: true }
}

#[async_trait]
impl SetupFlow for GlimdockSetup {
    async fn begin(&mut self, ctx: &SetupContext, existing: Option<&Value>) -> SetupStep {
        if let Some(existing) = existing.and_then(Value::as_object) {
            for key in CONFIG_KEYS {
                if let Some(value) = existing.get(key).and_then(Value::as_str) {
                    self.draft.insert(key.to_string(), value.to_string());
                }
            }
        }
        self.form(ctx)
    }

    async fn advance(&mut self, ctx: &SetupContext, input: SetupInput) -> SetupStep {
        match input {
            SetupInput::Values { values } => self.submit(ctx, values).await,
            SetupInput::Back => self.form(ctx),
            _ => failed("This step needs the form's values."),
        }
    }
}
