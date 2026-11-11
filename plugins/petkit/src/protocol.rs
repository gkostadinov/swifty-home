//! What goes over the wire, and nothing that sends it: dates the way Petkit writes them,
//! the login, the headers, the replies and what they say about the devices.
//!
//! The calls and their fields are the ones Petkit's own Android app makes, as the
//! open-source `pypetkitapi` client and the Home Assistant Petkit integration describe
//! them (see `DEVELOPING.md`). What Petkit sends back is not documented. The parts that
//! read it are small, work on whatever shape arrives, and say so when a reading is
//! missing rather than guessing.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use swifty_net::Reply;
use swifty_plugin_api::{DeviceError, Secret};
use url::Url;

use crate::config::{is_loopback, PetkitConfig};
use crate::identity;

pub const FEEDER: &str = "feeder";
pub const LITTER: &str = "litter";
pub const FEEDER_TYPE: &str = "d4";
pub const LITTER_TYPE: &str = "t4";
/// What the D4 will dispense in one go. Petkit refuses any other amount.
pub const FEED_GRAMS: [u8; 5] = [10, 20, 30, 40, 50];

pub const CHINA_GATEWAY: &str = "https://api.petkit.cn/6/";
/// A regional server named by Petkit's passport must be on one of these, or the login is
/// never sent to it.
pub const GATEWAY_DOMAINS: [&str; 4] = ["petkt.com", "eu-pet.com", "petktasia.com", "petkit.cn"];

/// What Petkit's login calls the field that holds the MD5. The hub has no sign-in of its
/// own that uses one, so its source scan allows the word only as a field label of this
/// plugin and Toshiba's. It is Petkit's name for it, and nothing else in this crate is
/// meant by it.
pub const LOGIN_SECRET_FIELD: &str = "password";

pub const LOGIN_REFUSED: &str = "Petkit refused the account or its sign-in secret. Run this integration's setup \
                                 again to fix the login: it will not try again by itself.";
pub const SESSION_LOST: &str = "Petkit had signed this session out, so nothing was sent. Try again.";

// ----- time

pub fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(0))
}

/// The year, month and day of a count of days since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 } as u32;
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

/// `20251231`, the way Petkit names a day, in the configured offset.
pub fn local_day(epoch: i64, offset_minutes: i32) -> String {
    let local = epoch + i64::from(offset_minutes) * 60;
    let (year, month, day) = civil_from_days(local.div_euclid(86_400));
    format!("{year:04}{month:02}{day:02}")
}

pub fn local_hhmm(epoch: i64, offset_minutes: i32) -> String {
    let seconds = (epoch + i64::from(offset_minutes) * 60).rem_euclid(86_400);
    format!("{:02}:{:02}", seconds / 3_600, seconds % 3_600 / 60)
}

/// Hours east of UTC the way a Python float prints them: `2.0`, `5.5`, `-3.0`.
pub fn offset_header(offset_minutes: i32) -> String {
    format!("{:?}", f64::from(offset_minutes) / 60.0)
}

// ----- how the app presents itself

/// What the login says about the phone it is on, printed the way the app's own client does.
pub fn client_info(config: &PetkitConfig) -> String {
    format!(
        "{{'locale': '{}', 'name': '{}', 'osVersion': '{}', 'phoneBrand': '{}', 'platform': 'android', \
         'source': 'app.petkit-android', 'version': '{}', 'timezoneId': '{}', 'timezone': '{}'}}",
        identity::LOCALE,
        config.phone_model,
        config.os_version,
        config.phone_brand,
        config.app_version,
        config.timezone,
        offset_header(config.utc_offset_minutes)
    )
}

