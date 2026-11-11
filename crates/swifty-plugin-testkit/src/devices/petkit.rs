//! Petkit's cloud: a feeder and a litter box.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::http::{MockHttp, Seen};
use crate::conformance::Switches;

// ----- Petkit's cloud

pub const PETKIT_ACCOUNT: &str = "family@example.com";
/// What the login file holds after the colon: the MD5 of a password nobody here knows.
pub const PETKIT_PASSWORD_MD5: &str = "5f4dcc3b5aa765d61d8327deb882cf99";
pub const PETKIT_SESSION: &str = "petkit-session-0123456789";
pub const PETKIT_FEEDER: u64 = 4_001;
pub const PETKIT_LITTER: u64 = 4_002;

/// What Petkit's servers know and have been asked, as the adapter's calls change it.
#[derive(Debug, Clone, Default)]
pub struct PetkitCloud {
    /// Every login that reached the server, good or not.
    pub login_attempts: usize,
    /// The session that is valid now. Replacing it signs the adapter out.
    pub session: String,
    pub refuse_login: bool,
    /// Answer the login itself with an HTTP error.
    pub login_server_error: bool,
    /// The devices belong to another account and are only in the roster.
    pub shared: bool,
    pub no_litter_box: bool,
    /// Name this server for the region instead of the mock's own address.
    pub gateway: Option<String>,
    pub feed_times: Vec<i64>,
    pub fed_grams: Vec<u64>,
    pub cleaning: bool,
    pub cleanings: Vec<i64>,
    /// How many feeds and cleans the server actually carried out.
    pub applied: usize,
}

pub type SharedPetkit = Arc<Mutex<PetkitCloud>>;

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'+' => out.push(b' '),
            b'%' if at + 2 < bytes.len() => {
                let pair =
                    std::str::from_utf8(&bytes[at + 1..at + 3]).ok().and_then(|hex| u8::from_str_radix(hex, 16).ok());
                match pair {
                    Some(byte) => {
                        out.push(byte);
                        at += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            other => out.push(other),
        }
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A form body or a query string as its pairs.
pub fn form_pairs(text: &str) -> BTreeMap<String, String> {
    text.split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(key), percent_decode(value))
        })
        .collect()
}

fn petkit_error(code: i64) -> (u16, String) {
    (200, json!({ "error": { "code": code, "msg": "Petkit's own words, which must never be repeated" } }).to_string())
}

fn petkit_result(result: Value) -> (u16, String) {
    (200, json!({ "result": result }).to_string())
}

fn epoch_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

/// Petkit's passport, region server, login and the calls the adapter makes, as far as the
/// adapter reads them. `base` is filled in once the mock is listening.
pub fn petkit_handler(
    cloud: SharedPetkit,
    base: Arc<Mutex<String>>,
) -> impl Fn(&Seen) -> (u16, String) + Send + Sync + 'static {
    let rigged = petkit_rigged(cloud, base);
    move |seen: &Seen| rigged(seen, &Switches::default())
}

