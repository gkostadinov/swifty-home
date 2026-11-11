//! The guided setup, scripted against the mock Moonraker.

mod common;

use std::collections::BTreeMap;

use common::*;
use serde_json::json;
use swifty_plugin_api::{Plugin, Scalar, SetupInput, SetupResult, SetupStep};
use swifty_plugin_moonraker::PLUGIN;
use swifty_plugin_testkit::devices::{MockMoonraker, MOONRAKER_KEY};
use swifty_plugin_testkit::trap::Trap;

fn form(url: &str, key: &str) -> SetupInput {
    let values: BTreeMap<String, Scalar> = [("url", url), ("api_key", key)]
        .into_iter()
        .map(|(field, value)| (field.to_string(), Scalar::Text(value.to_string())))
        .collect();
    SetupInput::Values { values }
}

fn done(step: SetupStep) -> SetupResult {
    match step {
        SetupStep::Done(result) => result,
        other => panic!("not done: {other:?}"),
    }
}

#[tokio::test]
async fn the_setup_reaches_done_against_the_mock() {
    let mock = MockMoonraker::start().await;
    let ctx = setup_context(true, &[]);
    let mut flow = PLUGIN.setup().unwrap();
    let step = flow.begin(&ctx, None).await;
    let SetupStep::Form { fields, .. } = &step else { panic!("{step:?}") };
    let keys: Vec<&str> = fields.iter().map(|field| field.key.as_str()).collect();
    assert_eq!(keys, ["url", "api_key"]);

    let result = done(flow.advance(&ctx, form(&mock.http.url, MOONRAKER_KEY)).await);
    PLUGIN.validate(&result.config).unwrap();
    assert_eq!(result.config, json!({ "url": mock.http.url }));
    let slots: Vec<(&str, &str)> = result.secrets.iter().map(|(slot, value)| (slot.as_str(), value.expose())).collect();
    assert_eq!(slots, [("api_key", MOONRAKER_KEY)]);
    assert!(result.adopt.is_empty(), "the printer comes from the config");
    assert_eq!(result.summary, "Moonraker answered: the printer is idle.");
    for shown in [result.config.to_string(), result.summary.clone(), format!("{result:?}")] {
        assert!(!shown.contains(MOONRAKER_KEY), "{shown}");
    }
    // The one test read, with the key as a header.
    let requests = mock.http.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!((requests[0].method.as_str(), requests[0].path.as_str()), ("GET", "/printer/objects/query"));
    assert_eq!(requests[0].header("x-api-key"), MOONRAKER_KEY);
}

#[tokio::test]
async fn the_setup_without_a_key_stores_none() {
    let mock = MockMoonraker::start().await;
    let ctx = setup_context(true, &[]);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    let result = done(flow.advance(&ctx, form(&mock.http.url, "")).await);
    assert!(result.secrets.is_empty());
    assert_eq!(mock.http.requests()[0].header("x-api-key"), "");
}

#[tokio::test]
async fn the_setup_in_dry_run_says_live_mode_is_needed_and_reaches_nothing() {
    let trap = Trap::start().await;
    let ctx = setup_context(false, &[]);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    let step = flow.advance(&ctx, form(&format!("http://{}", trap.addr), "")).await;
    let SetupStep::Failed { reason, retry } = step else { panic!("{step:?}") };
    assert!(reason.contains("live") && !retry, "{reason}");
    assert_eq!(trap.traffic(), 0);
}

#[tokio::test]
async fn a_printer_that_cannot_be_read_fails_and_the_form_comes_back() {
    let mock = MockMoonraker::start().await;
    mock.state.lock().unwrap().klippy_gone = true;
    let ctx = setup_context(true, &[]);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    let step = flow.advance(&ctx, form(&mock.http.url, MOONRAKER_KEY)).await;
    let SetupStep::Failed { reason, retry } = step else { panic!("{step:?}") };
    assert!(retry && reason.contains("Klipper"), "{reason}");
    assert!(!reason.contains(MOONRAKER_KEY));
    let step = flow.advance(&ctx, SetupInput::Back).await;
    let SetupStep::Form { fields, .. } = &step else { panic!("{step:?}") };
    assert_eq!(fields[0].value, Some(Scalar::Text(mock.http.url.clone())), "what was entered is offered again");
    assert!(fields[1].value.is_none());
    mock.state.lock().unwrap().klippy_gone = false;
    done(flow.advance(&ctx, form(&mock.http.url, MOONRAKER_KEY)).await);
}

#[tokio::test]
async fn a_bad_address_is_shown_on_the_form_and_contacts_nothing() {
    let ctx = setup_context(true, &[]);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    let step = flow.advance(&ctx, form("http://192.0.2.30:7125/printer", "")).await;
    assert!(matches!(&step, SetupStep::Form { text, .. } if text.starts_with("url:")), "{step:?}");
}

#[tokio::test]
async fn setting_up_again_with_the_stored_key_kept_asks_nothing() {
    let trap = Trap::start().await;
    let ctx = setup_context(true, &["api_key"]);
    let existing = json!({ "url": format!("http://{}", trap.addr) });
    let mut flow = PLUGIN.setup().unwrap();
    let step = flow.begin(&ctx, Some(&existing)).await;
    let SetupStep::Form { fields, .. } = &step else { panic!("{step:?}") };
    assert_eq!(fields[0].value, Some(Scalar::Text(format!("http://{}", trap.addr))));
    assert!(fields[1].help.contains("keep"));
    let result = done(flow.advance(&ctx, form(&format!("http://{}", trap.addr), "")).await);
    assert!(result.secrets.is_empty());
    assert_eq!(trap.traffic(), 0);
}
