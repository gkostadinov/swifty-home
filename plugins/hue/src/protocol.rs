//! The bridge's v1 REST API: what goes out and how a reply is read. Pure: nothing
//! here opens a socket.
//!
//! The application key travels in the URL path. Every error here is fixed text and
//! the bridge's numeric error type. Nothing the bridge wrote is repeated, so the
//! key cannot reach a log, a result, an audit row or a response.

use std::fmt;

use serde_json::{json, Value};
use swifty_net::Reply;
use swifty_plugin_api::{DeviceError, Op, State};

/// The local id of the bridge itself. It only reports that it can be reached.
pub const BRIDGE_ID: &str = "hue";
/// What a new pairing calls this hub on the bridge. Keys made before the rename
/// keep the bridge's old label, and still work.
pub const DEVICE_TYPE: &str = "swifty_home#hub";
pub(crate) const NO_COLOUR: &str = "This Hue light cannot show colour.";
const FULL_SATURATION: u64 = 254;
/// The most lights one listing offers.
pub(crate) const MAX_LIGHTS: usize = 128;
/// The longest key a bridge is believed to hand out.
const MAX_KEY_LEN: usize = 128;

/// A light on the bridge: `hue-` and the bridge's number for it, 1 to 99999.
pub fn is_light_id(local_id: &str) -> bool {
    local_id
        .strip_prefix("hue-")
        .is_some_and(|id| (1..=5).contains(&id.len()) && !id.starts_with('0') && id.bytes().all(|b| b.is_ascii_digit()))
}

/// The bridge's number for a light, from its local id.
pub fn bridge_number(local_id: &str) -> Option<&str> {
    local_id.strip_prefix("hue-").filter(|_| is_light_id(local_id))
}

/// Whether text can be a key: only letters, digits, `-` and `_`, so it can
/// never change the path it is put in.
pub fn is_usable_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_KEY_LEN
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 1 to 100 percent onto the bridge's 1 to 254, so every percentage reads back as itself.
fn percent_to_bri(percent: u8) -> u64 {
    1 + (u64::from(percent.clamp(1, 100) - 1) * 253 + 49) / 99
}

fn bri_to_percent(bri: u64) -> u8 {
    (1 + ((bri.clamp(1, 254) - 1) * 99 + 126) / 253) as u8
}

fn hue_to_wire(hue: u16) -> u64 {
    (u64::from(hue % 360) * 65_535 + 180) / 360
}

fn wire_to_hue(value: u64) -> u16 {
    (((value.min(65_535) * 360 + 32_767) / 65_535) % 360) as u16
}

/// What to send the bridge for this operation. A brightness of zero means off.
pub(crate) fn wire_state(op: &Op) -> Option<Value> {
    Some(match op {
        Op::LightPower { on } => json!({ "on": on }),
        Op::LightBrightness { percent: 0 } | Op::LightSet { percent: 0, .. } => json!({ "on": false }),
        Op::LightBrightness { percent } => json!({ "on": true, "bri": percent_to_bri(*percent) }),
        Op::LightHue { hue } => json!({ "on": true, "hue": hue_to_wire(*hue), "sat": FULL_SATURATION }),
        Op::LightSet { percent, hue } => {
            json!({ "on": true, "bri": percent_to_bri(*percent), "hue": hue_to_wire(*hue), "sat": FULL_SATURATION })
        }
        _ => return None,
    })
}

pub(crate) fn unreadable() -> DeviceError {
    DeviceError::Rejected("the bridge sent an unreadable reply".to_string())
}

/// Only the error's number is used. Its `address` and `description` are the
/// bridge's own text, and could repeat the path the key is in.
fn refused_by_bridge(error: &Value) -> DeviceError {
    let text = match error.get("type").and_then(Value::as_i64) {
        Some(1) => "the bridge did not accept the application key".to_string(),
        Some(3) => "the bridge does not have this light any more".to_string(),
        Some(6 | 7) => "the light cannot do that".to_string(),
        Some(201) => "the light is off, so that cannot be changed".to_string(),
        Some(code) => format!("the bridge refused with error {code}"),
        None => "the bridge refused".to_string(),
    };
    DeviceError::Rejected(text)
}

fn error_entry(body: &Value) -> Option<DeviceError> {
    body.as_array()?.iter().find_map(|entry| entry.get("error")).map(refused_by_bridge)
}

pub(crate) fn refused_status(reply: &Reply) -> DeviceError {
    DeviceError::Rejected(format!("the bridge refused with HTTP {}", reply.status.as_u16()))
}

