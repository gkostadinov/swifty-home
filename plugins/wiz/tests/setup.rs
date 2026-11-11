//! The setup flow, driven by scripted inputs against mock bulbs.

mod common;

use std::collections::BTreeMap;

use common::*;
use serde_json::json;
use swifty_plugin_api::{Plugin, Scalar, SetupInput, SetupStep};
use swifty_plugin_testkit::devices::{BulbMode, MockBroadcast, MockBulb};
use swifty_plugin_wiz::PLUGIN;

const MACS: [&str; 2] = ["020000000001", "020000000002"];

fn values(pairs: &[(&str, &str)]) -> SetupInput {
    let values: BTreeMap<String, Scalar> =
        pairs.iter().map(|(key, value)| (key.to_string(), Scalar::Text(value.to_string()))).collect();
    SetupInput::Values { values }
}

fn reason(step: &SetupStep) -> String {
    match step {
        SetupStep::Failed { reason, retry: true } => reason.clone(),
        other => panic!("expected a failure to retry, got {other:?}"),
    }
}

#[tokio::test]
async fn a_scripted_setup_finds_bulbs_lets_the_owner_choose_and_reaches_done() {
    let (broadcast, bulbs) = MockBroadcast::start(&MACS).await;
    let ctx = setup_context(true);
    let mut flow = PLUGIN.setup().unwrap();

    let SetupStep::Form { fields, .. } = flow.begin(&ctx, None).await else { panic!("a form first") };
    let keys: Vec<&str> = fields.iter().map(|field| field.key.as_str()).collect();
    assert_eq!(keys, ["broadcast", "addresses"]);
    assert_eq!(fields[0].value, Some(Scalar::Text("255.255.255.255:38899".to_string())), "a guess is prefilled");

    let step = flow.advance(&ctx, values(&[("broadcast", &broadcast.addr.to_string())])).await;
    let SetupStep::Choose { options, multiple, min, .. } = step else { panic!("{step:?}") };
    assert!(multiple);
    assert_eq!(min, 1);
    let ids: Vec<&str> = options.iter().map(|option| option.id.as_str()).collect();
    assert_eq!(ids, ["wiz-020000000001", "wiz-020000000002"]);
    assert!(bulbs.iter().all(|bulb| bulb.traffic() == 0), "choosing changes nothing");

    let step = flow.advance(&ctx, SetupInput::Chosen { ids: vec!["wiz-020000000002".to_string()] }).await;
    let SetupStep::Done(result) = step else { panic!("{step:?}") };
    PLUGIN.validate(&result.config).unwrap();
    assert_eq!(result.config["broadcast"], json!(broadcast.addr.to_string()));
    assert!(result.secrets.is_empty());
    assert_eq!(result.adopt.len(), 1);
    assert_eq!(result.adopt[0].local_id, "wiz-020000000002");
    assert_eq!(result.adopt[0].attrs["address"], json!(bulbs[1].addr.to_string()));
    assert!(result.summary.contains("1 WiZ bulb"), "{}", result.summary);
    // The flow ends with one reading of each chosen bulb, and nothing else.
    assert_eq!(bulbs[1].methods(), ["getPilot"]);
    assert_eq!(bulbs[0].traffic(), 0);
}

#[tokio::test]
async fn bulbs_by_address_finish_in_dry_run_and_send_nothing() {
    let bulb = MockBulb::start().await;
    let ctx = setup_context(false);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    let addresses = format!("desk={}, 192.0.2.22", bulb.addr);
    let step = flow.advance(&ctx, values(&[("broadcast", ""), ("addresses", &addresses)])).await;
    let SetupStep::Done(result) = step else { panic!("{step:?}") };
    PLUGIN.validate(&result.config).unwrap();
    assert_eq!(
        result.config["bulbs"],
        json!([{ "id": "desk", "address": bulb.addr.to_string() }, { "id": "bulb1", "address": "192.0.2.22" }])
    );
    assert!(result.adopt.is_empty());
    assert_eq!(bulb.traffic(), 0);
}

#[tokio::test]
async fn in_dry_run_discovery_is_refused_and_nothing_is_sent() {
    let (broadcast, _bulbs) = MockBroadcast::start(&MACS).await;
    let ctx = setup_context(false);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    let step = flow.advance(&ctx, values(&[("broadcast", &broadcast.addr.to_string())])).await;
    assert!(reason(&step).contains("live mode"), "{step:?}");
    assert_eq!(broadcast.traffic(), 0);
}

#[tokio::test]
async fn a_bulb_that_cannot_be_read_fails_the_setup_with_its_name() {
    let bulb = MockBulb::start().await;
    bulb.set_mode(BulbMode::Silent);
    let ctx = setup_context(true);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    let step = flow.advance(&ctx, values(&[("addresses", &format!("porch={}", bulb.addr))])).await;
    assert!(reason(&step).contains("porch"), "{step:?}");
    assert_eq!(bulb.traffic(), 1, "one reading, never retried");
}

#[tokio::test]
async fn nothing_answering_and_nothing_given_are_explained() {
    let (broadcast, _bulbs) = MockBroadcast::start(&[]).await;
    let ctx = setup_context(true);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    let step = flow.advance(&ctx, values(&[("broadcast", &broadcast.addr.to_string())])).await;
    assert!(reason(&step).contains("No WiZ bulb answered"), "{step:?}");
    let step = flow.advance(&ctx, values(&[("broadcast", ""), ("addresses", "")])).await;
    assert!(reason(&step).contains("at least one"), "{step:?}");
    let step = flow.advance(&ctx, values(&[("broadcast", "everywhere")])).await;
    assert!(reason(&step).contains("IPv4"), "{step:?}");
    let step = flow.advance(&ctx, SetupInput::Chosen { ids: vec!["wiz-020000000001".to_string()] }).await;
    assert!(reason(&step).contains("nothing to choose"), "{step:?}");
}

#[tokio::test]
async fn setting_up_again_starts_from_the_stored_config() {
    let ctx = setup_context(false);
    let mut flow = PLUGIN.setup().unwrap();
    let existing = json!({ "bulbs": [{ "id": "bulb1", "address": "192.0.2.21" }], "broadcast": "192.0.2.255:38899" });
    let SetupStep::Form { fields, .. } = flow.begin(&ctx, Some(&existing)).await else { panic!("a form first") };
    assert_eq!(fields[0].value, Some(Scalar::Text("192.0.2.255:38899".to_string())));
    assert_eq!(fields[1].value, Some(Scalar::Text("bulb1=192.0.2.21".to_string())));
    assert!(matches!(flow.advance(&ctx, SetupInput::Back).await, SetupStep::Form { .. }));
}
