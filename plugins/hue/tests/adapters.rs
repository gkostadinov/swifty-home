//! The Hue driver, called directly, against a mock bridge on loopback. The tests
//! under the hub's names come from the hub's former `tests/adapters.rs`; the key that
//! used to be read from a file now comes from the `app_key` secret slot.

mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use serde_json::json;
use swifty_plugin_api::{DeviceError, Driver, Op, State};
use swifty_plugin_testkit::devices::{
    hue_bridge_handler, hue_handler, HueLight, MockHttp, Seen, SharedBridge, HUE_KEY,
};

#[tokio::test]
async fn hue_reads_with_the_key_from_the_file() {
    let mock = MockHttp::start(hue_handler).await;
    let adapter = driver(&mock.url, HUE_KEY, Vec::new());
    assert_eq!(adapter.read_state("hue").await.unwrap(), State::Bridge { reachable: true });
    assert_eq!(mock.requests()[0].path, format!("/api/{HUE_KEY}/groups/0"));
}

#[tokio::test]
async fn hue_rejects_a_refused_key_without_showing_it() {
    let mock = MockHttp::start(hue_handler).await;
    let adapter = driver(&mock.url, "not-the-right-key", Vec::new());
    let error = adapter.read_state("hue").await.unwrap_err();
    assert!(matches!(error, DeviceError::Rejected(_)));
    assert!(!error.to_string().contains("not-the-right-key"));
}

#[tokio::test]
async fn the_hue_bridge_itself_takes_no_commands_and_needs_its_key_file() {
    let mock = MockHttp::start(hue_handler).await;
    let adapter = driver(&mock.url, HUE_KEY, Vec::new());
    assert!(refusal(&mock.url, &[], "hue", &Op::Play).is_some());
    assert!(matches!(adapter.command("hue", &Op::Play).await, Err(DeviceError::Disabled(_))));
    assert_eq!(mock.traffic(), 0);

    assert!(try_driver(&mock.url, None, Vec::new()).is_err(), "no key, no driver");
    assert!(try_driver(&mock.url, Some("a/../b"), Vec::new()).is_err(), "a key that could change the path");
}

async fn hue_lights() -> (MockHttp, SharedBridge, Arc<dyn Driver>) {
    let bridge = SharedBridge::default();
    {
        let mut shared = bridge.lock().unwrap();
        shared.lights.insert("1".to_string(), HueLight::colour("Lightstrip"));
        shared.lights.insert("2".to_string(), HueLight::white("Hall"));
    }
    let mock = MockHttp::start(hue_bridge_handler(bridge.clone())).await;
    let adapter = driver(&mock.url, HUE_KEY, vec![light("hue-1"), light("hue-2")]);
    (mock, bridge, adapter)
}

fn last_put(mock: &MockHttp) -> Seen {
    mock.requests().into_iter().rfind(|r| r.method == "PUT").expect("nothing was sent to the bridge")
}

fn sent_body(mock: &MockHttp) -> serde_json::Value {
    serde_json::from_str(&last_put(mock).body).unwrap()
}

#[tokio::test]
async fn hue_power_is_a_put_of_on_and_reads_back() {
    let (mock, bridge, adapter) = hue_lights().await;
    adapter.command("hue-1", &Op::LightPower { on: true }).await.unwrap();
    assert_eq!(last_put(&mock).path, format!("/api/{HUE_KEY}/lights/1/state"));
    assert_eq!(sent_body(&mock), json!({ "on": true }));
    assert!(bridge.lock().unwrap().lights["1"].on);
    let state = adapter.read_state("hue-1").await.unwrap();
    assert!(Op::LightPower { on: true }.is_confirmed_by(&state, None), "{state:?}");
    assert!(!Op::LightPower { on: false }.is_confirmed_by(&state, None), "{state:?}");
}

