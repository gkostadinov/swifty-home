//! The setup flow, scripted against a mock node: Form, then one read, then Done.

mod common;

use std::collections::BTreeSet;

use common::*;
use serde_json::json;
use swifty_plugin_api::{FieldType, Plugin, Scalar, SetupInput, SetupStep};
use swifty_plugin_glimdock::PLUGIN;
use swifty_plugin_testkit::devices::{collector_handler, MockHttp, COLLECTOR_TOKEN, SNAPSHOT_PATH};
use swifty_plugin_testkit::trap::Trap;

fn form_keys(step: &SetupStep) -> Vec<String> {
    match step {
        SetupStep::Form { fields, .. } => fields.iter().map(|field| field.key.clone()).collect(),
        other => panic!("expected a form, got {other:?}"),
    }
}

#[tokio::test]
async fn the_setup_reaches_done_after_one_read_of_the_node() {
    let mock = MockHttp::start(collector_handler).await;
    let ctx = setup_context(Some(grant()), BTreeSet::new());
    let mut flow = PLUGIN.setup().unwrap();
    let form = flow.begin(&ctx, None).await;
    assert_eq!(form_keys(&form), ["url", "snapshot_path", "token", "pairing_path"]);
    if let SetupStep::Form { fields, .. } = &form {
        assert!(matches!(fields[2].kind, FieldType::Secret));
        assert!(fields[2].required && fields[2].value.is_none());
        assert!(!fields[3].required);
    }
    let done = flow
        .advance(
            &ctx,
            values(&[
                ("url", &mock.url),
                ("snapshot_path", SNAPSHOT_PATH),
                ("token", COLLECTOR_TOKEN),
                ("pairing_path", PAIRING_PATH),
            ]),
        )
        .await;
    let SetupStep::Done(result) = done else { panic!("expected Done, got {done:?}") };
    assert_eq!(result.config, pairing_config(&mock.url, SNAPSHOT_PATH));
    assert!(PLUGIN.validate(&result.config).is_ok());
    assert_eq!(result.secrets.len(), 1);
    assert_eq!(result.secrets[0].0, "token");
    assert_eq!(result.secrets[0].1.expose(), COLLECTOR_TOKEN);
    assert!(result.adopt.is_empty(), "the node's device comes from its config");
    assert!(result.summary.contains("Connected"), "{}", result.summary);
    // The secret is in `secrets` and nowhere else.
    let shown = format!("{} {result:?}", serde_json::to_string(&result).unwrap());
    assert!(!shown.contains(COLLECTOR_TOKEN), "{shown}");
    // One read of the snapshot, and the pairing endpoint is never called.
    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!((requests[0].method.as_str(), requests[0].path.as_str()), ("GET", SNAPSHOT_PATH));
}

#[tokio::test]
async fn in_dry_run_the_setup_contacts_nothing() {
    let trap = Trap::start().await;
    let url = format!("http://{}", trap.addr);
    let ctx = setup_context(None, BTreeSet::new());
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    let done = flow
        .advance(&ctx, values(&[("url", &url), ("snapshot_path", "/api/snapshot"), ("token", COLLECTOR_TOKEN)]))
        .await;
    let SetupStep::Done(result) = done else { panic!("expected Done, got {done:?}") };
    assert_eq!(result.config, config(&url, "/api/snapshot"));
    assert!(result.summary.contains("dry-run"), "{}", result.summary);
    assert_eq!(result.secrets.len(), 1);
    assert_eq!(trap.traffic(), 0);
}

