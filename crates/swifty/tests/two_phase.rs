//! Two-phase results, no_response, retries and duplicate suppression.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::json;
use swifty_plugin_api::{Op, State};
use swifty_plugin_mock::Behavior;

#[tokio::test]
async fn success_is_reported_only_after_the_device_confirms() {
    let (harness, _auth, cookie) = Harness::signed_in_with(|c| c.timing.confirm_timeout_ms = Some(2_000)).await;
    harness.devices.set_behavior("bulb1", Behavior::Slow(40));
    let done = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    done.assert_ok();
    assert_eq!(done.phase(), "confirmed");
    // By the time success is returned, the state really has changed.
    assert_eq!(harness.devices.state("bulb1"), Some(State::Light { on: false, bri: 60, hue: 38, color: true }));
    assert!(harness.devices.total_reads() >= 1, "must read the device back");
    assert!(done.body["rtt_ms"].is_i64());
}

#[tokio::test]
async fn the_commanded_row_comes_before_the_result() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await.assert_ok();
    let results = harness.audit_results();
    let commanded = results.iter().position(|r| r == "commanded").unwrap();
    let confirmed = results.iter().position(|r| r == "confirmed").unwrap();
    assert!(commanded < confirmed);
}

#[tokio::test]
async fn a_silent_device_is_no_response() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    harness.devices.set_behavior("bulb1", Behavior::Silent);
    let done = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    done.assert_ok();
    assert_eq!(done.phase(), "no_response");
    assert!(harness.audit_has("no_response"));
    assert!(!harness.audit_has("confirmed"));
}

#[tokio::test]
async fn a_device_that_accepts_but_never_changes_is_no_response() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    harness.devices.set_behavior("bulb1", Behavior::AcceptsButIgnores);
    let done = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    assert_eq!(done.phase(), "no_response");
    assert!(harness.devices.total_reads() >= 1);
}

#[tokio::test]
async fn a_hung_adapter_is_ended_by_the_engine_deadline() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    harness.devices.set_behavior("bulb1", Behavior::Hang);
    let started = std::time::Instant::now();
    let done = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    assert_eq!(done.phase(), "no_response");
    assert!(started.elapsed().as_secs() < 3);
}

#[tokio::test]
async fn a_device_refusal_is_failed() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    harness.devices.set_behavior("printer", Behavior::Rejects("not homed".into()));
    let prepared = harness.prepare(&cookie, "print.pause", "printer", json!({})).await;
    let done = harness.confirm_tap(&cookie, &prepared).await;
    done.assert_ok();
    assert_eq!(done.phase(), "failed");
    assert!(done.body["detail"].as_str().unwrap().contains("not homed"));
    assert!(harness.audit_has("failed"));
    assert_eq!(harness.devices.command_count("printer"), 1, "a refusal is not retried");
}

#[tokio::test]
async fn non_idempotent_actions_are_sent_exactly_once() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    for (action, target, params) in [
        ("pets.feed", "feeder", json!({ "grams": 10 })),
        ("pets.clean", "litter", json!({})),
        ("print.start", "printer", json!({})),
    ] {
        // The device takes the command and never shows the change. Waiting for
        // it must not send the command again.
        harness.devices.set_behavior(target, Behavior::AcceptsButIgnores);
        let prepared = harness.prepare(&cookie, action, target, params).await;
        let done = harness.confirm_tap(&cookie, &prepared).await;
        assert_eq!(done.phase(), "no_response", "{action}: {}", done.text);
        assert_eq!(harness.devices.command_count(target), 1, "{action}");
    }
}

#[tokio::test]
async fn non_idempotent_silence_is_not_retried() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let mut sent = 0;
    for behavior in [Behavior::Hang, Behavior::Silent] {
        harness.devices.set_behavior("printer", behavior);
        let prepared = harness.prepare(&cookie, "print.start", "printer", json!({})).await;
        let done = harness.confirm_tap(&cookie, &prepared).await;
        assert_eq!(done.phase(), "no_response");
        sent += 1;
        assert_eq!(harness.devices.command_count("printer"), sent);
        harness.advance(10_001);
    }
}

#[tokio::test]
async fn idempotent_silence_gets_one_more_try() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    harness.devices.set_behavior("bulb1", Behavior::Silent);
    let done = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    assert_eq!(done.phase(), "no_response");
    assert_eq!(harness.devices.command_count("bulb1"), 2);
}

