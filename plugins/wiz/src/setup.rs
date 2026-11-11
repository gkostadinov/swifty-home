//! The guided setup: a form with a broadcast address (and, if wanted, bulbs by
//! address), then discovery, then a choice of the bulbs that answered, then one
//! reading of every chosen bulb.
//!
//! A form with addresses only and no broadcast needs no network, so it also
//! finishes in dry-run. Discovery and the final readings need the grant.

use std::collections::BTreeMap;
use std::time::Duration;
use swifty_net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use async_trait::async_trait;
use serde_json::Value;
use swifty_plugin_api::{
    Choice, Field, FieldType, Scalar, SetupContext, SetupFlow, SetupInput, SetupResult, SetupStep,
};

use crate::config::{self, parse_address, BulbRow, WizConfig};
use crate::driver::{discover, read_at, FoundBulb};
use crate::protocol::{discovered_id, DEFAULT_PORT};

/// How long setup listens for bulbs, at most. The read timeout is used when shorter.
const DISCOVERY_WINDOW: Duration = Duration::from_secs(3);
/// What the form offers when nothing is known yet: every bulb on this link.
const LIMITED_BROADCAST: &str = "255.255.255.255:38899";
const MAX_ADDRESSES_TEXT: usize = 2048;

/// The WiZ setup flow. One per setup session.
#[derive(Debug, Default)]
pub struct WizSetup {
    broadcast_text: String,
    addresses_text: String,
    port: Option<u16>,
    broadcast: Option<String>,
    fixed: Vec<BulbRow>,
    found: Vec<FoundBulb>,
    last: Option<SetupStep>,
}

fn text_field(key: &str, label: &str, help: &str, kind: FieldType, value: &str) -> Field {
    Field {
        key: key.to_string(),
        label: label.to_string(),
        help: help.to_string(),
        kind,
        required: false,
        value: (!value.is_empty()).then(|| Scalar::Text(value.to_string())),
    }
}

fn failed(reason: impl Into<String>) -> SetupStep {
    SetupStep::Failed { reason: reason.into(), retry: true }
}

fn text_value(values: &BTreeMap<String, Scalar>, key: &str) -> String {
    match values.get(key) {
        Some(Scalar::Text(text)) => text.trim().to_string(),
        _ => String::new(),
    }
}

/// `192.0.2.255` or `192.0.2.255:38899`, as the config wants it.
fn broadcast_from(text: &str) -> Option<String> {
    if let Ok(address) = text.parse::<SocketAddrV4>() {
        return Some(address.to_string());
    }
    text.parse::<Ipv4Addr>().ok().map(|ip| SocketAddrV4::new(ip, DEFAULT_PORT).to_string())
}

