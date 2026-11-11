//! The driver against the test kit's mock of Toshiba's cloud on loopback. These are the
//! hub's Toshiba tests from the hub's former `crates/swifty/tests/adapters.rs`, under the same names.

mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use common::*;
use serde_json::json;
use swifty_plugin_api::{DeviceError, Driver, Op, State};
use swifty_plugin_testkit::devices::*;
use swifty_plugin_testkit::trap::Trap;

async fn toshiba() -> (MockToshiba, Arc<dyn Driver>, String) {
    let cloud = MockToshiba::start().await;
    let adapter = connect(config(&cloud), Some(&login())).unwrap();
    let target = cloud.target(0);
    (cloud, adapter, target)
}

fn ac_power(on: bool) -> Op {
    Op::AirconPower { on }
}

/// A loopback port that nothing listens on any more: a trap's, once it has stopped.
/// The system hands out ports in turn, so another test is not given it again at once.
async fn free_port() -> u16 {
    let trap = Trap::start().await;
    let port = trap.addr.port();
    drop(trap);
    tokio::task::yield_now().await;
    port
}

/// Nothing about the account may appear in what the adapter says.
fn assert_nothing_private_in(error: &DeviceError) {
    let text = format!("{error} {error:?}");
    for secret in [TOSHIBA_PASSWORD, TOSHIBA_BEARER, TOSHIBA_SIGNATURE, TOSHIBA_EMAIL] {
        assert!(!text.contains(secret), "{text}");
    }
}

#[tokio::test]
async fn toshiba_reads_the_unit_and_signs_in_only_once() {
    let (cloud, adapter, target) = toshiba().await;
    let state = adapter.read_state(&target).await.unwrap();
    assert_eq!(
        state,
        State::Aircon {
            on: false,
            mode: Some("cool".to_string()),
            target: Some(22),
            fan: Some("auto".to_string()),
            indoor: Some(24),
            outdoor: Some(10),
        }
    );
    adapter.read_state(&cloud.target(1)).await.unwrap();
    let seen = cloud.cloud.lock().unwrap();
    assert_eq!(seen.logins, 1);
    assert_eq!(seen.state_reads, 2, "one for each unit");
    assert_eq!(seen.registrations, 0, "reading needs no hub token");
}

#[tokio::test]
async fn toshiba_says_which_client_it_is_on_every_request_to_the_cloud() {
    let (cloud, adapter, target) = toshiba().await;
    adapter.command(&target, &ac_power(true)).await.unwrap();
    let api: Vec<_> = cloud.http.requests().into_iter().filter(|seen| seen.path.starts_with("/api/")).collect();
    let paths: Vec<&str> = api.iter().map(|seen| seen.path.as_str()).collect();
    for path in ["/api/Consumer/Login", "/api/AC/GetCurrentACState", "/api/Consumer/RegisterMobileDevice"] {
        assert!(paths.contains(&path), "{paths:?}");
    }
    for seen in &api {
        assert_eq!(seen.header("device-id"), TOSHIBA_DEVICE, "{}", seen.path);
    }
}

