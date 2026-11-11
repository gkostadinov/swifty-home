//! The guided setup, scripted against the mock speakers.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use swifty_plugin_api::{Plugin, Scalar, SetupContext, SetupInput, SetupStep};
use swifty_plugin_sonos::PLUGIN;
use swifty_plugin_testkit::devices::MockSonos;
use swifty_plugin_testkit::trap::Trap;

fn context(live: bool) -> SetupContext {
    SetupContext {
        instance_id: "sonos".to_string(),
        grant: live.then(common::grant),
        timeouts: common::timeouts(),
        secrets_present: BTreeSet::new(),
    }
}

fn values(pairs: &[(&str, &str)]) -> SetupInput {
    let values: BTreeMap<String, Scalar> =
        pairs.iter().map(|(key, value)| (key.to_string(), Scalar::Text(value.to_string()))).collect();
    SetupInput::Values { values }
}

/// `127.0.0.1:<port>` of a mock.
fn address(url: &str) -> String {
    url.trim_start_matches("http://").trim_end_matches('/').to_string()
}

fn field_keys(step: &SetupStep) -> Vec<String> {
    match step {
        SetupStep::Form { fields, .. } => fields.iter().map(|field| field.key.clone()).collect(),
        other => panic!("expected a form, got {other:?}"),
    }
}

#[tokio::test]
async fn the_scripted_setup_reads_each_speaker_and_reaches_done() {
    let arc = MockSonos::start(true).await;
    let sub = MockSonos::start(false).await;
    let ctx = context(true);
    let mut flow = PLUGIN.setup().unwrap();

    let first = flow.begin(&ctx, None).await;
    assert_eq!(field_keys(&first), ["address", "sub_address"]);

    let input = values(&[("address", &address(&arc.http.url)), ("sub_address", &address(&sub.http.url))]);
    let SetupStep::Done(result) = flow.advance(&ctx, input).await else { panic!("not done") };
    assert!(result.secrets.is_empty());
    assert!(result.summary.contains("Found 2 speaker(s)"), "{}", result.summary);
    PLUGIN.validate(&result.config).unwrap();
    assert_eq!(
        result.config,
        json!({ "speakers": [
            { "id": "sonos", "url": format!("http://{}", address(&arc.http.url)) },
            { "id": "sub", "url": format!("http://{}", address(&sub.http.url)), "read_only": true },
        ]})
    );
    // One read of each: the subwoofer only through RenderingControl, and nothing commanded.
    assert!(arc.actions().iter().all(|action| action.starts_with("Get")), "{:?}", arc.actions());
    assert!(sub.http.requests().iter().all(|r| r.path == "/MediaRenderer/RenderingControl/Control"));
    assert!(sub.actions().iter().all(|action| action.starts_with("Get")));
}

#[tokio::test]
async fn a_speaker_that_cannot_be_read_fails_the_setup_with_its_address() {
    let gone = Trap::start().await;
    let ctx = context(true);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    let step = flow.advance(&ctx, values(&[("address", &gone.addr.to_string())])).await;
    let SetupStep::Failed { reason, retry } = step else { panic!("{step:?}") };
    assert!(retry);
    assert!(reason.contains(&gone.addr.to_string()), "{reason}");
}

#[tokio::test]
async fn in_dry_run_the_form_is_saved_and_nothing_is_contacted() {
    let speaker = Trap::start().await;
    let ctx = context(false);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    let step = flow.advance(&ctx, values(&[("address", &speaker.addr.to_string())])).await;
    let SetupStep::Done(result) = step else { panic!("{step:?}") };
    assert!(result.summary.contains("dry-run"), "{}", result.summary);
    PLUGIN.validate(&result.config).unwrap();
    assert_eq!(speaker.traffic(), 0);
}

#[tokio::test]
async fn a_name_or_a_missing_address_is_asked_for_again() {
    let ctx = context(false);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    for input in [values(&[]), SetupInput::Poll, values(&[("address", "speaker.example.org")])] {
        let step = flow.advance(&ctx, input).await;
        assert!(matches!(step, SetupStep::Failed { retry: true, .. }), "{step:?}");
    }
    let back = flow.advance(&ctx, SetupInput::Back).await;
    let SetupStep::Form { fields, .. } = back else { panic!("{back:?}") };
    assert_eq!(fields[0].value, Some(Scalar::Text("speaker.example.org".to_string())), "what was typed is kept");
}

#[tokio::test]
async fn setting_up_again_prefills_the_form_and_keeps_the_other_rows() {
    let existing: Value = json!({ "speakers": [
        { "id": "sonos", "url": "http://192.0.2.30:1400" },
        { "id": "sub", "url": "http://192.0.2.31:1400", "read_only": true },
        { "id": "kitchen", "url": "http://192.0.2.32:1400" },
    ]});
    let ctx = context(false);
    let mut flow = PLUGIN.setup().unwrap();
    let SetupStep::Form { fields, .. } = flow.begin(&ctx, Some(&existing)).await else { panic!("no form") };
    assert_eq!(fields[0].value, Some(Scalar::Text("192.0.2.30".to_string())));
    assert_eq!(fields[1].value, Some(Scalar::Text("192.0.2.31".to_string())));

    let SetupStep::Done(result) = flow.advance(&ctx, values(&[("address", "192.0.2.40")])).await else {
        panic!("not done")
    };
    assert_eq!(
        result.config,
        json!({ "speakers": [
            { "id": "sonos", "url": "http://192.0.2.40:1400" },
            { "id": "kitchen", "url": "http://192.0.2.32:1400", "read_only": false },
        ]})
    );
}
