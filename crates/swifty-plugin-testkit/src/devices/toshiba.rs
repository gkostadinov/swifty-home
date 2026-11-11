//! Toshiba's air-conditioner cloud.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::http::{MockHttp, Seen};
use crate::conformance::Switches;

pub const TOSHIBA_EMAIL: &str = "owner@example.test";
pub const TOSHIBA_PASSWORD: &str = "t0shiba-pa:ss;word";
pub const TOSHIBA_BEARER: &str = "toshiba-bearer-0123456789";
pub const TOSHIBA_SIGNATURE: &str = "toshiba-signature-0123456789";

// ----- Toshiba's cloud

pub const AC_IDS: [&str; 2] = ["0a1b2c3d-1111-2222-3333-444455556666", "0a1b2c3d-1111-2222-3333-777788889999"];
pub const AC_UNIQUE_IDS: [&str; 2] = ["UNIT-ONE", "UNIT-TWO"];
/// The names the account gives the two units, in its list of units.
pub const AC_NAMES: [&str; 2] = ["Living room", "Bedroom"];
/// Off, set to cool to 22 with the fan on automatic, 24 inside and 10 outside.
pub const AC_OFF: &str = "3142164131640010180affffffff10ffffffff";
/// As this service identifies itself to the cloud.
pub const TOSHIBA_DEVICE: &str = "owner@example.test_6f6c64687562";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudMode {
    Normal,
    /// Commands are taken and change nothing.
    Ignores,
    /// Every request is answered 403.
    SlowDown,
    /// Every sign-in is told the password is wrong.
    BadPassword,
}

#[derive(Debug)]
pub struct ToshibaCloud {
    pub mode: CloudMode,
    /// What each unit reports, by its id.
    pub states: HashMap<String, String>,
    /// What a command changed and has not shown up in a reading yet.
    pending: HashMap<String, String>,
    /// How many readings still give the old state after a command.
    pub lag: usize,
    pub bearer_ok: bool,
    pub hub_token_ok: bool,
    /// Set: the hub token names this host instead of the mock itself.
    pub hub_host: Option<String>,
    pub logins: usize,
    pub registrations: usize,
    pub state_reads: usize,
    pub commands: Vec<Value>,
}

pub struct MockToshiba {
    pub http: MockHttp,
    pub cloud: Arc<Mutex<ToshibaCloud>>,
}

