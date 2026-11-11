//! The instance config: the sockets, as rows. Each socket's local key lives in
//! the secret slot `local_key/<id>`, never here.

// Addresses are plain values here. `core::net` holds the same types as the
// standard library's, and nothing in it can open a socket.
use std::collections::HashSet;
use swifty_net::Ipv4Addr;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use swifty_plugin_api::ConfigProblem;

/// The port a socket listens on for its local protocol.
pub const DEFAULT_PORT: u16 = 6668;
/// The most sockets one instance holds.
pub const MAX_PLUGS: usize = 32;
/// The longest name a socket may be given.
pub const MAX_PLUG_NAME: usize = 64;

/// Smart sockets that speak the Tuya local protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TuyaConfig {
    /// One row per socket, in the order their default names count.
    pub plugs: Vec<PlugConfig>,
}

/// One socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlugConfig {
    /// The Tuya device id, in lower case.
    pub id: String,
    /// An IPv4 address and nothing else.
    pub address: String,
    /// The local protocol's port.
    #[serde(default = "default_port")]
    pub port: u16,
    /// Without one it is "Socket 1", "Socket 2" and so on, in this order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The data point of a USB port that switches on its own, if the socket has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usb_dp: Option<u16>,
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

impl PlugConfig {
    /// The local id (and the target it hints) of the socket itself.
    pub fn target(&self) -> String {
        format!("plug-{}", self.id)
    }

    /// The local id of its USB port, if it has one that switches on its own.
    pub fn usb_target(&self) -> Option<String> {
        self.usb_dp.map(|_| format!("plug-{}-usb", self.id))
    }

    /// The secret slot that holds this socket's local key.
    pub fn key_slot(&self) -> String {
        key_slot(&self.id)
    }
}

/// The secret slot that holds the local key of the socket with this id.
pub fn key_slot(id: &str) -> String {
    format!("local_key/{id}")
}

/// A Tuya device id: lower-case letters and digits, of a sensible length.
pub fn is_plug_id(id: &str) -> bool {
    (10..=32).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// Whether a name is plain text a person would give a socket.
pub fn is_plain_name(name: &str) -> bool {
    let plain = !name.trim().is_empty() && name.trim() == name && !name.chars().any(char::is_control);
    plain && name.chars().count() <= MAX_PLUG_NAME
}

/// Checks one socket's address: an IPv4 address that can be a socket.
pub fn check_address(address: &str) -> Result<Ipv4Addr, &'static str> {
    let ip = address.parse::<Ipv4Addr>().map_err(|_| "must be an IPv4 address")?;
    if ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast() {
        return Err("cannot be a socket");
    }
    Ok(ip)
}

fn problem(field: impl Into<String>, message: impl Into<String>) -> ConfigProblem {
    ConfigProblem { field: field.into(), message: message.into() }
}

/// Parses and checks a config document. Pure.
pub fn parse(config: &Value) -> Result<TuyaConfig, ConfigProblem> {
    let tuya: TuyaConfig = serde_json::from_value(config.clone()).map_err(|error| {
        // serde names the field in its message ("unknown field `x`", "missing field `id`").
        let text = error.to_string();
        let field = text.split('`').nth(1).unwrap_or("plugs").to_string();
        problem(field, text)
    })?;
    check(&tuya)?;
    Ok(tuya)
}

