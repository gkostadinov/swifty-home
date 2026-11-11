//! What a person fills in for one instance: the devices, as rows. Parsing and
//! checking only, so it is pure and safe for a hub in dry-run.
{{#cloud}}
//!
//! The access token is a secret slot, never part of this config.
{{/cloud}}

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
{{#local}}
// Addresses are plain values here: `swifty_plugin_api::net` re-exports the types and
// nothing in them can open a socket.
use swifty_plugin_api::net::{IpAddr, SocketAddr};
{{/local}}
use swifty_plugin_api::{ConfigProblem, DeviceClass};

use crate::protocol;

{{#local}}
/// The port the device listens on, unless its row says otherwise.
pub const DEFAULT_PORT: u16 = {{port}};
{{/local}}
{{#cloud}}
/// The vendor's API, unless the config says otherwise.
pub const DEFAULT_API_URL: &str = "https://api.example.org/";
/// Its host, as the manifest declares it.
pub const API_HOST: &str = "api.example.org";
/// The secret slot of the access token.
pub const TOKEN_SLOT: &str = "token";
{{/cloud}}
/// The most devices one instance holds.
pub const MAX_DEVICES: usize = 64;
/// The longest device id.
pub const MAX_ID: usize = 64;
/// The longest name a device may be given.
pub const MAX_NAME: usize = 64;

/// One instance of the plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
{{#cloud}}
    /// The vendor's API.
    #[serde(default = "default_api_url")]
    pub api_url: String,
{{/cloud}}
    /// One row per device, in the order their default names count.
    pub devices: Vec<DeviceRow>,
}

/// One device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceRow {
    /// How the device knows itself. Also its local id in the hub.
    pub id: String,
{{#local}}
    /// Its IP address.
    pub address: String,
    /// The port it listens on.
    #[serde(default = "default_port")]
    pub port: u16,
{{/local}}
    /// What kind of device it is, as a word. Without one it is "{{first_class}}".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// How it is shown. Without one it is named by its kind and its row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

{{#local}}
fn default_port() -> u16 {
    DEFAULT_PORT
}
{{/local}}
{{#cloud}}
fn default_api_url() -> String {
    DEFAULT_API_URL.to_string()
}
{{/cloud}}

impl DeviceRow {
    /// Its class. A row that passed [`check`] always names a known one.
    pub fn class(&self) -> DeviceClass {
        self.class.as_deref().and_then(protocol::class_named).unwrap_or(protocol::CLASSES[0].1)
    }
{{#local}}

    /// Where to send to it.
    pub fn socket(&self) -> Option<SocketAddr> {
        self.address.parse::<IpAddr>().ok().map(|ip| SocketAddr::new(ip, self.port))
    }
{{/local}}
}

impl Config {
    /// The row of the device with this id.
    pub fn device(&self, id: &str) -> Option<&DeviceRow> {
        self.devices.iter().find(|device| device.id == id)
    }
}

/// `[a-z0-9][a-z0-9_-]{0,63}`, the hub's grammar for a local id.
pub fn is_device_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_ID
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-' || *b == b'_')
}

/// Plain text a person would give a device.
pub fn is_plain_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.trim() == name
        && !name.chars().any(char::is_control)
        && name.chars().count() <= MAX_NAME
}

fn problem(field: impl Into<String>, message: impl Into<String>) -> ConfigProblem {
    ConfigProblem { field: field.into(), message: message.into() }
}

/// Parses and checks a config document. Pure.
pub fn parse(config: &Value) -> Result<Config, ConfigProblem> {
    let parsed: Config = serde_json::from_value(config.clone()).map_err(|error| {
        // serde names the key only of a field that is unknown or missing ("unknown field `x`", "missing
        // field `id`"). For a value of the wrong type it names the value, not the key, so the problem is
        // put on `devices`, with serde's words.
        let text = error.to_string();
        let named = ["unknown field `", "missing field `"].iter().find_map(|start| text.strip_prefix(start));
        let field = named.and_then(|rest| rest.split('`').next()).unwrap_or("devices").to_string();
        problem(field, text)
    })?;
    check(&parsed)?;
    Ok(parsed)
}

/// The rules a config must follow beyond its shape.
pub fn check(config: &Config) -> Result<(), ConfigProblem> {
{{#cloud}}
    let loopback = ["http://127.0.0.1:", "http://localhost:"].iter().any(|start| config.api_url.starts_with(start));
    if !config.api_url.starts_with("https://") && !loopback {
        return Err(problem("api_url", "must start with https://"));
    }
{{/cloud}}
    if config.devices.is_empty() || config.devices.len() > MAX_DEVICES {
        return Err(problem("devices", format!("must list 1 to {MAX_DEVICES} devices")));
    }
    let mut ids = HashSet::new();
    for (row, device) in config.devices.iter().enumerate() {
        let at = |key: &str| format!("devices[{row}].{key}");
        if !is_device_id(&device.id) {
            return Err(problem(at("id"), "an id is 1 to 64 lower-case letters, digits, - and _"));
        }
        let id = &device.id;
        if !ids.insert(id) {
            return Err(problem(at("id"), format!("{id} is listed twice")));
        }
{{#local}}
        let Ok(ip) = device.address.parse::<IpAddr>() else {
            return Err(problem(at("address"), format!("the address of {id} must be an IP address")));
        };
        if ip.is_unspecified() || ip.is_multicast() {
            return Err(problem(at("address"), format!("the address of {id} cannot be a device")));
        }
        if device.port == 0 {
            return Err(problem(at("port"), format!("the port of {id} cannot be 0")));
        }
{{/local}}
        if let Some(class) = &device.class {
            if protocol::class_named(class).is_none() {
                let words: Vec<&str> = protocol::CLASSES.iter().map(|(word, _, _)| *word).collect();
                return Err(problem(at("class"), format!("the class of {id} must be one of: {}", words.join(", "))));
            }
        }
        if device.name.as_deref().is_some_and(|name| !is_plain_name(name)) {
            return Err(problem(at("name"), format!("the name of {id} must be 1 to {MAX_NAME} plain characters")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn with_row(row: Value) -> Result<Config, ConfigProblem> {
        parse(&json!({ "devices": [row] }))
    }

    #[test]
    fn a_good_config_parses_and_round_trips() {
{{#local}}
        let config = json!({ "devices": [{ "id": "desk", "address": "192.0.2.20", "port": DEFAULT_PORT }] });
{{/local}}
{{#cloud}}
        let config = json!({ "api_url": DEFAULT_API_URL, "devices": [{ "id": "desk" }] });
{{/cloud}}
        let parsed = parse(&config).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), config);
        assert_eq!(parsed.device("desk").unwrap().class(), protocol::CLASSES[0].1);
    }

    #[test]
    fn each_mistake_is_refused_and_named() {
{{#local}}
        let good = json!({ "id": "desk", "address": "192.0.2.20" });
{{/local}}
{{#cloud}}
        let good = json!({ "id": "desk" });
{{/cloud}}
        assert!(with_row(good.clone()).is_ok());
        let with = |key: &str, value: Value| {
            let mut row = good.clone();
            row[key] = value;
            row
        };
        for (bad, field) in [
            (with("id", json!("Desk")), "id"),
            (with("id", json!("")), "id"),
            (with("id", json!("a/b")), "id"),
{{#local}}
            (with("address", json!("desk.example.org")), "address"),
            (with("address", json!("0.0.0.0")), "address"),
            (with("port", json!(0)), "port"),
{{/local}}
            (with("class", json!("spaceship")), "class"),
            (with("name", json!(" padded ")), "name"),
            (with("surprise", json!(true)), "surprise"),
        ] {
            let refused = with_row(bad.clone()).unwrap_err();
            assert!(refused.field.contains(field) || refused.message.contains(field), "{bad}: {refused}");
        }
        assert_eq!(parse(&json!({})).unwrap_err().field, "devices");
        assert!(parse(&json!({ "devices": [] })).is_err());
        assert!(parse(&json!({ "devices": [good.clone(), good] })).is_err(), "the same id twice");
    }

    #[test]
    fn a_value_of_the_wrong_type_is_not_taken_for_a_field() {
{{#local}}
        let row = json!({ "id": "desk", "address": "192.0.2.20", "name": 7 });
{{/local}}
{{#cloud}}
        let row = json!({ "id": "desk", "name": 7 });
{{/cloud}}
        // serde's message names the value (`7`), not the key: the field must not be `7`.
        let refused = with_row(row).unwrap_err();
        assert_eq!(refused.field, "devices");
        assert!(refused.message.contains("invalid type"), "{refused}");
    }
}