/// The headers every call carries, as the app sends them, and the session's twice when
/// there is one. Names are lower case, values are text.
pub fn header_pairs(config: &PetkitConfig, session: Option<&Secret>) -> Vec<(&'static str, String)> {
    let mut pairs = vec![
        ("accept", "*/*".to_string()),
        ("accept-language", identity::ACCEPT_LANGUAGE.to_string()),
        ("content-type", "application/x-www-form-urlencoded".to_string()),
        ("user-agent", config.user_agent.clone()),
        ("x-img-version", "1".to_string()),
        ("x-locale", identity::LOCALE.to_string()),
        ("x-client", format!("android({};{})", config.os_version, config.phone_model)),
        ("x-hour", "24".to_string()),
        ("x-timezoneid", config.timezone.clone()),
        ("x-api-version", config.app_version.clone()),
        ("x-timezone", offset_header(config.utc_offset_minutes)),
    ];
    if let Some(id) = session {
        pairs.push(("f-session", id.expose().to_string()));
        pairs.push(("x-session", id.expose().to_string()));
    }
    pairs
}

// ----- the login

pub struct Login {
    pub username: String,
    pub md5: Secret,
}

/// `account:md5`. The account may hold anything but a line break, and the hash is split off
/// at the last colon.
pub fn parse_credentials(text: &str) -> Result<Login, String> {
    let bad = || "the Petkit login must be one line: account:md5 of the sign-in secret, in hex".to_string();
    let (username, hash) = text.trim().rsplit_once(':').ok_or_else(bad)?;
    let hash = hash.trim().to_ascii_lowercase();
    if username.is_empty() || username.len() > 128 || username.chars().any(char::is_control) {
        return Err(bad());
    }
    if hash.len() != 32 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(bad());
    }
    Ok(Login { username: username.to_string(), md5: Secret::new(hash) })
}

/// The stored login for an account, as the setup writes it: the account, a colon, and
/// the MD5 of its sign-in secret in hex. The secret itself is never stored.
pub fn login_line(account: &str, sign_in_secret: &str) -> String {
    format!("{account}:{:x}", md5::compute(sign_in_secret.as_bytes()))
}

/// A regional server, if Petkit's passport named one this service will log in to.
pub fn accepted_gateway(candidate: &str, passport: &Url) -> Option<String> {
    let url = Url::parse(candidate).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let secure = url.scheme() == "https" || is_loopback(&url);
    let petkit = GATEWAY_DOMAINS.iter().any(|domain| host == *domain || host.ends_with(&format!(".{domain}")));
    let the_passport_itself = passport.host_str().is_some_and(|own| own.eq_ignore_ascii_case(&host))
        && passport.port_or_known_default() == url.port_or_known_default();
    let plain = !url.authority().contains('@') && url.query().is_none() && url.fragment().is_none();
    if !(secure && plain && (petkit || the_passport_itself)) {
        return None;
    }
    let mut text = url.to_string();
    if !text.ends_with('/') {
        text.push('/');
    }
    Some(text)
}

// ----- replies

/// What can go wrong with one call before its answer says what it means.
#[derive(Debug)]
pub enum Wire {
    /// Nothing usable came back.
    Fault(DeviceError),
    /// Petkit answered with an error number.
    Code(i64),
}

pub fn unreadable() -> DeviceError {
    DeviceError::Rejected("Petkit sent a reply that could not be read".to_string())
}

/// What an error number from Petkit means, in words that carry nothing Petkit wrote.
pub fn explain(code: i64) -> DeviceError {
    match code {
        1 => DeviceError::Unreachable("Petkit's server is busy".to_string()),
        5 => DeviceError::Rejected(SESSION_LOST.to_string()),
        122 | 125 => DeviceError::Rejected(LOGIN_REFUSED.to_string()),
        703 => DeviceError::Rejected("Petkit does not know this device".to_string()),
        other => DeviceError::Rejected(format!("Petkit refused with error {other}")),
    }
}

/// The `result` of a reply, or why there is none. Petkit's own message text is never used.
pub fn open(reply: &Reply) -> Result<Value, Wire> {
    if matches!(reply.status.as_u16(), 502..=504) {
        return Err(Wire::Fault(DeviceError::Unreachable("Petkit's server is busy".to_string())));
    }
    if !reply.status.is_success() {
        return Err(Wire::Fault(DeviceError::Rejected(format!("Petkit answered HTTP {}", reply.status.as_u16()))));
    }
    let body = reply.json().map_err(Wire::Fault)?;
    if let Some(error) = body.get("error") {
        let code = error
            .get("code")
            .and_then(|code| code.as_i64().or_else(|| code.as_str().and_then(|text| text.trim().parse().ok())))
            .unwrap_or(0);
        return Err(Wire::Code(code));
    }
    body.get("result").cloned().ok_or(Wire::Fault(unreadable()))
}

