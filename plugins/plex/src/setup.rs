//! The guided setup: one form (server URL and token), then one test read when
//! the hub is live, then done.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde_json::{json, Value};
use swifty_plugin_api::{
    Driver, Field, FieldType, Scalar, SetupContext, SetupFlow, SetupInput, SetupResult, SetupStep,
};

use crate::config::PlexConfig;
use crate::driver::PlexDriver;
use crate::{DEVICE, TOKEN_SLOT};

/// The longest token the form accepts.
const MAX_TOKEN: usize = 1024;

/// The setup flow of a Plex server.
#[derive(Debug, Default)]
pub struct PlexSetup {
    /// The URL last entered or stored, to fill the form in again.
    url: Option<String>,
}

impl PlexSetup {
    /// A flow with an empty form.
    pub fn new() -> Self {
        Self::default()
    }

    fn form(&self, ctx: &SetupContext) -> SetupStep {
        let stored = ctx.secrets_present.contains(TOKEN_SLOT);
        let token_help = if stored {
            "The server's X-Plex-Token. Leave it empty to keep the one already stored."
        } else {
            "The server's X-Plex-Token, as Plex's own help explains how to find it. Sent only in a header."
        };
        SetupStep::Form {
            title: "Connect a Plex server".to_string(),
            text: "The hub only asks the server whether it is up and accepts the token. It never plays, \
                   changes or lists anything."
                .to_string(),
            fields: vec![
                Field {
                    key: "url".to_string(),
                    label: "Server URL".to_string(),
                    help: "Scheme, host and port only, such as http://192.0.2.20:32400.".to_string(),
                    kind: FieldType::Url,
                    required: true,
                    value: self.url.clone().filter(|url| !url.is_empty()).map(Scalar::Text),
                },
                Field {
                    key: TOKEN_SLOT.to_string(),
                    label: "Token".to_string(),
                    help: token_help.to_string(),
                    kind: FieldType::Secret,
                    required: !stored,
                    value: None,
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
        self.url = text.get("url").cloned();
        let url = text.get("url").filter(|url| !url.is_empty());
        let config = match PlexConfig::parse(&url.map(|url| json!({ "url": url })).unwrap_or_else(|| json!({}))) {
            Ok(config) => config,
            Err(problem) => return failed(&format!("{problem}.")),
        };

        let token = text.get(TOKEN_SLOT).filter(|token| !token.is_empty()).cloned();
        if token.as_ref().is_some_and(|token| token.len() > MAX_TOKEN || token.chars().any(char::is_control)) {
            return failed("The token is too long or holds control characters.");
        }
        if token.is_none() && !ctx.secrets_present.contains(TOKEN_SLOT) {
            return failed("The token is needed.");
        }

        let summary = match (ctx.grant, &token) {
            (None, _) => "Saved. The hub is in dry-run, so the server was not contacted.".to_string(),
            (Some(_), None) => {
                "Saved with the token already stored. The server is read when the instance loads.".into()
            }
            (Some(grant), Some(token)) => {
                let driver = match PlexDriver::new(grant, &config, &token.clone().into(), ctx.timeouts) {
                    Ok(driver) => driver,
                    Err(reason) => return failed(&format!("The server cannot be used: {reason}.")),
                };
                match driver.read_state(DEVICE).await {
                    Ok(_) => "Connected. The server is up and accepts the token.".to_string(),
                    Err(error) => return failed(&format!("The server could not be read: {error}.")),
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
impl SetupFlow for PlexSetup {
    async fn begin(&mut self, ctx: &SetupContext, existing: Option<&Value>) -> SetupStep {
        if let Some(url) = existing.and_then(|config| config.get("url")).and_then(Value::as_str) {
            self.url = Some(url.to_string());
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
