//! The guided setup: Moonraker's address and, when it asks for one, its API key,
//! then one read of the printer. It replaces the hand-written `[devices.moonraker]`.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde_json::Value;
use swifty_plugin_api::{
    Field, FieldType, Scalar, Secret, SetupContext, SetupFlow, SetupInput, SetupResult, SetupStep, State,
};

use crate::config::{self, MoonrakerConfig, KEY_SLOT};
use crate::driver::MoonrakerDriver;
use crate::protocol;

const LIVE_NEEDED: &str = "Setting up the printer reads it once, which needs live mode. Set live = true and start \
     the setup again.";

/// Which step the person is answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Form,
    /// The read failed; going back shows the form again.
    Failed,
    Done,
}

/// The setup flow of one instance.
pub struct MoonrakerSetup {
    stage: Stage,
    url: Option<String>,
    problem: Option<String>,
}

impl MoonrakerSetup {
    /// A flow that has not begun.
    pub fn new() -> Self {
        Self { stage: Stage::Form, url: None, problem: None }
    }

    fn form(&mut self, ctx: &SetupContext) -> SetupStep {
        self.stage = Stage::Form;
        let kept = ctx.secrets_present.contains(KEY_SLOT);
        let key_help = if kept {
            "Only when Moonraker asks for one. Leave it empty to keep the stored one."
        } else {
            "Only when Moonraker asks for one; leave it empty otherwise."
        };
        let intro = "Where Moonraker answers: http:// and the printer's address with port 7125.";
        let text = match self.problem.take() {
            Some(problem) => format!("{problem} {intro}"),
            None => intro.to_string(),
        };
        SetupStep::Form {
            title: "Connect to the printer".to_string(),
            text,
            fields: vec![
                Field {
                    key: "url".to_string(),
                    label: "Moonraker address".to_string(),
                    help: "The scheme, the address and the port, and nothing after it.".to_string(),
                    kind: FieldType::Url,
                    required: true,
                    value: self.url.clone().map(Scalar::Text),
                },
                Field {
                    key: KEY_SLOT.to_string(),
                    label: "API key".to_string(),
                    help: key_help.to_string(),
                    kind: FieldType::Secret,
                    required: false,
                    value: None,
                },
            ],
        }
    }

    fn fail(&mut self, reason: String, retry: bool) -> SetupStep {
        self.stage = if retry { Stage::Failed } else { Stage::Done };
        SetupStep::Failed { reason, retry }
    }

    async fn submit(&mut self, ctx: &SetupContext, values: &BTreeMap<String, Scalar>) -> SetupStep {
        let text = |key: &str| match values.get(key) {
            Some(Scalar::Text(text)) => text.trim().to_string(),
            _ => String::new(),
        };
        let url = text("url");
        let key = Some(text(KEY_SLOT)).filter(|key| !key.is_empty());
        self.url = Some(url.clone());
        let config = MoonrakerConfig { url };
        let document: Value = match serde_json::to_value(&config) {
            Ok(document) => document,
            Err(_) => return self.fail("The settings could not be stored.".to_string(), true),
        };
        if let Err(problem) = config::parse(&document) {
            self.problem = Some(format!("{}: {}.", problem.field, problem.message));
            return self.form(ctx);
        }
        if key.is_none() && ctx.secrets_present.contains(KEY_SLOT) {
            self.stage = Stage::Done;
            return SetupStep::Done(SetupResult {
                config: document,
                secrets: Vec::new(),
                adopt: Vec::new(),
                summary: "Saved. The stored API key was kept, so the printer was not asked.".to_string(),
            });
        }
        let Some(grant) = ctx.grant else {
            return self.fail(LIVE_NEEDED.to_string(), false);
        };
        let driver = match MoonrakerDriver::new(grant, &config, key.as_deref(), ctx.timeouts) {
            Ok(driver) => driver,
            Err(why) => {
                self.problem = Some(format!("{why}."));
                return self.form(ctx);
            }
        };
        let status = match driver.status().await {
            Ok(status) => status,
            Err(error) => return self.fail(format!("The printer could not be read: {error}."), true),
        };
        let State::Printer { state, .. } = protocol::printer_state(&status) else {
            return self.fail("The printer could not be read.".to_string(), true);
        };
        let stored: Vec<(String, Secret)> = key.map(|key| (KEY_SLOT.to_string(), key.into())).into_iter().collect();
        self.stage = Stage::Done;
        SetupStep::Done(SetupResult {
            config: document,
            secrets: stored,
            adopt: Vec::new(),
            summary: format!("Moonraker answered: the printer is {state}."),
        })
    }
}

impl Default for MoonrakerSetup {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SetupFlow for MoonrakerSetup {
    async fn begin(&mut self, ctx: &SetupContext, existing: Option<&Value>) -> SetupStep {
        self.url = existing.and_then(|config| config::parse(config).ok()).map(|config| config.url);
        self.form(ctx)
    }

    async fn advance(&mut self, ctx: &SetupContext, input: SetupInput) -> SetupStep {
        match (self.stage, input) {
            (Stage::Done, _) => SetupStep::Failed { reason: "This setup has finished.".to_string(), retry: false },
            (Stage::Form, SetupInput::Values { values }) => self.submit(ctx, &values).await,
            (Stage::Form, SetupInput::Back | SetupInput::Poll) | (Stage::Failed, SetupInput::Back) => self.form(ctx),
            (Stage::Failed, _) => self.fail("Go back to try again.".to_string(), true),
            (Stage::Form, SetupInput::Chosen { .. }) => {
                self.fail("That answer does not fit this step.".to_string(), true)
            }
        }
    }
}