pub fn id_of(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str().and_then(|text| text.trim().parse().ok())).filter(|id| *id > 0)
}

// ----- reading what Petkit sends

/// Every `completedAt` anywhere in a reply, newest first. Whatever is not a plausible time
/// in seconds, or in milliseconds, is ignored.
pub fn latest_completion(value: &Value) -> Option<i64> {
    fn walk(value: &Value, newest: &mut Option<i64>) {
        match value {
            Value::Object(map) => {
                for (key, inner) in map {
                    if key == "completedAt" {
                        if let Some(epoch) = inner.as_i64().and_then(plausible_epoch) {
                            *newest = Some(newest.map_or(epoch, |seen| seen.max(epoch)));
                        }
                    }
                    walk(inner, newest);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| walk(item, newest)),
            _ => {}
        }
    }
    let mut newest = None;
    walk(value, &mut newest);
    newest
}

/// A time in seconds, given seconds or milliseconds, if it falls between 2001 and 2096.
pub fn plausible_epoch(raw: i64) -> Option<i64> {
    let seconds = if raw > 100_000_000_000 { raw / 1_000 } else { raw };
    (1_000_000_000..4_000_000_000).contains(&seconds).then_some(seconds)
}

/// How many of the day's records say a cleaning finished, and when the last one did. A
/// record is a cleaning when one of its parts has event type 5, and it finished when that
/// part's result is 0.
pub fn finished_cleanings(records: &Value) -> (u32, Option<i64>) {
    let items = records.as_array().or_else(|| records.get("list").and_then(Value::as_array));
    let mut count = 0u32;
    let mut newest: Option<i64> = None;
    for record in items.into_iter().flatten() {
        let finished = record.get("subContent").and_then(Value::as_array).is_some_and(|parts| {
            parts.iter().any(|part| {
                part.get("eventType").and_then(Value::as_i64) == Some(5)
                    && part.pointer("/content/result").and_then(Value::as_i64) == Some(0)
            })
        });
        if finished {
            count = count.saturating_add(1);
            if let Some(epoch) = record.get("timestamp").and_then(Value::as_i64).and_then(plausible_epoch) {
                newest = Some(newest.map_or(epoch, |seen| seen.max(epoch)));
            }
        }
    }
    (count, newest)
}

pub fn feeder_portions(detail: &Value) -> Result<u32, DeviceError> {
    if !detail.get("state").is_some_and(Value::is_object) {
        return Err(unreadable());
    }
    let times = detail.pointer("/state/feedState/times").and_then(Value::as_u64).unwrap_or(0);
    Ok(u32::try_from(times).unwrap_or(u32::MAX))
}

/// The box is working when Petkit reports a work state at all: it leaves that out when idle.
pub fn litter_busy(detail: &Value) -> Result<bool, DeviceError> {
    if !detail.get("state").is_some_and(Value::is_object) {
        return Err(unreadable());
    }
    Ok(detail.pointer("/state/workState/workMode").is_some_and(Value::is_number))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn config(zone: &str, offset: i32) -> PetkitConfig {
        crate::config::parse(&json!({ "region": "DE", "timezone": zone, "utc_offset_minutes": offset })).unwrap()
    }

    #[test]
    fn days_and_times_follow_the_offset() {
        assert_eq!(local_day(0, 0), "19700101");
        assert_eq!(local_day(1_700_000_000, 0), "20231114");
        assert_eq!(local_hhmm(1_700_000_000, 0), "22:13");
        // Two hours east tips the same moment into the next day.
        assert_eq!(local_day(1_700_000_000, 120), "20231115");
        assert_eq!(local_hhmm(1_700_000_000, 120), "00:13");
        // West of UTC, and a leap day.
        assert_eq!(local_day(1_709_164_800, -60), "20240228");
        assert_eq!(local_day(1_709_164_800, 0), "20240229");
        assert_eq!(local_day(-1, 0), "19691231");
    }

    #[test]
    fn the_offset_prints_like_a_python_float() {
        for (minutes, text) in [(0, "0.0"), (120, "2.0"), (330, "5.5"), (-180, "-3.0"), (345, "5.75")] {
            assert_eq!(offset_header(minutes), text);
        }
    }

    #[test]
    fn the_client_info_is_the_dict_the_app_prints() {
        assert_eq!(
            client_info(&config("Europe/Paris", 120)),
            "{'locale': 'en-US', 'name': '23127PN0CG', 'osVersion': '16.1', 'phoneBrand': 'Xiaomi', 'platform': 'android', \
             'source': 'app.petkit-android', 'version': '13.2.1', 'timezoneId': 'Europe/Paris', 'timezone': '2.0'}"
        );
    }

    #[test]
    fn the_identity_comes_from_the_config() {
        let mut petkit = config("UTC", 0);
        petkit.app_version = "14.0.0".to_string();
        petkit.phone_model = "Model-X".to_string();
        petkit.user_agent = "okhttp/4.12.0".to_string();
        assert!(client_info(&petkit).contains("'name': 'Model-X'"));
        assert!(client_info(&petkit).contains("'version': '14.0.0'"));
        let headers = header_pairs(&petkit, None);
        let header = |name: &str| headers.iter().find(|(key, _)| *key == name).map(|(_, value)| value.as_str());
        assert_eq!(header("user-agent"), Some("okhttp/4.12.0"));
        assert_eq!(header("x-client"), Some("android(16.1;Model-X)"));
        assert_eq!(header("x-api-version"), Some("14.0.0"));
        assert_eq!(header("f-session"), None);
        let id = "s-1".to_string();
        let session = Secret::new(id);
        let headers = header_pairs(&petkit, Some(&session));
        assert_eq!(headers.iter().filter(|(_, value)| value == "s-1").count(), 2);
    }

    #[test]
    fn a_login_line_is_the_account_and_the_hash() {
        let hash = "5f4dcc3b5aa765d61d8327deb882cf99";
        let login = parse_credentials(&format!("me@example.com:{}\n", hash.to_ascii_uppercase())).unwrap();
        assert_eq!((login.username.as_str(), login.md5.expose()), ("me@example.com", hash));
        // An account with a colon in it still splits at the hash.
        assert_eq!(parse_credentials(&format!("a:b:{hash}")).unwrap().username, "a:b");
        for bad in [
            "",
            "nocolon",
            ":5f4dcc3b5aa765d61d8327deb882cf99",
            "me:short",
            "me:5f4dcc3b5aa765d61d8327deb882cf9z",
            "me: ",
        ] {
            let error = parse_credentials(bad).err().unwrap_or_default();
            assert!(!error.is_empty(), "{bad:?}");
            assert!(!error.contains(bad) || bad.is_empty(), "the file's text must not come back: {error}");
        }
    }

    #[test]
    fn the_setup_stores_the_hash_and_never_the_secret() {
        // RFC 1321's own test vectors.
        assert_eq!(login_line("me@example.com", ""), "me@example.com:d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(login_line("a:b", "abc"), "a:b:900150983cd24fb0d6963f7d28e17f72");
        let login = parse_credentials(&login_line("me@example.com", "abc")).unwrap();
        assert_eq!(login.md5.expose(), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn only_a_petkit_server_is_ever_logged_in_to() {
        let passport = Url::parse("https://passport.petkt.com/").unwrap();
        assert_eq!(
            accepted_gateway("https://api.eu-pet.com/latest/", &passport).as_deref(),
            Some("https://api.eu-pet.com/latest/")
        );
        assert_eq!(
            accepted_gateway("https://api.petkt.com/latest", &passport).as_deref(),
            Some("https://api.petkt.com/latest/")
        );
        assert!(accepted_gateway("https://api.petktasia.com/latest/", &passport).is_some());
        // A name that ends in a Petkit server's, but not at a label: built, so that no look-alike is written out.
        let look_alike = format!("https://evil-{}/latest/", "eu-pet.com");
        for bad in [
            "http://api.eu-pet.com/latest/",
            "https://api.eu-pet.com.evil.example/latest/",
            look_alike.as_str(),
            "https://evil.example/latest/",
            "https://user:pw@api.eu-pet.com/latest/",
            "https://api.eu-pet.com/latest/?x=1",
            "https://127.0.0.1/latest/",
            "not a url",
        ] {
            assert!(accepted_gateway(bad, &passport).is_none(), "{bad}");
        }
        // A test's own passport may name a server on its own address, and nothing else.
        let local = Url::parse("http://127.0.0.1:9000/").unwrap();
        assert!(accepted_gateway("http://127.0.0.1:9000/gw/", &local).is_some());
        assert!(accepted_gateway("http://127.0.0.1:9001/gw/", &local).is_none());
    }

    #[test]
    fn error_numbers_become_fixed_text() {
        assert!(matches!(explain(1), DeviceError::Unreachable(_)));
        assert!(explain(1).is_silence());
        assert_eq!(explain(5), DeviceError::Rejected(SESSION_LOST.to_string()));
        assert_eq!(explain(122), explain(125));
        assert!(explain(703).to_string().contains("does not know"));
        assert!(explain(9999).to_string().contains("9999"));
    }

    #[test]
    fn the_newest_completion_is_found_wherever_it_sits() {
        let records = json!({ "feed": [
            { "day": 20251110, "items": [
                { "time": 28_800, "state": { "completedAt": 1_762_760_000, "result": 0 } },
                { "time": -1, "completedAt": 1_762_790_400_000_i64 },
                { "time": 61_200, "state": { "completedAt": "2025-11-10T08:00:00", "result": 1 } },
                { "state": { "completedAt": 12 } }
            ] },
            { "day": 20251109, "items": [ { "completedAt": 1_762_600_000 } ] }
        ] });
        assert_eq!(latest_completion(&records), Some(1_762_790_400));
        assert_eq!(latest_completion(&json!([])), None);
        assert_eq!(latest_completion(&json!({ "feed": [ { "items": [ { "completedAt": "soon" } ] } ] })), None);
        assert_eq!(plausible_epoch(5), None);
        assert_eq!(plausible_epoch(1_762_790_400_000), Some(1_762_790_400));
    }

    #[test]
    fn a_finished_cleaning_is_event_type_five_with_result_zero() {
        let part = |kind: i64, result: i64| json!({ "eventType": kind, "content": { "result": result } });
        let records = json!([
            { "timestamp": 1_762_700_000, "subContent": [ part(5, 0) ] },
            { "timestamp": 1_762_790_400, "subContent": [ part(10, 0), part(5, 0) ] },
            { "timestamp": 1_762_800_000, "subContent": [ part(5, 1) ] },
            { "timestamp": 1_762_810_000, "subContent": [ part(6, 0) ] },
            { "timestamp": 1_762_820_000 },
            { "subContent": [ part(5, 0) ] }
        ]);
        assert_eq!(finished_cleanings(&records), (3, Some(1_762_790_400)));
        assert_eq!(finished_cleanings(&json!({ "list": records })), (3, Some(1_762_790_400)));
        assert_eq!(finished_cleanings(&json!({})), (0, None));
        assert_eq!(finished_cleanings(&json!("nothing")), (0, None));
    }

    #[test]
    fn states_need_a_state_and_read_what_is_there() {
        assert_eq!(feeder_portions(&json!({ "state": { "feedState": { "times": 3 } } })).unwrap(), 3);
        assert_eq!(feeder_portions(&json!({ "state": {} })).unwrap(), 0);
        assert!(feeder_portions(&json!({})).is_err(), "no state is not zero portions");
        assert!(feeder_portions(&json!({ "state": 4 })).is_err());
        assert!(!litter_busy(&json!({ "state": {} })).unwrap());
        assert!(!litter_busy(&json!({ "state": { "workState": null } })).unwrap());
        assert!(litter_busy(&json!({ "state": { "workState": { "workMode": 0, "workProcess": 12 } } })).unwrap());
        assert!(litter_busy(&json!([])).is_err());
    }
}
