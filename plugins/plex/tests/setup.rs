//! The setup flow, scripted against a mock server: Form, then one read, then Done.

mod common;

use std::collections::BTreeSet;

use common::*;
use swifty_plugin_api::{FieldType, Plugin, Scalar, SetupInput, SetupStep};
use swifty_plugin_plex::PLUGIN;
use swifty_plugin_testkit::devices::{plex_handler, MockHttp, PLEX_TOKEN};
use swifty_plugin_testkit::trap::Trap;

#[tokio::test]
async fn the_setup_reaches_done_after_one_read_of_the_server() {
    let mock = MockHttp::start(plex_handler).await;
    let ctx = setup_context(Some(grant()), BTreeSet::new());
    let mut flow = PLUGIN.setup().unwrap();
    let SetupStep::Form { fields, .. } = flow.begin(&ctx, None).await else { panic!("expected the form") };
    assert_eq!(fields.iter().map(|field| field.key.as_str()).collect::<Vec<_>>(), ["url", "token"]);
    assert!(matches!(fields[1].kind, FieldType::Secret) && fields[1].required);

    let done = flow.advance(&ctx, values(&[("url", &mock.url), ("token", PLEX_TOKEN)])).await;
    let SetupStep::Done(result) = done else { panic!("expected Done, got {done:?}") };
    assert_eq!(result.config, config(&mock.url));
    assert!(PLUGIN.validate(&result.config).is_ok());
    assert_eq!(result.secrets.len(), 1);
    assert_eq!((result.secrets[0].0.as_str(), result.secrets[0].1.expose()), ("token", PLEX_TOKEN));
    let shown = format!("{} {result:?}", serde_json::to_string(&result).unwrap());
    assert!(!shown.contains(PLEX_TOKEN), "{shown}");
    assert_eq!(mock.requests().len(), 1);
    assert_eq!(mock.requests()[0].header("x-plex-token"), PLEX_TOKEN);
}

#[tokio::test]
async fn in_dry_run_the_setup_contacts_nothing() {
    let trap = Trap::start().await;
    let url = format!("http://{}", trap.addr);
    let ctx = setup_context(None, BTreeSet::new());
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    let done = flow.advance(&ctx, values(&[("url", &url), ("token", PLEX_TOKEN)])).await;
    let SetupStep::Done(result) = done else { panic!("expected Done, got {done:?}") };
    assert!(result.summary.contains("dry-run"), "{}", result.summary);
    assert_eq!(trap.traffic(), 0);
}

#[tokio::test]
async fn a_refused_token_fails_the_setup_and_the_form_comes_back() {
    let mock = MockHttp::start(plex_handler).await;
    let ctx = setup_context(Some(grant()), BTreeSet::new());
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    let wrong = "wrong-plex-token-abcdefghij";
    let step = flow.advance(&ctx, values(&[("url", &mock.url), ("token", wrong)])).await;
    let SetupStep::Failed { reason, retry } = step else { panic!("expected Failed, got {step:?}") };
    assert!(retry && reason.contains("token") && !reason.contains(wrong), "{reason}");

    let SetupStep::Form { fields, .. } = flow.advance(&ctx, SetupInput::Back).await else { panic!("the form") };
    assert_eq!(fields[0].value, Some(Scalar::Text(mock.url.clone())));
    assert!(fields[1].value.is_none());
}

#[tokio::test]
async fn a_bad_form_is_refused_before_anything_is_sent() {
    let trap = Trap::start().await;
    let url = format!("http://{}", trap.addr);
    let ctx = setup_context(Some(grant()), BTreeSet::new());
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    for (form, named) in [
        (vec![("url", "http://192.0.2.20:32400/web"), ("token", "t")], "url"),
        (vec![("token", "t")], "url"),
        (vec![("url", url.as_str())], "token"),
        (vec![("url", url.as_str()), ("token", "a\u{7}b")], "token"),
    ] {
        let step = flow.advance(&ctx, values(&form)).await;
        assert!(matches!(&step, SetupStep::Failed { reason, .. } if reason.contains(named)), "{named}: {step:?}");
    }
    assert_eq!(trap.traffic(), 0);
}

#[tokio::test]
async fn setting_up_again_keeps_the_stored_token() {
    let trap = Trap::start().await;
    let url = format!("http://{}", trap.addr);
    let ctx = setup_context(Some(grant()), with_token());
    let mut flow = PLUGIN.setup().unwrap();
    let SetupStep::Form { fields, .. } = flow.begin(&ctx, Some(&config(&url))).await else { panic!("the form") };
    assert_eq!(fields[0].value, Some(Scalar::Text(url.clone())));
    assert!(!fields[1].required);
    let done = flow.advance(&ctx, values(&[("url", &url), ("token", "")])).await;
    let SetupStep::Done(result) = done else { panic!("expected Done, got {done:?}") };
    assert!(result.secrets.is_empty());
    assert_eq!(trap.traffic(), 0);
}
