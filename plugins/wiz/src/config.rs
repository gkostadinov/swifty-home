//! The config of a WiZ instance:
//!
//! ```json
//! { "bulbs": [{ "id": "bulb1", "address": "192.0.2.21" }], "port": 38899, "broadcast": "192.0.2.255:38899" }
//! ```
//!
//! Every key is optional. `bulbs` names bulbs by address. `broadcast` is where
//! discovery sends its datagram, and discovery is refused until it is set. WiZ
//! keeps no credential, so there are no secret slots.

use swifty_net::{IpAddr, SocketAddr};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use swifty_plugin_api::ConfigProblem;

use crate::protocol::{is_discovered_id, DEFAULT_PORT};

/// One bulb named by address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BulbRow {
    /// Its local id, which is also the target it asks for: `bulb1`.
    pub id: String,
    /// An IP address, or an IP address and port.
    pub address: String,
}

/// The typed config. What [`crate::Wiz`] validates and the driver runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WizConfig {
    /// Bulbs by address.
    #[serde(default)]
    pub bulbs: Vec<BulbRow>,
    /// The port of a bulb whose address has none.
    #[serde(default = "default_port")]
    pub port: u16,
    /// Where to broadcast the discovery datagram, as IPv4 address:port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broadcast: Option<String>,
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

impl Default for WizConfig {
    fn default() -> Self {
        Self { bulbs: Vec::new(), port: DEFAULT_PORT, broadcast: None }
    }
}

impl WizConfig {
    /// The address of one bulb named in the config.
    pub fn address(&self, id: &str) -> Option<SocketAddr> {
        let text = &self.bulbs.iter().find(|bulb| bulb.id == id)?.address;
        parse_address(text, self.port)
    }

    /// The discovery broadcast, once it is set and valid.
    pub fn broadcast(&self) -> Option<SocketAddr> {
        self.broadcast.as_deref().and_then(|text| text.parse::<SocketAddr>().ok()).filter(SocketAddr::is_ipv4)
    }
}

/// An IP address, with this port unless it names its own.
pub(crate) fn parse_address(text: &str, port: u16) -> Option<SocketAddr> {
    text.parse().ok().or_else(|| text.parse::<IpAddr>().ok().map(|ip| SocketAddr::new(ip, port)))
}

/// `[a-z0-9][a-z0-9_-]{0,63}`: what a local id may be.
pub(crate) fn is_local_id(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && text.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn problem(field: impl Into<String>, message: impl Into<String>) -> ConfigProblem {
    ConfigProblem { field: field.into(), message: message.into() }
}

/// The field a serde error is about: the first name in backticks, if any.
fn field_of(message: &str) -> String {
    message.split('`').nth(1).unwrap_or("config").to_string()
}

/// Parses and checks a config document.
pub fn parse(config: &Value) -> Result<WizConfig, ConfigProblem> {
    let parsed: WizConfig = serde_json::from_value(config.clone()).map_err(|error| {
        let message = error.to_string();
        problem(field_of(&message), message)
    })?;
    check(&parsed)?;
    Ok(parsed)
}

/// Today's checks on `[devices.wiz]` and `lights.wiz_broadcast`.
fn check(config: &WizConfig) -> Result<(), ConfigProblem> {
    for (index, bulb) in config.bulbs.iter().enumerate() {
        let at = format!("bulbs[{index}].id");
        if !is_local_id(&bulb.id) {
            return Err(problem(at, format!("{} must match [a-z0-9][a-z0-9_-]{{0,63}}", bulb.id)));
        }
        if is_discovered_id(&bulb.id) {
            return Err(problem(at, format!("{} is the id of a discovered bulb; choose another", bulb.id)));
        }
        if config.bulbs[..index].iter().any(|earlier| earlier.id == bulb.id) {
            return Err(problem(at, format!("{} is listed twice", bulb.id)));
        }
        if config.address(&bulb.id).is_none() {
            return Err(problem(format!("bulbs[{index}].address"), format!("{} must be an IP address", bulb.id)));
        }
    }
    if config.broadcast.is_some() && config.broadcast().is_none() {
        return Err(problem("broadcast", "broadcast must be IPv4 address:port"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn bulb_addresses_are_checked() {
        let wiz = |bulbs: Value| parse(&json!({ "bulbs": bulbs }));
        let ok =
            wiz(json!([{ "id": "bulb1", "address": "192.0.2.1" }, { "id": "bulb2", "address": "192.0.2.2:40000" }]))
                .unwrap();
        assert_eq!(ok.address("bulb1").unwrap().to_string(), "192.0.2.1:38899");
        assert_eq!(ok.address("bulb2").unwrap().to_string(), "192.0.2.2:40000");
        assert_eq!(ok.address("bulb3"), None);
        assert!(wiz(json!([{ "id": "wiz-a8bb50aabbcc", "address": "192.0.2.1" }])).is_err(), "a discovered id");
        assert!(wiz(json!([{ "id": "Bulb 5", "address": "192.0.2.1" }])).is_err(), "not a local id");
        assert!(wiz(json!([{ "id": "bulb1", "address": "not an address" }])).is_err());
        let twice = json!([{ "id": "bulb1", "address": "192.0.2.1" }, { "id": "bulb1", "address": "192.0.2.2" }]);
        assert!(wiz(twice).is_err());
    }

    #[test]
    fn the_broadcast_must_be_an_ipv4_address_and_port() {
        assert!(parse(&json!({ "broadcast": "192.0.2.255:38899" })).is_ok());
        for bad in ["everywhere", "192.0.2.255", "[fd00::ff]:38899"] {
            let error = parse(&json!({ "broadcast": bad })).unwrap_err();
            assert_eq!(error.field, "broadcast", "{bad}");
        }
    }

    #[test]
    fn every_key_is_optional_and_nothing_else_is_accepted() {
        assert_eq!(parse(&json!({})).unwrap(), WizConfig::default());
        let error = parse(&json!({ "bulbs": [], "colour": true })).unwrap_err();
        assert_eq!(error.field, "colour", "{error}");
        let error = parse(&json!({ "bulbs": [{ "id": "bulb1" }] })).unwrap_err();
        assert_eq!(error.field, "address", "{error}");
    }
}
