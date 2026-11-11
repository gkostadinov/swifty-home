//! What goes over the wire, and nothing that sends it: the unit's compact state, the
//! command message, the hub token and the stored sign-in.
//!
//! The field layout and the message come from the open-source `toshiba-ac` library,
//! which sends the same message over AMQP (see `DEVELOPING.md`).

use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use http::HeaderValue;
use serde_json::Value;
use swifty_plugin_api::{DeviceError, Op, Secret, State};
use url::Url;

pub const LOGIN_PATH: &str = "/api/Consumer/Login";
pub const REGISTER_PATH: &str = "/api/Consumer/RegisterMobileDevice";
pub const STATE_PATH: &str = "/api/AC/GetCurrentACState";
/// The units on the account. Only the setup asks for it.
pub const MAPPING_PATH: &str = "/api/AC/GetConsumerACMapping";
pub const HUB_API_VERSION: &str = "2020-03-13";
/// The header that says which client is asking. Without it every sign-in is answered 429.
pub const DEVICE_ID: &str = "Device-ID";

/// The key under which Toshiba's sign-in takes the account's sign-in secret. It is
/// Toshiba's name for the field, which the hub's source scan allows as this plugin's
/// field label. Nothing else in this crate is meant by it.
pub const SIGN_IN_SECRET_KEY: &str = "Password";

/// A hub token is renewed when it has less than this long to run.
pub const HUB_TOKEN_MARGIN: u64 = 600;
/// What a token with no expiry in it is taken to be good for.
pub const HUB_TOKEN_GUESS: u64 = 3600;

/// How many hex digits the compact state has. Each field below starts at that digit.
pub const STATE_LEN: usize = 38;
pub const POWER_AT: usize = 0;
pub const MODE_AT: usize = 2;
pub const TARGET_AT: usize = 4;
pub const FAN_AT: usize = 6;
/// Merit A takes one digit, so the fields after it are at odd-looking places.
pub const MERIT_A_AT: usize = 13;
pub const INDOOR_AT: usize = 16;
pub const OUTDOOR_AT: usize = 18;
pub const CLEANING_AT: usize = 28;

pub const POWER_ON: u8 = 0x30;
pub const POWER_OFF: u8 = 0x31;
pub const CLEANING_ON: u8 = 0x18;
pub const CLEANING_OFF: u8 = 0x10;
const MERIT_HEATING_8C: u8 = 0x4;
const HEATING_8C_OFFSET: i16 = 16;

pub const MODE_CODES: [(&str, u8); 5] = [("auto", 0x41), ("cool", 0x42), ("heat", 0x43), ("dry", 0x44), ("fan", 0x45)];
pub const FAN_CODES: [(&str, u8); 7] = [
    ("auto", 0x41),
    ("quiet", 0x31),
    ("low", 0x32),
    ("medium_low", 0x33),
    ("medium", 0x34),
    ("medium_high", 0x35),
    ("high", 0x36),
];

/// The setpoints a unit accepts, in Celsius.
pub const TARGET_MIN: u8 = 17;
pub const TARGET_MAX: u8 = 30;

pub fn unreadable() -> DeviceError {
    DeviceError::Rejected("Toshiba's cloud sent a reply that could not be read".to_string())
}

pub fn cannot() -> DeviceError {
    DeviceError::Disabled("An air conditioner cannot do that.".to_string())
}

/// This service's id at the cloud, the shape the Toshiba app's own ids have.
pub fn client_id(email: &str, suffix: &str) -> String {
    format!("{email}_{suffix}")
}

pub fn now_seconds() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs())
}

/// Everything but the unreserved characters, as a URL path needs.
pub fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