/// `192.0.2.21, desk=192.0.2.22:40000`: bulbs by address, with an optional id.
/// A bulb without an id gets the next free `bulbN`.
fn bulbs_from(text: &str, port: u16) -> Result<Vec<BulbRow>, String> {
    let mut named: Vec<(Option<String>, String)> = Vec::new();
    for entry in text.split(|c: char| c == ',' || c.is_whitespace()).filter(|entry| !entry.is_empty()) {
        let (id, address) = match entry.split_once('=') {
            Some((id, address)) => (Some(id.trim().to_string()), address.trim().to_string()),
            None => (None, entry.to_string()),
        };
        if parse_address(&address, port).is_none() {
            return Err(format!("{address} is not an IP address."));
        }
        named.push((id, address));
    }
    let mut rows: Vec<BulbRow> = Vec::new();
    let mut next = 1;
    for (id, address) in &named {
        let id = match id {
            Some(id) => id.clone(),
            None => loop {
                let candidate = format!("bulb{next}");
                next += 1;
                let taken = rows.iter().any(|row| row.id == candidate)
                    || named.iter().any(|(other, _)| other.as_deref() == Some(candidate.as_str()));
                if !taken {
                    break candidate;
                }
            },
        };
        rows.push(BulbRow { id, address: address.clone() });
    }
    Ok(rows)
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

impl WizSetup {
    fn form(&self) -> SetupStep {
        SetupStep::Form {
            title: "Find WiZ bulbs".to_string(),
            text: "Swifty Home can ask every WiZ bulb on your network to answer, and you then pick the ones to add. \
                   You can also list bulbs by address. Discovery changes nothing on a bulb."
                .to_string(),
            fields: vec![
                text_field(
                    "broadcast",
                    "Broadcast address",
                    "The top address of the bulbs' network with the port, such as 192.0.2.255:38899. \
                     255.255.255.255:38899 reaches every bulb on this link. Leave empty to skip discovery.",
                    FieldType::HostPort,
                    &self.broadcast_text,
                ),
                text_field(
                    "addresses",
                    "Bulbs by address",
                    "Optional. IP addresses separated by commas, each with an id if you like: desk=192.0.2.21.",
                    FieldType::Text { max_len: MAX_ADDRESSES_TEXT },
                    &self.addresses_text,
                ),
            ],
        }
    }

    fn remember(&mut self, step: SetupStep) -> SetupStep {
        self.last = Some(step.clone());
        step
    }

    fn config(&self) -> WizConfig {
        WizConfig {
            bulbs: self.fixed.clone(),
            port: self.port.unwrap_or(DEFAULT_PORT),
            broadcast: self.broadcast.clone(),
        }
    }

    async fn submitted(&mut self, ctx: &SetupContext, values: &BTreeMap<String, Scalar>) -> SetupStep {
        self.broadcast_text = text_value(values, "broadcast");
        self.addresses_text = text_value(values, "addresses");
        self.found.clear();
        self.broadcast = None;
        if !self.broadcast_text.is_empty() {
            match broadcast_from(&self.broadcast_text) {
                Some(broadcast) => self.broadcast = Some(broadcast),
                None => return failed("The broadcast address must be an IPv4 address, with a port if you like."),
            }
        }
        self.fixed = match bulbs_from(&self.addresses_text, self.port.unwrap_or(DEFAULT_PORT)) {
            Ok(rows) => rows,
            Err(reason) => return failed(reason),
        };
        if self.broadcast.is_none() && self.fixed.is_empty() {
            return failed("Give a broadcast address to find bulbs, or at least one bulb address.");
        }
        let value = match serde_json::to_value(self.config()) {
            Ok(value) => value,
            Err(_) => return failed("The settings could not be stored."),
        };
        if let Err(problem) = config::parse(&value) {
            return failed(format!("The settings cannot be used: {problem}"));
        }
        let Some(broadcast) = self.config().broadcast() else { return self.finish(ctx, &[]).await };
        let Some(grant) = ctx.grant else {
            return SetupStep::Failed {
                reason: "Finding bulbs needs live mode: in dry-run Swifty Home sends nothing. \
                         Set live = true, or leave the broadcast address empty and list the bulbs by address."
                    .to_string(),
                retry: true,
            };
        };
        let found = match discover(grant, broadcast, ctx.timeouts.read.min(DISCOVERY_WINDOW)).await {
            Ok(found) => found,
            Err(error) => return failed(format!("Discovery did not work: {error}")),
        };
        if found.is_empty() {
            if self.fixed.is_empty() {
                return failed(
                    "No WiZ bulb answered. Check the broadcast address, that the bulbs have power, \
                     and that they are on the same network as Swifty Home.",
                );
            }
            return self.finish(ctx, &[]).await;
        }
        self.found = found;
        let options = self
            .found
            .iter()
            .map(|bulb| Choice {
                id: discovered_id(&bulb.mac),
                label: format!("WiZ bulb {}", bulb.mac),
                detail: bulb.address.ip().to_string(),
                selected: true,
            })
            .collect();
        SetupStep::Choose {
            title: "Choose bulbs".to_string(),
            text: format!("{} answered. Tick the ones to add.", plural(self.found.len(), "bulb", "bulbs")),
            options,
            multiple: true,
            min: u32::from(self.fixed.is_empty()),
        }
    }

    /// One reading of every bulb, when live, then the result.
    async fn finish(&mut self, ctx: &SetupContext, chosen: &[FoundBulb]) -> SetupStep {
        let config = self.config();
        if let Some(grant) = ctx.grant {
            let mut to_read: Vec<(String, SocketAddr)> =
                self.fixed.iter().filter_map(|row| Some((row.id.clone(), config.address(&row.id)?))).collect();
            to_read.extend(chosen.iter().map(|bulb| (discovered_id(&bulb.mac), bulb.address)));
            for (id, address) in to_read {
                if let Err(error) = read_at(grant, address, ctx.timeouts.read).await {
                    return failed(format!("{id} at {} could not be read: {error}", address.ip()));
                }
            }
        }
        let config = match serde_json::to_value(&config) {
            Ok(value) => value,
            Err(_) => return failed("The settings could not be stored."),
        };
        let total = self.fixed.len() + chosen.len();
        SetupStep::Done(SetupResult {
            config,
            secrets: Vec::new(),
            adopt: chosen.iter().map(FoundBulb::decl).collect(),
            summary: format!("Set up. {} added.", plural(total, "WiZ bulb", "WiZ bulbs")),
        })
    }

    async fn chosen(&mut self, ctx: &SetupContext, ids: &[String]) -> SetupStep {
        if self.found.is_empty() {
            return failed("There is nothing to choose from yet. Start with the form.");
        }
        let mut chosen = Vec::new();
        for id in ids {
            match self.found.iter().find(|bulb| &discovered_id(&bulb.mac) == id) {
                Some(bulb) if !chosen.contains(bulb) => chosen.push(bulb.clone()),
                Some(_) => {}
                None => return failed(format!("{id} was not among the bulbs that answered.")),
            }
        }
        if chosen.is_empty() && self.fixed.is_empty() {
            return failed("Choose at least one bulb.");
        }
        self.finish(ctx, &chosen).await
    }
}

#[async_trait]
impl SetupFlow for WizSetup {
    async fn begin(&mut self, _ctx: &SetupContext, existing: Option<&Value>) -> SetupStep {
        let existing = existing.and_then(|value| config::parse(value).ok());
        self.port = existing.as_ref().map(|config| config.port);
        self.broadcast_text = existing
            .as_ref()
            .and_then(|config| config.broadcast.clone())
            .unwrap_or_else(|| LIMITED_BROADCAST.to_string());
        self.addresses_text = existing
            .map(|config| {
                config.bulbs.iter().map(|row| format!("{}={}", row.id, row.address)).collect::<Vec<_>>().join(", ")
            })
            .unwrap_or_default();
        let step = self.form();
        self.remember(step)
    }

    async fn advance(&mut self, ctx: &SetupContext, input: SetupInput) -> SetupStep {
        let step = match input {
            SetupInput::Values { values } => self.submitted(ctx, &values).await,
            SetupInput::Chosen { ids } => self.chosen(ctx, &ids).await,
            SetupInput::Back => self.form(),
            // Nothing here waits on anything.
            SetupInput::Poll => return self.last.clone().unwrap_or_else(|| self.form()),
        };
        if matches!(step, SetupStep::Failed { .. } | SetupStep::Done(_)) {
            step
        } else {
            self.remember(step)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_get_free_ids_in_order() {
        let rows = bulbs_from("192.0.2.21, bulb1=192.0.2.22 192.0.2.23:40000", DEFAULT_PORT).unwrap();
        let pairs: Vec<(&str, &str)> = rows.iter().map(|r| (r.id.as_str(), r.address.as_str())).collect();
        assert_eq!(pairs, [("bulb2", "192.0.2.21"), ("bulb1", "192.0.2.22"), ("bulb3", "192.0.2.23:40000")]);
        assert!(bulbs_from("192.0.2.21, nowhere", DEFAULT_PORT).is_err());
        assert!(bulbs_from("", DEFAULT_PORT).unwrap().is_empty());
    }

    #[test]
    fn a_broadcast_gets_the_usual_port() {
        assert_eq!(broadcast_from("192.0.2.255").as_deref(), Some("192.0.2.255:38899"));
        assert_eq!(broadcast_from("192.0.2.255:40000").as_deref(), Some("192.0.2.255:40000"));
        assert_eq!(broadcast_from("[fd00::ff]:38899"), None);
        assert_eq!(broadcast_from("everywhere"), None);
    }
}
