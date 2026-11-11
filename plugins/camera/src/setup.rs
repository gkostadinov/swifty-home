//! The guided setup: one form (address, RTSP port, login), then one probe when
//! the hub is live, then done.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde_json::{Map, Value};
use swifty_plugin_api::{
    Driver, Field, FieldType, Scalar, SetupContext, SetupFlow, SetupInput, SetupResult, SetupStep,
};

use crate::config::{CameraConfig, DEFAULT_PORT};
use crate::driver::CameraDriver;
use crate::{DEVICE, LOGIN_SLOT};

/// The longest login the form accepts.
const MAX_LOGIN: usize = 512;

/// The setup flow of a camera.
#[derive(Debug, Default)]
pub struct CameraSetup {
    host: Option<String>,
    port: Option<u16>,
}

impl CameraSetup {
    /// A flow with an empty form.
    pub fn new() -> Self {
        Self::default()
    }

    fn form(&self, ctx: &SetupContext) -> SetupStep {
        let stored = ctx.secrets_present.contains(LOGIN_SLOT);
        let login_help = if stored {
            "The camera's login as user:secret. Leave it empty to keep the one already stored."
        } else {
            "The camera's login as user:secret. It is stored for viewing and never sent by the status probe."
        };
        SetupStep::Form {
            title: "Add an IP camera".to_string(),
            text: "The hub only checks that the camera answers in RTSP. It never sends the login to do so, \
                   and it never moves or changes the camera."
                .to_string(),
            fields: vec![
                Field {
                    key: "host".to_string(),
                    label: "Address".to_string(),
                    help: "The camera's IP address or host name, such as 192.0.2.30.".to_string(),
                    kind: FieldType::Host,
                    required: true,
                    value: self.host.clone().filter(|host| !host.is_empty()).map(Scalar::Text),
                },
                Field {
                    key: "port".to_string(),
                    label: "RTSP port".to_string(),
                    help: "Usually 554.".to_string(),
                    kind: FieldType::Port,
                    required: false,
                    value: Some(Scalar::Int(i64::from(self.port.unwrap_or(DEFAULT_PORT)))),
                },
                Field {
                    key: LOGIN_SLOT.to_string(),
                    label: "Login".to_string(),
                    help: login_help.to_string(),
                    kind: FieldType::Secret,
                    required: !stored,
                    value: None,
                },
            ],
        }
    }

    async fn submit(&mut self, ctx: &SetupContext, values: BTreeMap<String, Scalar>) -> SetupStep {
        let mut config = Map::new();
        let mut login = None;
        for (key, value) in values {
            match (key.as_str(), value) {
                ("host", Scalar::Text(host)) => {
                    let host = host.trim().to_string();
                    self.host = Some(host.clone());
                    if !host.is_empty() {
                        config.insert(key, Value::String(host));
                    }
                }
                ("port", Scalar::Int(port)) => match u16::try_from(port) {
                    Ok(port) if port > 0 => {
                        self.port = Some(port);
                        config.insert(key, Value::from(port));
                    }
                    _ => return failed("port must be 1 to 65535."),
                },
                ("port", Scalar::Text(port)) if port.trim().is_empty() => {}
                ("port", Scalar::Text(port)) => match port.trim().parse::<u16>() {
                    Ok(port) if port > 0 => {
                        self.port = Some(port);
                        config.insert(key, Value::from(port));
                    }
                    _ => return failed("port must be 1 to 65535."),
                },
                (LOGIN_SLOT, Scalar::Text(text)) => login = Some(text).filter(|text| !text.is_empty()),
                (key, _) => return failed(&format!("{key} is not a value this form takes.")),
            }
        }
        let config = match CameraConfig::parse(&Value::Object(config)) {
            Ok(config) => config,
            Err(problem) => return failed(&format!("{problem}.")),
        };
        if login.as_ref().is_some_and(|login| login.len() > MAX_LOGIN || login.chars().any(char::is_control)) {
            return failed("The login is too long or holds control characters.");
        }
        if login.is_none() && !ctx.secrets_present.contains(LOGIN_SLOT) {
            return failed("The camera's login is needed.");
        }

        // The probe never uses the login, so a stored one does not stop the test.
        let summary = match ctx.grant {
            None => "Saved. The hub is in dry-run, so the camera was not contacted.".to_string(),
            Some(grant) => match CameraDriver::new(grant, &config, ctx.timeouts).read_state(DEVICE).await {
                Ok(_) => "Connected. The camera answers in RTSP.".to_string(),
                Err(error) => return failed(&format!("The camera could not be reached: {error}.")),
            },
        };
        SetupStep::Done(SetupResult {
            config: config.to_value(),
            secrets: login.map(|login| (LOGIN_SLOT.to_string(), login.into())).into_iter().collect(),
            adopt: Vec::new(),
            summary,
        })
    }
}

fn failed(reason: &str) -> SetupStep {
    SetupStep::Failed { reason: reason.to_string(), retry: true }
}

#[async_trait]
impl SetupFlow for CameraSetup {
    async fn begin(&mut self, ctx: &SetupContext, existing: Option<&Value>) -> SetupStep {
        if let Some(existing) = existing.and_then(|config| CameraConfig::parse(config).ok()) {
            self.host = Some(existing.host);
            self.port = Some(existing.port);
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
