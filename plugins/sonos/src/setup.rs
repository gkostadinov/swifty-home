//! The guided setup: a form with the speaker's address and, optionally, a
//! subwoofer's; then one read of each when the hub is live; then done.
//!
//! The speakers get the ids `sonos` and `sub`. More speakers are more rows of
//! the config, which the setup leaves as it found them when it is run again.

// Address types only: every socket is opened by `swifty-net`.
use std::collections::BTreeMap;
use swifty_net::{IpAddr, SocketAddr};

use async_trait::async_trait;
use serde_json::{json, Value};
use swifty_plugin_api::{
    Driver, Field, FieldType, Scalar, SetupContext, SetupFlow, SetupInput, SetupResult, SetupStep, State,
};

use crate::config::{SonosConfig, SpeakerConfig};
use crate::driver::SonosDriver;
use crate::PORT;

/// The local id of the speaker the form names.
pub const MAIN_ID: &str = "sonos";
/// The local id of the subwoofer the form names.
pub const SUB_ID: &str = "sub";

const ADDRESS: &str = "address";
const SUB_ADDRESS: &str = "sub_address";

/// The setup flow. It holds nothing between steps but what the form showed.
#[derive(Default)]
pub struct SonosSetup {
    /// Rows of an existing config other than the two the form names. Kept as they are.
    others: Vec<SpeakerConfig>,
    /// The values shown in the form, for going back to it.
    shown: BTreeMap<String, String>,
}

fn text(values: &BTreeMap<String, Scalar>, key: &str) -> Option<String> {
    match values.get(key) {
        Some(Scalar::Text(text)) if !text.trim().is_empty() => Some(text.trim().to_string()),
        _ => None,
    }
}

/// `http://<address>:1400`, if `address` is an IP address. An address with a
/// port of its own keeps it.
fn speaker_url(address: &str) -> Option<String> {
    if let Ok(with_port) = address.parse::<SocketAddr>() {
        return Some(format!("http://{with_port}"));
    }
    let ip: IpAddr = address.trim_matches(['[', ']']).parse().ok()?;
    Some(match ip {
        IpAddr::V4(ip) => format!("http://{ip}:{PORT}"),
        IpAddr::V6(ip) => format!("http://[{ip}]:{PORT}"),
    })
}

/// The address in a speaker URL, for the form: the port only when it is not 1400.
fn address_of(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) if port != PORT => format!("{host}:{port}"),
        _ => host.trim_matches(['[', ']']).to_string(),
    })
}

fn describe(state: &State) -> String {
    match state {
        State::Speaker { vol, playing, source, .. } => {
            format!("volume {vol}, {}", if *playing { format!("playing {source}") } else { "not playing".to_string() })
        }
        _ => "read".to_string(),
    }
}

impl SonosSetup {
    fn form(&self) -> SetupStep {
        let prefill = |key: &str| self.shown.get(key).cloned().map(Scalar::Text);
        SetupStep::Form {
            title: "Sonos speakers".to_string(),
            text: "Enter the IP address of the speaker, as the Sonos app shows it under About My System. \
                   Give it a fixed address in your router first. A subwoofer can be added too; it is only read."
                .to_string(),
            fields: vec![
                Field {
                    key: ADDRESS.to_string(),
                    label: "Speaker address".to_string(),
                    help: "The speaker's IP address, such as 192.0.2.30. Add :port only if it is not 1400.".to_string(),
                    kind: FieldType::Text { max_len: 64 },
                    required: true,
                    value: prefill(ADDRESS),
                },
                Field {
                    key: SUB_ADDRESS.to_string(),
                    label: "Subwoofer address".to_string(),
                    help: "Optional. The subwoofer's IP address. It is shown but never commanded.".to_string(),
                    kind: FieldType::Text { max_len: 64 },
                    required: false,
                    value: prefill(SUB_ADDRESS),
                },
            ],
        }
    }

    fn failed(reason: impl Into<String>) -> SetupStep {
        SetupStep::Failed { reason: reason.into(), retry: true }
    }