fn path_encoded(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// The fields in `data` that are not "no change", laid over `state`.
fn with_changes(state: &str, data: &str) -> String {
    let mut out = state.to_string();
    // The two merit fields are single digits.
    let fields =
        (0..12).step_by(2).map(|at| at..at + 2).chain([12..13, 13..14]).chain((14..38).step_by(2).map(|at| at..at + 2));
    for range in fields {
        let digits = &data[range.clone()];
        if digits.bytes().any(|b| b != b'f') {
            out.replace_range(range, digits);
        }
    }
    out
}

fn toshiba_handler(
    cloud: Arc<Mutex<ToshibaCloud>>,
) -> impl Fn(&Seen, &Switches) -> (u16, String) + Send + Sync + 'static {
    let ok = |result: Value| (200, json!({ "IsSuccess": true, "StatusCode": "Success", "ResObj": result }).to_string());
    move |seen, switches| {
        let mut cloud = cloud.lock().unwrap();
        if cloud.mode == CloudMode::SlowDown {
            return (403, format!("blocked {TOSHIBA_PASSWORD}"));
        }
        // As the real cloud has since about July 2026: a client that does not say which it is
        // gets "too many requests", however rarely it asks.
        if seen.path.starts_with("/api/") && seen.header("device-id") != TOSHIBA_DEVICE {
            return (
                429,
                json!({ "isSuccess": false, "message": "Too many requests. Try again in 60 seconds." }).to_string(),
            );
        }
        let bearer = format!("Bearer {TOSHIBA_BEARER}");
        let signed_in = seen.header("authorization") == bearer && cloud.bearer_ok;
        match (seen.method.as_str(), seen.path.as_str()) {
            ("POST", "/api/Consumer/Login") => {
                cloud.logins += 1;
                let body: Value = serde_json::from_str(&seen.body).unwrap();
                if !seen.header("user-agent").contains("Mozilla") {
                    return (403, String::new());
                }
                if cloud.mode == CloudMode::BadPassword
                    || body["Password"] != TOSHIBA_PASSWORD
                    || body["Username"] != TOSHIBA_EMAIL
                {
                    let refusal = json!({ "IsSuccess": false, "StatusCode": "InvalidUserNameorPassword", "Message": format!("no {TOSHIBA_PASSWORD}") });
                    return (200, refusal.to_string());
                }
                cloud.bearer_ok = true;
                ok(json!({ "access_token": TOSHIBA_BEARER, "token_type": "Bearer", "consumerId": "consumer-7" }))
            }
            ("POST", "/api/Consumer/RegisterMobileDevice") => {
                if !signed_in {
                    return (401, String::new());
                }
                cloud.registrations += 1;
                let body: Value = serde_json::from_str(&seen.body).unwrap();
                assert_eq!(body["DeviceID"], TOSHIBA_DEVICE);
                assert_eq!(body["DeviceType"], "1");
                assert_eq!(body["Username"], TOSHIBA_EMAIL);
                cloud.hub_token_ok = true;
                let host = cloud.hub_host.clone().unwrap_or_else(|| seen.header("host").to_string());
                let resource = path_encoded(&format!("{host}/devices/{TOSHIBA_DEVICE}"));
                ok(
                    json!({ "SasToken": format!("SharedAccessSignature sr={resource}&sig={TOSHIBA_SIGNATURE}&se=4000000000") }),
                )
            }
            // The account's units, in groups, as the setup lists them.
            ("GET", "/api/AC/GetConsumerACMapping") => {
                if !signed_in {
                    return (401, String::new());
                }
                if !seen.header("user-agent").contains("Mozilla") {
                    return (403, String::new());
                }
                assert!(seen.query.contains("consumerId=consumer-7"), "{}", seen.query);
                let units: Vec<Value> = AC_IDS
                    .iter()
                    .zip(AC_UNIQUE_IDS)
                    .zip(AC_NAMES)
                    .map(|((id, unique), name)| {
                        json!({ "Id": id, "DeviceUniqueId": unique, "Name": switches.device_text(name) })
                    })
                    .collect();
                ok(json!([{ "GroupName": "Home", "ACList": units }]))
            }
            ("GET", "/api/AC/GetCurrentACState") => {
                if !signed_in {
                    return (401, String::new());
                }
                if !seen.header("user-agent").contains("Mozilla") {
                    return (403, String::new());
                }
                cloud.state_reads += 1;
                let id =
                    seen.query.split('&').find_map(|pair| pair.strip_prefix("ACId=")).unwrap_or_default().to_string();
                assert!(seen.query.contains("consumerId=consumer-7"), "{}", seen.query);
                if cloud.lag > 0 {
                    cloud.lag -= 1;
                } else if let Some(changed) = cloud.pending.remove(&id) {
                    cloud.states.insert(id.clone(), changed);
                }
                match cloud.states.get(&id) {
                    Some(state) => ok(json!({ "ACStateData": state })),
                    None => (404, String::new()),
                }
            }
            ("POST", path) if path.ends_with("/messages/events") => {
                let token = format!(
                    "SharedAccessSignature sr={}&sig={TOSHIBA_SIGNATURE}&se=4000000000",
                    path_encoded(&format!(
                        "{}/devices/{TOSHIBA_DEVICE}",
                        cloud.hub_host.clone().unwrap_or_else(|| seen.header("host").to_string())
                    ))
                );
                if seen.header("authorization") != token || !cloud.hub_token_ok {
                    return (401, String::new());
                }
                assert_eq!(path, format!("/devices/{}/messages/events", path_encoded(TOSHIBA_DEVICE)));
                assert!(seen.query.contains("api-version="), "{}", seen.query);
                assert_eq!(seen.header("iothub-app-type"), "mob");
                assert_eq!(seen.header("iothub-contenttype"), "application/json");
                let body: Value = serde_json::from_str(&seen.body).unwrap();
                let data = body["payload"]["data"].as_str().unwrap().to_string();
                let unique = body["targetId"][0].as_str().unwrap().to_string();
                cloud.commands.push(body);
                if cloud.mode != CloudMode::Ignores {
                    let index = AC_UNIQUE_IDS.iter().position(|known| *known == unique).expect("a known unit");
                    let id = AC_IDS[index].to_string();
                    let latest = cloud.pending.get(&id).or_else(|| cloud.states.get(&id)).unwrap().clone();
                    cloud.pending.insert(id, with_changes(&latest, &data));
                }
                (204, String::new())
            }
            _ => (404, String::new()),
        }
    }
}

impl MockToshiba {
    pub async fn start() -> Self {
        let cloud = Arc::new(Mutex::new(ToshibaCloud {
            mode: CloudMode::Normal,
            states: AC_IDS.iter().map(|id| (id.to_string(), AC_OFF.to_string())).collect(),
            pending: HashMap::new(),
            lag: 0,
            bearer_ok: true,
            hub_token_ok: true,
            hub_host: None,
            logins: 0,
            registrations: 0,
            state_reads: 0,
            commands: Vec::new(),
        }));
        let http = MockHttp::start_rigged(toshiba_handler(cloud.clone())).await;
        // A message to the unit asks it to do something; sign-ins and readings do not.
        http.count_commands(|seen| seen.method == "POST" && seen.path.ends_with("/messages/events"));
        Self { http, cloud }
    }

    pub fn traffic(&self) -> usize {
        self.http.traffic()
    }

    pub fn target(&self, index: usize) -> String {
        format!("ac-{}", AC_IDS[index])
    }

    /// What a unit reports now, once anything still in flight has arrived.
    pub fn state(&self, index: usize) -> String {
        let mut cloud = self.cloud.lock().unwrap();
        if let Some(changed) = cloud.pending.remove(AC_IDS[index]) {
            cloud.states.insert(AC_IDS[index].to_string(), changed);
        }
        cloud.states[AC_IDS[index]].clone()
    }

    pub fn set_state(&self, index: usize, state: &str) {
        self.cloud.lock().unwrap().states.insert(AC_IDS[index].to_string(), state.to_string());
    }
}
