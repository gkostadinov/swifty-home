//! The guided setup: the bridge's address, then a wait while the person presses
//! the link button (each poll asks the bridge for a key once), then a choice of
//! the bridge's lights, then one reading of every chosen light.
//!
//! The flow does not count its polls or watch a clock: the hub ends a wait after
//! the `timeout_s` the step announced, and a Try again after that is polled like
//! the first wait.
//!
//! The key goes into the result's secrets and nowhere else. An instance set up
//! again that already holds a key keeps it unless the person asks to pair again.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use swifty_plugin_api::{
    Choice, DeviceRecord, Directory, Field, FieldType, Scalar, Secret, SetupContext, SetupFlow, SetupInput,
    SetupResult, SetupStep,
};

use crate::config::{self, APP_KEY};
use crate::driver::{pair, FoundLight, HueDriver, PairError};

/// How often the hub asks during the wait, and for how long. The hub ends the wait.
pub const POLL_MS: u32 = 2_000;
/// The bridge accepts pairing for 30 seconds after the press; this leaves time to walk there.
pub const TIMEOUT_S: u32 = 90;

/// The Hue setup flow. One per setup session.
#[derive(Default)]
pub struct HueSetup {
    url: String,
    key: Option<Secret>,
    lights: Vec<FoundLight>,
}

/// A directory with nothing in it: setup reads lights by number, not by adoption.
struct Nothing;

impl Directory for Nothing {
    fn get(&self, _local_id: &str) -> Option<DeviceRecord> {
        None
    }

    fn all(&self) -> Vec<DeviceRecord> {
        Vec::new()
    }
}

