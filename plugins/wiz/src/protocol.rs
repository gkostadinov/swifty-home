//! The WiZ wire format: JSON over UDP port 38899, with no key. State is read
//! back with `getPilot`. Everything here is pure: it encodes and decodes, and
//! opens nothing.

use swifty_net::{IpAddr, SocketAddr};

use serde_json::{json, Value};
use swifty_plugin_api::{DeviceError, Op, State, MIN_LIGHT_DIMMING};

/// The port every WiZ bulb listens on.
pub const DEFAULT_PORT: u16 = 38899;

/// The prefix of a bulb found by discovery. The rest is its MAC in lower-case hex.
pub const DISCOVERED_PREFIX: &str = "wiz-";

/// What a discovery broadcast asks. Nothing here changes a bulb.
pub fn discovery_message() -> Value {
    json!({ "method": "getPilot", "params": {} })
}

/// Asks one bulb for its state.
pub fn get_pilot() -> Value {
    json!({ "id": 1, "method": "getPilot", "params": {} })
}

/// Sets one bulb, or `None` for an operation a bulb cannot do.
pub fn set_pilot(op: &Op) -> Option<Value> {
    Some(json!({ "id": 1, "method": "setPilot", "params": set_pilot_params(op)? }))
}

/// Whether a reply can be from a bulb that heard this broadcast. A bulb answers from
/// the port it listens on, and a directed broadcast only reaches its own network.
pub(crate) fn answered_the_broadcast(broadcast: SocketAddr, from: SocketAddr) -> bool {
    let (IpAddr::V4(sent_to), IpAddr::V4(ip)) = (broadcast.ip(), from.ip()) else {
        return false;
    };
    if from.port() != broadcast.port() || ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast() {
        return false;
    }
    // The limited broadcast says nothing about the network, and stays on the link.
    if sent_to.is_broadcast() {
        return true;
    }
    // A directed broadcast is the top address of its network, so its trailing one
    // bits are the host part.
    let mask = u32::MAX.checked_shl(u32::from(sent_to).trailing_ones()).unwrap_or(0);
    u32::from(ip) & mask == u32::from(sent_to) & mask
}

/// The MAC from a bulb's reply, if this really is one: twelve hex digits, so the
/// value can become part of a device id.
pub(crate) fn discovered_mac(reply: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(reply).ok()?;
    if value.get("method").and_then(Value::as_str) != Some("getPilot") {
        return None;
    }
    let mac = value.pointer("/result/mac")?.as_str()?.to_ascii_lowercase();
    (mac.len() == 12 && mac.bytes().all(|b| b.is_ascii_hexdigit())).then_some(mac)
}

/// The local id of a bulb found by discovery.
pub fn discovered_id(mac: &str) -> String {
    format!("{DISCOVERED_PREFIX}{mac}")
}