/// The body of a successful reply, or the bridge's refusal by number.
pub(crate) fn checked(reply: &Reply) -> Result<Value, DeviceError> {
    if !reply.status.is_success() {
        return Err(refused_status(reply));
    }
    let body = reply.json()?;
    match error_entry(&body) {
        Some(error) => Err(error),
        None => Ok(body),
    }
}

/// Whether every entry of a reply to a change says it worked.
pub(crate) fn all_succeeded(entries: &Value) -> Result<(), DeviceError> {
    match entries.as_array() {
        Some(list) if !list.is_empty() && list.iter().all(|entry| entry.get("success").is_some()) => Ok(()),
        _ => Err(unreadable()),
    }
}

/// Whether a light can show colour.
pub(crate) fn has_colour(light: &Value) -> bool {
    light.pointer("/state/hue").is_some()
}

/// What a light in white mode reports as its hue. It only tints the app:
/// `color` stays false, so it can never confirm a colour change.
fn white_hue(kelvin: Option<u64>) -> u16 {
    match kelvin {
        Some(kelvin) if kelvin <= 3500 => 34,
        Some(kelvin) if kelvin >= 5000 => 210,
        _ => 40,
    }
}

pub(crate) fn bulb_state(light: &Value) -> Result<State, DeviceError> {
    let state = light.get("state").ok_or_else(unreadable)?;
    if state.get("reachable").and_then(Value::as_bool) == Some(false) {
        return Err(DeviceError::Unreachable("the bridge cannot reach this light".to_string()));
    }
    let on = state.get("on").and_then(Value::as_bool).ok_or_else(unreadable)?;
    // A plug has no brightness of its own.
    let bri = state.get("bri").and_then(Value::as_u64).map_or(if on { 100 } else { 0 }, bri_to_percent);
    let colour = state
        .get("hue")
        .and_then(Value::as_u64)
        .filter(|_| state.get("colormode").and_then(Value::as_str) != Some("ct"))
        .map(wire_to_hue);
    let kelvin = state.get("ct").and_then(Value::as_u64).filter(|mired| *mired > 0).map(|mired| 1_000_000 / mired);
    Ok(State::Light { on, bri, hue: colour.unwrap_or_else(|| white_hue(kelvin)), color: colour.is_some() })
}

/// What the bridge calls a light, made safe to show and to pass the checks on a name.
pub(crate) fn light_name(light: &Value, id: &str) -> String {
    let name = swifty_net::device_text(light.get("name").and_then(Value::as_str).unwrap_or_default());
    match name.trim() {
        "" => format!("Hue light {id}"),
        name => name.chars().take(64).collect::<String>().trim().to_string(),
    }
}

/// The body that asks the bridge for a new application key.
pub fn pairing_request() -> Value {
    json!({ "devicetype": DEVICE_TYPE })
}

/// What a bridge said to a pairing request.
#[derive(PartialEq, Eq)]
pub(crate) enum Pairing {
    /// The new key.
    Key(String),
    /// Error 101: the link button has not been pressed.
    ButtonNotPressed,
    /// Anything else, as fixed text.
    Refused(DeviceError),
}

/// `Debug` hides the key: it is a credential from the moment the bridge gives it, and a debug print
/// of a pairing answer must never show it.
impl fmt::Debug for Pairing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Pairing::Key(_) => f.write_str("Key(<redacted>)"),
            Pairing::ButtonNotPressed => f.write_str("ButtonNotPressed"),
            Pairing::Refused(error) => f.debug_tuple("Refused").field(error).finish(),
        }
    }
}

