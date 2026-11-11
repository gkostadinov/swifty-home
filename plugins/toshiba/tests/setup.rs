//! The setup flow, run step by step as the hub runs it, against a mock of the three
//! calls it makes: the sign-in, the list of units on the account, and one reading each.
//!
//! The test kit's `MockToshiba` has no list of units, so the mock is built here on the
//! kit's generic `MockHttp`.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use base64::Engine;
use common::{grant, timeouts, PINNED_SUFFIX};
use serde_json::{json, Value};
use swifty_plugin_api::{Plugin, Scalar, SetupContext, SetupInput, SetupResult, SetupStep};
use swifty_plugin_testkit::devices::{MockHttp, Seen, AC_OFF, TOSHIBA_EMAIL, TOSHIBA_PASSWORD};
use swifty_plugin_toshiba::protocol::SIGN_IN_SECRET_KEY;
use swifty_plugin_toshiba::{identity, LOGIN_SLOT, PLUGIN};

const ONE: &str = "0a1b2c3d-1111-2222-3333-444455556666";
const TWO: &str = "0a1b2c3d-1111-2222-3333-777788889999";
const BEARER: &str = "setup-bearer";

#[derive(Default)]
struct Calls {
    logins: usize,
    listings: usize,
    readings: Vec<String>,
    device_ids: Vec<String>,
}

/// Toshiba's cloud as the setup sees it. It answers only a client that looks like a
/// browser and names itself, as the real one does.
async fn cloud() -> (MockHttp, Arc<Mutex<Calls>>) {
    let seen = Arc::new(Mutex::new(Calls::default()));
    let record = seen.clone();
    let mock = MockHttp::start(move |request: &Seen| {
        let mut seen = record.lock().unwrap();
        let ok =
            |result: Value| (200, json!({ "IsSuccess": true, "StatusCode": "Success", "ResObj": result }).to_string());
        if !request.header("user-agent").contains("Mozilla") {
            return (403, String::new());
        }
        let device_id = request.header("device-id").to_string();
        if !device_id.starts_with(&format!("{TOSHIBA_EMAIL}_")) {
            return (429, String::new());
        }
        seen.device_ids.push(device_id);
        let signed_in = request.header("authorization") == format!("Bearer {BEARER}");
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/api/Consumer/Login") => {
                seen.logins += 1;
                let body: Value = serde_json::from_str(&request.body).unwrap();
                if body["Username"] != TOSHIBA_EMAIL || body[SIGN_IN_SECRET_KEY] != TOSHIBA_PASSWORD {
                    let refusal = json!({ "IsSuccess": false, "StatusCode": "InvalidUserNameorPassword" });
                    return (200, refusal.to_string());
                }
                ok(json!({ "access_token": BEARER, "token_type": "Bearer", "consumerId": "consumer-9" }))
            }
            ("GET", "/api/AC/GetConsumerACMapping") if signed_in => {
                seen.listings += 1;
                assert_eq!(request.query, "consumerId=consumer-9");
                ok(json!([{ "GroupName": "Home", "ACList": [
                    { "Id": ONE, "DeviceUniqueId": "UNIT-ONE", "Name": "Upstairs" },
                    { "Id": TWO, "DeviceUniqueId": "UNIT-TWO", "Name": "" },
                    { "Id": "bad", "DeviceUniqueId": "UNIT-BAD", "Name": "Not usable" }
                ] }]))
            }
            ("GET", "/api/AC/GetCurrentACState") if signed_in => {
                let id = request.query.split('&').find_map(|pair| pair.strip_prefix("ACId=")).unwrap_or_default();
                seen.readings.push(id.to_string());
                ok(json!({ "ACStateData": AC_OFF }))
            }
            _ => (401, String::new()),
        }
    })
    .await;
    (mock, seen)
}

fn context(live: bool) -> SetupContext {
    SetupContext {
        instance_id: "toshiba".to_string(),
        grant: if live { Some(grant()) } else { None },
        timeouts: timeouts(),
        secrets_present: BTreeSet::new(),
    }
}

fn values(pairs: &[(&str, Scalar)]) -> SetupInput {
    let values: BTreeMap<String, Scalar> = pairs.iter().map(|(key, value)| (key.to_string(), value.clone())).collect();
    SetupInput::Values { values }
}

fn accept() -> SetupInput {
    values(&[("accept", Scalar::Bool(true))])
}

fn account(sign_in: &str) -> SetupInput {
    values(&[("account", Scalar::Text(TOSHIBA_EMAIL.to_string())), ("sign_in", Scalar::Text(sign_in.to_string()))])
}

fn existing(mock: &MockHttp) -> Value {
    json!({ "api_url": mock.url, "hub_url": mock.url })
}

fn field_keys(step: &SetupStep) -> Vec<String> {
    match step {
        SetupStep::Form { fields, .. } => fields.iter().map(|field| field.key.clone()).collect(),
        other => panic!("not a form: {other:?}"),
    }
}

/// Runs the whole flow with these inputs and hands back the last step.
async fn run(existing: Option<&Value>, inputs: Vec<SetupInput>, live: bool) -> Vec<SetupStep> {
    let ctx = context(live);
    let mut flow = PLUGIN.setup().unwrap();
    let mut steps = vec![flow.begin(&ctx, existing).await];
    for input in inputs {
        steps.push(flow.advance(&ctx, input).await);
    }
    steps
}

fn done(step: &SetupStep) -> &SetupResult {
    match step {
        SetupStep::Done(result) => result,
        other => panic!("not done: {other:?}"),
    }
}

