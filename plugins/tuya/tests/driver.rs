//! The driver against a mock socket on loopback. These are the Tuya tests of the
//! hub's `tests/adapters.rs`, under the same names, with the adapter built by
//! `Plugin::connect` and the keys read from secret slots instead of key files.

mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::*;
use serde_json::json;
use swifty_plugin_api::{DeviceError, Op, State};
use swifty_plugin_testkit::devices::*;

const SOCKET: &str = "bf0123456789abcdef";

/// Nothing about a key may appear in what the driver says.
fn assert_no_key_in(error: &DeviceError) {
    let text = format!("{error} {error:?}");
    assert!(!text.contains(TUYA_KEY_BASE64), "{text}");
    assert!(!text.contains(std::str::from_utf8(TUYA_KEY).unwrap()), "{text}");
}

#[tokio::test]
async fn tuya_a_usb_port_is_its_own_switch_and_shares_one_reading() {
    let plug = MockTuya::start(SOCKET).await;
    plug.state.lock().unwrap().usb = Some(false);
    let mut config = row(&plug);
    config["usb_dp"] = json!(7);
    let (main, usb) = (plug.target(), format!("{}-usb", plug.target()));
    let adapter = driver(vec![config], vec![key(&plug, TUYA_KEY_BASE64)]);

    adapter.command(&usb, &power(true)).await.unwrap();
    assert_eq!(plug.state.lock().unwrap().usb, Some(true));
    assert!(!plug.state.lock().unwrap().on, "the USB port moved the socket");
    assert_eq!(adapter.read_state(&usb).await.unwrap(), State::Plug { on: true });

    let before = plug.connections.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(adapter.read_state(&main).await.unwrap(), State::Plug { on: false });
    assert_eq!(plug.connections.load(std::sync::atomic::Ordering::SeqCst), before, "one reading serves both switches");
}

#[tokio::test]
async fn tuya_turns_on_and_off_and_each_is_read_back() {
    let (plug, adapter, target) = tuya(SOCKET).await;
    adapter.command(&target, &power(true)).await.unwrap();
    assert!(plug.state.lock().unwrap().on);
    let state = adapter.read_state(&target).await.unwrap();
    assert_eq!(state, State::Plug { on: true });
    assert!(power(true).is_confirmed_by(&state, None));
    assert!(!power(false).is_confirmed_by(&state, None));

    adapter.command(&target, &power(false)).await.unwrap();
    assert!(!plug.state.lock().unwrap().on);
    let state = adapter.read_state(&target).await.unwrap();
    assert_eq!(state, State::Plug { on: false });
    assert!(power(false).is_confirmed_by(&state, None));
}

#[tokio::test]
async fn tuya_sends_the_documented_requests() {
    let (plug, adapter, target) = tuya(SOCKET).await;
    let before = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    adapter.command(&target, &power(true)).await.unwrap();
    adapter.read_state(&target).await.unwrap();
    let requests = plug.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);

    let (command, control) = &requests[0];
    assert_eq!(*command, 7);
    assert_eq!(control["devId"], json!(SOCKET));
    assert_eq!(control["uid"], json!(SOCKET));
    assert_eq!(control["dps"], json!({ "1": true }));
    let sent: u64 = control["t"].as_str().expect("the time is a string").parse().unwrap();
    assert!(sent >= before && sent < before + 60, "{sent}");

    let (command, query) = &requests[1];
    assert_eq!(*command, 10);
    for field in ["gwId", "devId", "uid"] {
        assert_eq!(query[field], json!(SOCKET), "{field}");
    }
    assert!(query["t"].is_string());
    assert!(query.get("dps").is_none());
}

#[tokio::test]
async fn tuya_reads_a_reply_with_or_without_the_version_header() {
    let (plug, adapter, target) = tuya(SOCKET).await;
    plug.state.lock().unwrap().on = true;
    for header in [false, true] {
        plug.state.lock().unwrap().reply_header = header;
        assert_eq!(adapter.read_state(&target).await.unwrap(), State::Plug { on: true }, "header {header}");
    }
}

#[tokio::test]
async fn tuya_a_command_with_an_empty_reply_is_still_taken() {
    let (plug, adapter, target) = tuya(SOCKET).await;
    plug.state.lock().unwrap().empty_control_reply = true;
    adapter.command(&target, &power(true)).await.unwrap();
    assert_eq!(adapter.read_state(&target).await.unwrap(), State::Plug { on: true });
}

#[tokio::test]
async fn tuya_a_wrong_key_is_a_refusal_changes_nothing_and_is_never_shown() {
    let other = STANDARD_KEY_ELSEWHERE;
    let (plug, adapter, target) = tuya_with_key(SOCKET, other).await;
    let error = adapter.command(&target, &power(true)).await.unwrap_err();
    assert!(matches!(error, DeviceError::Rejected(_)), "{error:?}");
    assert_no_key_in(&error);
    assert!(!format!("{error}").contains(other), "{error}");
    let error = adapter.read_state(&target).await.unwrap_err();
    assert!(matches!(error, DeviceError::Rejected(_)), "{error:?}");
    assert!(!plug.state.lock().unwrap().on);
}

/// Sixteen bytes that are not the socket's key, in base64.
const STANDARD_KEY_ELSEWHERE: &str = "MDEyMzQ1Njc4OWFiY2RlZg==";

#[tokio::test]
async fn tuya_a_reply_with_a_bad_checksum_or_prefix_is_refused() {
    let (plug, adapter, target) = tuya(SOCKET).await;
    plug.state.lock().unwrap().on = true;
    for mode in [TuyaMode::BadCrc, TuyaMode::BadPrefix] {
        plug.state.lock().unwrap().mode = mode;
        let error = adapter.read_state(&target).await.unwrap_err();
        assert!(matches!(error, DeviceError::Rejected(_)), "{mode:?}: {error:?}");
    }
}