/// [`petkit_handler`], reporting the conformance suite's text as each device's name.
pub fn petkit_rigged(
    cloud: SharedPetkit,
    base: Arc<Mutex<String>>,
) -> impl Fn(&Seen, &Switches) -> (u16, String) + Send + Sync + 'static {
    move |seen: &Seen, switches: &Switches| {
        let mut cloud = cloud.lock().unwrap();
        let own = base.lock().unwrap().clone();
        if seen.path == "/v1/regionservers" {
            let gateway = cloud.gateway.clone().unwrap_or_else(|| format!("{own}/gw/"));
            return petkit_result(json!({ "list": [
                { "accountType": "", "gateway": "https://api.example.invalid/other/", "id": "DE", "name": "Germany" },
                { "accountType": "", "gateway": gateway, "id": "FR", "name": "France" }
            ] }));
        }
        let Some(call) = seen.path.strip_prefix("/gw/") else {
            return (404, String::new());
        };
        let form = form_pairs(&seen.body);
        let query = form_pairs(&seen.query);
        let is = |pairs: &BTreeMap<String, String>, key: &str, value: &str| {
            pairs.get(key).map(String::as_str) == Some(value)
        };
        if call == "user/login" {
            cloud.login_attempts += 1;
            if cloud.login_server_error {
                return (503, "busy".to_string());
            }
            let known = is(&form, "username", PETKIT_ACCOUNT)
                && is(&form, "password", PETKIT_PASSWORD_MD5)
                && is(&form, "region", "fr")
                && is(&form, "encrypt", "1")
                && form.get("client").is_some_and(|client| client.contains("'platform': 'android'"));
            if cloud.refuse_login || !known {
                return petkit_error(122);
            }
            cloud.session = format!("{PETKIT_SESSION}-{}", cloud.login_attempts);
            return petkit_result(json!({ "session": {
                "id": cloud.session, "userId": "9", "expiresIn": 604_800, "createdAt": "2025-11-10T08:00:00.000+0000"
            } }));
        }
        let signed_in = !cloud.session.is_empty()
            && seen.header("f-session") == cloud.session
            && seen.header("x-session") == cloud.session;
        if !signed_in {
            return petkit_error(5);
        }
        let name = switches.device_text("named by Petkit");
        let listed =
            |kind: &str, id: u64| json!({ "deviceId": id, "deviceType": kind, "groupId": 77, "deviceName": name });
        match call {
            "group/family/list" => petkit_result(if cloud.shared {
                json!([{ "groupId": 77, "deviceList": [] }])
            } else if cloud.no_litter_box {
                json!([{ "groupId": 77, "deviceList": [listed("D4", PETKIT_FEEDER)] }])
            } else {
                json!([{ "groupId": 77, "deviceList": [listed("D4", PETKIT_FEEDER), listed("T4", PETKIT_LITTER)] }])
            }),
            "discovery/device_roster_v2" if is(&form, "groupId", "77") => {
                let mut devices = vec![json!({ "id": PETKIT_FEEDER, "type": "d4" })];
                if !cloud.no_litter_box {
                    devices.push(json!({ "id": PETKIT_LITTER, "type": "t4" }));
                }
                petkit_result(json!({ "devices": devices }))
            }
            "d4/device_detail" if is(&query, "id", "4001") => petkit_result(
                json!({ "id": PETKIT_FEEDER, "state": { "feedState": { "times": cloud.feed_times.len() }, "food": 1 } }),
            ),
            "d4/dailyFeeds" if is(&query, "deviceId", "4001") && query.contains_key("days") => {
                let items: Vec<Value> = cloud
                    .feed_times
                    .iter()
                    .map(|at| json!({ "time": -1, "state": { "completedAt": at, "result": 0 } }))
                    .collect();
                petkit_result(json!([{ "day": 20_251_110, "items": items }]))
            }
            "d4/saveDailyFeed" => {
                let valid = is(&form, "deviceId", "4001")
                    && is(&form, "time", "-1")
                    && is(&form, "name", "")
                    && form.get("day").is_some_and(|day| day.len() == 8 && day.bytes().all(|b| b.is_ascii_digit()));
                match form.get("amount").and_then(|grams| grams.parse::<u64>().ok()) {
                    Some(grams) if valid && [10, 20, 30, 40, 50].contains(&grams) => {
                        cloud.fed_grams.push(grams);
                        cloud.feed_times.push(epoch_now());
                        cloud.applied += 1;
                        petkit_result(json!({ "id": "feed-1" }))
                    }
                    _ => petkit_error(1000),
                }
            }
            "t4/device_detail" if is(&query, "id", "4002") && !cloud.no_litter_box => {
                let work = if cloud.cleaning { json!({ "workMode": 0, "workProcess": 12 }) } else { Value::Null };
                petkit_result(json!({ "id": PETKIT_LITTER, "state": { "workState": work, "sandPercent": 80 } }))
            }
            "t4/getDeviceRecord"
                if is(&query, "deviceId", "4002") && query.get("date").is_some_and(|date| date.len() == 8) =>
            {
                let records: Vec<Value> = cloud
                    .cleanings
                    .iter()
                    .map(|at| json!({ "timestamp": at, "subContent": [{ "eventType": 5, "content": { "result": 0, "startReason": 0 } }] }))
                    .chain([json!({ "timestamp": epoch_now(), "subContent": [{ "eventType": 10, "content": { "result": 0 } }] })])
                    .collect();
                petkit_result(json!(records))
            }
            "t4/controlDevice" => {
                let start =
                    is(&form, "kv", r#"{"start_action":0}"#) && is(&form, "type", "start") && is(&form, "id", "4002");
                if start && !cloud.no_litter_box {
                    cloud.cleaning = true;
                    cloud.applied += 1;
                    petkit_result(json!("ok"))
                } else {
                    petkit_error(1000)
                }
            }
            _ => petkit_error(703),
        }
    }
}

pub struct MockPetkit {
    pub http: MockHttp,
    pub cloud: SharedPetkit,
}

impl MockPetkit {
    pub async fn start() -> Self {
        let cloud = SharedPetkit::default();
        let base = Arc::new(Mutex::new(String::new()));
        let http = MockHttp::start_rigged(petkit_rigged(cloud.clone(), base.clone())).await;
        *base.lock().unwrap() = http.url.clone();
        // A feed and a clean ask the devices to do something; the rest reads or signs in.
        http.count_commands(|seen| seen.path.ends_with("/saveDailyFeed") || seen.path.ends_with("/controlDevice"));
        Self { http, cloud }
    }

    /// What the adapter sent to this call, oldest first.
    pub fn calls(&self, path: &str) -> Vec<Seen> {
        self.http.requests().into_iter().filter(|seen| seen.path == path).collect()
    }
}