#[tokio::test]
async fn a_node_that_refuses_the_token_fails_the_setup_and_offers_it_again() {
    let mock = MockHttp::start(collector_handler).await;
    let ctx = setup_context(Some(grant()), BTreeSet::new());
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    let wrong = "wrong-token-abcdefghijklmnop";
    let failed =
        flow.advance(&ctx, values(&[("url", &mock.url), ("snapshot_path", SNAPSHOT_PATH), ("token", wrong)])).await;
    let SetupStep::Failed { reason, retry } = failed else { panic!("expected Failed, got {failed:?}") };
    assert!(retry);
    assert!(reason.contains("token"), "{reason}");
    assert!(!reason.contains(wrong));
    assert_eq!(mock.requests().len(), 1, "one read, no retry");

    // Back to the form, filled in again, without the token.
    let form = flow.advance(&ctx, SetupInput::Back).await;
    let SetupStep::Form { fields, .. } = form else { panic!("expected the form") };
    assert_eq!(fields[0].value, Some(Scalar::Text(mock.url.clone())));
    assert_eq!(fields[1].value, Some(Scalar::Text(SNAPSHOT_PATH.to_string())));
    assert!(fields[2].value.is_none(), "a secret is never filled in");

    let done = flow
        .advance(&ctx, values(&[("url", &mock.url), ("snapshot_path", SNAPSHOT_PATH), ("token", COLLECTOR_TOKEN)]))
        .await;
    assert!(matches!(done, SetupStep::Done(_)), "{done:?}");
}

#[tokio::test]
async fn a_form_that_breaks_the_rules_is_refused_before_anything_is_sent() {
    let trap = Trap::start().await;
    let url = format!("http://{}", trap.addr);
    let ctx = setup_context(Some(grant()), BTreeSet::new());
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    for (form, named) in [
        (vec![("url", url.as_str()), ("snapshot_path", "//elsewhere/x"), ("token", "t")], "snapshot_path"),
        (vec![("url", "http://192.0.2.10/api"), ("snapshot_path", "/s"), ("token", "t")], "url"),
        (
            vec![("url", url.as_str()), ("snapshot_path", "/s"), ("token", "t"), ("pairing_path", "pair")],
            "pairing_path",
        ),
        (vec![("url", url.as_str()), ("snapshot_path", "/s")], "token"),
        (vec![("url", url.as_str()), ("snapshot_path", "/s"), ("token", "a\u{7}b")], "token"),
        (vec![("snapshot_path", "/s"), ("token", "t")], "url"),
    ] {
        let step = flow.advance(&ctx, values(&form)).await;
        assert!(
            matches!(&step, SetupStep::Failed { reason, retry: true } if reason.contains(named)),
            "{named}: {step:?}"
        );
    }
    let mut numbers = std::collections::BTreeMap::new();
    numbers.insert("url".to_string(), Scalar::Int(5));
    let step = flow.advance(&ctx, SetupInput::Values { values: numbers }).await;
    assert!(matches!(step, SetupStep::Failed { .. }), "{step:?}");
    let step = flow.advance(&ctx, SetupInput::Poll).await;
    assert!(matches!(step, SetupStep::Failed { .. }), "{step:?}");
    assert_eq!(trap.traffic(), 0);
}

#[tokio::test]
async fn setting_up_again_keeps_the_stored_token() {
    let trap = Trap::start().await;
    let url = format!("http://{}", trap.addr);
    let existing = pairing_config(&url, "/api/snapshot");
    let ctx = setup_context(Some(grant()), with_token());
    let mut flow = PLUGIN.setup().unwrap();
    let form = flow.begin(&ctx, Some(&existing)).await;
    let SetupStep::Form { fields, .. } = form else { panic!("expected the form") };
    assert_eq!(fields[0].value, Some(Scalar::Text(url.clone())));
    assert_eq!(fields[3].value, Some(Scalar::Text(PAIRING_PATH.to_string())));
    assert!(!fields[2].required, "the stored token can be kept");
    assert!(fields[2].value.is_none());

    let done = flow.advance(&ctx, values(&[("url", &url), ("snapshot_path", "/api/v2/snapshot"), ("token", "")])).await;
    let SetupStep::Done(result) = done else { panic!("expected Done, got {done:?}") };
    assert_eq!(result.config, json!({ "url": url, "snapshot_path": "/api/v2/snapshot" }));
    assert!(result.secrets.is_empty(), "the stored token is left as it is");
    assert_eq!(trap.traffic(), 0, "without the token's value the flow cannot read the node");
}