fn failed(reason: impl Into<String>) -> SetupStep {
    SetupStep::Failed { reason: reason.into(), retry: true }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

impl HueSetup {
    fn form(&self, secrets_present: &BTreeSet<String>) -> SetupStep {
        let mut fields = vec![Field {
            key: "url".to_string(),
            label: "Bridge address".to_string(),
            help: "The bridge's address on your network, such as http://192.0.2.10. The Hue app shows it under \
                   the bridge's settings."
                .to_string(),
            kind: FieldType::Url,
            required: true,
            value: (!self.url.is_empty()).then(|| Scalar::Text(self.url.clone())),
        }];
        if secrets_present.contains(APP_KEY) {
            fields.push(Field {
                key: "pair_again".to_string(),
                label: "Pair again".to_string(),
                help: "This instance already has a key from the bridge, and keeps it unless you tick this.".to_string(),
                kind: FieldType::Bool,
                required: false,
                value: Some(Scalar::Bool(false)),
            });
        }
        SetupStep::Form {
            title: "Philips Hue Bridge".to_string(),
            text: "Swifty Home talks to the bridge on your network. It never uses the Hue cloud.".to_string(),
            fields,
        }
    }

    fn wait() -> SetupStep {
        SetupStep::Wait {
            title: "Press the link button".to_string(),
            text: "Press the round button on top of the bridge. Swifty Home asks the bridge for a key every few \
                   seconds and goes on by itself once the bridge agrees."
                .to_string(),
            poll_ms: POLL_MS,
            timeout_s: TIMEOUT_S,
        }
    }

    fn driver(&self, ctx: &SetupContext) -> Result<HueDriver, String> {
        let (Some(grant), Some(key)) = (ctx.grant, self.key.clone()) else {
            return Err("Pairing needs live mode.".to_string());
        };
        HueDriver::new(grant, &self.url, key, ctx.timeouts, Arc::new(Nothing))
    }

    fn submitted(&mut self, ctx: &SetupContext, values: &BTreeMap<String, Scalar>) -> SetupStep {
        self.url = match values.get("url") {
            Some(Scalar::Text(url)) => url.trim().to_string(),
            _ => String::new(),
        };
        let config = json!({ "url": self.url });
        if let Err(problem) = config::parse(&config) {
            return failed(format!("That is not a bridge address: {problem}"));
        }
        let pair_again = matches!(values.get("pair_again"), Some(Scalar::Bool(true)));
        if ctx.secrets_present.contains(APP_KEY) && !pair_again {
            return SetupStep::Done(SetupResult {
                config,
                secrets: Vec::new(),
                adopt: Vec::new(),
                summary: "The bridge address is saved, and its key is kept.".to_string(),
            });
        }
        if ctx.grant.is_none() {
            return SetupStep::Failed {
                reason: "Pairing the bridge needs live mode: in dry-run Swifty Home sends nothing. Set live = true \
                         and start the setup again."
                    .to_string(),
                retry: false,
            };
        }
        self.key = None;
        self.lights.clear();
        Self::wait()
    }

    /// Asks for a key once, then lists the lights once there is one.
    async fn polled(&mut self, ctx: &SetupContext) -> SetupStep {
        let Some(grant) = ctx.grant else { return failed("Pairing the bridge needs live mode.") };
        if self.url.is_empty() {
            return failed("Start with the bridge's address.");
        }
        if self.key.is_none() {
            match pair(grant, &self.url, ctx.timeouts).await {
                Ok(key) => self.key = Some(key),
                Err(PairError::LinkButtonNotPressed) => return Self::wait(),
                Err(PairError::Failed(error)) => return failed(format!("The bridge could not be paired: {error}")),
            }
        }
        let driver = match self.driver(ctx) {
            Ok(driver) => driver,
            Err(reason) => return failed(format!("The bridge gave a key that cannot be used: {reason}")),
        };
        // The key is kept, so trying again lists the lights without pairing again.
        match driver.list_lights().await {
            Ok(lights) => self.lights = lights,
            Err(error) => return failed(format!("Paired, but the bridge's lights could not be listed: {error}")),
        }
        if self.lights.is_empty() {
            return self.finish(Vec::new(), "Paired. The bridge has no lights yet.".to_string());
        }
        SetupStep::Choose {
            title: "Choose lights".to_string(),
            text: format!(
                "Paired. The bridge has {}. Tick the ones to add.",
                plural(self.lights.len(), "light", "lights")
            ),
            options: self
                .lights
                .iter()
                .map(|light| Choice {
                    id: light.local_id(),
                    label: light.name.clone(),
                    detail: if light.colour { "Colour light" } else { "White light" }.to_string(),
                    selected: true,
                })
                .collect(),
            multiple: true,
            min: 0,
        }
    }

    async fn chosen(&mut self, ctx: &SetupContext, ids: &[String]) -> SetupStep {
        if self.key.is_none() || self.lights.is_empty() {
            return failed("There is nothing to choose from yet. Pair the bridge first.");
        }
        let mut chosen: Vec<FoundLight> = Vec::new();
        for id in ids {
            match self.lights.iter().find(|light| &light.local_id() == id) {
                Some(light) if !chosen.contains(light) => chosen.push(light.clone()),
                Some(_) => {}
                None => return failed(format!("{id} is not a light on this bridge.")),
            }
        }
        let driver = match self.driver(ctx) {
            Ok(driver) => driver,
            Err(reason) => return failed(reason),
        };
        for light in &chosen {
            if let Err(error) = driver.light_state(&light.number).await {
                return failed(format!("{} could not be read: {error}", light.name));
            }
        }
        let summary = format!("Paired. {} added.", plural(chosen.len(), "light", "lights"));
        self.finish(chosen, summary)
    }

    fn finish(&mut self, chosen: Vec<FoundLight>, summary: String) -> SetupStep {
        let Some(key) = self.key.clone() else { return failed("Pair the bridge first.") };
        SetupStep::Done(SetupResult {
            config: json!({ "url": self.url }),
            secrets: vec![(APP_KEY.to_string(), key)],
            adopt: chosen.iter().map(FoundLight::decl).collect(),
            summary,
        })
    }
}

#[async_trait]
impl SetupFlow for HueSetup {
    async fn begin(&mut self, ctx: &SetupContext, existing: Option<&Value>) -> SetupStep {
        self.url = existing.and_then(|value| config::parse(value).ok()).map(|config| config.url).unwrap_or_default();
        self.form(&ctx.secrets_present)
    }

    async fn advance(&mut self, ctx: &SetupContext, input: SetupInput) -> SetupStep {
        match input {
            SetupInput::Values { values } => self.submitted(ctx, &values),
            SetupInput::Poll => self.polled(ctx).await,
            SetupInput::Chosen { ids } => self.chosen(ctx, &ids).await,
            SetupInput::Back => self.form(&ctx.secrets_present),
        }
    }
}
