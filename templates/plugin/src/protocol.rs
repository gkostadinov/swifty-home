//! What the device speaks, as pure functions: what to send for an operation,
//! and what a reply means. Nothing here opens a socket; `driver.rs` does that.
//!
//! The template speaks a stand-in protocol so that it works from the start: one
//! JSON document per request and one per reply.
//!
//! - read:   `{"id": "desk", "get": true}`
//! - change: `{"id": "desk", "set": {"power": true}}`
//! - reply:  `{"ok": true, "state": {"power": true}}` or `{"ok": false, "error": "why"}`
//!
//! Replace it with what your device really speaks, and keep the tests: each
//! operation has to turn into bytes you can check, and each reply into a `State`.

use serde_json::{json, Map, Value};
use swifty_plugin_api::net::device_text;
use swifty_plugin_api::{Capability, DeviceClass, DeviceError, Op, Scalar, State};

/// A device's fields, as it reports them or as they are to be set.
pub type Fields = Map<String, Value>;

/// The classes this plugin offers: the word in its config, the class, and the
/// start of a default name. The first is the default.
pub const CLASSES: &[(&str, DeviceClass, &str)] = &[
    // word, class, name
    {{class_table}}
];

/// The answer for an operation this device cannot do.
pub const CANNOT: &str = "This device cannot do that.";

/// The class a config word names.
pub fn class_named(word: &str) -> Option<DeviceClass> {
    CLASSES.iter().find(|(name, _, _)| *name == word).map(|(_, class, _)| *class)
}

/// The default name of the device in row `row` (from 0): "{{first_thing}} 1", and so on.
pub fn default_name(class: DeviceClass, row: usize) -> String {
    let thing = CLASSES.iter().find(|(_, known, _)| *known == class).map_or("Device", |(_, _, thing)| *thing);
    format!("{thing} {}", row + 1)
}

/// What a device of this class can do.
pub fn capabilities(class: DeviceClass) -> Vec<Capability> {
    match class {
        {{capability_arms}}
        _ => Vec::<Capability>::new(),
    }
}

/// The fields to set on the device for an operation, or why it cannot be done.
pub fn encode(class: DeviceClass, op: &Op) -> Result<Fields, &'static str> {
    {{encode_body}}
}

/// What the device's fields say, as the state of its class.
pub fn decode(class: DeviceClass, fields: &Fields) -> Result<State, DeviceError> {
    match class {
        {{decode_arms}}
        _ => Err(unreadable()),
    }
}

/// The request that reads a device.
pub fn read_request(device: &str) -> Vec<u8> {
    json!({ "id": device, "get": true }).to_string().into_bytes()
}

/// The request that changes a device.
pub fn set_request(device: &str, set: &Fields) -> Vec<u8> {
    json!({ "id": device, "set": set }).to_string().into_bytes()
}

/// What a reply says: the device's fields, or why it refused.
pub fn parse_reply(reply: &[u8]) -> Result<Fields, DeviceError> {
    let value: Value = serde_json::from_slice(reply).map_err(|_| unreadable())?;
    let reply = value.as_object().ok_or_else(unreadable)?;
    match reply.get("ok").and_then(Value::as_bool) {
        Some(true) => match reply.get("state") {
            None => Ok(Fields::new()),
            Some(Value::Object(state)) => Ok(state.clone()),
            Some(_) => Err(unreadable()),
        },
        // What the device says it could not do is not repeated. The text of an error is fixed English
        // (a device's own words can hold anything); map its error codes to sentences of your own here.
        Some(false) => Err(DeviceError::Rejected(REFUSED.to_string())),
        None => Err(unreadable()),
    }
}

/// What a device that answered and said no is reported as, whatever it said.
pub const REFUSED: &str = "the device refused the request";

/// The error for a reply that does not make sense.
pub fn unreadable() -> DeviceError {
    DeviceError::Rejected("the device sent an unreadable reply".to_string())
}

/// Fields from pairs.
pub fn fields(pairs: &[(&str, Value)]) -> Fields {
    pairs.iter().map(|(key, value)| (key.to_string(), value.clone())).collect()
}

/// A field that must be `true` or `false`.
pub fn flag(fields: &Fields, key: &str) -> Result<bool, DeviceError> {
    fields.get(key).and_then(Value::as_bool).ok_or_else(unreadable)
}

/// Words, as the strings a capability lists.
pub fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|word| word.to_string()).collect()
}

