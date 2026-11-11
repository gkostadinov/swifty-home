//! The driver, called directly, against a mock camera on loopback. The tests
//! for the camera in the hub's former `tests/adapters.rs`, under the same names.

mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::*;
use swifty_plugin_api::{DeviceError, Op, Plugin, Readiness, State};
use swifty_plugin_camera::PLUGIN;
use swifty_plugin_testkit::devices::{MockRtsp, CAMERA_LOGIN};

#[tokio::test]
async fn the_camera_probe_speaks_rtsp_and_does_not_send_credentials() {
    let mock = MockRtsp::start().await;
    let driver = PLUGIN.connect(live(camera_config(mock.port), CAMERA_LOGIN)).unwrap();
    assert_eq!(driver.read_state("camera").await.unwrap(), State::Camera { streaming: true });
    let received = mock.received.lock().unwrap();
    assert!(received[0].starts_with(&format!("OPTIONS rtsp://127.0.0.1:{}/ RTSP/1.0\r\n", mock.port)));
    assert!(!received[0].contains("camera-login"));
    assert!(!received[0].to_lowercase().contains("authorization"));
}

#[tokio::test]
async fn the_camera_counts_a_refusal_as_up_but_not_a_stranger() {
    let mock = MockRtsp::start().await;
    let driver = PLUGIN.connect(live(camera_config(mock.port), CAMERA_LOGIN)).unwrap();
    *mock.reply.lock().unwrap() = "RTSP/1.0 401 Unauthorized\r\nCSeq: 1\r\n\r\n".to_string();
    assert_eq!(driver.read_state("camera").await.unwrap(), State::Camera { streaming: true });
    *mock.reply.lock().unwrap() = "HTTP/1.1 200 OK\r\n\r\n".to_string();
    assert!(matches!(driver.read_state("camera").await, Err(DeviceError::Rejected(_))));
}

#[tokio::test]
async fn the_camera_has_no_action_and_stays_off_until_configured() {
    let mock = MockRtsp::start().await;
    let config = camera_config(mock.port);
    let driver = PLUGIN.connect(live(config.clone(), CAMERA_LOGIN)).unwrap();
    let present = with_login();
    assert!(PLUGIN.refusal(&view(&config, &present), "camera", &Op::Play).is_some());
    assert!(matches!(driver.command("camera", &Op::Play).await, Err(DeviceError::Disabled(_))));
    assert_eq!(mock.traffic(), 0);

    let mut missing = live(config.clone(), CAMERA_LOGIN);
    missing.secrets = Default::default();
    assert!(PLUGIN.connect(missing).is_err());
    let none = Default::default();
    assert!(matches!(PLUGIN.readiness(&view(&config, &none)), Readiness::NeedsSetup(_)));
    assert_eq!(mock.traffic(), 0);
}

// ----- every driver has a read timeout, and none retries

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

#[tokio::test]
async fn read_only_adapters_report_a_hung_device_as_silence_and_never_as_a_reading() {
    let camera = MockRtsp::start().await;
    camera.hang.store(true, Ordering::SeqCst);
    let driver = PLUGIN.connect(live(camera_config(camera.port), CAMERA_LOGIN)).unwrap();
    assert_gives_up("camera", driver.read_state("camera")).await;
    assert_eq!(camera.traffic(), 1, "asked once");
}

#[tokio::test]
async fn nothing_listening_is_unreachable() {
    let mut context = live(camera_config(NOTHING_LISTENS), CAMERA_LOGIN);
    context.timeouts =
        swifty_plugin_api::net::Timeouts { connect: Duration::from_secs(3), read: Duration::from_millis(300) };
    let camera = PLUGIN.connect(context).unwrap();
    let error = camera.read_state("camera").await.unwrap_err();
    assert!(error.is_silence(), "{error:?}");
    assert!(!error.to_string().contains("127.0.0.1"), "{error}");
}

#[tokio::test]
async fn a_device_the_camera_does_not_have_is_refused_without_a_probe() {
    let mock = MockRtsp::start().await;
    let driver = PLUGIN.connect(live(camera_config(mock.port), CAMERA_LOGIN)).unwrap();
    assert!(matches!(driver.read_state("camera-2").await, Err(DeviceError::Disabled(_))));
    assert_eq!(mock.traffic(), 0);
}

#[tokio::test]
async fn nothing_it_says_carries_the_login() {
    let mock = MockRtsp::start().await;
    *mock.reply.lock().unwrap() = format!("HTTP/1.1 400 {CAMERA_LOGIN}\r\n\r\n");
    let driver = PLUGIN.connect(live(camera_config(mock.port), CAMERA_LOGIN)).unwrap();
    let error = driver.read_state("camera").await.unwrap_err();
    assert!(!format!("{error} {error:?}").contains(CAMERA_LOGIN), "{error:?}");
    let mut missing = live(camera_config(mock.port), CAMERA_LOGIN);
    missing.secrets = Default::default();
    let refused = PLUGIN.connect(missing).err().unwrap();
    assert!(refused.0.contains("login"), "{refused}");
}
