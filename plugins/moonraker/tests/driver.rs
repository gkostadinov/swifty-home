//! The driver against the mock Moonraker on loopback: the tests of the hub's
//! adapter, under the same names.

mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::*;
use serde_json::json;
use swifty_plugin_api::net::Timeouts;
use swifty_plugin_api::{DeviceError, Driver, Op, Secrets, State};
use swifty_plugin_testkit::devices::{MockHttp, MockMoonraker, MOONRAKER_KEY};

async fn moonraker() -> (MockMoonraker, std::sync::Arc<dyn Driver>) {
    let mock = MockMoonraker::start().await;
    let driver = connect(&mock.http.url);
    (mock, driver)
}

#[tokio::test]
async fn moonraker_pause_resume_and_emergency_stop_hit_their_endpoints() {
    let (mock, driver) = moonraker().await;
    for op in [Op::PrintPause, Op::PrintResume, Op::PrintEmergencyStop] {
        driver.command("printer", &op).await.unwrap();
    }
    let paths: Vec<String> = mock.http.posts().iter().map(|r| r.path.clone()).collect();
    assert_eq!(paths, ["/printer/print/pause", "/printer/print/resume", "/printer/emergency_stop"]);
    let state = driver.read_state("printer").await.unwrap();
    assert!(Op::PrintEmergencyStop.is_confirmed_by(&state, None));
}

#[tokio::test]
async fn moonraker_status_fills_the_console_fields() {
    let (mock, driver) = moonraker().await;
    {
        let mut printer = mock.state.lock().unwrap();
        printer.print = "printing".to_string();
        printer.nozzle = 215.2;
        printer.bed = 60.1;
        printer.progress = 0.374;
    }
    let State::Printer { state, nozzle, bed, layer, layers, pct } = driver.read_state("printer").await.unwrap() else {
        panic!("not a printer state");
    };
    assert_eq!((state.as_str(), nozzle, bed, layer, layers), ("printing", 215.2, 60.1, 12, 90));
    assert!((pct - 37.4).abs() < 0.001);
    let request = &mock.http.requests()[0];
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/printer/objects/query");
    assert!(request.query.contains("print_stats"));
}

#[tokio::test]
async fn moonraker_start_prints_the_loaded_file_with_one_post() {
    let (mock, driver) = moonraker().await;
    mock.state.lock().unwrap().filename = "my part (v2).gcode".to_string();
    driver.command("printer", &Op::PrintStart).await.unwrap();
    let posts = mock.http.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].path, "/printer/print/start");
    assert!(posts[0].query.starts_with("filename="));
    assert!(posts[0].query.contains("my+part") || posts[0].query.contains("my%20part"), "{}", posts[0].query);
}

#[tokio::test]
async fn moonraker_start_sends_nothing_without_a_loaded_job() {
    let (mock, driver) = moonraker().await;
    mock.state.lock().unwrap().filename = String::new();
    let result = driver.command("printer", &Op::PrintStart).await;
    let Err(DeviceError::Rejected(text)) = result else { panic!("{result:?}") };
    assert!(text.contains("no job is loaded"), "{text}");
    assert!(mock.http.posts().is_empty());
}

#[tokio::test]
async fn moonraker_start_sends_nothing_while_busy() {
    let (mock, driver) = moonraker().await;
    mock.state.lock().unwrap().print = "printing".to_string();
    assert!(matches!(driver.command("printer", &Op::PrintStart).await, Err(DeviceError::Rejected(_))));
    assert!(mock.http.posts().is_empty());
}

#[tokio::test]
async fn moonraker_start_sends_nothing_if_the_printer_cannot_be_read() {
    let (mock, driver) = moonraker().await;
    mock.state.lock().unwrap().klippy_gone = true;
    let result = driver.command("printer", &Op::PrintStart).await;
    let Err(DeviceError::Unreachable(text)) = result else { panic!("{result:?}") };
    assert!(text.contains("Nothing was sent"), "{text}");
    assert!(mock.http.posts().is_empty());
}

#[tokio::test]
async fn moonraker_losing_klipper_is_silence_not_a_state() {
    let (mock, driver) = moonraker().await;
    mock.state.lock().unwrap().klippy_gone = true;
    let result = driver.read_state("printer").await;
    assert!(result.as_ref().unwrap_err().is_silence(), "{result:?}");
}

#[tokio::test]
async fn moonraker_device_errors_carry_its_cleaned_message() {
    let device =
        MockHttp::start(|_| (400, json!({ "error": { "message": "Printer is not printing\u{7}" } }).to_string())).await;
    let driver = connect(&device.url);
    let result = driver.command("printer", &Op::PrintPause).await;
    let Err(DeviceError::Rejected(text)) = result else { panic!("{result:?}") };
    assert_eq!(text, "the printer said: Printer is not printing");
}

#[tokio::test]
async fn moonraker_sends_its_key_only_as_a_header() {
    let mock = MockMoonraker::start().await;
    let driver = connect_with(&mock.http.url, Secrets::of([("api_key", MOONRAKER_KEY)]), timeouts());
    driver.read_state("printer").await.unwrap();
    let request = &mock.http.requests()[0];
    assert_eq!(request.header("x-api-key"), MOONRAKER_KEY);
    assert!(!request.query.contains(MOONRAKER_KEY));
}

/// Generous, but far below the 120 seconds a hung mock would take.
const OUTER_LIMIT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn a_device_that_never_answers_times_out_and_is_asked_once() {
    let printer = MockMoonraker::start().await;
    printer.http.hang.store(true, Ordering::SeqCst);
    let driver = connect(&printer.http.url);
    let started = Instant::now();
    let result = tokio::time::timeout(OUTER_LIMIT, driver.command("printer", &Op::PrintPause))
        .await
        .unwrap_or_else(|_| panic!("moonraker never gave up"));
    assert!(started.elapsed() < OUTER_LIMIT, "moonraker");
    assert!(matches!(&result, Err(DeviceError::Timeout)), "moonraker: {result:?}");
    assert_eq!(printer.http.requests().len(), 1);
}

#[tokio::test]
async fn nothing_listening_is_unreachable() {
    // Port 9 (discard) has no listener on a test machine, and no mock can be given it.
    let printer = connect_with(
        "http://127.0.0.1:9",
        Secrets::default(),
        Timeouts { connect: Duration::from_secs(3), read: Duration::from_millis(300) },
    );
    let error = printer.command("printer", &Op::PrintPause).await.unwrap_err();
    assert!(error.is_silence(), "{error:?}");
    assert!(!error.to_string().contains("127.0.0.1"), "{error}");
}

#[tokio::test]
async fn an_oversized_reply_is_refused() {
    let mock = MockHttp::start(|_| (200, "x".repeat(2 * 1024 * 1024))).await;
    let driver = connect_with(
        &mock.url,
        Secrets::default(),
        Timeouts { connect: Duration::from_millis(500), read: Duration::from_millis(2000) },
    );
    let error = driver.read_state("printer").await.unwrap_err();
    assert!(matches!(&error, DeviceError::Rejected(text) if text.contains("too much")), "{error:?}");
}

#[tokio::test]
async fn another_device_gets_nothing() {
    let (mock, driver) = moonraker().await;
    assert!(matches!(driver.command("printer2", &Op::PrintPause).await, Err(DeviceError::Disabled(_))));
    assert!(matches!(driver.read_state("printer2").await, Err(DeviceError::Disabled(_))));
    assert!(matches!(driver.command("printer", &Op::VmStart).await, Err(DeviceError::Disabled(_))));
    assert_eq!(mock.http.traffic(), 0);
}