pub(crate) fn pairing_answer(body: &Value) -> Pairing {
    if body.pointer("/0/error/type").and_then(Value::as_i64) == Some(101) {
        return Pairing::ButtonNotPressed;
    }
    if let Some(error) = error_entry(body) {
        return Pairing::Refused(error);
    }
    match body.pointer("/0/success/username").and_then(Value::as_str).filter(|key| is_usable_key(key)) {
        Some(key) => Pairing::Key(key.to_string()),
        None => Pairing::Refused(DeviceError::Rejected("the bridge did not give a usable key".to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_percentage_survives_the_trip_through_the_bridge() {
        for percent in 1..=100u8 {
            let bri = percent_to_bri(percent);
            assert!((1..=254).contains(&bri), "{percent} became {bri}");
            assert_eq!(bri_to_percent(bri), percent, "{percent} went out as {bri}");
        }
        assert_eq!(percent_to_bri(1), 1);
        assert_eq!(percent_to_bri(100), 254);
    }

    #[test]
    fn every_hue_survives_the_trip_through_the_bridge() {
        for hue in 0..360u16 {
            let wire = hue_to_wire(hue);
            assert!(wire <= 65_535, "{hue} became {wire}");
            assert_eq!(wire_to_hue(wire), hue, "{hue} went out as {wire}");
        }
        assert_eq!(wire_to_hue(65_535), 0, "the top of the scale is red again");
    }

    #[test]
    fn a_brightness_of_zero_is_off() {
        assert_eq!(wire_state(&Op::LightBrightness { percent: 0 }), Some(json!({ "on": false })));
        assert_eq!(wire_state(&Op::LightSet { percent: 0, hue: 10 }), Some(json!({ "on": false })));
        assert_eq!(wire_state(&Op::Play), None);
    }

    #[test]
    fn a_bridge_error_never_repeats_the_bridge() {
        let error =
            refused_by_bridge(&json!({ "type": 7, "address": "/api/SECRET/lights/1", "description": "SECRET" }));
        assert!(!error.to_string().contains("SECRET"), "{error}");
        let unknown = refused_by_bridge(&json!({ "type": 999, "description": "SECRET" }));
        assert!(unknown.to_string().contains("999") && !unknown.to_string().contains("SECRET"), "{unknown}");
    }

    #[test]
    fn white_mode_is_not_colour() {
        let white = bulb_state(&json!({ "state": { "on": true, "bri": 254, "hue": 8000, "ct": 366, "colormode": "ct", "reachable": true } })).unwrap();
        assert!(matches!(white, State::Light { color: false, hue: 34, .. }), "{white:?}");
        let colour = bulb_state(
            &json!({ "state": { "on": true, "bri": 254, "hue": 21845, "colormode": "hs", "reachable": true } }),
        )
        .unwrap();
        assert!(matches!(colour, State::Light { color: true, hue: 120, .. }), "{colour:?}");
    }

    #[test]
    fn a_name_from_the_bridge_is_made_safe() {
        assert_eq!(light_name(&json!({ "name": "  Strip\u{7}  " }), "4"), "Strip");
        assert_eq!(light_name(&json!({ "name": "\u{7}" }), "4"), "Hue light 4");
        assert_eq!(light_name(&json!({}), "4"), "Hue light 4");
    }

    #[test]
    fn a_light_id_is_hue_and_the_bridges_number() {
        assert!(is_light_id("hue-1") && is_light_id("hue-99999"));
        assert_eq!(bridge_number("hue-12"), Some("12"));
        for bad in ["hue", "hue-", "hue-0", "hue-01", "hue-1/../2", "hue-a", "hue-123456", "bulb1"] {
            assert!(!is_light_id(bad), "{bad}");
            assert_eq!(bridge_number(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_pairing_answer_is_a_usable_key_or_a_fixed_reason() {
        let key = json!([{ "success": { "username": "abc-DEF_123" } }]);
        assert_eq!(pairing_answer(&key), Pairing::Key("abc-DEF_123".to_string()));
        let early = json!([{ "error": { "type": 101, "description": "link button not pressed" } }]);
        assert_eq!(pairing_answer(&early), Pairing::ButtonNotPressed);
        for bad in [
            json!([{ "success": { "username": "../../x" } }]),
            json!([{ "success": { "username": "" } }]),
            json!([{ "success": { "username": "k".repeat(129) } }]),
            json!([{ "error": { "type": 7, "description": "SECRET" } }]),
            json!({}),
        ] {
            match pairing_answer(&bad) {
                Pairing::Refused(error) => assert!(!error.to_string().contains("SECRET"), "{error}"),
                other => panic!("{bad}: {other:?}"),
            }
        }
        assert_eq!(pairing_request(), json!({ "devicetype": "swifty_home#hub" }));
    }

    #[test]
    fn a_debug_print_of_a_pairing_answer_never_shows_the_key() {
        let key = json!([{ "success": { "username": "abc-DEF_123" } }]);
        let printed = format!("{:?} {:#?}", pairing_answer(&key), pairing_answer(&key));
        assert!(!printed.contains("abc-DEF_123"), "{printed}");
        assert!(printed.contains("redacted"), "{printed}");
        assert_eq!(format!("{:?}", Pairing::ButtonNotPressed), "ButtonNotPressed");
    }
}
