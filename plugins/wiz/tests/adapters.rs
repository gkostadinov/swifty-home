//! The WiZ driver, called directly, against a mock bulb on loopback. The tests
//! under the hub's names come from the hub's former `tests/adapters.rs`.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use serde_json::{json, Value};
use swifty_plugin_api::{DeviceClass, DeviceError, DeviceRecord, Driver, Op, State};
use swifty_plugin_testkit::devices::{BulbMode, BulbState, MockBroadcast, MockBulb};

fn wiz_config(bulb: &MockBulb) -> Value {
    json!({ "bulbs": [{ "id": "bulb1", "address": bulb.addr.to_string() }], "port": 38899 })
}

async fn wiz() -> (MockBulb, Arc<dyn Driver>) {
    let bulb = MockBulb::start().await;
    let adapter = driver(wiz_config(&bulb), Vec::new());
    (bulb, adapter)
}

#[tokio::test]
async fn wiz_power_is_a_set_pilot_and_reads_back_with_get_pilot() {
    let (bulb, adapter) = wiz().await;
    adapter.command("bulb1", &Op::LightPower { on: false }).await.unwrap();
    let sent = bulb.received.lock().unwrap().last().cloned().unwrap();
    assert_eq!(sent["method"], json!("setPilot"));
    assert_eq!(sent["params"], json!({ "state": false }));
    assert!(!bulb.state.lock().unwrap().on);

    let state = adapter.read_state("bulb1").await.unwrap();
    assert!(matches!(state, State::Light { on: false, .. }));
    assert_eq!(bulb.methods(), ["setPilot", "getPilot"]);
}

#[tokio::test]
async fn wiz_brightness_turns_the_bulb_on_and_never_goes_under_ten() {
    let (bulb, adapter) = wiz().await;
    bulb.state.lock().unwrap().on = false;
    adapter.command("bulb1", &Op::LightBrightness { percent: 75 }).await.unwrap();
    assert_eq!(*bulb.state.lock().unwrap(), BulbState { on: true, dimming: 75, rgb: None, temp: Some(2700) });

    adapter.command("bulb1", &Op::LightBrightness { percent: 4 }).await.unwrap();
    assert_eq!(bulb.state.lock().unwrap().dimming, 10);
    let state = adapter.read_state("bulb1").await.unwrap();
    assert!(Op::LightBrightness { percent: 4 }.is_confirmed_by(&state, None));
    assert!(!Op::LightBrightness { percent: 11 }.is_confirmed_by(&state, None));
}

#[tokio::test]
async fn wiz_colour_goes_out_as_rgb_and_comes_back_as_the_same_hue() {
    let (bulb, adapter) = wiz().await;
    adapter.command("bulb1", &Op::LightHue { hue: 225 }).await.unwrap();
    let state = adapter.read_state("bulb1").await.unwrap();
    assert_eq!(state, State::Light { on: true, bri: 60, hue: 225, color: true });
    assert!(Op::LightHue { hue: 225 }.is_confirmed_by(&state, None));
    assert!(bulb.state.lock().unwrap().rgb.is_some());
}

#[tokio::test]
async fn wiz_white_mode_cannot_confirm_a_colour() {
    let (bulb, adapter) = wiz().await;
    bulb.set_mode(BulbMode::AcksOnly);
    adapter.command("bulb1", &Op::LightHue { hue: 34 }).await.unwrap();
    // A bulb at warm white reports a warm tint, which must not count as hue 34.
    let state = adapter.read_state("bulb1").await.unwrap();
    assert_eq!(state, State::Light { on: true, bri: 60, hue: 34, color: false });
    assert!(!Op::LightHue { hue: 34 }.is_confirmed_by(&state, None));
}

#[tokio::test]
async fn wiz_failures_map_to_the_right_errors() {
    let (bulb, adapter) = wiz().await;
    bulb.set_mode(BulbMode::Silent);
    let op = Op::LightPower { on: true };
    assert_eq!(adapter.command("bulb1", &op).await, Err(DeviceError::Timeout));
    assert_eq!(adapter.read_state("bulb1").await, Err(DeviceError::Timeout));
    bulb.set_mode(BulbMode::Error);
    assert!(matches!(adapter.command("bulb1", &op).await, Err(DeviceError::Rejected(_))));
    bulb.set_mode(BulbMode::Garbage);
    assert!(matches!(adapter.read_state("bulb1").await, Err(DeviceError::Rejected(_))));
}