/// A bulb found by discovery: `wiz-` and a lower-case MAC.
pub fn is_discovered_id(local_id: &str) -> bool {
    local_id
        .strip_prefix(DISCOVERED_PREFIX)
        .is_some_and(|mac| mac.len() == 12 && mac.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

/// One fully saturated colour for a hue angle.
pub fn hue_to_rgb(hue: u16) -> (u8, u8, u8) {
    let hue = f64::from(hue % 360);
    let sector = (hue / 60.0).floor() as u8;
    let fraction = hue / 60.0 - f64::from(sector);
    let rising = (fraction * 255.0).round() as u8;
    let falling = 255 - rising;
    match sector {
        0 => (255, rising, 0),
        1 => (falling, 255, 0),
        2 => (0, 255, rising),
        3 => (0, falling, 255),
        4 => (rising, 0, 255),
        _ => (255, 0, falling),
    }
}

/// The hue angle of a colour, or `None` for a grey with no hue.
pub fn rgb_to_hue(r: u8, g: u8, b: u8) -> Option<u16> {
    let (r, g, b) = (f64::from(r), f64::from(g), f64::from(b));
    let max = r.max(g).max(b);
    let delta = max - r.min(g).min(b);
    if delta == 0.0 {
        return None;
    }
    let angle = if max == r {
        60.0 * ((g - b) / delta)
    } else if max == g {
        60.0 * ((b - r) / delta) + 120.0
    } else {
        60.0 * ((r - g) / delta) + 240.0
    };
    Some((angle.round() as i32).rem_euclid(360) as u16)
}

fn wire_dimming(percent: u8) -> u8 {
    percent.max(MIN_LIGHT_DIMMING)
}

/// What a bulb in white or scene mode reports as its hue. It only tints the
/// app: `color` stays false, so it can never confirm a colour change.
pub(crate) fn white_hue(temp: Option<u64>) -> u16 {
    match temp {
        Some(kelvin) if kelvin <= 3500 => 34,
        Some(kelvin) if kelvin >= 5000 => 210,
        _ => 40,
    }
}

pub(crate) fn unreadable() -> DeviceError {
    DeviceError::Rejected("the bulb sent an unreadable reply".to_string())
}

fn set_pilot_params(op: &Op) -> Option<Value> {
    let colour = |hue: u16| hue_to_rgb(hue);
    Some(match op {
        Op::LightPower { on } => json!({ "state": on }),
        Op::LightBrightness { percent } => {
            json!({ "state": true, "dimming": wire_dimming(*percent) })
        }
        Op::LightHue { hue } => {
            let (r, g, b) = colour(*hue);
            json!({ "state": true, "r": r, "g": g, "b": b })
        }
        Op::LightSet { percent, hue } => {
            let (r, g, b) = colour(*hue);
            json!({ "state": true, "dimming": wire_dimming(*percent), "r": r, "g": g, "b": b })
        }
        _ => return None,
    })
}

/// The `result` of a reply to `method`, or why there is none. Only the bulb's
/// error message is repeated, and only after it was made safe to show.
pub(crate) fn result_of(reply: &[u8], method: &str) -> Result<Value, DeviceError> {
    let value: Value = serde_json::from_slice(reply).map_err(|_| unreadable())?;
    if let Some(error) = value.get("error") {
        let text = error.get("message").and_then(Value::as_str).unwrap_or("error");
        return Err(DeviceError::Rejected(swifty_net::device_text(text)));
    }
    if value.get("method").and_then(Value::as_str) != Some(method) {
        return Err(unreadable());
    }
    value.get("result").cloned().ok_or_else(unreadable)
}

/// Whether the bulb took a `setPilot`.
pub(crate) fn accepted(result: &Value) -> Result<(), DeviceError> {
    match result.get("success").and_then(Value::as_bool) {
        Some(true) => Ok(()),
        _ => Err(DeviceError::Rejected("the bulb did not accept the command".to_string())),
    }
}

pub(crate) fn pilot_state(result: &Value) -> Result<State, DeviceError> {
    let on = result.get("state").and_then(Value::as_bool).ok_or_else(unreadable)?;
    let bri = result.get("dimming").and_then(Value::as_u64).unwrap_or(0).min(100) as u8;
    let channel = |name: &str| result.get(name).and_then(Value::as_u64).map(|v| v.min(255) as u8);
    let scene = result.get("sceneId").and_then(Value::as_u64).unwrap_or(0);
    let colour = match (channel("r"), channel("g"), channel("b")) {
        (Some(r), Some(g), Some(b)) if scene == 0 => rgb_to_hue(r, g, b),
        _ => None,
    };
    let temp = result.get("temp").and_then(Value::as_u64);
    Ok(State::Light { on, bri, hue: colour.unwrap_or_else(|| white_hue(temp)), color: colour.is_some() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_hue_survives_the_trip_through_rgb() {
        for hue in 0..360u16 {
            let (r, g, b) = hue_to_rgb(hue);
            assert_eq!(rgb_to_hue(r, g, b), Some(hue), "hue {hue} became rgb {r},{g},{b}");
        }
    }

    #[test]
    fn only_a_real_bulb_reply_counts_as_found() {
        let good = json!({ "method": "getPilot", "result": { "mac": "A8BB50AABBCC", "state": true } }).to_string();
        assert_eq!(discovered_mac(good.as_bytes()).as_deref(), Some("a8bb50aabbcc"));
        for bad in [
            json!({ "method": "getPilot", "result": { "mac": "../../etc" } }).to_string(),
            json!({ "method": "getPilot", "result": { "state": true } }).to_string(),
            json!({ "method": "other", "result": { "mac": "a8bb50aabbcc" } }).to_string(),
            "not json".to_string(),
        ] {
            assert_eq!(discovered_mac(bad.as_bytes()), None, "{bad}");
        }
    }

    #[test]
    fn only_a_reply_that_could_be_a_bulb_counts() {
        let address = |text: &str| -> SocketAddr { text.parse().unwrap() };
        let lan = address("192.0.2.255:38899");
        assert!(answered_the_broadcast(lan, address("192.0.2.40:38899")));
        assert!(!answered_the_broadcast(lan, address("192.0.2.40:53812")), "another port");
        assert!(!answered_the_broadcast(lan, address("198.51.100.40:38899")), "another network");
        assert!(!answered_the_broadcast(lan, address("0.0.0.0:38899")));
        assert!(!answered_the_broadcast(lan, address("[fd00::5]:38899")));

        // The top address of a /16, which no documentation range is: given as numbers, not written out.
        let sixteen = SocketAddr::from(([192, 0, 255, 255], 38899));
        assert!(answered_the_broadcast(sixteen, address("192.0.2.9:38899")));
        assert!(!answered_the_broadcast(sixteen, address("198.51.100.9:38899")));

        let everyone = address("255.255.255.255:38899");
        assert!(answered_the_broadcast(everyone, address("198.51.100.9:38899")));
        assert!(!answered_the_broadcast(everyone, address("198.51.100.9:1")));
        assert!(!answered_the_broadcast(everyone, address("224.0.0.1:38899")));

        let one_bulb = address("192.0.2.40:38899");
        assert!(answered_the_broadcast(one_bulb, address("192.0.2.40:38899")));
        assert!(!answered_the_broadcast(one_bulb, address("192.0.2.41:38899")));
    }

    #[test]
    fn grey_has_no_hue() {
        assert_eq!(rgb_to_hue(0, 0, 0), None);
        assert_eq!(rgb_to_hue(9, 9, 9), None);
    }

    #[test]
    fn dimming_on_the_wire_respects_the_bulb_minimum() {
        let params = set_pilot_params(&Op::LightBrightness { percent: 4 }).unwrap();
        assert_eq!(params, json!({ "state": true, "dimming": 10 }));
        let params = set_pilot_params(&Op::LightBrightness { percent: 55 }).unwrap();
        assert_eq!(params, json!({ "state": true, "dimming": 55 }));
    }

    #[test]
    fn white_and_scene_modes_are_not_colour() {
        let white = pilot_state(&json!({ "state": true, "dimming": 50, "temp": 2700 })).unwrap();
        assert_eq!(white, State::Light { on: true, bri: 50, hue: 34, color: false });
        let scene =
            pilot_state(&json!({ "state": true, "dimming": 50, "sceneId": 6, "r": 255, "g": 0, "b": 0 })).unwrap();
        assert!(matches!(scene, State::Light { color: false, .. }));
        assert!(pilot_state(&json!({ "dimming": 50 })).is_err());
    }

    #[test]
    fn a_discovered_id_is_wiz_and_a_lower_case_mac() {
        assert!(is_discovered_id("wiz-a8bb50aabbcc"));
        assert_eq!(discovered_id("a8bb50aabbcc"), "wiz-a8bb50aabbcc");
        for bad in ["wiz-", "wiz-A8BB50AABBCC", "wiz-a8bb50aabbc", "wiz-a8bb50aabbccd", "wiz-g8bb50aabbcc", "bulb1"] {
            assert!(!is_discovered_id(bad), "{bad}");
        }
    }

    #[test]
    fn a_reply_is_read_only_for_the_method_that_was_asked() {
        let ok = json!({ "method": "setPilot", "result": { "success": true } }).to_string();
        assert!(accepted(&result_of(ok.as_bytes(), "setPilot").unwrap()).is_ok());
        assert!(result_of(ok.as_bytes(), "getPilot").is_err(), "an answer to another question");
        let refused = json!({ "method": "setPilot", "error": { "message": "no\u{7}" } }).to_string();
        assert_eq!(result_of(refused.as_bytes(), "setPilot"), Err(DeviceError::Rejected("no".to_string())));
        let no = json!({ "method": "setPilot", "result": { "success": false } }).to_string();
        assert!(accepted(&result_of(no.as_bytes(), "setPilot").unwrap()).is_err());
        assert_eq!(set_pilot(&Op::Play), None);
    }
}
