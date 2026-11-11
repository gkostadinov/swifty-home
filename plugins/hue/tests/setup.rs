//! The setup flow, driven by scripted inputs against a mock bridge with a link
//! button.

mod common;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use common::*;
use serde_json::json;
use swifty_plugin_api::{Plugin, Scalar, SetupInput, SetupStep};
use swifty_plugin_hue::setup::{POLL_MS, TIMEOUT_S};
use swifty_plugin_hue::PLUGIN;
use swifty_plugin_testkit::devices::{hue_bridge_handler, HueLight, MockHttp, SharedBridge, HUE_KEY};

/// A bridge with two lights, which hands out [`HUE_KEY`] once its button is pressed.
struct PairingBridge {
    http: MockHttp,
    button: Arc<AtomicBool>,
    bridge: SharedBridge,
}

async fn pairing_bridge() -> PairingBridge {
    let button = Arc::new(AtomicBool::new(false));
    let bridge = SharedBridge::default();
    {
        let mut shared = bridge.lock().unwrap();
        shared.lights.insert("1".to_string(), HueLight::colour("Lightstrip"));
        shared.lights.insert("2".to_string(), HueLight::white("Hall"));
    }
    let (pressed, lights) = (button.clone(), hue_bridge_handler(bridge.clone()));
    let http = MockHttp::start(move |seen| {
        if seen.method == "POST" && seen.path == "/api" {
            return if pressed.load(Ordering::SeqCst) {
                (200, json!([{ "success": { "username": HUE_KEY } }]).to_string())
            } else {
                let early =
                    json!([{ "error": { "type": 101, "address": "", "description": "link button not pressed" } }]);
                (200, early.to_string())
            };
        }
        lights(seen)
    })
    .await;
    PairingBridge { http, button, bridge }
}

fn url(value: &str) -> SetupInput {
    let values: BTreeMap<String, Scalar> = [("url".to_string(), Scalar::Text(value.to_string()))].into();
    SetupInput::Values { values }
}

