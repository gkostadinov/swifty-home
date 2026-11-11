//! The setup flow, run step by step as the hub runs it, against the test kit's mock of
//! Petkit's cloud.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::{grant, login, timeouts};
use serde_json::{json, Value};
use swifty_plugin_api::{Plugin, Scalar, SetupContext, SetupInput, SetupResult, SetupStep};
use swifty_plugin_petkit::{LOGIN_SLOT, PLUGIN};
use swifty_plugin_testkit::devices::{MockPetkit, PETKIT_ACCOUNT, PETKIT_PASSWORD_MD5};

/// The sign-in secret whose MD5 the mock knows.
const KNOWN_SECRET: &str = "password";

fn context(live: bool) -> SetupContext {
    SetupContext {
        instance_id: "petkit".to_string(),
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
    values(&[
        ("region", Scalar::Text("FR".to_string())),
        ("account", Scalar::Text(PETKIT_ACCOUNT.to_string())),
        ("sign_in", Scalar::Text(sign_in.to_string())),
        ("timezone", Scalar::Text("Europe/Paris".to_string())),
        ("utc_offset_minutes", Scalar::Int(120)),
    ])
}

fn both() -> SetupInput {
    SetupInput::Chosen { ids: vec!["4001".to_string(), "4002".to_string()] }
}

fn existing(mock: &MockPetkit) -> Value {
    json!({ "region": "FR", "passport_url": mock.http.url })
}

fn field_keys(step: &SetupStep) -> Vec<String> {
    match step {
        SetupStep::Form { fields, .. } => fields.iter().map(|field| field.key.clone()).collect(),
        other => panic!("not a form: {other:?}"),
    }
}

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
async fn the_setup_starts_with_the_disclaimer_and_reaches_done_with_commands_off() {
    let mock = MockPetkit::start().await;
    let steps = run(Some(&existing(&mock)), vec![accept(), account(KNOWN_SECRET), both()], true).await;

    let SetupStep::Form { text, .. } = &steps[0] else { panic!("{:?}", steps[0]) };
    assert!(text.contains("unofficial") && text.contains("second Petkit account"), "{text}");
    assert_eq!(field_keys(&steps[0]), ["accept"]);
    assert_eq!(field_keys(&steps[1]), ["region", "account", "sign_in", "timezone", "utc_offset_minutes"]);
    let SetupStep::Form { fields, .. } = &steps[1] else { unreachable!() };
    assert!(fields.iter().find(|field| field.key == "sign_in").unwrap().value.is_none(), "a secret is never prefilled");
    let SetupStep::Choose { options, .. } = &steps[2] else { panic!("{:?}", steps[2]) };
    let listed: Vec<(&str, &str, bool)> =
        options.iter().map(|option| (option.id.as_str(), option.label.as_str(), option.selected)).collect();
    assert_eq!(listed, [("4001", "Petkit D4 feeder", true), ("4002", "Petkit T4 litter box", true)]);

    let result = done(&steps[3]);
    PLUGIN.validate(&result.config).unwrap();
    assert_eq!(result.config["commands"], false, "commands start off");
    assert_eq!((result.config["feeder_id"].as_u64(), result.config["litter_id"].as_u64()), (Some(4001), Some(4002)));
    assert_eq!(result.config["timezone"], "Europe/Paris");
    assert_eq!(result.secrets.len(), 1);
    let (slot, stored) = &result.secrets[0];
    assert_eq!(
        (slot.as_str(), stored.expose()),
        (LOGIN_SLOT, login().as_str()),
        "the account and the MD5, nothing else"
    );
    let shown = format!("{} {result:?}", serde_json::to_string(result).unwrap());
    assert!(!shown.contains(PETKIT_PASSWORD_MD5) && !shown.contains(PETKIT_ACCOUNT), "{shown}");

    let cloud = mock.cloud.lock().unwrap().clone();
    assert_eq!(cloud.login_attempts, 1, "one login serves the listing and the readings");
    assert_eq!(mock.calls("/gw/d4/device_detail").len(), 1, "the feeder is read once");
    assert_eq!(mock.calls("/gw/t4/device_detail").len(), 1, "the litter box is read once");
    assert_eq!((cloud.applied, cloud.fed_grams.len()), (0, 0), "nothing was fed or cleaned");
}

#[tokio::test]
async fn devices_shared_to_the_account_can_be_chosen_one_at_a_time() {
    let mock = MockPetkit::start().await;
    mock.cloud.lock().unwrap().shared = true;
    let feeder = SetupInput::Chosen { ids: vec!["4001".to_string()] };
    let steps = run(Some(&existing(&mock)), vec![accept(), account(KNOWN_SECRET), feeder], true).await;
    let result = done(&steps[3]);
    assert_eq!(result.config["feeder_id"], 4001);
    assert!(result.config.get("litter_id").is_none());
    assert!(mock.calls("/gw/t4/device_detail").is_empty(), "only what was chosen is read");
}

#[tokio::test]
async fn setting_up_again_keeps_commands_as_they_were() {
    let mock = MockPetkit::start().await;
    let mut stored = existing(&mock);
    stored["commands"] = json!(true);
    stored["poll_secs"] = json!(120);
    let steps = run(Some(&stored), vec![accept(), account(KNOWN_SECRET), both()], true).await;
    let SetupStep::Form { fields, .. } = &steps[1] else { panic!("{:?}", steps[1]) };
    assert_eq!(fields[0].value, Some(Scalar::Text("FR".to_string())), "the region is prefilled");
    let result = done(&steps[3]);
    assert_eq!((result.config["commands"].as_bool(), result.config["poll_secs"].as_u64()), (Some(true), Some(120)));
}

#[tokio::test]
async fn a_refused_login_and_a_wrong_choice_are_said_plainly() {
    let mock = MockPetkit::start().await;
    let steps = run(Some(&existing(&mock)), vec![accept(), account("not it")], true).await;
    let SetupStep::Failed { reason, retry } = &steps[2] else { panic!("{:?}", steps[2]) };
    assert!(*retry && reason.contains("refused"), "{reason}");
    assert!(!reason.contains(PETKIT_ACCOUNT), "{reason}");

    let nothing = SetupInput::Chosen { ids: vec!["999".to_string()] };
    let steps = run(Some(&existing(&mock)), vec![accept(), account(KNOWN_SECRET), nothing], true).await;
    assert!(matches!(&steps[3], SetupStep::Failed { retry: true, .. }), "{:?}", steps[3]);
    let steps = run(Some(&existing(&mock)), vec![accept(), account(KNOWN_SECRET), SetupInput::Back], true).await;
    assert_eq!(field_keys(&steps[3])[0], "region", "back to the account");
}

#[tokio::test]
async fn in_dry_run_the_setup_says_live_mode_is_needed_and_reaches_nothing() {
    let mock = MockPetkit::start().await;
    let steps = run(Some(&existing(&mock)), vec![values(&[("accept", Scalar::Bool(false))])], false).await;
    assert!(matches!(&steps[1], SetupStep::Failed { retry: true, .. }), "the notice has to be accepted");
    let steps = run(Some(&existing(&mock)), vec![accept(), account(KNOWN_SECRET)], false).await;
    let SetupStep::Failed { reason, retry } = &steps[2] else { panic!("{:?}", steps[2]) };
    assert!(reason.contains("live") && !retry, "{reason}");
    assert_eq!(mock.http.traffic(), 0);
}