#[tokio::test]
async fn hue_brightness_maps_percent_to_the_bridges_scale_and_turns_the_light_on() {
    let (mock, bridge, adapter) = hue_lights().await;
    for (percent, bri) in [(1, 1), (50, 126), (100, 254)] {
        let op = Op::LightBrightness { percent };
        adapter.command("hue-1", &op).await.unwrap();
        assert_eq!(sent_body(&mock), json!({ "on": true, "bri": bri }), "{percent}");
        let state = adapter.read_state("hue-1").await.unwrap();
        assert!(matches!(state, State::Light { on: true, bri: read, .. } if read == percent), "{state:?}");
        assert!(op.is_confirmed_by(&state, None), "{percent}: {state:?}");
    }

    let off = Op::LightBrightness { percent: 0 };
    adapter.command("hue-1", &off).await.unwrap();
    assert_eq!(sent_body(&mock), json!({ "on": false }));
    assert!(!bridge.lock().unwrap().lights["1"].on);
    assert!(off.is_confirmed_by(&adapter.read_state("hue-1").await.unwrap(), None));
}

#[tokio::test]
async fn hue_colour_is_a_full_saturation_hue_and_reads_back_as_the_same_angle() {
    let (mock, bridge, adapter) = hue_lights().await;
    let op = Op::LightHue { hue: 120 };
    assert!(!op.is_confirmed_by(&adapter.read_state("hue-1").await.unwrap(), None), "white mode is not a colour");
    adapter.command("hue-1", &op).await.unwrap();
    assert_eq!(sent_body(&mock), json!({ "on": true, "hue": 21845, "sat": 254 }));
    assert_eq!(bridge.lock().unwrap().lights["1"].colormode, "hs");
    let state = adapter.read_state("hue-1").await.unwrap();
    assert!(matches!(state, State::Light { on: true, hue: 120, color: true, .. }), "{state:?}");
    assert!(op.is_confirmed_by(&state, None));

    let set = Op::LightSet { percent: 100, hue: 225 };
    adapter.command("hue-1", &set).await.unwrap();
    assert_eq!(sent_body(&mock), json!({ "on": true, "bri": 254, "hue": 40959, "sat": 254 }));
    assert!(set.is_confirmed_by(&adapter.read_state("hue-1").await.unwrap(), None));
}

#[tokio::test]
async fn hue_a_light_the_bridge_cannot_reach_is_unavailable_and_never_a_reading() {
    let (_mock, bridge, adapter) = hue_lights().await;
    bridge.lock().unwrap().lights.get_mut("1").unwrap().reachable = false;
    let error = adapter.read_state("hue-1").await.unwrap_err();
    assert!(error.is_silence(), "{error:?}");
}

#[tokio::test]
async fn hue_an_error_in_the_reply_is_a_failure_and_never_shows_the_key() {
    let (_mock, bridge, adapter) = hue_lights().await;
    bridge.lock().unwrap().error = Some(7);
    let error = adapter.command("hue-1", &Op::LightPower { on: true }).await.unwrap_err();
    assert!(matches!(error, DeviceError::Rejected(_)), "{error:?}");
    assert!(!error.to_string().contains(HUE_KEY), "{error}");
    assert!(!bridge.lock().unwrap().lights["1"].on);

    bridge.lock().unwrap().lights.remove("1");
    let gone = adapter.read_state("hue-1").await.unwrap_err();
    assert!(matches!(gone, DeviceError::Rejected(_)), "{gone:?}");
    assert!(!gone.to_string().contains(HUE_KEY), "{gone}");
}

#[tokio::test]
async fn hue_a_colour_on_a_light_without_colour_is_refused_and_not_sent() {
    let (mock, _bridge, adapter) = hue_lights().await;
    for op in [Op::LightHue { hue: 10 }, Op::LightSet { percent: 50, hue: 10 }] {
        match adapter.command("hue-2", &op).await {
            Err(DeviceError::Disabled(text)) => assert!(text.contains("colour"), "{text}"),
            other => panic!("{other:?}"),
        }
    }
    assert!(mock.requests().iter().all(|r| r.method == "GET"));
    adapter.command("hue-2", &Op::LightBrightness { percent: 40 }).await.unwrap();
    let state = adapter.read_state("hue-2").await.unwrap();
    assert!(matches!(state, State::Light { on: true, color: false, .. }), "{state:?}");
}