pub fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let digits = text.get(at + 1..at + 3)?;
            if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return None;
            }
            out.push(u8::from_str_radix(digits, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Reads a text field as a JSON string, for building a message by hand.
pub fn quoted(text: &str) -> String {
    Value::String(text.to_string()).to_string()
}

/// A temperature byte, which is signed. Three values mean there is no reading.
pub fn temperature(raw: u8) -> Option<i16> {
    match raw as i8 {
        -1 | 127 | -128 => None,
        126 => Some(-1),
        degrees => Some(i16::from(degrees)),
    }
}

/// What one state string says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    pub on: bool,
    pub mode: Option<&'static str>,
    pub target: Option<i16>,
    pub fan: Option<&'static str>,
    pub indoor: Option<i16>,
    pub outdoor: Option<i16>,
    pub cleaning: bool,
    /// Holding the room at 8 degrees. Its setpoint is reported 16 too high.
    pub heating_8c: bool,
}

impl Reading {
    pub fn parse(state: &str) -> Result<Self, DeviceError> {
        if state.len() < STATE_LEN || !state.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(unreadable());
        }
        let byte = |at: usize| -> u8 {
            state.get(at..at + 2).and_then(|digits| u8::from_str_radix(digits, 16).ok()).unwrap_or(0xFF)
        };
        let named = |table: &[(&'static str, u8)], code: u8| {
            table.iter().find(|(_, value)| *value == code).map(|(name, _)| *name)
        };
        let on = match byte(POWER_AT) {
            POWER_ON => true,
            POWER_OFF => false,
            _ => return Err(DeviceError::Rejected("the air conditioner has not reported its state yet".to_string())),
        };
        let mode = named(&MODE_CODES, byte(MODE_AT));
        let merit_a = state.get(MERIT_A_AT..MERIT_A_AT + 1).and_then(|digit| u8::from_str_radix(digit, 16).ok());
        let heating_8c = mode == Some("heat") && merit_a == Some(MERIT_HEATING_8C);
        let target =
            temperature(byte(TARGET_AT)).map(|degrees| if heating_8c { degrees - HEATING_8C_OFFSET } else { degrees });
        Ok(Self {
            on,
            mode,
            target,
            fan: named(&FAN_CODES, byte(FAN_AT)),
            indoor: temperature(byte(INDOOR_AT)),
            outdoor: temperature(byte(OUTDOOR_AT)),
            cleaning: byte(CLEANING_AT) == CLEANING_ON,
            heating_8c,
        })
    }

    pub fn device_state(&self) -> State {
        State::Aircon {
            on: self.on,
            mode: self.mode.map(str::to_string),
            target: self.target,
            fan: self.fan.map(str::to_string),
            indoor: self.indoor,
            outdoor: self.outdoor,
        }
    }
}

/// A state string that changes the given fields and leaves every other as it is.
pub fn command_data(changes: &[(usize, u8)]) -> String {
    let mut digits = "f".repeat(STATE_LEN);
    for (at, value) in changes {
        digits.replace_range(*at..*at + 2, &format!("{value:02x}"));
    }
    digits
}

pub fn code(table: &[(&str, u8)], name: &str) -> Result<u8, DeviceError> {
    table.iter().find(|(known, _)| *known == name).map(|(_, value)| *value).ok_or_else(cannot)
}

/// Which fields this operation changes. `current` is the state, where the operation depends on it.
pub fn changes_for(op: &Op, current: Option<&Reading>) -> Result<Vec<(usize, u8)>, DeviceError> {
    match op {
        Op::AirconPower { on: true } => {
            let mut changes = vec![(POWER_AT, POWER_ON)];
            // A unit cleaning itself will not start until that is called off.
            if current.is_some_and(|reading| reading.cleaning) {
                changes.push((CLEANING_AT, CLEANING_OFF));
            }
            Ok(changes)
        }
        Op::AirconPower { on: false } => Ok(vec![(POWER_AT, POWER_OFF)]),
        Op::AirconMode { mode } => Ok(vec![(MODE_AT, code(&MODE_CODES, mode)?)]),
        Op::AirconFan { fan } => Ok(vec![(FAN_AT, code(&FAN_CODES, fan)?)]),
        Op::AirconTemp { celsius } => {
            if current.is_some_and(|reading| reading.heating_8c) {
                return Err(DeviceError::Rejected(
                    "the unit is holding 8 degrees. Turn that off on its remote or in the Toshiba app first"
                        .to_string(),
                ));
            }
            Ok(vec![(TARGET_AT, *celsius)])
        }
        _ => Err(cannot()),
    }
}

/// The message one command is, in the library's own field order.
pub fn message_body(device_id: &str, unique_id: &str, data: &str) -> String {
    format!(
        r#"{{"sourceId":{},"messageId":"0000000","targetId":[{}],"cmd":"CMD_FCU_TO_AC","payload":{{"data":"{data}"}},"timeStamp":"0000000"}}"#,
        quoted(device_id),
        quoted(unique_id),
    )
}

/// The stored sign-in is one line: base64 of the JSON that signs in, exactly as the
/// cloud takes it. This service sends it as it stands and looks only at the account's
/// name in it, and gives back that name and the JSON.
pub fn load_credentials(text: &str) -> Option<(Secret, Secret)> {
    let plain = String::from_utf8(STANDARD.decode(text.trim().as_bytes()).ok()?).ok()?;
    let body: Value = serde_json::from_str(&plain).ok()?;
    let email = body.get("Username")?.as_str().filter(|email| !email.is_empty())?.to_string();
    (body.as_object()?.len() >= 2).then(|| (Secret::new(email), Secret::new(plain)))
}

/// The stored sign-in for an account, as the setup writes it: the JSON Toshiba's app
/// sends, in its order, then base64.
pub fn encode_credentials(email: &str, sign_in_secret: &str) -> String {
    let json = format!("{{\"Username\": {}, \"{SIGN_IN_SECRET_KEY}\": {}}}", quoted(email), quoted(sign_in_secret));
    STANDARD.encode(json)
}

/// A header value the client never prints.
pub fn sensitive(text: &str) -> Option<HeaderValue> {
    let mut value = HeaderValue::from_str(text).ok()?;
    value.set_sensitive(true);
    Some(value)
}

#[derive(Clone)]
pub struct HubToken {
    pub authorization: HeaderValue,
    pub device_id: String,
    pub expires_at: u64,
}

impl HubToken {
    /// Takes a token apart, and only if it is for this hub, so that it is never sent
    /// anywhere else and no command can be aimed at a hub named by the reply.
    pub fn parse(token: &str, hub: &Url) -> Option<Self> {
        let body = token.strip_prefix("SharedAccessSignature ")?;
        let (mut resource, mut expiry) = (None, None);
        for part in body.split('&') {
            let (name, value) = part.split_once('=')?;
            match name {
                "sr" => resource = Some(value),
                "se" => expiry = value.parse::<u64>().ok(),
                _ => {}
            }
        }
        let resource = percent_decode(resource?)?;
        let (host, rest) = resource.split_once('/')?;
        let device_id = rest.strip_prefix("devices/")?;
        if device_id.is_empty() || device_id.contains('/') || !host.eq_ignore_ascii_case(hub.authority()) {
            return None;
        }
        Some(Self {
            authorization: sensitive(token)?,
            device_id: device_id.to_string(),
            expires_at: expiry.unwrap_or(now_seconds() + HUB_TOKEN_GUESS),
        })
    }

    pub fn lasting(&self) -> bool {
        self.expires_at > now_seconds() + HUB_TOKEN_MARGIN
    }
}

/// One unit on the account, as the cloud lists it, and only if this plugin can use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedUnit {
    pub id: String,
    pub unique_id: String,
    pub name: Option<String>,
}

