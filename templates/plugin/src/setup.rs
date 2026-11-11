//! The guided setup. The web app and `swifty plugin add` draw the same steps.
//!
//! ```text
{{#cloud}}
//! Form (service address, access token) -> Form (the device) -> test read -> Done
{{/cloud}}
{{#local}}
//! Form (the device) -> test read -> Done
{{/local}}
//! ```
//!
//! Each run adds one device to the instance, or changes the one with the same id.
//! In a live hub the device is read once before the result is handed over, and a
//! device that does not answer stops the setup with the reason. In dry-run nothing
//! is contacted and the device is added untested.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde_json::Value;
use swifty_plugin_api::{
    DeviceClass, Driver, Field, FieldType, Scalar, {{#cloud}}Secret, {{/cloud}}SetupContext, SetupFlow, SetupInput, SetupResult, SetupStep,
};

use crate::config::{self, DeviceRow};
use crate::driver::{{Type}}Driver;
use crate::protocol;

const DOES_NOT_FIT: &str = "That answer does not fit this step.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Stage {
{{#cloud}}
    #[default]
    Account,
    Device,
{{/cloud}}
{{#local}}
    #[default]
    Device,
{{/local}}
}

/// The setup flow for one instance of the plugin.
#[derive(Default)]
pub struct {{Type}}Setup {
    stage: Stage,
    /// The devices the instance has already, when it is set up again.
    existing: Vec<DeviceRow>,
{{#cloud}}
    api_url: String,
    token: Option<Secret>,
{{/cloud}}
}

fn text(values: &BTreeMap<String, Scalar>, key: &str) -> Option<String> {
    match values.get(key) {
        Some(Scalar::Text(text)) if !text.trim().is_empty() => Some(text.trim().to_string()),
        _ => None,
    }
}

{{#local}}
fn port(values: &BTreeMap<String, Scalar>) -> Option<u16> {
    match values.get("port") {
        None => Some(config::DEFAULT_PORT),
        Some(Scalar::Int(port)) => u16::try_from(*port).ok(),
        Some(Scalar::Text(port)) => port.trim().parse().ok(),
        Some(_) => None,
    }
}

{{/local}}
fn field(key: &str, label: &str, help: &str, kind: FieldType, required: bool, value: Option<Scalar>) -> Field {
    Field { key: key.to_string(), label: label.to_string(), help: help.to_string(), kind, required, value }
}

impl {{Type}}Setup {
{{#cloud}}
    fn account_form(&self) -> SetupStep {
        SetupStep::Form {
            title: "Sign in to {{name}}".to_string(),
            text: "Swifty Home reaches these devices through the vendor's servers. The token is stored as a \
                   secret on this hub and never shown again."
                .to_string(),
            fields: vec![
                field(
                    "api_url",
                    "Service address",
                    "The address of the vendor's API.",
                    FieldType::Url,
                    true,
                    Some(Scalar::Text(self.api_url.clone())),
                ),
                field("token", "Access token", "Paste the token from the vendor.", FieldType::Secret, true, None),
            ],
        }
    }

{{/cloud}}
    fn device_form(&self) -> SetupStep {
        let classes = protocol::CLASSES.iter().map(|(word, _, _)| word.to_string()).collect();
        SetupStep::Form {
            title: "Add a device".to_string(),
{{#local}}
            text: "The device must be on the same network as this hub.".to_string(),
{{/local}}
{{#cloud}}
            text: "The device id is shown in the vendor's app.".to_string(),
{{/cloud}}
            fields: vec![
                field(
                    "id",
                    "Device id",
                    "1 to 64 lower-case letters, digits, - and _.",
                    FieldType::Text { max_len: config::MAX_ID },
                    true,
                    None,
                ),
{{#local}}
                field("address", "Address", "Its IP address, such as 192.0.2.20.", FieldType::Host, true, None),
                field(
                    "port",
                    "Port",
                    "The port it listens on.",
                    FieldType::Port,
                    false,
                    Some(Scalar::Int(i64::from(config::DEFAULT_PORT))),
                ),
{{/local}}
                field(
                    "class",
                    "Kind",
                    "What kind of device it is.",
                    FieldType::Choice { options: classes },
                    false,
                    Some(Scalar::Text(protocol::CLASSES[0].0.to_string())),
                ),
                field(
                    "name",
                    "Name",
                    "How it is shown. Leave it empty for a default name.",
                    FieldType::Text { max_len: config::MAX_NAME },
                    false,
                    None,
                ),
            ],
        }
    }

    fn first_step(&mut self) -> SetupStep {
        self.stage = Stage::default();
{{#cloud}}
        self.account_form()
{{/cloud}}
{{#local}}
        self.device_form()
{{/local}}
    }

{{#cloud}}
    fn take_account(&mut self, values: &BTreeMap<String, Scalar>) -> SetupStep {
        let api_url = text(values, "api_url").unwrap_or_else(|| config::DEFAULT_API_URL.to_string());
        let Some(token) = text(values, "token") else {
            return SetupStep::Failed { reason: "The access token is needed.".to_string(), retry: true };
        };
        self.api_url = api_url;
        self.token = Some(token.into());
        self.stage = Stage::Device;
        self.device_form()
    }

{{/cloud}}
    /// The instance config with this device in it, in place of one with the same id.
    fn config_with(&self, row: DeviceRow) -> config::Config {
        let mut devices: Vec<DeviceRow> = self.existing.iter().filter(|known| known.id != row.id).cloned().collect();
        devices.push(row);
{{#cloud}}
        config::Config { api_url: self.api_url.clone(), devices }
{{/cloud}}
{{#local}}
        config::Config { devices }
{{/local}}
    }

    async fn take_device(&mut self, ctx: &SetupContext, values: &BTreeMap<String, Scalar>) -> SetupStep {
        let fail = |reason: String| SetupStep::Failed { reason, retry: true };
        let Some(id) = text(values, "id") else { return fail("The device id is needed.".to_string()) };
{{#local}}
        let Some(address) = text(values, "address") else { return fail("The address is needed.".to_string()) };
        let Some(port) = port(values) else { return fail("The port must be a number from 1 to 65535.".to_string()) };
        let row = DeviceRow { id, address, port, class: text(values, "class"), name: text(values, "name") };
{{/local}}
{{#cloud}}
        let row = DeviceRow { id, class: text(values, "class"), name: text(values, "name") };
{{/cloud}}
        let config = self.config_with(row.clone());
        if let Err(problem) = config::check(&config) {
            return fail(format!("That does not work: {problem}."));
        }
        let class: DeviceClass = row.class();
        let name = row.name.clone().unwrap_or_else(|| protocol::default_name(class, config.devices.len() - 1));
        let tested = match ctx.grant {
            None => false,
            Some(grant) => {
{{#local}}
                let driver = {{Type}}Driver::new(grant, ctx.timeouts, &config);
{{/local}}
{{#cloud}}
                let Some(token) = self.token.clone() else { return self.first_step() };
                let driver = match {{Type}}Driver::new(grant, ctx.timeouts, &config, token) {
                    Ok(driver) => driver,
                    Err(error) => return fail(error.to_string()),
                };
{{/cloud}}
                if let Err(error) = driver.read_state(&row.id).await {
                    return fail(format!("{name} did not answer as expected: {error}."));
                }
                true
            }
        };
        let config: Value = match serde_json::to_value(&config) {
            Ok(config) => config,
            Err(_) => return fail("The configuration could not be written.".to_string()),
        };
        let summary = if tested {
            format!("Added {name}. It answered.")
        } else {
            format!("Added {name}. It was not contacted, because the hub is in dry-run.")
        };
{{#local}}
        SetupStep::Done(SetupResult { config, secrets: Vec::new(), adopt: Vec::new(), summary })
{{/local}}
{{#cloud}}
        SetupStep::Done(SetupResult {
            config,
            secrets: self.token.clone().map(|token| (config::TOKEN_SLOT.to_string(), token)).into_iter().collect(),
            adopt: Vec::new(),
            summary,
        })
{{/cloud}}
    }
}

#[async_trait]
impl SetupFlow for {{Type}}Setup {
    async fn begin(&mut self, _ctx: &SetupContext, existing: Option<&Value>) -> SetupStep {
        let existing = existing.and_then(|config| config::parse(config).ok());
{{#cloud}}
        self.api_url =
            existing.as_ref().map_or_else(|| config::DEFAULT_API_URL.to_string(), |config| config.api_url.clone());
{{/cloud}}
        self.existing = existing.map(|config| config.devices).unwrap_or_default();
        self.first_step()
    }

    async fn advance(&mut self, ctx: &SetupContext, input: SetupInput) -> SetupStep {
        match (self.stage, input) {
            (_, SetupInput::Back) => self.first_step(),
{{#cloud}}
            (Stage::Account, SetupInput::Values { values }) => self.take_account(&values),
{{/cloud}}
            (Stage::Device, SetupInput::Values { values }) => self.take_device(ctx, &values).await,
            _ => SetupStep::Failed { reason: DOES_NOT_FIT.to_string(), retry: true },
        }
    }
}
