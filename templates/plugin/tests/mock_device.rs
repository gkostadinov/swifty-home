//! The driver and the setup flow against the stand-in device: what each
//! operation sends, what is read back, that it is confirmed, and what happens
//! when the device is silent, refuses, or answers nonsense.

mod common;

use std::sync::atomic::Ordering;

use common::*;
use serde_json::json;
use swifty_plugin_api::{DeviceError, Op, Plugin, SetupStep};

use {{lib}}::protocol;
use {{lib}}::PLUGIN;

fn class_of(id: &str) -> swifty_plugin_api::DeviceClass {
    let word = DEVICES.iter().find(|(known, _)| *known == id).unwrap().1;
    protocol::class_named(word).unwrap()
}

#[tokio::test]
async fn every_device_reads_as_its_class() {
    let stand = Stand::start().await;
    let driver = stand.driver();
    for (id, _) in DEVICES {
        let state = driver.read_state(id).await.unwrap();
        assert_eq!(protocol::decode(class_of(id), &stand.states.lock().unwrap()[*id]).unwrap(), state, "{id}");
    }
}

#[tokio::test]
async fn each_operation_sends_its_fields_and_is_confirmed() {
    let stand = Stand::start().await;
    let driver = stand.driver();
    for (id, op) in sample_ops() {
        let before = driver.read_state(&id).await.unwrap();
        driver.command(&id, &op).await.unwrap();
        let (to, sent) = stand.sets.lock().unwrap().last().cloned().unwrap();
        assert_eq!(to, id);
        assert_eq!(sent, protocol::encode(class_of(&id), &op).unwrap(), "{op:?}");
        let after = driver.read_state(&id).await.unwrap();
        assert!(op.is_confirmed_by(&after, Some(&before)), "{op:?} is not confirmed by {after:?}");
        assert_eq!(refusal(&stand.config(), &id, &op), None, "{op:?}");
    }
}

#[tokio::test]
async fn a_silent_device_times_out() {
    let stand = Stand::start().await;
    let driver = stand.driver();
{{#local}}
    stand.device.silent.store(true, Ordering::SeqCst);
{{/local}}
{{#cloud}}
    stand.device.hang.store(true, Ordering::SeqCst);
{{/cloud}}
    let (id, _) = DEVICES[0];
    let error = driver.read_state(id).await.unwrap_err();
    assert!(error.is_silence(), "{error:?}");
}

#[tokio::test]
async fn a_refusal_from_the_device_is_reported_in_the_plugins_own_words() {
    let stand = Stand::start().await;
    // The stand-in does not know this one, and says so; its reason is not repeated.
    let ghost = stand.row("ghost", DEVICES[0].1);
    let driver = stand.driver_for(stand.config_with(&[ghost]));
    let error = driver.read_state("ghost").await.unwrap_err();
    assert_eq!(error, DeviceError::Rejected(protocol::REFUSED.to_string()));
    assert!(!error.to_string().contains("no such device"), "{error}");
}

#[tokio::test]
async fn what_cannot_be_done_never_reaches_the_device() {
    let stand = Stand::start().await;
    let driver = stand.driver();
    let (id, _) = DEVICES[0];
    assert!(matches!(driver.command(id, &Op::VmStart).await, Err(DeviceError::Disabled(_))));
    assert!(matches!(driver.read_state("nobody").await, Err(DeviceError::Disabled(_))));
    assert_eq!(stand.traffic(), 0);
    // The pure refusal says the same, in dry-run as well.
    assert!(refusal(&stand.config(), id, &Op::VmStart).is_some());
    assert!(refusal(&stand.config(), "nobody", &Op::VmStart).is_some());
}

#[tokio::test]
async fn nonsense_from_the_device_is_an_error_not_a_panic() {
{{#local}}
    let nonsense = swifty_plugin_testkit::devices::MockUdp::start(|_| Some(b"\x00not json".to_vec())).await;
    let config = json!({ "devices": [{ "id": "odd", "address": "127.0.0.1", "port": nonsense.addr.port() }] });
{{/local}}
{{#cloud}}
    let nonsense = swifty_plugin_testkit::devices::MockHttp::start(|_| (200, "not json".to_string())).await;
    let config = json!({ "api_url": nonsense.url, "devices": [{ "id": "odd" }] });
{{/cloud}}
    let stand = Stand::start().await;
    let driver = stand.driver_for(config);
    assert_eq!(driver.read_state("odd").await.unwrap_err(), protocol::unreadable());
}

#[tokio::test]
async fn the_setup_reaches_done_and_reads_the_device_once() {
    let stand = Stand::start().await;
    let ctx = context(true);
    let mut flow = PLUGIN.setup().unwrap();
    let mut step = flow.begin(&ctx, None).await;
    for input in stand.setup_script() {
        assert!(!matches!(step, SetupStep::Done(_) | SetupStep::Failed { .. }), "{step:?}");
        step = flow.advance(&ctx, input).await;
    }
    let SetupStep::Done(result) = step else { panic!("{step:?}") };
    PLUGIN.validate(&result.config).unwrap();
    assert_eq!(result.config["devices"][0]["name"], json!("Desk"));
    assert_eq!(stand.traffic(), 1, "one read of the new device");
{{#cloud}}
    assert!(!result.config.to_string().contains(TOKEN), "the token stays out of the config");
    assert_eq!(result.secrets.len(), 1);
{{/cloud}}
}

#[tokio::test]
async fn in_dry_run_the_setup_contacts_nothing() {
    let stand = Stand::start().await;
    let ctx = context(false);
    let mut flow = PLUGIN.setup().unwrap();
    let mut step = flow.begin(&ctx, None).await;
    for input in stand.setup_script() {
        step = flow.advance(&ctx, input).await;
    }
    let SetupStep::Done(result) = step else { panic!("{step:?}") };
    assert!(result.summary.contains("dry-run"), "{}", result.summary);
    assert_eq!(stand.traffic(), 0);
}