#[tokio::test]
async fn wiz_sends_nothing_for_a_bulb_it_has_no_address_for() {
    let (bulb, adapter) = wiz().await;
    let op = Op::LightPower { on: true };
    assert!(matches!(adapter.command("bulb2", &op).await, Err(DeviceError::Disabled(_))));
    assert!(refusal(&wiz_config(&bulb), &[], "bulb2", &op).is_some());
    assert_eq!(bulb.traffic(), 0);
}

#[tokio::test]
async fn wiz_refuses_operations_that_are_not_for_a_bulb() {
    let (bulb, adapter) = wiz().await;
    assert!(refusal(&wiz_config(&bulb), &[], "bulb1", &Op::VmStart).is_some());
    assert!(matches!(adapter.command("bulb1", &Op::VmStart).await, Err(DeviceError::Disabled(_))));
    assert_eq!(bulb.traffic(), 0);
}

// ----- every adapter has a read timeout, and none retries

/// Generous, but far below the 120 seconds a hung mock would take.
const OUTER_LIMIT: Duration = Duration::from_secs(5);

async fn assert_gives_up<T: std::fmt::Debug>(
    name: &str,
    call: impl std::future::Future<Output = Result<T, DeviceError>>,
) {
    let started = Instant::now();
    let result = tokio::time::timeout(OUTER_LIMIT, call).await.unwrap_or_else(|_| panic!("{name} never gave up"));
    assert!(started.elapsed() < OUTER_LIMIT, "{name}");
    assert!(matches!(&result, Err(DeviceError::Timeout)), "{name}: {result:?}");
}

/// The WiZ part of the hub's test of the same name.
#[tokio::test]
async fn a_device_that_never_answers_times_out_and_is_asked_once() {
    let bulb = MockBulb::start().await;
    bulb.set_mode(BulbMode::Silent);
    let adapter = driver(wiz_config(&bulb), Vec::new());
    assert_gives_up("wiz", adapter.command("bulb1", &Op::LightPower { on: true })).await;
    assert_eq!(bulb.traffic(), 1);
}

// ----- discovery and adopted bulbs

const MACS: [&str; 2] = ["020000000001", "020000000002"];

fn adopted(local_id: &str, address: &str) -> DeviceRecord {
    DeviceRecord {
        target: local_id.to_string(),
        local_id: local_id.to_string(),
        class: DeviceClass::Light,
        name: "Hall lamp".to_string(),
        attrs: json!({ "address": address }),
    }
}

#[tokio::test]
async fn discovery_offers_every_bulb_that_answers_and_changes_none() {
    let (broadcast, bulbs) = MockBroadcast::start(&MACS).await;
    let adapter = driver(json!({ "broadcast": broadcast.addr.to_string() }), Vec::new());
    let found = adapter.discover(Duration::from_millis(500)).await.unwrap();
    let ids: Vec<&str> = found.iter().map(|decl| decl.local_id.as_str()).collect();
    assert_eq!(ids, ["wiz-020000000001", "wiz-020000000002"]);
    assert_eq!(found[0].target_hint.as_deref(), Some("wiz-020000000001"));
    assert_eq!(found[0].class, DeviceClass::Light);
    assert_eq!(found[0].attrs["address"], json!(bulbs[0].addr.to_string()));
    assert_eq!(found[0].attrs["mac"], json!(MACS[0]));
    assert_eq!(broadcast.datagrams.lock().unwrap()[0]["method"], json!("getPilot"));
    assert!(bulbs.iter().all(|bulb| bulb.traffic() == 0), "discovery changes nothing on a bulb");
}

#[tokio::test]
async fn discovery_without_a_broadcast_address_says_so_and_sends_nothing() {
    let bulb = MockBulb::start().await;
    let adapter = driver(wiz_config(&bulb), Vec::new());
    match adapter.discover(Duration::from_millis(200)).await {
        Err(DeviceError::Disabled(text)) => assert!(text.contains("broadcast"), "{text}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(bulb.traffic(), 0);
}

#[tokio::test]
async fn an_adopted_bulb_is_reached_at_its_stored_address_and_no_other() {
    let bulb = MockBulb::start().await;
    let id = "wiz-020000000001";
    let adapter = driver(json!({}), vec![adopted(id, &bulb.addr.to_string())]);
    adapter.command(id, &Op::LightPower { on: false }).await.unwrap();
    assert!(!bulb.state.lock().unwrap().on);
    assert!(matches!(adapter.read_state(id).await, Ok(State::Light { on: false, .. })));

    let stranger = "wiz-020000000002";
    assert!(matches!(adapter.command(stranger, &Op::LightPower { on: true }).await, Err(DeviceError::Disabled(_))));
    assert!(refusal(&json!({}), &[adopted(id, "192.0.2.30")], stranger, &Op::LightPower { on: true }).is_some());
    assert_eq!(bulb.traffic(), 2);
}
