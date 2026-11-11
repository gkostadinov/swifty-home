//! Shared helpers: building the plugin's driver and views against the loopback
//! mocks, and a mock of Tuya's cloud OpenAPI with its own copy of the signing.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use swifty_plugin_api::net::{LiveGrant, Timeouts};
use swifty_plugin_api::{
    DeviceRecord, Directory, Driver, InstanceView, LiveContext, Op, Plugin, Scalar, Secrets, SetupContext,
};
use swifty_plugin_testkit::devices::{MockHttp, MockTuya, Seen, TUYA_KEY_BASE64};
use swifty_plugin_tuya::PLUGIN;

pub fn grant() -> LiveGrant {
    LiveGrant::when_live(true).unwrap()
}

pub fn timeouts() -> Timeouts {
    Timeouts { connect: Duration::from_millis(300), read: Duration::from_millis(300) }
}

/// No adopted devices: Tuya's sockets all come from the config.
pub struct NoDevices;

impl Directory for NoDevices {
    fn get(&self, _local_id: &str) -> Option<DeviceRecord> {
        None
    }
    fn all(&self) -> Vec<DeviceRecord> {
        Vec::new()
    }
}

/// The config row for a mock socket on loopback.
pub fn row(plug: &MockTuya) -> Value {
    json!({ "id": plug.id, "address": "127.0.0.1", "port": plug.port })
}

/// The slot and stored value of a mock socket's key.
pub fn key(plug: &MockTuya, stored: &str) -> (String, String) {
    (format!("local_key/{}", plug.id), stored.to_string())
}

/// The live driver for these rows and keys, built by the plugin itself.
pub fn driver(rows: Vec<Value>, keys: Vec<(String, String)>) -> Arc<dyn Driver> {
    PLUGIN
        .connect(LiveContext {
            instance_id: "tuya".to_string(),
            config: json!({ "plugs": rows }),
            secrets: Secrets::of(keys),
            grant: grant(),
            timeouts: timeouts(),
            directory: Arc::new(NoDevices),
        })
        .unwrap()
}

/// One mock socket and the driver for it, holding `stored` as its key.
pub async fn tuya_with_key(id: &str, stored: &str) -> (MockTuya, Arc<dyn Driver>, String) {
    let plug = MockTuya::start(id).await;
    let driver = driver(vec![row(&plug)], vec![key(&plug, stored)]);
    let target = plug.target();
    (plug, driver, target)
}

pub async fn tuya(id: &str) -> (MockTuya, Arc<dyn Driver>, String) {
    tuya_with_key(id, TUYA_KEY_BASE64).await
}

pub fn power(on: bool) -> Op {
    Op::PlugPower { on }
}

/// What the pure `refusal` says for this config and these filled slots.
pub fn refusal(config: &Value, present: &[String], device: &str, op: &Op) -> Option<String> {
    let present: BTreeSet<String> = present.iter().cloned().collect();
    let view = InstanceView { id: "tuya", config, secrets_present: &present, adopted: &[] };
    PLUGIN.refusal(&view, device, op)
}

pub fn context(live: bool, present: &[&str]) -> SetupContext {
    SetupContext {
        instance_id: "tuya".to_string(),
        grant: LiveGrant::when_live(live),
        timeouts: timeouts(),
        secrets_present: present.iter().map(|slot| slot.to_string()).collect(),
    }
}

pub fn values<K: AsRef<str>>(pairs: &[(K, Scalar)]) -> swifty_plugin_api::SetupInput {
    swifty_plugin_api::SetupInput::Values {
        values: pairs.iter().map(|(key, value)| (key.as_ref().to_string(), value.clone())).collect(),
    }
}

pub fn text(value: &str) -> Scalar {
    Scalar::Text(value.to_string())
}

pub fn chosen(ids: &[&str]) -> swifty_plugin_api::SetupInput {
    swifty_plugin_api::SetupInput::Chosen { ids: ids.iter().map(|id| id.to_string()).collect() }
}