fn reason(step: &SetupStep) -> String {
    match step {
        SetupStep::Failed { reason, .. } => reason.clone(),
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[tokio::test]
async fn a_scripted_setup_pairs_with_the_link_button_and_reaches_done() {
    let mock = pairing_bridge().await;
    let ctx = setup_context(true, false);
    let mut flow = PLUGIN.setup().unwrap();

    let SetupStep::Form { fields, .. } = flow.begin(&ctx, None).await else { panic!("a form first") };
    assert_eq!(fields.iter().map(|f| f.key.as_str()).collect::<Vec<_>>(), ["url"]);

    let step = flow.advance(&ctx, url(&mock.http.url)).await;
    assert!(matches!(step, SetupStep::Wait { poll_ms: POLL_MS, timeout_s: TIMEOUT_S, .. }), "{step:?}");
    assert_eq!(mock.http.traffic(), 0, "nothing is asked before the first poll");

    let step = flow.advance(&ctx, SetupInput::Poll).await;
    assert!(matches!(step, SetupStep::Wait { .. }), "the button is not pressed yet: {step:?}");
    mock.button.store(true, Ordering::SeqCst);
    let step = flow.advance(&ctx, SetupInput::Poll).await;
    let SetupStep::Choose { options, multiple, min, .. } = step else { panic!("{step:?}") };
    assert!(multiple);
    assert_eq!(min, 0);
    let listed: Vec<(&str, &str, &str)> =
        options.iter().map(|o| (o.id.as_str(), o.label.as_str(), o.detail.as_str())).collect();
    assert_eq!(listed, [("hue-1", "Lightstrip", "Colour light"), ("hue-2", "Hall", "White light")]);

    let posts = mock.http.posts();
    assert_eq!(posts.len(), 2, "one pairing request per poll, none after the key");
    for post in &posts {
        let body: serde_json::Value = serde_json::from_str(&post.body).unwrap();
        assert_eq!(body, json!({ "devicetype": "swifty_home#hub" }));
    }

    let step = flow.advance(&ctx, SetupInput::Chosen { ids: vec!["hue-1".to_string()] }).await;
    let SetupStep::Done(result) = step else { panic!("{step:?}") };
    PLUGIN.validate(&result.config).unwrap();
    assert_eq!(result.config, json!({ "url": mock.http.url }));
    assert_eq!(result.secrets.len(), 1);
    assert_eq!(result.secrets[0].0, "app_key");
    assert_eq!(result.secrets[0].1.expose(), HUE_KEY);
    assert_eq!(result.adopt.len(), 1);
    assert_eq!(result.adopt[0].local_id, "hue-1");
    assert_eq!(result.adopt[0].attrs["colour"], json!(true));
    assert_eq!(result.summary, "Paired. 1 light added.");
    // The key is only in `secrets`.
    assert!(!serde_json::to_string(&result).unwrap().contains(HUE_KEY));
    assert!(!format!("{result:?}").contains(HUE_KEY));
    // One reading of the chosen light ended the flow, and nothing was changed.
    let last = mock.http.requests().last().cloned().unwrap();
    assert_eq!(last.path, format!("/api/{HUE_KEY}/lights/1"));
    assert!(mock.http.requests().iter().all(|r| r.method != "PUT"));
}

#[tokio::test]
async fn in_dry_run_pairing_sends_nothing() {
    let mock = pairing_bridge().await;
    mock.button.store(true, Ordering::SeqCst);
    let ctx = setup_context(false, false);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    let step = flow.advance(&ctx, url(&mock.http.url)).await;
    assert!(matches!(step, SetupStep::Failed { retry: false, .. }), "{step:?}");
    assert!(reason(&step).contains("live mode"));
    let step = flow.advance(&ctx, SetupInput::Poll).await;
    assert!(matches!(step, SetupStep::Failed { .. }), "{step:?}");
    assert_eq!(mock.http.traffic(), 0);
}

#[tokio::test]
async fn an_existing_key_is_kept_unless_the_owner_pairs_again() {
    let mock = pairing_bridge().await;
    mock.button.store(true, Ordering::SeqCst);
    let ctx = setup_context(true, true);
    let mut flow = PLUGIN.setup().unwrap();
    let existing = json!({ "url": mock.http.url });
    let SetupStep::Form { fields, .. } = flow.begin(&ctx, Some(&existing)).await else { panic!("a form first") };
    assert_eq!(fields[0].value, Some(Scalar::Text(mock.http.url.clone())));
    assert_eq!(fields[1].key, "pair_again");

    let step = flow.advance(&ctx, url(&mock.http.url)).await;
    let SetupStep::Done(result) = step else { panic!("{step:?}") };
    assert!(result.secrets.is_empty(), "the stored key is untouched");
    assert_eq!(mock.http.traffic(), 0);

    let values: BTreeMap<String, Scalar> =
        [("url".to_string(), Scalar::Text(mock.http.url.clone())), ("pair_again".to_string(), Scalar::Bool(true))]
            .into();
    let step = flow.advance(&ctx, SetupInput::Values { values }).await;
    assert!(matches!(step, SetupStep::Wait { .. }), "{step:?}");
}

#[tokio::test]
async fn a_bridge_address_with_a_path_is_refused_before_anything_is_sent() {
    let mock = pairing_bridge().await;
    let ctx = setup_context(true, false);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    let step = flow.advance(&ctx, url(&format!("{}/api", mock.http.url))).await;
    assert!(matches!(step, SetupStep::Failed { retry: true, .. }), "{step:?}");
    assert_eq!(mock.http.traffic(), 0);
}

/// The hub ends a wait by its own clock after `timeout_s`, and the person's Try again polls the same wait
/// again. The flow must not end it earlier or sooner than that by a count of its own: a count of polls
/// that reaches its limit just before the hub's clock does made the first Try again fail at once.
#[tokio::test]
async fn the_flow_keeps_no_count_of_its_own_so_a_try_again_after_the_hubs_timeout_can_pair() {
    let mock = pairing_bridge().await;
    let ctx = setup_context(true, false);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    flow.advance(&ctx, url(&mock.http.url)).await;
    // More polls than the wait's time holds at its own pace: the hub gave up and the person tries again.
    let polls = TIMEOUT_S * 1000 / POLL_MS + 10;
    for number in 1..=polls {
        let step = flow.advance(&ctx, SetupInput::Poll).await;
        assert!(matches!(step, SetupStep::Wait { .. }), "poll {number}: {step:?}");
    }
    assert_eq!(mock.http.posts().len(), polls as usize, "every poll asked the bridge");
    mock.button.store(true, Ordering::SeqCst);
    let step = flow.advance(&ctx, SetupInput::Poll).await;
    assert!(matches!(step, SetupStep::Choose { .. }), "the button was pressed after the long wait: {step:?}");
}

#[tokio::test]
async fn a_light_that_cannot_be_read_fails_the_setup_and_keeps_the_pairing() {
    let mock = pairing_bridge().await;
    mock.button.store(true, Ordering::SeqCst);
    let ctx = setup_context(true, false);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    flow.advance(&ctx, url(&mock.http.url)).await;
    assert!(matches!(flow.advance(&ctx, SetupInput::Poll).await, SetupStep::Choose { .. }));
    mock.bridge.lock().unwrap().lights.get_mut("1").unwrap().reachable = false;
    let step = flow.advance(&ctx, SetupInput::Chosen { ids: vec!["hue-1".to_string()] }).await;
    assert!(reason(&step).contains("Lightstrip"), "{step:?}");
    assert!(!reason(&step).contains(HUE_KEY));
    let step = flow.advance(&ctx, SetupInput::Chosen { ids: vec!["hue-9".to_string()] }).await;
    assert!(reason(&step).contains("hue-9"), "{step:?}");
    // Trying again lists the lights with the key already given, without pairing again.
    assert!(matches!(flow.advance(&ctx, SetupInput::Poll).await, SetupStep::Choose { .. }));
    assert_eq!(mock.http.posts().len(), 1);
    let step = flow.advance(&ctx, SetupInput::Chosen { ids: vec!["hue-2".to_string()] }).await;
    assert!(matches!(step, SetupStep::Done(_)), "{step:?}");
}