#[tokio::test]
async fn a_device_that_cannot_be_read_first_gets_no_command() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    harness.devices.set_behavior("feeder", Behavior::Silent);
    let prepared = harness.prepare(&cookie, "pets.feed", "feeder", json!({ "grams": 10 })).await;
    let done = harness.confirm_tap(&cookie, &prepared).await;
    assert_eq!(done.phase(), "failed");
    assert!(done.body["detail"].as_str().unwrap().contains("Nothing was sent"));
    assert_eq!(harness.devices.command_count("feeder"), 0);
}

#[tokio::test]
async fn a_feed_is_confirmed_by_the_portion_count_moving() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    // A portion the feeder dispenses: it takes 10 g steps.
    let prepared = harness.prepare(&cookie, "pets.feed", "feeder", json!({ "grams": 20 })).await;
    let done = harness.confirm_tap(&cookie, &prepared).await;
    assert_eq!(done.phase(), "confirmed");
    assert_eq!(harness.devices.commands(), vec![("feeder".to_string(), Op::PetFeed { grams: 20 })]);
}

#[tokio::test]
async fn a_slow_action_answers_202_and_can_be_followed() {
    let (harness, _auth, cookie) = Harness::signed_in_with(|c| {
        c.timing.sync_wait_ms = 5;
        c.timing.confirm_timeout_ms = Some(2_000);
    })
    .await;
    harness.devices.set_behavior("bulb1", Behavior::Slow(150));
    let first = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    assert_eq!(first.status, StatusCode::ACCEPTED);
    assert_eq!(first.phase(), "commanded");
    let run_id = first.body["run_id"].as_str().unwrap().to_string();
    let path = format!("/api/v1/actions/runs/{run_id}");

    let mut phase = String::new();
    for _ in 0..200 {
        phase = harness.get(&path, &cookie).await.phase().to_string();
        if phase != "commanded" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(phase, "confirmed");
    let runs = harness.get("/api/v1/actions/runs", &cookie).await;
    assert_eq!(runs.body["runs"][0]["run_id"], json!(run_id));
    harness.get("/api/v1/actions/runs/nope", &cookie).await.assert_refused(StatusCode::NOT_FOUND, "unknown_run");
}

#[tokio::test]
async fn a_repeated_request_id_runs_once() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let body = json!({
        "action": "light.power", "target": "bulb1", "params": { "on": false }, "request_id": "tap-1",
    });
    let first = harness.post("/api/v1/actions/execute", &cookie, body.clone()).await;
    let second = harness.post("/api/v1/actions/execute", &cookie, body).await;
    second.assert_ok();
    assert_eq!(first.body["run_id"], second.body["run_id"]);
    assert_eq!(first.body["duplicate"], json!(false));
    assert_eq!(second.body["duplicate"], json!(true));
    assert_eq!(harness.devices.command_count("bulb1"), 1);

    let reused = harness
        .post(
            "/api/v1/actions/execute",
            &cookie,
            json!({ "action": "light.power", "target": "bulb2", "params": { "on": false }, "request_id": "tap-1" }),
        )
        .await;
    reused.assert_refused(StatusCode::CONFLICT, "request_id_reused");

    let bad = harness
        .post(
            "/api/v1/actions/execute",
            &cookie,
            json!({ "action": "light.power", "target": "bulb1", "params": { "on": false }, "request_id": "has space" }),
        )
        .await;
    bad.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_request_id");
}

#[tokio::test]
async fn an_identical_action_in_flight_is_not_sent_again() {
    let (harness, _auth, cookie) = Harness::signed_in_with(|c| {
        c.timing.sync_wait_ms = 5;
        c.timing.confirm_timeout_ms = Some(400);
    })
    .await;
    harness.devices.set_behavior("bulb1", Behavior::AcceptsButIgnores);
    let first = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    assert_eq!(first.phase(), "commanded");
    let second = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    assert_eq!(second.body["duplicate"], json!(true));
    assert_eq!(second.body["run_id"], first.body["run_id"]);
    assert_eq!(harness.devices.command_count("bulb1"), 1);

    // A different value is a different request.
    let other = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": true })).await;
    assert_ne!(other.body["run_id"], first.body["run_id"]);
}