#[tokio::test]
async fn tuya_an_oversized_frame_is_refused_without_reading_it() {
    let (plug, adapter, target) = tuya(SOCKET).await;
    plug.state.lock().unwrap().mode = TuyaMode::Oversized;
    let started = Instant::now();
    let error = adapter.read_state(&target).await.unwrap_err();
    assert!(matches!(error, DeviceError::Rejected(_)), "{error:?}");
    assert!(started.elapsed() < Duration::from_millis(250), "it waited for bytes it should have refused");
}

#[tokio::test]
async fn tuya_a_refusal_carries_only_its_code() {
    let (plug, adapter, target) = tuya(SOCKET).await;
    plug.state.lock().unwrap().mode = TuyaMode::Refuses(11);
    match adapter.command(&target, &power(true)).await {
        Err(DeviceError::Rejected(text)) => assert!(text.contains("11"), "{text}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn tuya_an_unreachable_socket_is_silence_and_never_a_reading() {
    // A port that was just closed: the mock listened there and is gone.
    let closed = {
        let gone = MockTuya::start(SOCKET).await;
        gone.port
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    let plug = MockTuya::start(SOCKET).await;
    let mut config = row(&plug);
    config["port"] = json!(closed);
    let adapter = driver(vec![config], vec![key(&plug, TUYA_KEY_BASE64)]);
    let target = plug.target();
    assert!(adapter.read_state(&target).await.unwrap_err().is_silence());
    assert!(adapter.command(&target, &power(true)).await.unwrap_err().is_silence());
    assert_eq!(plug.traffic(), 0);
}

#[tokio::test]
async fn tuya_a_socket_that_says_nothing_is_silence() {
    let (plug, adapter, target) = tuya(SOCKET).await;
    plug.state.lock().unwrap().mode = TuyaMode::Silent;
    assert_eq!(adapter.read_state(&target).await.unwrap_err(), DeviceError::Timeout);
}

#[tokio::test]
async fn tuya_keeps_to_one_connection_at_a_time() {
    let (plug, adapter, target) = tuya(SOCKET).await;
    plug.state.lock().unwrap().hold_ms = 30;
    let on = power(true);
    let send = || adapter.command(&target, &on);
    let (a, b, c, d) = tokio::join!(send(), send(), send(), send());
    for result in [a, b, c, d] {
        result.unwrap();
    }
    assert_eq!(plug.traffic(), 4, "one connection for each command");
    assert_eq!(plug.most_at_once.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn tuya_reads_at_the_same_moment_share_one_connection() {
    let (plug, adapter, target) = tuya(SOCKET).await;
    plug.state.lock().unwrap().hold_ms = 30;
    let read = || adapter.read_state(&target);
    let (a, b, c, d) = tokio::join!(read(), read(), read(), read());
    for result in [a, b, c, d] {
        assert_eq!(result.unwrap(), State::Plug { on: false });
    }
    assert_eq!(plug.traffic(), 1, "a socket allows so few connections");
}

/// The key files became secret slots. A slot that holds no value is refused by
/// the pure `refusal` already; a slot that holds a malformed value can only be
/// found by the driver, which disables that one socket with the same fixed text.
#[tokio::test]
async fn tuya_a_bad_key_file_disables_that_socket_and_says_nothing_of_the_key() {
    let good = MockTuya::start("bf0000000000000001").await;
    let short = MockTuya::start("bf0000000000000002").await;
    let not_base64 = MockTuya::start("bf0000000000000003").await;
    let missing = MockTuya::start("bf0000000000000004").await;
    let rows = vec![row(&good), row(&short), row(&not_base64), row(&missing)];
    let keys = vec![key(&good, TUYA_KEY_BASE64), key(&short, "c2hvcnQ="), key(&not_base64, "not*base64*at*all")];
    let present: Vec<String> = keys.iter().map(|(slot, _)| slot.clone()).collect();
    let config = json!({ "plugs": rows.clone() });
    let adapter = driver(rows, keys);
    assert_eq!(adapter.read_state(&good.target()).await.unwrap(), State::Plug { on: false });
    assert!(refusal(&config, &present, &good.target(), &power(true)).is_none());
    assert!(refusal(&config, &present, &missing.target(), &power(true)).is_some(), "an empty slot");
    for bad in [&short, &not_base64, &missing] {
        let target = bad.target();
        let error = adapter.command(&target, &power(true)).await.unwrap_err();
        assert!(matches!(error, DeviceError::Disabled(_)), "{error:?}");
        assert!(error.to_string().contains(&target), "{error}");
        assert!(!error.to_string().contains("not*base64"), "{error}");
        assert!(!error.to_string().contains("c2hvcnQ"), "{error}");
        assert!(matches!(adapter.read_state(&target).await, Err(DeviceError::Disabled(_))));
        assert_eq!(bad.traffic(), 0, "{target}");
    }
}

#[tokio::test]
async fn tuya_only_configured_sockets_and_only_power() {
    let (plug, adapter, target) = tuya(SOCKET).await;
    let config = json!({ "plugs": [row(&plug)] });
    let present = vec![key(&plug, TUYA_KEY_BASE64).0];
    assert!(refusal(&config, &present, "plug-bf0000000000000099", &power(true)).is_some());
    assert!(matches!(adapter.command("plug-bf0000000000000099", &power(true)).await, Err(DeviceError::Disabled(_))));
    assert!(adapter.read_state("bulb1").await.is_err());
    assert!(refusal(&config, &present, &target, &Op::Play).is_some());
    assert!(matches!(adapter.command(&target, &Op::Play).await, Err(DeviceError::Disabled(_))));
    assert_eq!(plug.traffic(), 0);
}