#[tokio::test]
async fn the_setup_starts_with_the_disclaimer_and_reaches_done() {
    let (mock, seen) = cloud().await;
    let chosen = SetupInput::Chosen { ids: vec![ONE.to_string(), TWO.to_string()] };
    let steps = run(Some(&existing(&mock)), vec![accept(), account(TOSHIBA_PASSWORD), chosen], true).await;

    let SetupStep::Form { text, .. } = &steps[0] else { panic!("{:?}", steps[0]) };
    assert!(text.contains("unofficial"), "{text}");
    assert_eq!(field_keys(&steps[0]), ["accept"]);
    assert_eq!(field_keys(&steps[1]), ["account", "sign_in"]);
    let SetupStep::Form { fields, .. } = &steps[1] else { unreachable!() };
    assert!(fields.iter().all(|field| field.value.is_none()), "nothing is prefilled, least of all a secret");
    let SetupStep::Choose { options, multiple, min, .. } = &steps[2] else { panic!("{:?}", steps[2]) };
    let labels: Vec<&str> = options.iter().map(|option| option.label.as_str()).collect();
    assert_eq!(labels, ["Upstairs", "Air conditioner 2"], "the unit with an id it cannot use is left out");
    assert!(*multiple && *min == 1);

    let result = done(&steps[3]);
    PLUGIN.validate(&result.config).unwrap();
    assert_eq!(result.config["client_suffix"], identity::CLIENT_SUFFIX, "a new install gets the new default");
    assert_eq!(
        result.config["units"],
        json!([{ "id": ONE, "unique_id": "UNIT-ONE", "name": "Upstairs" }, { "id": TWO, "unique_id": "UNIT-TWO" }])
    );
    assert_eq!(result.secrets.len(), 1);
    let (slot, login) = &result.secrets[0];
    assert_eq!(slot, LOGIN_SLOT);
    let plain = base64::engine::general_purpose::STANDARD.decode(login.expose()).unwrap();
    let plain: Value = serde_json::from_slice(&plain).unwrap();
    assert_eq!(plain["Username"], TOSHIBA_EMAIL);
    assert!(result.summary.contains("2 air conditioners"), "{}", result.summary);
    let shown = format!("{} {result:?}", serde_json::to_string(result).unwrap());
    assert!(!shown.contains(TOSHIBA_PASSWORD) && !shown.contains(login.expose()), "{shown}");

    let seen = seen.lock().unwrap();
    assert_eq!((seen.logins, seen.listings), (1, 1), "one sign-in serves the listing and the readings");
    assert_eq!(seen.readings, [ONE, TWO], "each chosen unit is read once");
    let expected = format!("{TOSHIBA_EMAIL}_{}", identity::CLIENT_SUFFIX);
    assert!(seen.device_ids.iter().all(|id| *id == expected), "{:?}", seen.device_ids);
}

#[tokio::test]
async fn setting_up_again_keeps_the_client_id_the_cloud_knows() {
    let (mock, seen) = cloud().await;
    let mut stored = existing(&mock);
    stored["client_suffix"] = json!(PINNED_SUFFIX);
    stored["units"] = json!([{ "id": ONE, "unique_id": "UNIT-ONE" }]);
    let chosen = SetupInput::Chosen { ids: vec![TWO.to_string()] };
    let steps = run(Some(&stored), vec![accept(), account(TOSHIBA_PASSWORD), chosen], true).await;
    let result = done(&steps[3]);
    assert_eq!(result.config["client_suffix"], PINNED_SUFFIX);
    assert_eq!(result.config["units"], json!([{ "id": TWO, "unique_id": "UNIT-TWO" }]));
    let expected = format!("{TOSHIBA_EMAIL}_{PINNED_SUFFIX}");
    assert!(seen.lock().unwrap().device_ids.iter().all(|id| *id == expected));
}

#[tokio::test]
async fn nothing_goes_on_until_the_disclaimer_is_accepted() {
    let (mock, seen) = cloud().await;
    let steps = run(Some(&existing(&mock)), vec![values(&[("accept", Scalar::Bool(false))])], true).await;
    assert!(matches!(&steps[1], SetupStep::Failed { retry: true, .. }), "{:?}", steps[1]);
    let steps = run(Some(&existing(&mock)), vec![accept(), SetupInput::Back], true).await;
    assert_eq!(field_keys(&steps[2]), ["accept"], "back to the notice");
    assert_eq!(seen.lock().unwrap().logins, 0);
    assert_eq!(mock.traffic(), 0);
}

#[tokio::test]
async fn a_refused_sign_in_is_said_plainly_and_stays_private() {
    let (mock, seen) = cloud().await;
    let steps = run(Some(&existing(&mock)), vec![accept(), account("not it at all")], true).await;
    let SetupStep::Failed { reason, retry } = &steps[2] else { panic!("{:?}", steps[2]) };
    assert!(*retry);
    assert!(reason.contains("refused the sign-in"), "{reason}");
    assert!(!reason.contains("not it at all") && !reason.contains(TOSHIBA_EMAIL), "{reason}");
    assert_eq!(seen.lock().unwrap().listings, 0);
}

#[tokio::test]
async fn in_dry_run_the_setup_says_live_mode_is_needed_and_reaches_nothing() {
    let (mock, _seen) = cloud().await;
    let steps = run(Some(&existing(&mock)), vec![accept(), account(TOSHIBA_PASSWORD)], false).await;
    assert_eq!(field_keys(&steps[1]), ["account", "sign_in"], "the forms still work");
    let SetupStep::Failed { reason, retry } = &steps[2] else { panic!("{:?}", steps[2]) };
    assert!(reason.contains("live"), "{reason}");
    assert!(!retry);
    assert_eq!(mock.traffic(), 0);
}
