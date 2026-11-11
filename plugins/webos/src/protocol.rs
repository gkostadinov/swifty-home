//! SSAP, the TV's JSON-over-websocket protocol, and the Wake-on-LAN packet.
//! Pure: what is sent and how replies are read. Nothing here touches the network.

use serde_json::{json, Value};
use swifty_net::device_text;
use swifty_plugin_api::DeviceError;

/// The id of the registration message, and of the TV's answers to it.
pub const REGISTER_ID: &str = "register_0";
/// A TV can send unrelated messages. This bounds how many are skipped.
pub const MAX_SKIPPED: usize = 8;
/// Where a Wake-on-LAN packet goes when the config does not say.
pub const DEFAULT_BROADCAST: &str = "255.255.255.255:9";
/// The longest input name kept from what the TV reports.
pub const MAX_INPUT_LEN: usize = 64;
/// The longest client key accepted from the TV. Real keys are 32 hex digits.
pub const MAX_KEY_LEN: usize = 128;
/// Where a key press asks for the pointer socket.
pub const POINTER_SOCKET_URI: &str = "ssap://com.webos.service.networkinput/getPointerInputSocket";
/// Every webOS TV also listens here without TLS, and may describe a socket by it.
pub const PLAIN_PORT: u16 = 3000;

/// Each input by its name in the catalog, and the app the TV runs for it.
pub const INPUTS: &[(&str, &str)] = &[
    ("HDMI 1", "com.webos.app.hdmi1"),
    ("HDMI 2", "com.webos.app.hdmi2"),
    ("HDMI 3", "com.webos.app.hdmi3"),
    ("HDMI 4", "com.webos.app.hdmi4"),
    ("Live TV", "com.webos.app.livetv"),
];

/// Each button on the remote and what the TV calls it.
pub const BUTTONS: &[(&str, &str)] = &[
    ("up", "UP"),
    ("down", "DOWN"),
    ("left", "LEFT"),
    ("right", "RIGHT"),
    ("ok", "ENTER"),
    ("back", "BACK"),
    ("home", "HOME"),
    ("exit", "EXIT"),
    ("volume_up", "VOLUMEUP"),
    ("volume_down", "VOLUMEDOWN"),
    ("mute", "MUTE"),
    ("channel_up", "CHANNELUP"),
    ("channel_down", "CHANNELDOWN"),
    ("play", "PLAY"),
    ("pause", "PAUSE"),
];

/// What the TV is asked to allow. Pairing asks for exactly this and the driver
/// presents exactly this. The TV grants these when it is paired, so changing
/// this means pairing it again.
pub const PERMISSIONS: &[&str] = &[
    "CONTROL_POWER",
    "READ_POWER_STATE",
    "LAUNCH",
    "READ_RUNNING_APPS",
    "READ_INPUT_DEVICE_LIST",
    "CONTROL_INPUT_TV",
    "CONTROL_AUDIO",
    "CONTROL_INPUT_MEDIA_PLAYBACK",
    "CONTROL_INPUT_JOYSTICK",
    "CONTROL_MOUSE_AND_KEYBOARD",
];

/// The name the TV gives a button, if `key` is one.
pub fn button_name(key: &str) -> Option<&'static str> {
    BUTTONS.iter().find(|(name, _)| *name == key).map(|(_, button)| *button)
}

/// The app the TV runs for an input, if `input` is one.
pub fn input_app(input: &str) -> Option<&'static str> {
    INPUTS.iter().find(|(name, _)| *name == input).map(|(_, id)| *id)
}

/// The input an app stands for, or the app's id, cleaned and bounded.
pub fn input_name(app_id: &str) -> String {
    INPUTS
        .iter()
        .find(|(_, id)| *id == app_id)
        .map(|(name, _)| name.to_string())
        .unwrap_or_else(|| device_text(app_id).chars().take(MAX_INPUT_LEN).collect())
}

fn manifest() -> Value {
    json!({ "manifestVersion": 1, "permissions": PERMISSIONS })
}

/// The registration that presents a stored key. It never forces pairing: a TV
/// that does not know the key offers a prompt instead, and the driver hangs up.
pub fn register_with_key(key: &str) -> String {
    json!({
        "type": "register",
        "id": REGISTER_ID,
        "payload": {
            "forcePairing": false,
            "pairingType": "PROMPT",
            "client-key": key,
            "manifest": manifest(),
        },
    })
    .to_string()
}

/// The registration that asks for a new key: no key, and a prompt on the TV.
/// Forced, so that pairing again (after the permissions change) still prompts.
pub fn register_for_pairing() -> String {
    json!({
        "type": "register",
        "id": REGISTER_ID,
        "payload": { "forcePairing": true, "pairingType": "PROMPT", "manifest": manifest() },
    })
    .to_string()
}

/// One request.
pub fn request(id: &str, uri: &str, payload: Value) -> String {
    json!({ "type": "request", "id": id, "uri": uri, "payload": payload }).to_string()
}

/// One button press on the pointer socket.
pub fn button_frame(button: &str) -> String {
    format!("type:button\nname:{button}\n\n")
}