// ----- a mock of Tuya's cloud OpenAPI

/// The test project's access id and secret. Not real values.
pub const ACCESS_ID: &str = "testprojectaccessid0";
pub const ACCESS_SECRET: &str = "0123456789abcdef0123456789abcdef";
/// What the mock hands out as a token.
const ISSUED: &str = "mocktokenforthetests";

/// What a mock cloud does.
#[derive(Clone)]
pub struct CloudPlan {
    /// The pages of the device list, in order. Each is the `devices` array.
    pub pages: Vec<Value>,
    /// When set, the device list is refused with this code.
    pub refuse_list: Option<i64>,
}

/// The signature the mock expects, built here from the documented recipe and
/// not with the plugin's code, so a mistake in one cannot agree with itself.
fn expected_sign(seen: &Seen, token: &str) -> String {
    let empty = hex::encode(Sha256::digest(b""));
    let url = if seen.query.is_empty() { seen.path.clone() } else { format!("{}?{}", seen.path, seen.query) };
    let string_to_sign = format!("{}\n{empty}\n\n{url}", seen.method);
    let message = format!("{ACCESS_ID}{token}{}{string_to_sign}", seen.header("t"));
    let mut mac = Hmac::<Sha256>::new_from_slice(ACCESS_SECRET.as_bytes()).unwrap();
    mac.update(message.as_bytes());
    hex::encode(mac.finalize().into_bytes()).to_uppercase()
}

fn refused(code: i64, msg: &str) -> (u16, String) {
    (200, json!({ "success": false, "code": code, "msg": msg, "t": 1 }).to_string())
}

fn answered(result: Value) -> (u16, String) {
    (200, json!({ "success": true, "result": result, "t": 1 }).to_string())
}

/// Checks what every signed request must carry.
fn signed_well(seen: &Seen, token: &str) -> bool {
    seen.method == "GET"
        && seen.header("client_id") == ACCESS_ID
        && seen.header("sign_method") == "HMAC-SHA256"
        && seen.header("t").len() == 13
        && seen.header("t").bytes().all(|b| b.is_ascii_digit())
        && seen.header("sign") == expected_sign(seen, token)
}

pub async fn mock_cloud(plan: CloudPlan) -> MockHttp {
    MockHttp::start(move |seen| match seen.path.as_str() {
        "/v1.0/token" => {
            if seen.query != "grant_type=1" || !seen.header("access_token").is_empty() {
                return (400, "bad token request".to_string());
            }
            if !signed_well(seen, "") {
                return refused(1004, "sign invalid");
            }
            answered(json!({ "access_token": ISSUED, "expire_time": 7200, "refresh_token": "r", "uid": "u" }))
        }
        "/v1.0/iot-01/associated-users/devices" => {
            if seen.header("access_token") != ISSUED || !signed_well(seen, ISSUED) {
                return refused(1004, "sign invalid");
            }
            if let Some(code) = plan.refuse_list {
                return refused(code, "permission deny");
            }
            let page = match seen.query.as_str() {
                "size=50" => 0,
                query => match query.strip_prefix("last_row_key=ROW").and_then(|rest| rest.strip_suffix("&size=50")) {
                    Some(number) => number.parse::<usize>().unwrap(),
                    None => return (400, format!("unexpected query {query}")),
                },
            };
            let devices = plan.pages.get(page).cloned().unwrap_or(json!([]));
            let more = page + 1 < plan.pages.len();
            answered(json!({
                "devices": devices,
                "has_more": more,
                "last_row_key": if more { format!("ROW{}", page + 1) } else { String::new() },
                "total": 0,
            }))
        }
        other => (404, format!("no route {other}")),
    })
    .await
}

/// A device as the cloud lists it.
pub fn listed(id: &str, name: &str, local_key: &str, category: &str) -> Value {
    json!({ "id": id, "name": name, "local_key": local_key, "category": category,
            "product_name": "Smart socket", "online": true, "ip": "198.51.100.7", "uid": "u" })
}