/// Today's rules for `[[devices.tuya.plugs]]`, with the key file replaced by a secret slot.
pub fn check(tuya: &TuyaConfig) -> Result<(), ConfigProblem> {
    if tuya.plugs.is_empty() || tuya.plugs.len() > MAX_PLUGS {
        return Err(problem("plugs", format!("must list 1 to {MAX_PLUGS} sockets")));
    }
    let mut ids = HashSet::new();
    let mut places = HashSet::new();
    for (row, plug) in tuya.plugs.iter().enumerate() {
        let at = |key: &str| format!("plugs[{row}].{key}");
        if !is_plug_id(&plug.id) {
            return Err(problem(at("id"), "an id must be 10 to 32 lower-case letters and digits"));
        }
        let id = &plug.id;
        if !ids.insert(id) {
            return Err(problem(at("id"), format!("{id} is listed twice")));
        }
        let ip =
            check_address(&plug.address).map_err(|why| problem(at("address"), format!("the address of {id} {why}")))?;
        if plug.port == 0 {
            return Err(problem(at("port"), format!("the port of {id} cannot be 0")));
        }
        if !places.insert((ip, plug.port)) {
            return Err(problem(at("address"), format!("the address of {id} is used twice")));
        }
        if plug.usb_dp.is_some_and(|point| !(2..=255).contains(&point)) {
            return Err(problem(at("usb_dp"), format!("the usb_dp of {id} must be 2 to 255")));
        }
        if plug.name.as_deref().is_some_and(|name| !is_plain_name(name)) {
            return Err(problem(at("name"), format!("the name of {id} must be 1 to {MAX_PLUG_NAME} plain characters")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn plug(row: Value) -> Result<TuyaConfig, ConfigProblem> {
        parse(&json!({ "plugs": [row] }))
    }

    /// The Tuya half of the hub's `device_urls_are_only_an_authority`, with the key
    /// file gone: a key is a secret slot now, so an inline key is an unknown field.
    #[test]
    fn the_socket_rules_of_today_hold() {
        let good = json!({ "id": "bf0123456789abcdef", "address": "192.0.2.40" });
        assert_eq!(plug(good.clone()).unwrap().plugs[0].port, DEFAULT_PORT);
        let mut named = good.clone();
        named["name"] = json!("Desk");
        named["port"] = json!(7000);
        assert!(plug(named).is_ok());

        let with = |key: &str, value: Value| {
            let mut row = good.clone();
            row[key] = value;
            row
        };
        for (bad, field) in [
            (with("id", json!("BF0123456789ABCDEF")), "id"),
            (with("id", json!("short")), "id"),
            (with("id", json!("bf0123456789abcde/")), "id"),
            (with("address", json!("socket.example.org")), "address"),
            (with("address", json!("192.0.2.40:6668")), "address"),
            (with("address", json!("::1")), "address"),
            (with("address", json!("0.0.0.0")), "address"),
            (with("address", json!("224.0.0.1")), "address"),
            (with("address", json!("255.255.255.255")), "address"),
            (json!({ "id": "bf0123456789abcdef" }), "address"),
            (with("port", json!(0)), "port"),
            (with("name", json!(" x ")), "name"),
            (with("name", json!("")), "name"),
            (with("name", json!("x".repeat(MAX_PLUG_NAME + 1))), "name"),
            (with("usb_dp", json!(1)), "usb_dp"),
            (with("usb_dp", json!(256)), "usb_dp"),
            (with("key", json!("inline")), "key"),
            (with("key_file", json!("/k")), "key_file"),
        ] {
            let refused = plug(bad.clone()).unwrap_err();
            assert!(refused.field.contains(field) || refused.message.contains(field), "{bad}: {refused}");
        }

        let twice = |second_id: &str, second_ip: &str| {
            parse(&json!({ "plugs": [good, { "id": second_id, "address": second_ip }] }))
        };
        assert!(twice("bf0123456789abcde2", "192.0.2.41").is_ok());
        assert!(twice("bf0123456789abcdef", "192.0.2.41").is_err(), "the same id");
        assert!(twice("bf0123456789abcde2", "192.0.2.40").is_err(), "the same address");
        assert!(parse(&json!({ "plugs": [] })).is_err(), "nothing listed");
        let many: Vec<Value> = (0..=MAX_PLUGS)
            .map(|n| json!({ "id": format!("bf{n:016}"), "address": format!("192.0.2.{}", n + 1) }))
            .collect();
        assert!(parse(&json!({ "plugs": many })).is_err(), "too many");
        assert_eq!(parse(&json!({})).unwrap_err().field, "plugs");
    }

    #[test]
    fn a_config_round_trips_without_empty_fields() {
        let config = json!({ "plugs": [{ "id": "bf0123456789abcdef", "address": "192.0.2.40", "port": 6668 }] });
        let parsed = parse(&config).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), config);
        assert_eq!(parsed.plugs[0].key_slot(), "local_key/bf0123456789abcdef");
    }
}
