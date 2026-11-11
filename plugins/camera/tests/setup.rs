//! The setup flow, scripted against a mock camera: Form, then one probe, then Done.

mod common;

use std::collections::BTreeSet;

use common::*;
use serde_json::json;
use swifty_plugin_api::{FieldType, Plugin, Scalar, SetupInput, SetupStep};
use swifty_plugin_camera::PLUGIN;
use swifty_plugin_testkit::devices::{MockRtsp, CAMERA_LOGIN};
use swifty_plugin_testkit::trap::Trap;

#[tokio::test]
async fn the_setup_reaches_done_after_one_probe_of_the_camera() {
    let mock = MockRtsp::start().await;
    let ctx = setup_context(Some(grant()), BTreeSet::new());
    let mut flow = PLUGIN.setup().unwrap();
    let SetupStep::Form { fields, .. } = flow.begin(&ctx, None).await else { panic!("expected the form") };
    assert_eq!(fields.iter().map(|field| field.key.as_str()).collect::<Vec<_>>(), ["host", "port", "login"]);
    assert_eq!(fields[1].value, Some(Scalar::Int(554)));
    assert!(matches!(fields[2].kind, FieldType::Secret) && fields[2].required);

    let input = values(&[
        ("host", text("127.0.0.1")),
        ("port", Scalar::Int(i64::from(mock.port))),
        ("login", text(CAMERA_LOGIN)),
    ]);
    let done = flow.advance(&ctx, input).await;
    let SetupStep::Done(result) = done else { panic!("expected Done, got {done:?}") };
    assert_eq!(result.config, camera_config(mock.port));
    assert!(PLUGIN.validate(&result.config).is_ok());
    assert_eq!(result.secrets.len(), 1);
    assert_eq!((result.secrets[0].0.as_str(), result.secrets[0].1.expose()), ("login", CAMERA_LOGIN));
    let shown = format!("{} {result:?}", serde_json::to_string(&result).unwrap());
    assert!(!shown.contains(CAMERA_LOGIN), "{shown}");
    assert_eq!(mock.traffic(), 1);
    assert!(!mock.received.lock().unwrap()[0].contains("camera-login"), "the probe never sends the login");
}

#[tokio::test]
async fn in_dry_run_the_setup_contacts_nothing() {
    let trap = Trap::start().await;
    let ctx = setup_context(None, BTreeSet::new());
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    let port = trap.addr.port().to_string();
    let input = values(&[("host", text("127.0.0.1")), ("port", text(&port)), ("login", text(CAMERA_LOGIN))]);
    let done = flow.advance(&ctx, input).await;
    let SetupStep::Done(result) = done else { panic!("expected Done, got {done:?}") };
    assert_eq!(result.config, camera_config(trap.addr.port()));
    assert!(result.summary.contains("dry-run"), "{}", result.summary);
    assert_eq!(trap.traffic(), 0);
}

#[tokio::test]
async fn something_that_is_not_a_camera_fails_the_setup_and_the_form_comes_back() {
    let mock = MockRtsp::start().await;
    *mock.reply.lock().unwrap() = "HTTP/1.1 200 OK\r\n\r\n".to_string();
    let ctx = setup_context(Some(grant()), BTreeSet::new());
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    let input = values(&[
        ("host", text("127.0.0.1")),
        ("port", Scalar::Int(i64::from(mock.port))),
        ("login", text(CAMERA_LOGIN)),
    ]);
    let step = flow.advance(&ctx, input).await;
    let SetupStep::Failed { reason, retry } = step else { panic!("expected Failed, got {step:?}") };
    assert!(retry && reason.contains("RTSP"), "{reason}");
    assert!(!reason.contains(CAMERA_LOGIN));

    let SetupStep::Form { fields, .. } = flow.advance(&ctx, SetupInput::Back).await else { panic!("the form") };
    assert_eq!(fields[0].value, Some(text("127.0.0.1")));
    assert_eq!(fields[1].value, Some(Scalar::Int(i64::from(mock.port))));
    assert!(fields[2].value.is_none());
}

#[tokio::test]
async fn a_bad_form_is_refused_before_anything_is_sent() {
    let trap = Trap::start().await;
    let port = Scalar::Int(i64::from(trap.addr.port()));
    let ctx = setup_context(Some(grant()), BTreeSet::new());
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    for (form, named) in [
        (vec![("host", text("rtsp://127.0.0.1")), ("port", port.clone()), ("login", text("a:b"))], "host"),
        (vec![("host", text("127.0.0.1")), ("port", Scalar::Int(0)), ("login", text("a:b"))], "port"),
        (vec![("host", text("127.0.0.1")), ("port", text("many")), ("login", text("a:b"))], "port"),
        (vec![("host", text("127.0.0.1")), ("port", port.clone())], "login"),
        (vec![("port", port.clone()), ("login", text("a:b"))], "host"),
        (vec![("host", text("127.0.0.1")), ("port", port.clone()), ("login", Scalar::Bool(true))], "login"),
    ] {
        let step = flow.advance(&ctx, values(&form)).await;
        assert!(matches!(&step, SetupStep::Failed { reason, .. } if reason.contains(named)), "{named}: {step:?}");
    }
    assert_eq!(trap.traffic(), 0);
}

#[tokio::test]
async fn setting_up_again_keeps_the_stored_login_and_still_probes() {
    let mock = MockRtsp::start().await;
    let ctx = setup_context(Some(grant()), with_login());
    let mut flow = PLUGIN.setup().unwrap();
    let existing = json!({ "host": "127.0.0.1", "port": mock.port });
    let SetupStep::Form { fields, .. } = flow.begin(&ctx, Some(&existing)).await else { panic!("the form") };
    assert_eq!(fields[0].value, Some(text("127.0.0.1")));
    assert!(!fields[2].required);
    let input =
        values(&[("host", text("127.0.0.1")), ("port", Scalar::Int(i64::from(mock.port))), ("login", text(""))]);
    let done = flow.advance(&ctx, input).await;
    let SetupStep::Done(result) = done else { panic!("expected Done, got {done:?}") };
    assert!(result.secrets.is_empty());
    assert_eq!(mock.traffic(), 1, "the probe needs no login, so it still runs");
}