#[tokio::test]
async fn a_read_of_a_silent_device_is_no_response_and_says_so_in_the_log() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    harness.devices.set_behavior("bulb1", Behavior::Silent);
    let read = harness.execute(&cookie, "state.read", "bulb1", json!({})).await;
    read.assert_ok();
    assert_eq!(read.phase(), "no_response", "{}", read.text);
    assert_eq!(read.body["state"]["bulb1"]["available"], json!(false));
    assert!(harness.audit_has("no_response"));
    assert!(!harness.audit_has("confirmed"), "{:?}", harness.audit_results());
}

#[tokio::test]
async fn a_read_is_failed_if_any_device_is_disabled_and_only_confirmed_if_all_answer() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    harness.devices.set_disabled("wiz", "off for the test");
    let read = harness.execute(&cookie, "state.read", "all_bulbs", json!({})).await;
    assert_eq!(read.phase(), "failed", "{}", read.text);
    assert!(read.body["detail"].as_str().unwrap().contains("off for the test"), "{}", read.text);

    let read = harness.execute(&cookie, "state.read", "vm100", json!({})).await;
    assert_eq!(read.phase(), "confirmed", "{}", read.text);
    assert_eq!(harness.audit_count("confirmed"), 1);
    assert_eq!(harness.audit_count("failed"), 1);
}

#[tokio::test]
async fn a_non_idempotent_action_in_flight_cannot_be_repeated_with_other_parameters() {
    let (harness, mut auth, cookie) = Harness::signed_in_with(|c| {
        c.timing.sync_wait_ms = 5;
        c.timing.confirm_timeout_ms = Some(1_500);
    })
    .await;
    harness.devices.set_behavior("vm100", Behavior::AcceptsButIgnores);
    let first = harness.critical(&cookie, &mut auth, "vm.restart", "vm100", json!({})).await;
    assert_eq!(first.phase(), "commanded", "{}", first.text);

    // Past the time window, with a parameter that changes nothing.
    harness.advance(10_001);
    let params = json!({ "acknowledge_console_offline": false });
    let second = harness.critical(&cookie, &mut auth, "vm.restart", "vm100", params).await;
    assert_eq!(second.body["duplicate"], json!(true), "{}", second.text);
    assert_eq!(second.body["run_id"], first.body["run_id"]);
    assert_eq!(harness.devices.command_count("vm100"), 1, "the reboot was sent twice");
}

#[tokio::test]
async fn an_idempotent_action_with_a_new_value_is_still_a_new_request_in_flight() {
    let (harness, _auth, cookie) = Harness::signed_in_with(|c| {
        c.timing.sync_wait_ms = 5;
        c.timing.confirm_timeout_ms = Some(400);
    })
    .await;
    harness.devices.set_behavior("sonos", Behavior::AcceptsButIgnores);
    let first = harness.execute(&cookie, "speaker.volume", "sonos", json!({ "level": 10 })).await;
    let second = harness.execute(&cookie, "speaker.volume", "sonos", json!({ "level": 20 })).await;
    assert_eq!(second.body["duplicate"], json!(false), "{}", second.text);
    assert_ne!(first.body["run_id"], second.body["run_id"]);
}

#[tokio::test]
async fn a_second_feed_right_after_the_first_is_refused() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let params = json!({ "grams": 10 });
    let first = harness.prepare(&cookie, "pets.feed", "feeder", params.clone()).await;
    let second = harness.prepare(&cookie, "pets.feed", "feeder", params.clone()).await;
    harness.confirm_tap(&cookie, &first).await.assert_ok();

    // Prepared before the first ran, so the check at confirm time catches it.
    let double = harness.confirm_tap(&cookie, &second).await;
    double.assert_refused(StatusCode::CONFLICT, "duplicate_recent");
    assert_eq!(harness.devices.command_count("feeder"), 1);

    let again = harness.prepare(&cookie, "pets.feed", "feeder", params.clone()).await;
    again.assert_refused(StatusCode::CONFLICT, "duplicate_recent");

    // Once the window has passed it is a new, deliberate feed.
    harness.advance(10_001);
    let later = harness.prepare(&cookie, "pets.feed", "feeder", params).await;
    harness.confirm_tap(&cookie, &later).await.assert_ok();
    assert_eq!(harness.devices.command_count("feeder"), 2);
}

