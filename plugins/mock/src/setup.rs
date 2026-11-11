//! The setup flow: Choose the demo devices, then an optional access code, then
//! Done. It reaches nothing, so it works the same in dry-run.

use std::collections::BTreeSet;

use async_trait::async_trait;
use serde_json::Value;
use swifty_plugin_api::{
    Choice, Field, FieldType, Scalar, Secret, SetupContext, SetupFlow, SetupInput, SetupResult, SetupStep,
};

use crate::config::{self, DeviceRow, MockConfig};
use crate::house::{demo_rows, DEMO_DEVICES};
use crate::ACCESS_CODE;

/// The longest access code the form takes.
const MAX_CODE: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Stage {
    #[default]
    Choose,
    Code,
    Finished,
}

/// The mock plugin's setup flow.
#[derive(Default)]
pub struct MockSetup {
    stage: Stage,
    /// Picked on the first step, or found in the config being set up again.
    chosen: BTreeSet<String>,
    /// The parts of an existing config this flow does not ask about.
    kept: Option<MockConfig>,
    code_stored: bool,
}

impl MockSetup {
    fn choose(&self) -> SetupStep {
        SetupStep::Choose {
            title: "Demo devices".to_string(),
            text: "Pick the devices to add. They live in the hub's memory, so nothing on your network is contacted."
                .to_string(),
            options: DEMO_DEVICES
                .iter()
                .map(|(id, class, name)| Choice {
                    id: id.to_string(),
                    label: name.to_string(),
                    detail: serde_json::to_value(class)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default(),
                    selected: self.chosen.is_empty() || self.chosen.contains(*id),
                })
                .collect(),
            multiple: true,
            min: 1,
        }
    }

    fn code(&self) -> SetupStep {
        let keep = if self.code_stored { " Leave it empty to keep the one that is stored." } else { "" };
        SetupStep::Form {
            title: "Access code".to_string(),
            text: format!(
                "Optional. The mock stores it like any plugin's credential and never shows it, so you can see how secrets are kept.{keep}"
            ),
            fields: vec![Field {
                key: ACCESS_CODE.to_string(),
                label: "Access code".to_string(),
                help: "Any text. Leave it empty to skip.".to_string(),
                kind: FieldType::Secret,
                required: false,
                value: None,
            }],
        }
    }

    fn current(&self) -> SetupStep {
        match self.stage {
            Stage::Choose => self.choose(),
            Stage::Code => self.code(),
            Stage::Finished => {
                SetupStep::Failed { reason: "This setup has already finished.".to_string(), retry: false }
            }
        }
    }

    fn finish(&mut self, code: Option<String>) -> SetupStep {
        let mut config =
            self.kept.clone().unwrap_or(MockConfig { devices: Vec::new(), disabled: None, require_code: false });
        let demo: Vec<DeviceRow> = demo_rows().into_iter().filter(|row| self.chosen.contains(&row.id)).collect();
        let added = demo.len();
        config.devices.retain(|row| !DEMO_DEVICES.iter().any(|(id, _, _)| *id == row.id));
        config.devices.extend(demo);
        let config = match serde_json::to_value(&config) {
            Ok(value) => value,
            Err(_) => return SetupStep::Failed { reason: "The config could not be written.".to_string(), retry: true },
        };
        if let Err(problem) = config::parse(&config) {
            return SetupStep::Failed { reason: format!("The config does not validate: {problem}"), retry: true };
        }
        self.stage = Stage::Finished;
        let secrets: Vec<(String, Secret)> =
            code.into_iter().map(|code| (ACCESS_CODE.to_string(), code.into())).collect();
        let noun = if added == 1 { "device" } else { "devices" };
        SetupStep::Done(SetupResult {
            config,
            secrets,
            adopt: Vec::new(),
            summary: format!("{added} demo {noun} added."),
        })
    }
}

#[async_trait]
impl SetupFlow for MockSetup {
    async fn begin(&mut self, ctx: &SetupContext, existing: Option<&Value>) -> SetupStep {
        self.stage = Stage::Choose;
        self.code_stored = ctx.secrets_present.contains(ACCESS_CODE);
        self.kept = existing.and_then(|value| config::parse(value).ok());
        self.chosen = self
            .kept
            .iter()
            .flat_map(|config| config.devices.iter())
            .filter(|row| DEMO_DEVICES.iter().any(|(id, _, _)| *id == row.id))
            .map(|row| row.id.clone())
            .collect();
        self.choose()
    }

    async fn advance(&mut self, _ctx: &SetupContext, input: SetupInput) -> SetupStep {
        match (self.stage, input) {
            (Stage::Finished, _) => self.current(),
            (_, SetupInput::Poll) => self.current(),
            (Stage::Choose, SetupInput::Back) => self.choose(),
            (Stage::Code, SetupInput::Back) => {
                self.stage = Stage::Choose;
                self.choose()
            }
            (Stage::Choose, SetupInput::Chosen { ids }) => {
                let known = |id: &String| DEMO_DEVICES.iter().any(|(demo, _, _)| demo == id);
                if ids.is_empty() {
                    return SetupStep::Failed { reason: "Pick at least one device.".to_string(), retry: true };
                }
                if !ids.iter().all(known) {
                    return SetupStep::Failed {
                        reason: "That is not one of the demo devices.".to_string(),
                        retry: true,
                    };
                }
                self.chosen = ids.into_iter().collect();
                self.stage = Stage::Code;
                self.code()
            }
            (Stage::Code, SetupInput::Values { values }) => {
                let code = match values.get(ACCESS_CODE) {
                    None => None,
                    Some(Scalar::Text(text)) if text.is_empty() => None,
                    Some(Scalar::Text(text))
                        if text.chars().count() <= MAX_CODE && !text.chars().any(char::is_control) =>
                    {
                        Some(text.clone())
                    }
                    Some(_) => {
                        return SetupStep::Failed {
                            reason: format!("The access code must be text of at most {MAX_CODE} printable characters."),
                            retry: true,
                        }
                    }
                };
                if values.keys().any(|key| key != ACCESS_CODE) {
                    return SetupStep::Failed { reason: "The form has only the access code.".to_string(), retry: true };
                }
                self.finish(code)
            }
            _ => SetupStep::Failed { reason: "That answer does not fit this step.".to_string(), retry: true },
        }
    }
}