/// A `true` or `false` field, when there is one.
pub fn maybe_flag(fields: &Fields, key: &str) -> Option<bool> {
    fields.get(key).and_then(Value::as_bool)
}

/// A field that must be a whole number from 0 to 255.
pub fn byte(fields: &Fields, key: &str) -> Result<u8, DeviceError> {
    fields.get(key).and_then(Value::as_u64).and_then(|n| u8::try_from(n).ok()).ok_or_else(unreadable)
}

/// A field that must be an angle from 0 to 359.
pub fn angle(fields: &Fields, key: &str) -> Result<u16, DeviceError> {
    fields.get(key).and_then(Value::as_u64).filter(|n| *n < 360).map(|n| n as u16).ok_or_else(unreadable)
}

/// A field that must be a count.
pub fn count(fields: &Fields, key: &str) -> Result<u32, DeviceError> {
    fields.get(key).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).ok_or_else(unreadable)
}

/// A text field, when there is one.
pub fn maybe_text(fields: &Fields, key: &str) -> Option<String> {
    fields.get(key).and_then(Value::as_str).map(device_text)
}

/// A temperature in whole degrees, when there is a sensible one.
pub fn maybe_degrees(fields: &Fields, key: &str) -> Option<i16> {
    fields.get(key).and_then(Value::as_i64).and_then(|n| i16::try_from(n).ok()).filter(|n| (-60..=100).contains(n))
}

/// A number with a fraction, when there is one.
pub fn maybe_real(fields: &Fields, key: &str) -> Option<f64> {
    fields.get(key).and_then(Value::as_f64).filter(|n| n.is_finite())
}

/// The plain fields, for a device the hub has no shape for.
pub fn scalars(fields: &Fields) -> std::collections::BTreeMap<String, Scalar> {
    fields
        .iter()
        .filter_map(|(key, value)| {
            let scalar = match value {
                Value::Bool(flag) => Scalar::Bool(*flag),
                Value::Number(number) => Scalar::Int(number.as_i64()?),
                Value::String(text) => Scalar::Text(device_text(text)),
                _ => return None,
            };
            Some((key.clone(), scalar))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_one_json_document_each() {
        let sent = |bytes: Vec<u8>| serde_json::from_slice::<Value>(&bytes).unwrap();
        assert_eq!(sent(read_request("desk")), json!({ "id": "desk", "get": true }));
        let set = fields(&[("power", json!(true))]);
        assert_eq!(sent(set_request("desk", &set)), json!({ "id": "desk", "set": { "power": true } }));
    }

    #[test]
    fn replies_are_read_or_refused() {
        assert_eq!(parse_reply(br#"{"ok":true}"#).unwrap(), Fields::new());
        assert_eq!(parse_reply(br#"{"ok":true,"state":{"power":true}}"#).unwrap()["power"], json!(true));
        // The device's own reason is not carried into the error.
        let refused = parse_reply(br#"{"ok":false,"error":"busy"}"#).unwrap_err();
        assert_eq!(refused, DeviceError::Rejected(REFUSED.to_string()));
        assert!(!refused.to_string().contains("busy"));
        for garbage in [&b""[..], b"null", b"[]", b"{}", b"{\"ok\":true,\"state\":3}", b"\xff\xfe"] {
            assert_eq!(parse_reply(garbage).unwrap_err(), unreadable(), "{garbage:?}");
        }
    }

    #[test]
    fn every_class_has_a_name_and_decodes_its_own_fields() {
        for (word, class, _) in CLASSES {
            assert_eq!(class_named(word), Some(*class));
            assert!(default_name(*class, 0).ends_with(" 1"));
        }
        assert_eq!(class_named("spaceship"), None);
    }

    #[test]
    fn an_operation_of_another_kind_is_refused() {
        for (_, class, _) in CLASSES {
            assert_eq!(encode(*class, &Op::VmStart), Err(CANNOT));
        }
    }

    #[test]
    fn a_missing_field_is_unreadable_not_a_guess() {
        for (_, class, _) in CLASSES {
            if let Ok(state) = decode(*class, &Fields::new()) {
                // Only a class that reads nothing from its fields may decode an empty reply.
                assert!(
                    matches!(
                        state,
                        State::Bridge { .. } | State::Service { .. } | State::Telemetry { .. } | State::Custom { .. }
                    ),
                    "{state:?}"
                );
            }
        }
    }
}