#[tokio::test]
async fn a_scene_stops_at_the_first_refusal_and_is_not_success() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    harness.devices.set_behavior("bulb2", Behavior::Rejects("no".to_string()));
    let done = harness.execute(&cookie, "scene.apply", "evening", json!({})).await;
    assert_eq!(done.phase(), "failed", "{}", done.text);
    let steps = done.body["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0]["phase"], json!("confirmed"));
    assert_eq!(steps[1]["target"], json!("bulb2"));
    assert_eq!(steps[1]["phase"], json!("failed"));
    assert_eq!(harness.devices.command_count("bulb3"), 0);
}

fn silent(harness: &Harness, bulbs: &[&str]) {
    for bulb in bulbs {
        harness.devices.set_behavior(bulb, Behavior::Silent);
    }
}

#[tokio::test]
async fn lights_that_do_not_answer_leave_the_others_a_success_with_a_note() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    silent(&harness, &["bulb2", "bulb3", "bulb4"]);
    let done = harness.execute(&cookie, "lights.brightness", "all_bulbs", json!({ "percent": 20 })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(done.body["detail"], json!("3 of 4 lights did not answer."));
    let phases: Vec<&str> =
        done.body["steps"].as_array().unwrap().iter().map(|s| s["phase"].as_str().unwrap()).collect();
    assert_eq!(phases, ["confirmed", "no_response", "no_response", "no_response"]);
    assert!(matches!(harness.devices.state("bulb1"), Some(State::Light { bri: 20, .. })));
    assert!(harness.audit_has("confirmed"));
    assert!(!harness.audit_has("no_response"));

    // It is the stored result as well as the immediate one.
    let run = harness.get(&format!("/api/v1/actions/runs/{}", done.body["run_id"].as_str().unwrap()), &cookie).await;
    assert_eq!(run.phase(), "confirmed", "{}", run.text);
    assert_eq!(run.body["detail"], json!("3 of 4 lights did not answer."));
}

#[tokio::test]
async fn a_silent_light_does_not_stop_the_ones_after_it() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    silent(&harness, &["bulb1"]);
    let done = harness.execute(&cookie, "scene.apply", "focus", json!({})).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(done.body["detail"], json!("1 of 4 lights did not answer."));
    for bulb in ["bulb2", "bulb3", "bulb4"] {
        assert_eq!(harness.devices.command_count(bulb), 1, "{bulb}");
    }
}

#[tokio::test]
async fn a_room_with_a_silent_light_is_a_success_with_a_note() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    silent(&harness, &["bulb2"]);
    let created = harness.post("/api/v1/rooms", &cookie, json!({ "name": "Kitchen" })).await;
    let id = created.body["room"]["id"].as_str().unwrap().to_string();
    harness
        .send(
            Method::PUT,
            &format!("/api/v1/rooms/{id}/devices"),
            Some(&cookie),
            Some(ORIGIN),
            Some(json!({ "targets": ["bulb1", "bulb2"] })),
        )
        .await
        .assert_ok();
    let done = harness.execute(&cookie, "room.power", &id, json!({ "on": false })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(done.body["detail"], json!("1 of 2 lights did not answer."));
}

#[tokio::test]
async fn if_no_light_answers_it_is_still_no_response() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    silent(&harness, &["bulb1", "bulb2", "bulb3", "bulb4"]);
    let done = harness.execute(&cookie, "lights.power", "all_bulbs", json!({ "on": false })).await;
    assert_eq!(done.phase(), "no_response", "{}", done.text);
    assert_eq!(done.body["detail"], json!("no reply from the device"));
    assert!(!harness.audit_has("confirmed"));
}

#[tokio::test]
async fn a_light_that_accepts_and_changes_nothing_counts_as_not_answering() {
    let (harness, _auth, cookie) = Harness::signed_in_with(|c| c.timing.confirm_timeout_ms = Some(40)).await;
    harness.devices.set_behavior("bulb2", Behavior::AcceptsButIgnores);
    let done = harness.execute(&cookie, "lights.brightness", "all_bulbs", json!({ "percent": 21 })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(done.body["detail"], json!("1 of 4 lights did not answer."));
}

#[tokio::test]
async fn a_refusal_after_silence_is_still_a_failure() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    silent(&harness, &["bulb1"]);
    harness.devices.set_behavior("bulb2", Behavior::Rejects("no".to_string()));
    let done = harness.execute(&cookie, "lights.power", "all_bulbs", json!({ "on": false })).await;
    assert_eq!(done.phase(), "failed", "{}", done.text);
    assert_eq!(harness.devices.command_count("bulb3"), 0);
}