/// The standard Wake-on-LAN packet: six 0xFF, then the MAC sixteen times.
pub fn magic_packet(mac: [u8; 6]) -> Vec<u8> {
    let mut packet = vec![0xFF; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&mac);
    }
    packet
}

/// The error for a reply that cannot be read.
pub fn unreadable() -> DeviceError {
    DeviceError::Rejected("the TV sent an unreadable reply".to_string())
}

/// A message from the TV, as JSON.
pub fn parse(text: &str) -> Result<Value, DeviceError> {
    serde_json::from_str(text).map_err(|_| unreadable())
}

/// The message's id, when it has one.
pub fn id_of(message: &Value) -> Option<&str> {
    message.get("id").and_then(Value::as_str)
}

/// The message's type, when it has one.
pub fn type_of(message: &Value) -> Option<&str> {
    message.get("type").and_then(Value::as_str)
}

/// What a reply to a request means: its payload, or the TV's refusal.
pub fn reply(message: &Value) -> Result<Value, DeviceError> {
    let refused = |text: &str| DeviceError::Rejected(format!("the TV said: {}", device_text(text)));
    match type_of(message) {
        Some("response") => {
            let payload = message.get("payload").cloned().unwrap_or(Value::Null);
            if payload.get("returnValue").and_then(Value::as_bool) == Some(false) {
                Err(refused(payload.get("errorText").and_then(Value::as_str).unwrap_or("it refused")))
            } else {
                Ok(payload)
            }
        }
        Some("error") => Err(refused(message.get("error").and_then(Value::as_str).unwrap_or("it refused"))),
        _ => Err(unreadable()),
    }
}

/// The key in a `registered` message, if it is one that can be stored. Never
/// shown: a key the TV sends that cannot be used is reported without it.
pub fn client_key(message: &Value) -> Option<String> {
    let key = message.get("payload")?.get("client-key")?.as_str()?;
    let usable = !key.is_empty() && key.len() <= MAX_KEY_LEN && key.chars().all(|c| c.is_ascii_graphic());
    usable.then(|| key.to_string())
}

/// Whether the TV says it is on, from its power state. No state at all is on:
/// firmware without the call only answers when it is on.
pub fn is_on(power: &Value) -> bool {
    matches!(power.get("state").and_then(Value::as_str), None | Some("Active") | Some("Screen Saver"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_magic_packet_has_the_standard_shape() {
        let mac = [1, 2, 3, 4, 5, 6];
        let packet = magic_packet(mac);
        assert_eq!(packet.len(), 102);
        assert_eq!(&packet[..6], &[0xFF; 6]);
        assert!(packet[6..].chunks(6).all(|chunk| chunk == mac));
    }

    /// The catalog's `TV_KEYS`, as the hub has them. The plugin cannot depend on the hub.
    const CATALOG_TV_KEYS: &[&str] = &[
        "up",
        "down",
        "left",
        "right",
        "ok",
        "back",
        "home",
        "exit",
        "volume_up",
        "volume_down",
        "mute",
        "channel_up",
        "channel_down",
        "play",
        "pause",
    ];
    /// The catalog's `TV_INPUTS`, as the hub has them.
    const CATALOG_TV_INPUTS: &[&str] = &["HDMI 1", "HDMI 2", "HDMI 3", "HDMI 4", "Live TV"];

    #[test]
    fn every_key_in_the_catalog_is_a_button() {
        for key in CATALOG_TV_KEYS {
            assert!(button_name(key).is_some(), "{key}");
        }
        assert_eq!(BUTTONS.len(), CATALOG_TV_KEYS.len());
        assert_eq!(button_name("ok"), Some("ENTER"));
        assert_eq!(button_name("UP"), None);
    }

    #[test]
    fn inputs_map_both_ways() {
        for input in CATALOG_TV_INPUTS {
            let (_, id) = INPUTS.iter().find(|(name, _)| name == input).unwrap();
            assert_eq!(input_name(id), *input);
        }
        assert_eq!(input_name("netflix"), "netflix");
    }

    #[test]
    fn pairing_asks_without_a_key_and_the_driver_never_forces_it() {
        let pairing = parse(&register_for_pairing()).unwrap();
        assert!(pairing["payload"].get("client-key").is_none());
        assert_eq!(pairing["payload"]["forcePairing"], json!(true));
        let presenting = parse(&register_with_key("k")).unwrap();
        assert_eq!(presenting["payload"]["client-key"], json!("k"));
        assert_eq!(presenting["payload"]["forcePairing"], json!(false));
        assert_eq!(pairing["payload"]["manifest"], presenting["payload"]["manifest"]);
    }

    #[test]
    fn only_a_usable_key_is_taken_from_the_tv() {
        let registered =
            |key: Value| json!({ "type": "registered", "id": REGISTER_ID, "payload": { "client-key": key } });
        assert_eq!(client_key(&registered(json!("0123abcd"))).as_deref(), Some("0123abcd"));
        for bad in [json!(""), json!("a b"), json!("a\nb"), json!(7), json!("k".repeat(MAX_KEY_LEN + 1))] {
            assert_eq!(client_key(&registered(bad.clone())), None, "{bad}");
        }
        assert_eq!(client_key(&json!({ "type": "registered" })), None);
    }
}