/// The units of `GetConsumerACMapping`: a list of groups, each with an `ACList`. A unit
/// whose ids this plugin would not accept in its config is left out. A name keeps only
/// plain characters, so that it can go into the config and onto the page.
pub fn listed_units(result: &Value) -> Vec<ListedUnit> {
    let groups = result.as_array().map(Vec::as_slice).unwrap_or_default();
    let mut units = Vec::new();
    for unit in groups.iter().filter_map(|group| group.get("ACList")?.as_array()).flatten() {
        let text = |key: &str| unit.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
        let (id, unique_id) = (text("Id"), text("DeviceUniqueId"));
        if !crate::config::is_aircon_id(&id) || !crate::config::is_plain_unique_id(&unique_id) {
            continue;
        }
        if units.iter().any(|known: &ListedUnit| known.id.eq_ignore_ascii_case(&id)) {
            continue;
        }
        let name: String = text("Name")
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-'))
            .take(crate::config::MAX_NAME)
            .collect();
        let name = name.trim().to_string();
        units.push(ListedUnit { id, unique_id, name: (!name.is_empty()).then_some(name) });
    }
    units
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The values the hub's catalog offers for `ac.mode` and `ac.fan` (its `AC_MODES` and
    /// `AC_FANS`). A plugin does not see the catalog, so they are written out here.
    const AC_MODES: &[&str] = &["auto", "cool", "heat", "dry", "fan"];
    const AC_FANS: &[&str] = &["auto", "quiet", "low", "medium_low", "medium", "medium_high", "high"];

    /// Built field by field from the library's layout: cooling to 22, fan automatic,
    /// 24 inside and 10 outside.
    fn cooling() -> String {
        ["30", "42", "16", "41", "31", "64", "0", "0", "10", "18", "0a", "ffffffff", "10", "ffffffff"].concat()
    }

    fn hub() -> Url {
        Url::parse("https://example-hub.azure-devices.net").unwrap()
    }

    #[test]
    fn the_sample_is_the_length_the_cloud_sends() {
        assert_eq!(cooling().len(), STATE_LEN);
    }

    #[test]
    fn a_state_is_read_field_by_field() {
        let reading = Reading::parse(&cooling()).unwrap();
        assert_eq!(
            reading,
            Reading {
                on: true,
                mode: Some("cool"),
                target: Some(22),
                fan: Some("auto"),
                indoor: Some(24),
                outdoor: Some(10),
                cleaning: false,
                heating_8c: false,
            }
        );
    }

    #[test]
    fn extra_digits_are_ignored_and_too_few_are_refused() {
        assert!(Reading::parse(&format!("{}ffff", cooling())).is_ok());
        assert!(Reading::parse(&cooling()[..STATE_LEN - 1]).is_err());
        assert!(Reading::parse("").is_err());
        assert!(Reading::parse(&format!("zz{}", &cooling()[2..])).is_err());
    }

    #[test]
    fn a_state_with_no_power_field_is_not_taken_for_off() {
        let unset = format!("ff{}", &cooling()[2..]);
        assert!(matches!(Reading::parse(&unset), Err(DeviceError::Rejected(text)) if text.contains("not reported")));
    }

    #[test]
    fn temperatures_are_signed_and_some_mean_nothing() {
        assert_eq!(temperature(0x16), Some(22));
        assert_eq!(temperature(0xF6), Some(-10));
        assert_eq!(temperature(126), Some(-1), "the library's own special case");
        for raw in [0xFF, 0x7F, 0x80] {
            assert_eq!(temperature(raw), None, "{raw}");
        }
    }

    #[test]
    fn the_setpoint_of_an_8_degree_hold_is_corrected() {
        // Heating, reported 24, with merit A saying 8 degree heating.
        let mut state = cooling().into_bytes();
        state[MODE_AT..MODE_AT + 2].copy_from_slice(b"43");
        state[TARGET_AT..TARGET_AT + 2].copy_from_slice(b"18");
        state[MERIT_A_AT] = b'4';
        let reading = Reading::parse(std::str::from_utf8(&state).unwrap()).unwrap();
        assert!(reading.heating_8c);
        assert_eq!(reading.target, Some(8));
        let op = Op::AirconTemp { celsius: 22 };
        assert!(matches!(changes_for(&op, Some(&reading)), Err(DeviceError::Rejected(_))));
    }

    #[test]
    fn a_command_leaves_every_other_field_alone() {
        let all_f = |digits: &str| digits.bytes().all(|byte| byte == b'f');
        let data = command_data(&[(POWER_AT, POWER_ON)]);
        assert_eq!(data.len(), STATE_LEN);
        assert_eq!(&data[..2], "30");
        assert!(all_f(&data[2..]));
        let data = command_data(&[(TARGET_AT, 22)]);
        assert_eq!(&data[..6], "ffff16");
        assert!(all_f(&data[6..]));
    }

    #[test]
    fn each_operation_changes_only_its_own_field() {
        let cases: [(Op, usize, u8); 6] = [
            (Op::AirconPower { on: true }, POWER_AT, POWER_ON),
            (Op::AirconPower { on: false }, POWER_AT, POWER_OFF),
            (Op::AirconMode { mode: "dry".to_string() }, MODE_AT, 0x44),
            (Op::AirconFan { fan: "medium_high".to_string() }, FAN_AT, 0x35),
            (Op::AirconTemp { celsius: 30 }, TARGET_AT, 30),
            (Op::AirconTemp { celsius: 17 }, TARGET_AT, 17),
        ];
        for (op, at, value) in cases {
            assert_eq!(changes_for(&op, None).unwrap(), vec![(at, value)], "{op:?}");
        }
        assert!(matches!(changes_for(&Op::PlugPower { on: true }, None), Err(DeviceError::Disabled(_))));
        assert!(matches!(
            changes_for(&Op::AirconMode { mode: "boil".to_string() }, None),
            Err(DeviceError::Disabled(_))
        ));
    }

    #[test]
    fn turning_on_a_unit_that_is_cleaning_itself_calls_off_the_cleaning() {
        let mut state = cooling().into_bytes();
        state[POWER_AT..POWER_AT + 2].copy_from_slice(b"31");
        state[CLEANING_AT..CLEANING_AT + 2].copy_from_slice(b"18");
        let cleaning = Reading::parse(std::str::from_utf8(&state).unwrap()).unwrap();
        assert!(cleaning.cleaning);
        let changes = changes_for(&Op::AirconPower { on: true }, Some(&cleaning)).unwrap();
        assert_eq!(changes, vec![(POWER_AT, POWER_ON), (CLEANING_AT, CLEANING_OFF)]);
        let changes = changes_for(&Op::AirconPower { on: false }, Some(&cleaning)).unwrap();
        assert_eq!(changes, vec![(POWER_AT, POWER_OFF)], "only turning on needs it");
    }

    #[test]
    fn every_catalog_value_has_a_code() {
        for mode in AC_MODES {
            assert!(code(&MODE_CODES, mode).is_ok(), "{mode}");
        }
        for fan in AC_FANS {
            assert!(code(&FAN_CODES, fan).is_ok(), "{fan}");
        }
        assert_eq!(AC_MODES.len(), MODE_CODES.len());
        assert_eq!(AC_FANS.len(), FAN_CODES.len());
    }

    #[test]
    fn a_hub_token_is_taken_apart_and_only_for_the_right_hub() {
        let good = "SharedAccessSignature sr=example-hub.azure-devices.net%2Fdevices%2Fa%40b.test_123&sig=c2ln%3D&se=4000000000";
        let token = HubToken::parse(good, &hub()).unwrap();
        assert_eq!(token.device_id, "a@b.test_123");
        assert_eq!(token.expires_at, 4_000_000_000);
        assert!(token.lasting());
        assert!(token.authorization.is_sensitive());
        for bad in [
            "",
            "sr=example-hub.azure-devices.net%2Fdevices%2Fx&sig=a&se=1",
            "SharedAccessSignature sr=evil.test%2Fdevices%2Fx&sig=a&se=4000000000",
            "SharedAccessSignature sr=example-hub.azure-devices.net%2Fother%2Fx&sig=a",
            "SharedAccessSignature sr=example-hub.azure-devices.net%2Fdevices%2F&sig=a",
            "SharedAccessSignature sr=example-hub.azure-devices.net%2Fdevices%2Fa%2Fb&sig=a",
            "SharedAccessSignature sr=example-hub.azure-devices.net%2Fdevices%2Fx%ZZ&sig=a",
            "SharedAccessSignature sig=a",
        ] {
            assert!(HubToken::parse(bad, &hub()).is_none(), "{bad}");
        }
    }

    #[test]
    fn a_token_that_is_ending_is_renewed() {
        let ends_soon = format!(
            "SharedAccessSignature sr=example-hub.azure-devices.net%2Fdevices%2Fx&sig=a&se={}",
            now_seconds() + HUB_TOKEN_MARGIN - 5
        );
        assert!(!HubToken::parse(&ends_soon, &hub()).unwrap().lasting());
        let no_expiry = "SharedAccessSignature sr=example-hub.azure-devices.net%2Fdevices%2Fx&sig=a";
        assert!(HubToken::parse(no_expiry, &hub()).unwrap().lasting());
    }

    #[test]
    fn device_ids_go_into_a_path_safely() {
        assert_eq!(percent_encode("a@b.test_ab-c~1"), "a%40b.test_ab-c~1");
        assert_eq!(percent_encode("a/b?c#d"), "a%2Fb%3Fc%23d");
        assert_eq!(percent_decode(&percent_encode("a@b/c d")).as_deref(), Some("a@b/c d"));
        for bad in ["%", "%2", "%G0", "%+1"] {
            assert_eq!(percent_decode(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_sign_in_is_one_line_of_base64_json() {
        let sign_in = r#"{"Username":"a@b.test","Other":"p:ss wörd"}"#;
        let (email, kept) = load_credentials(&format!("{}\n", STANDARD.encode(sign_in))).unwrap();
        assert_eq!((email.expose(), kept.expose()), ("a@b.test", sign_in));
        for bad in [
            String::new(),
            "plain".to_string(),
            "!!!".to_string(),
            STANDARD.encode("not json"),
            STANDARD.encode(r#"["Username"]"#),
            STANDARD.encode(r#"{"Username":"","Other":"x"}"#),
            STANDARD.encode(r#"{"Username":"a@b.test"}"#),
            STANDARD.encode(r#"{"Other":"x","More":"y"}"#),
        ] {
            assert!(load_credentials(&bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn the_setup_stores_the_sign_in_the_way_it_is_read() {
        let stored = encode_credentials("a@b.test", "p:ss \"wörd\"");
        let (email, kept) = load_credentials(&stored).unwrap();
        assert_eq!(email.expose(), "a@b.test");
        let body: Value = serde_json::from_str(kept.expose()).unwrap();
        assert_eq!(body, json!({ "Username": "a@b.test", SIGN_IN_SECRET_KEY: "p:ss \"wörd\"" }));
        assert!(kept.expose().starts_with(r#"{"Username": "#), "in the app's own order: {}", kept.expose());
    }

    #[test]
    fn only_units_with_ids_this_plugin_accepts_are_listed() {
        let mapping = json!([
            { "GroupName": "Home", "ACList": [
                { "Id": "0a1b2c3d-1111-2222-3333-444455556666", "DeviceUniqueId": "UNIT-ONE", "Name": "Living <room>" },
                { "Id": "short", "DeviceUniqueId": "UNIT-X", "Name": "Bad id" },
                { "Id": "0a1b2c3d-1111-2222-3333-777788889999", "DeviceUniqueId": "has space", "Name": "Bad unique id" },
                { "Id": "0A1B2C3D-1111-2222-3333-444455556666", "DeviceUniqueId": "UNIT-AGAIN", "Name": "Twice" }
            ] },
            { "GroupName": "Other", "ACList": [
                { "Id": "0a1b2c3d-1111-2222-3333-aaaabbbbcccc", "DeviceUniqueId": "UNIT-TWO", "Name": "\u{7}" }
            ] },
            { "GroupName": "Empty", "ACList": null }
        ]);
        let units = listed_units(&mapping);
        assert_eq!(
            units,
            vec![
                ListedUnit {
                    id: "0a1b2c3d-1111-2222-3333-444455556666".to_string(),
                    unique_id: "UNIT-ONE".to_string(),
                    name: Some("Living room".to_string()),
                },
                ListedUnit {
                    id: "0a1b2c3d-1111-2222-3333-aaaabbbbcccc".to_string(),
                    unique_id: "UNIT-TWO".to_string(),
                    name: None,
                },
            ]
        );
        assert!(listed_units(&json!({ "not": "a list" })).is_empty());
    }

    /// Whatever arrives, the answer is a result and never a panic.
    #[test]
    fn nothing_the_cloud_sends_can_panic_this() {
        let mut seed: u32 = 0x2468_ACE1;
        let mut next = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 16) as u8
        };
        for round in 0..4_000usize {
            let text: String = (0..round % 90).map(|_| next() as char).collect();
            let _ = Reading::parse(&text);
            let _ = HubToken::parse(&text, &hub());
            let _ = percent_decode(&text);
            let digits: String =
                (0..STATE_LEN + round % 4).map(|_| char::from_digit(u32::from(next() % 16), 16).unwrap()).collect();
            let _ = Reading::parse(&digits);
            let _ = HubToken::parse(&format!("SharedAccessSignature sr={digits}&se={text}"), &hub());
            let _ = listed_units(&json!([{ "ACList": [{ "Id": text, "DeviceUniqueId": digits, "Name": text }] }]));
        }
    }
}