#[tokio::test]
async fn toshiba_sends_the_documented_message() {
    let (cloud, adapter, target) = toshiba().await;
    adapter.command(&target, &Op::AirconTemp { celsius: 24 }).await.unwrap();
    let body = cloud.cloud.lock().unwrap().commands[0].clone();
    assert_eq!(
        body,
        json!({
            "sourceId": TOSHIBA_DEVICE,
            "messageId": "0000000",
            "targetId": ["UNIT-ONE"],
            "cmd": "CMD_FCU_TO_AC",
            "payload": { "data": "ffff18ffffffffffffffffffffffffffffffff" },
            "timeStamp": "0000000",
        })
    );
    let posts = cloud.http.posts();
    let hub = posts.iter().find(|p| p.path.ends_with("/messages/events")).unwrap();
    assert!(hub.path.contains("owner%40example.test"), "{}", hub.path);
    assert_eq!(hub.header("content-type"), "application/json");
    assert!(hub.body.starts_with(r#"{"sourceId":"#), "in the library's own order: {}", hub.body);
}

#[tokio::test]
async fn toshiba_each_command_changes_only_its_own_field() {
    let (cloud, adapter, target) = toshiba().await;
    adapter.command(&target, &ac_power(true)).await.unwrap();
    adapter.command(&target, &Op::AirconMode { mode: "heat".to_string() }).await.unwrap();
    adapter.command(&target, &Op::AirconTemp { celsius: 25 }).await.unwrap();
    adapter.command(&target, &Op::AirconFan { fan: "quiet".to_string() }).await.unwrap();
    assert_eq!(cloud.state(0), "3043193131640010180affffffff10ffffffff");
    assert_eq!(cloud.state(1), AC_OFF, "the other unit was not asked");
    let seen = cloud.cloud.lock().unwrap();
    let data: Vec<&str> = seen.commands.iter().map(|command| command["payload"]["data"].as_str().unwrap()).collect();
    assert_eq!(data.len(), 4);
    assert!(data.iter().all(|one| one.len() == 38));
    assert_eq!(seen.registrations, 1, "one token serves every command");
}

#[tokio::test]
async fn toshiba_a_command_is_followed_by_a_fresh_reading_at_once() {
    let (_cloud, adapter, target) = toshiba().await;
    adapter.read_state(&target).await.unwrap();
    adapter.command(&target, &ac_power(true)).await.unwrap();
    assert!(matches!(adapter.read_state(&target).await.unwrap(), State::Aircon { on: true, .. }));
}

#[tokio::test]
async fn toshiba_readings_are_shared_so_the_cloud_is_not_pestered() {
    let (cloud, adapter, target) = toshiba().await;
    let reads = futures_util::future::join_all((0..8).map(|_| adapter.read_state(&target))).await;
    assert!(reads.iter().all(Result::is_ok));
    assert!(adapter.read_state(&target).await.is_ok());
    assert_eq!(cloud.cloud.lock().unwrap().state_reads, 1, "they all share one reading");
}

#[tokio::test]
async fn toshiba_a_stale_sign_in_is_replaced_and_the_request_sent_again() {
    let (cloud, adapter, target) = toshiba().await;
    adapter.read_state(&target).await.unwrap();
    assert_eq!(cloud.cloud.lock().unwrap().logins, 1);
    adapter.command(&target, &ac_power(false)).await.unwrap();
    cloud.cloud.lock().unwrap().bearer_ok = false;
    assert!(adapter.read_state(&target).await.is_ok(), "the cloud did nothing with the first try");
    assert_eq!(cloud.cloud.lock().unwrap().logins, 2);
}

#[tokio::test]
async fn toshiba_a_stale_hub_token_is_replaced_and_the_command_sent_again() {
    let (cloud, adapter, target) = toshiba().await;
    adapter.command(&target, &Op::AirconTemp { celsius: 20 }).await.unwrap();
    cloud.cloud.lock().unwrap().hub_token_ok = false;
    adapter.command(&target, &Op::AirconTemp { celsius: 21 }).await.unwrap();
    let seen = cloud.cloud.lock().unwrap();
    assert_eq!(seen.registrations, 2);
    assert_eq!(seen.commands.len(), 2, "the hub refused the first try, so it was never delivered");
}

#[tokio::test]
async fn toshiba_a_wrong_password_is_tried_once_says_so_and_stays_private() {
    let (cloud, adapter, target) = toshiba().await;
    cloud.cloud.lock().unwrap().mode = CloudMode::BadPassword;
    for _ in 0..5 {
        let error = adapter.read_state(&target).await.unwrap_err();
        assert!(matches!(&error, DeviceError::Rejected(text) if text.contains("refused the sign-in")), "{error:?}");
        assert_nothing_private_in(&error);
    }
    let error = adapter.command(&target, &ac_power(true)).await.unwrap_err();
    assert_nothing_private_in(&error);
    assert_eq!(cloud.cloud.lock().unwrap().logins, 1, "an account can be locked for trying again");
}

#[tokio::test]
async fn toshiba_being_told_to_slow_down_stops_the_asking_for_a_while() {
    let (cloud, adapter, target) = toshiba().await;
    adapter.read_state(&target).await.unwrap();
    cloud.cloud.lock().unwrap().mode = CloudMode::SlowDown;
    adapter.command(&target, &Op::AirconMode { mode: "cool".to_string() }).await.unwrap_err();
    let requests = cloud.http.requests().len();
    for _ in 0..4 {
        let error = adapter.read_state(&cloud.target(1)).await.unwrap_err();
        assert!(error.is_silence(), "it may simply not have heard: {error:?}");
        assert_nothing_private_in(&error);
    }
    assert_eq!(cloud.http.requests().len(), requests, "nothing more was sent");
}

#[tokio::test]
async fn toshiba_after_a_command_a_burst_of_polls_costs_one_reading() {
    let (cloud, adapter, target) = toshiba().await;
    adapter.read_state(&target).await.unwrap();
    adapter.command(&target, &ac_power(true)).await.unwrap();
    let before = cloud.cloud.lock().unwrap().state_reads;
    for _ in 0..10 {
        assert!(matches!(adapter.read_state(&target).await.unwrap(), State::Aircon { on: true, .. }));
    }
    assert_eq!(cloud.cloud.lock().unwrap().state_reads, before + 1, "looked once, and waits before the next look");
}

#[tokio::test]
async fn toshiba_the_last_reading_stands_while_the_cloud_rests() {
    let (cloud, adapter, target) = toshiba().await;
    let seen = adapter.read_state(&target).await.unwrap();
    adapter.command(&target, &Op::AirconFan { fan: "quiet".to_string() }).await.unwrap();
    cloud.cloud.lock().unwrap().mode = CloudMode::SlowDown;
    assert_eq!(adapter.read_state(&target).await.unwrap(), seen, "the unit stays on the page as it was last seen");
    let requests = cloud.http.requests().len();
    for _ in 0..4 {
        assert_eq!(adapter.read_state(&target).await.unwrap(), seen);
    }
    assert_eq!(cloud.http.requests().len(), requests, "a resting cloud is not asked again");
}

#[tokio::test]
async fn toshiba_a_token_for_another_hub_is_never_used() {
    let (cloud, adapter, target) = toshiba().await;
    cloud.cloud.lock().unwrap().hub_host = Some("elsewhere.example.test".to_string());
    let error = adapter.command(&target, &ac_power(true)).await.unwrap_err();
    assert!(matches!(&error, DeviceError::Rejected(text) if text.contains("hub")), "{error:?}");
    assert!(cloud.cloud.lock().unwrap().commands.is_empty());
}

#[tokio::test]
async fn toshiba_turning_on_a_unit_that_is_cleaning_calls_the_cleaning_off() {
    let (cloud, adapter, target) = toshiba().await;
    let mut cleaning = AC_OFF.to_string();
    cleaning.replace_range(28..30, "18");
    cloud.set_state(0, &cleaning);
    adapter.command(&target, &ac_power(true)).await.unwrap();
    let seen = cloud.cloud.lock().unwrap();
    assert_eq!(seen.commands[0]["payload"]["data"], "30ffffffffffffffffffffffffff10ffffffff");
}

#[tokio::test]
async fn toshiba_a_temperature_is_refused_while_the_unit_holds_8_degrees() {
    let (cloud, adapter, target) = toshiba().await;
    let mut holding = "3043".to_string();
    holding.push_str("18413164");
    holding.push_str("04");
    holding.push_str(&AC_OFF[14..]);
    cloud.set_state(0, &holding);
    assert!(matches!(adapter.read_state(&target).await.unwrap(), State::Aircon { target: Some(8), .. }));
    let error = adapter.command(&target, &Op::AirconTemp { celsius: 22 }).await.unwrap_err();
    assert!(matches!(&error, DeviceError::Rejected(text) if text.contains("8 degrees")), "{error:?}");
    assert!(cloud.cloud.lock().unwrap().commands.is_empty(), "nothing was sent");
}

#[tokio::test]
async fn toshiba_a_cloud_that_cannot_be_reached_is_silence_and_never_a_reading() {
    let cloud = MockToshiba::start().await;
    let mut settings = config(&cloud);
    let nowhere = format!("http://127.0.0.1:{}", free_port().await);
    settings["api_url"] = json!(nowhere);
    settings["hub_url"] = json!(nowhere);
    let adapter = connect(settings, Some(&login())).unwrap();
    let target = cloud.target(0);
    let error = adapter.read_state(&target).await.unwrap_err();
    assert!(error.is_silence(), "{error:?}");
    let error = adapter.command(&target, &ac_power(false)).await.unwrap_err();
    assert!(error.is_silence(), "{error:?}");
}

#[tokio::test]
async fn toshiba_a_cloud_that_says_nothing_is_silence() {
    let (cloud, adapter, target) = toshiba().await;
    cloud.http.hang.store(true, Ordering::SeqCst);
    let error = adapter.read_state(&target).await.unwrap_err();
    assert_eq!(error, DeviceError::Timeout);
}

/// A poll every few seconds must not queue up behind a cloud that has stopped answering.
#[tokio::test]
async fn toshiba_a_failed_reading_stands_for_a_moment() {
    let (cloud, adapter, target) = toshiba().await;
    cloud.http.hang.store(true, Ordering::SeqCst);
    let first = adapter.read_state(&target).await.unwrap_err();
    let asked = cloud.http.requests().len();
    assert!(asked > 0);
    for _ in 0..3 {
        assert_eq!(adapter.read_state(&target).await.unwrap_err(), first);
    }
    assert_eq!(cloud.http.requests().len(), asked, "the same failure, without asking again");
}

#[tokio::test]
async fn toshiba_a_reply_that_cannot_be_read_is_a_refusal_and_private() {
    let (cloud, adapter, target) = toshiba().await;
    cloud.set_state(0, "not hex at all");
    let error = adapter.read_state(&target).await.unwrap_err();
    assert!(matches!(&error, DeviceError::Rejected(_)), "{error:?}");
    assert_nothing_private_in(&error);
}

#[tokio::test]
async fn toshiba_a_bad_credentials_file_says_only_that() {
    let cloud = MockToshiba::start().await;
    for contents in ["no colon here", "!!:!!"] {
        let Err(reason) = connect(config(&cloud), Some(contents)) else { panic!("{contents} was accepted") };
        assert!(reason.0.contains("sign-in"), "{reason}");
        assert!(!reason.0.contains(contents), "{reason}");
    }
    assert!(connect(config(&cloud), None).is_err());
    assert_eq!(cloud.traffic(), 0, "building the driver reaches nothing");
}

#[tokio::test]
async fn toshiba_only_configured_units_and_only_its_own_operations() {
    let (cloud, adapter, target) = toshiba().await;
    let settings = config(&cloud);
    assert!(refusal(&settings, &target, &ac_power(true)).is_none());
    assert!(refusal(&settings, &target, &Op::AirconFan { fan: "low".to_string() }).is_none());
    assert!(refusal(&settings, &target, &Op::PlugPower { on: true }).is_some());
    assert!(refusal(&settings, "ac-99999999-1111-2222-3333-444455556666", &ac_power(true)).is_some());
    assert!(matches!(
        adapter.command("ac-99999999-1111-2222-3333-444455556666", &ac_power(true)).await,
        Err(DeviceError::Disabled(_))
    ));
    assert!(matches!(adapter.command(&target, &Op::PlugPower { on: true }).await, Err(DeviceError::Disabled(_))));
    assert!(matches!(
        adapter.command(&target, &Op::AirconMode { mode: "boil".to_string() }).await,
        Err(DeviceError::Disabled(_))
    ));
    assert_eq!(cloud.traffic(), 0);
}