    async fn submit(&mut self, ctx: &SetupContext, values: &BTreeMap<String, Scalar>) -> SetupStep {
        self.shown.clear();
        for key in [ADDRESS, SUB_ADDRESS] {
            if let Some(value) = text(values, key) {
                self.shown.insert(key.to_string(), value);
            }
        }
        let Some(address) = text(values, ADDRESS) else { return Self::failed("Enter the speaker's address.") };
        let Some(url) = speaker_url(&address) else {
            return Self::failed("The speaker's address must be an IP address: a Sonos speaker refuses a name.");
        };
        let mut speakers = vec![json!({ "id": MAIN_ID, "url": url })];
        if let Some(sub) = text(values, SUB_ADDRESS) {
            let Some(url) = speaker_url(&sub) else {
                return Self::failed("The subwoofer's address must be an IP address.");
            };
            speakers.push(json!({ "id": SUB_ID, "url": url, "read_only": true }));
        }
        for other in &self.others {
            speakers.push(json!({ "id": other.id, "url": other.url, "read_only": other.read_only }));
        }
        let config = json!({ "speakers": speakers });
        let parsed = match SonosConfig::parse(&config) {
            Ok(parsed) => parsed,
            Err(problem) => return Self::failed(problem.to_string()),
        };

        let Some(grant) = ctx.grant else {
            let summary = "Saved. The hub is in dry-run, so the speakers were not contacted.".to_string();
            return SetupStep::Done(SetupResult { config, secrets: Vec::new(), adopt: Vec::new(), summary });
        };
        let driver = match SonosDriver::new(grant, &parsed, ctx.timeouts) {
            Ok(driver) => driver,
            Err(why) => return Self::failed(why),
        };
        let mut found = Vec::new();
        for speaker in &parsed.speakers {
            match driver.read_state(&speaker.id).await {
                Ok(state) => found.push(format!("{}: {}", speaker.id, describe(&state))),
                Err(error) => {
                    let address = address_of(&speaker.url).unwrap_or_default();
                    return Self::failed(format!(
                        "The speaker at {address} could not be read: {error}. Check the address and that it is on."
                    ));
                }
            }
        }
        let summary = format!("Found {} speaker(s). {}.", found.len(), found.join("; "));
        SetupStep::Done(SetupResult { config, secrets: Vec::new(), adopt: Vec::new(), summary })
    }
}

#[async_trait]
impl SetupFlow for SonosSetup {
    async fn begin(&mut self, _ctx: &SetupContext, existing: Option<&Value>) -> SetupStep {
        self.others.clear();
        self.shown.clear();
        if let Some(existing) = existing.and_then(|config| SonosConfig::parse(config).ok()) {
            for speaker in existing.speakers {
                let key = match speaker.id.as_str() {
                    MAIN_ID => ADDRESS,
                    SUB_ID => SUB_ADDRESS,
                    _ => {
                        self.others.push(speaker);
                        continue;
                    }
                };
                if let Some(address) = address_of(&speaker.url) {
                    self.shown.insert(key.to_string(), address);
                }
            }
        }
        self.form()
    }

    async fn advance(&mut self, ctx: &SetupContext, input: SetupInput) -> SetupStep {
        match input {
            SetupInput::Values { values } => self.submit(ctx, &values).await,
            SetupInput::Back => self.form(),
            _ => Self::failed("This step needs the speaker's address."),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_speaker_is_addressed_by_ip_on_its_port() {
        assert_eq!(speaker_url("192.0.2.30").as_deref(), Some("http://192.0.2.30:1400"));
        assert_eq!(speaker_url("2001:db8::1").as_deref(), Some("http://[2001:db8::1]:1400"));
        assert_eq!(speaker_url("speaker.example.org"), None);
        assert_eq!(speaker_url("192.0.2.30:1401").as_deref(), Some("http://192.0.2.30:1401"));
        assert_eq!(speaker_url("192.0.2.30:x"), None);
        assert_eq!(address_of("http://[2001:db8::1]:1400").as_deref(), Some("2001:db8::1"));
        assert_eq!(address_of("http://192.0.2.30:1401").as_deref(), Some("192.0.2.30:1401"));
        assert_eq!(address_of("http://192.0.2.30:1400").as_deref(), Some("192.0.2.30"));
    }
}