#[tokio::test]
async fn one_light_on_its_own_is_unchanged() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    silent(&harness, &["bulb1"]);
    let done = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    assert_eq!(done.phase(), "no_response", "{}", done.text);
}

async fn movie(harness: &Harness, cookie: &str) -> Resp {
    let prepared = harness.prepare(cookie, "scene.movie", "movie", json!({})).await;
    harness.confirm_tap(cookie, &prepared).await
}

#[tokio::test]
async fn movie_passes_over_silent_lights_and_still_reaches_the_tv() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    silent(&harness, &["bulb2", "bulb3", "bulb4"]);
    let done = movie(&harness, &cookie).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(done.body["detail"], json!("3 of 4 lights did not answer."));
    assert_eq!(harness.devices.command_count("tv"), 2, "power and input");
    assert_eq!(harness.devices.command_count("sonos"), 1);
    let phases: Vec<&str> =
        done.body["steps"].as_array().unwrap().iter().map(|s| s["phase"].as_str().unwrap()).collect();
    assert_eq!(
        phases,
        ["confirmed", "no_response", "no_response", "no_response", "confirmed", "confirmed", "confirmed"]
    );
}

#[tokio::test]
async fn movie_with_no_lights_answering_still_counts_what_did_work() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    silent(&harness, &["bulb1", "bulb2", "bulb3", "bulb4"]);
    let done = movie(&harness, &cookie).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(done.body["detail"], json!("4 of 4 lights did not answer."));
}

#[tokio::test]
async fn movie_stops_if_the_tv_does_not_answer() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    silent(&harness, &["bulb2", "tv"]);
    let done = movie(&harness, &cookie).await;
    assert_eq!(done.phase(), "no_response", "{}", done.text);
    assert_eq!(harness.devices.command_count("sonos"), 0);
}

#[tokio::test]
async fn movie_stops_at_a_light_that_refuses() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    harness.devices.set_behavior("bulb2", Behavior::Rejects("no".to_string()));
    let done = movie(&harness, &cookie).await;
    assert_eq!(done.phase(), "failed", "{}", done.text);
    assert_eq!(harness.devices.command_count("bulb3"), 0);
    assert_eq!(harness.devices.command_count("tv"), 0);
}

#[tokio::test]
async fn movie_reads_every_device_back() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    harness.devices.set_behavior("tv", Behavior::Normal);
    let prepared = harness.prepare(&cookie, "scene.movie", "movie", json!({})).await;
    let done = harness.confirm_tap(&cookie, &prepared).await;
    assert_eq!(done.phase(), "confirmed");
    let targets: Vec<_> =
        done.body["steps"].as_array().unwrap().iter().map(|s| s["target"].as_str().unwrap().to_string()).collect();
    assert_eq!(targets, ["bulb1", "bulb2", "bulb3", "bulb4", "tv", "tv", "sonos"]);
}

#[tokio::test]
async fn a_reboot_is_confirmed_only_when_uptime_resets() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    harness.devices.set_behavior("vm100", Behavior::AcceptsButIgnores);
    let done = harness.critical(&cookie, &mut auth, "vm.restart", "vm100", json!({})).await;
    assert_eq!(done.phase(), "no_response", "still running, but it never restarted");
}

#[tokio::test]
async fn runs_interrupted_by_a_restart_are_not_left_open_or_called_success() {
    use std::sync::Arc;
    use swifty_hub::app::App;

    let (harness, _auth, cookie) = Harness::signed_in_with(|c| {
        c.timing.sync_wait_ms = 1;
        c.timing.confirm_timeout_ms = Some(60_000);
    })
    .await;
    harness.devices.set_behavior("bulb1", Behavior::AcceptsButIgnores);
    let first = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    assert_eq!(first.phase(), "commanded");

    let config = harness.app.config.clone();
    let restarted =
        App::new(config.clone(), harness.clock.clone() as Arc<_>, mock_registry(true, &harness.devices)).unwrap();
    let run = restarted
        .store
        .with(|conn| swifty_hub::actions::load_run(conn, first.body["run_id"].as_str().unwrap()))
        .unwrap()
        .unwrap();
    assert_eq!(run.phase, "no_response");
    assert!(run.detail.unwrap().contains("restarted"));
    assert!(harness.audit_has("no_response"));
}