#[tokio::test]
async fn hue_only_lights_that_were_added_can_be_reached() {
    let (mock, _bridge, adapter) = hue_lights().await;
    let adopted = [light("hue-1"), light("hue-2")];
    let op = Op::LightPower { on: true };
    for target in ["hue-3", "hue-01", "hue-1/../2", "bulb1"] {
        assert!(refusal(&mock.url, &adopted, target, &op).is_some(), "{target}");
        assert!(matches!(adapter.command(target, &op).await, Err(DeviceError::Disabled(_))), "{target}");
        assert!(adapter.read_state(target).await.is_err(), "{target}");
    }
    assert!(refusal(&mock.url, &adopted, "hue-1", &Op::Play).is_some());
    assert_eq!(mock.traffic(), 0);
}

#[tokio::test]
async fn hue_lists_the_bridges_lights_in_id_order() {
    let (_mock, bridge, adapter) = hue_lights().await;
    {
        let mut shared = bridge.lock().unwrap();
        shared.lights.insert("10".to_string(), HueLight::white("Late\u{7}"));
        shared.lights.insert("not-a-number".to_string(), HueLight::white("Bad"));
    }
    let found = adapter.discover(Duration::from_secs(1)).await.unwrap();
    let seen: Vec<(&str, &str)> = found.iter().map(|l| (l.local_id.as_str(), l.name.as_str())).collect();
    assert_eq!(seen, [("hue-1", "Lightstrip"), ("hue-2", "Hall"), ("hue-10", "Late")]);
    assert_eq!(found[0].attrs, json!({ "light": "1", "colour": true }));
    assert_eq!(found[1].attrs, json!({ "light": "2", "colour": false }));
    assert!(found.iter().all(|light| light.target_hint.as_deref() == Some(light.local_id.as_str())));
}

#[tokio::test]
async fn hue_silence_is_an_error_and_never_a_reading() {
    // A bridge that was there and is gone: nothing listens on its port any more.
    let gone = MockHttp::start(hue_handler).await;
    let url = gone.url.clone();
    drop(gone);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let adapter = driver(&url, HUE_KEY, Vec::new());
    assert!(adapter.read_state("hue").await.unwrap_err().is_silence());
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

/// The Hue part of the hub's test of the same name.
#[tokio::test]
async fn read_only_adapters_report_a_hung_device_as_silence_and_never_as_a_reading() {
    let hue = MockHttp::start(hue_handler).await;
    hue.hang.store(true, Ordering::SeqCst);
    let adapter = driver(&hue.url, HUE_KEY, Vec::new());
    assert_gives_up("hue", adapter.read_state("hue")).await;
}

#[tokio::test]
async fn a_hung_light_is_asked_once_and_times_out() {
    let (mock, _bridge, adapter) = hue_lights().await;
    mock.hang.store(true, Ordering::SeqCst);
    assert_gives_up("hue", adapter.command("hue-1", &Op::LightPower { on: true })).await;
    assert_eq!(mock.requests().len(), 1);
}

#[tokio::test]
async fn the_bridges_health_is_its_reachability() {
    let (mock, _bridge, adapter) = hue_lights().await;
    assert_eq!(adapter.health().await.status, swifty_plugin_api::HealthStatus::Ok);
    mock.hang.store(true, Ordering::SeqCst);
    let health = adapter.health().await;
    assert_eq!(health.status, swifty_plugin_api::HealthStatus::Down);
    assert!(!health.detail.unwrap_or_default().contains(HUE_KEY));
}
