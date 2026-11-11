//! Every risk tier, and every way of trying to get around one.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::json;

use swifty_plugin_api::{Op, State};

// ----- tiers 0 and 1, and S

#[tokio::test]
async fn tier_0_reads_with_a_session_and_changes_nothing() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let read = harness.execute(&cookie, "state.read", "bulb1", json!({})).await;
    read.assert_ok();
    assert_eq!(read.phase(), "confirmed");
    assert_eq!(read.body["state"]["bulb1"]["state"]["bri"], json!(60));
    assert_eq!(harness.devices.total_commands(), 0);

    let all = harness.get("/api/v1/state", &cookie).await;
    all.assert_ok();
    assert_eq!(all.body["devices"]["sonos"]["state"]["vol"], json!(24));
    assert_eq!(all.body["devices"]["vm102"]["state"]["state"], json!("running"));

    let one = harness.get("/api/v1/state/tv", &cookie).await;
    assert_eq!(one.body["state"]["input"], json!("HDMI 1"));
    harness.get("/api/v1/state/nope", &cookie).await.assert_refused(StatusCode::NOT_FOUND, "unknown_device");

    let group = harness.execute(&cookie, "state.read", "all_vms", json!({})).await;
    assert_eq!(group.body["state"].as_object().unwrap().len(), 4);
}

#[tokio::test]
async fn tier_1_runs_on_a_session_alone() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let done = harness.execute(&cookie, "light.brightness", "bulb2", json!({ "percent": 77 })).await;
    done.assert_ok();
    assert_eq!(done.phase(), "confirmed");
    assert_eq!(done.body["step_up"], json!("none"));
    assert_eq!(harness.devices.commands(), vec![("bulb2".to_string(), Op::LightBrightness { percent: 77 })]);
    assert_eq!(harness.devices.state("bulb2"), Some(State::Light { on: true, bri: 77, hue: 30, color: true }));
}

#[tokio::test]
async fn every_tier_1_action_executes() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let cases = [
        ("light.power", "bulb1", json!({ "on": false })),
        ("light.hue", "bulb1", json!({ "hue": 200 })),
        ("lights.power", "all_bulbs", json!({ "on": true })),
        ("lights.brightness", "all_bulbs", json!({ "percent": 33 })),
        ("speaker.volume", "sonos", json!({ "level": 40 })),
        ("speaker.mute", "sonos", json!({ "muted": true })),
        ("speaker.pause", "sonos", json!({})),
        ("speaker.play", "sonos", json!({})),
        ("tv.input", "tv", json!({ "input": "HDMI 2" })),
        ("scene.apply", "evening", json!({})),
        ("scene.apply", "focus", json!({})),
        ("scene.apply", "all_off", json!({})),
    ];
    for (action, target, params) in cases {
        let done = harness.execute(&cookie, action, target, params).await;
        done.assert_ok();
        assert_eq!(done.phase(), "confirmed", "{action}: {}", done.text);
    }
}

#[tokio::test]
async fn tier_s_needs_only_a_session_and_is_not_two_step() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let done = harness.execute(&cookie, "print.estop", "printer", json!({})).await;
    done.assert_ok();
    assert_eq!(done.phase(), "confirmed");
    let refused = harness.prepare(&cookie, "print.estop", "printer", json!({})).await;
    refused.assert_refused(StatusCode::CONFLICT, "not_two_step");
}

#[tokio::test]
async fn direct_actions_cannot_be_prepared() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let refused = harness.prepare(&cookie, "light.power", "bulb1", json!({ "on": true })).await;
    refused.assert_refused(StatusCode::CONFLICT, "not_two_step");
}

// ----- tier 2

const TIER_2: &[(&str, &str, &str)] = &[
    ("pets.feed", "feeder", r#"{"grams":10}"#),
    ("pets.clean", "litter", "{}"),
    ("vm.start", "vm101", "{}"),
    ("print.start", "printer", "{}"),
    ("print.pause", "printer", "{}"),
    ("print.resume", "printer", "{}"),
    ("tv.power", "tv", r#"{"on":false}"#),
    ("scene.movie", "movie", "{}"),
];

fn parsed(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap()
}

#[tokio::test]
async fn tier_2_cannot_be_executed_directly() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    for (action, target, params) in TIER_2 {
        let refused = harness.execute(&cookie, action, target, parsed(params)).await;
        refused.assert_refused(StatusCode::FORBIDDEN, "confirmation_required");
    }
    assert_eq!(harness.devices.total_commands(), 0);
    assert_eq!(harness.audit_count("refused: confirmation_required"), TIER_2.len());
}

#[tokio::test]
async fn every_tier_2_action_runs_after_prepare_and_confirm() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    for (action, target, params) in TIER_2 {
        let prepared = harness.prepare(&cookie, action, target, parsed(params)).await;
        prepared.assert_ok();
        assert_eq!(prepared.body["tier"], json!(2));
        assert_eq!(prepared.body["needs"], json!("tap"));
        assert!(prepared.body["confirmation"].is_string());
        assert!(!prepared.body["what"].as_str().unwrap().is_empty());
        assert_eq!(prepared.body["expires_at"], json!(harness.now() + 30_000));
        let before = harness.devices.total_commands();
        let done = harness.confirm_tap(&cookie, &prepared).await;
        done.assert_ok();
        assert_eq!(done.phase(), "confirmed", "{action}: {}", done.text);
        assert_eq!(done.body["step_up"], json!("confirm"));
        assert!(harness.devices.total_commands() > before);
    }
}

#[tokio::test]
async fn prepare_alone_sends_nothing() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    harness.prepare(&cookie, "pets.feed", "feeder", json!({ "grams": 10 })).await.assert_ok();
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn a_confirmation_works_once() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "tv.power", "tv", json!({ "on": false })).await;
    let first = harness.confirm_tap(&cookie, &prepared).await;
    first.assert_ok();
    let replay = harness.confirm_tap(&cookie, &prepared).await;
    replay.assert_refused(StatusCode::CONFLICT, "already_used");
    assert_eq!(replay.body["error"]["detail"]["run_id"], first.body["run_id"]);
    assert_eq!(harness.devices.command_count("tv"), 1);
}

#[tokio::test]
async fn a_confirmation_expires_after_thirty_seconds() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "pets.feed", "feeder", json!({ "grams": 10 })).await;
    harness.advance(30_000);
    harness.confirm_tap(&cookie, &prepared).await.assert_refused(StatusCode::GONE, "expired");
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn a_confirmation_is_good_until_just_before_thirty_seconds() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "pets.feed", "feeder", json!({ "grams": 10 })).await;
    harness.advance(29_999);
    harness.confirm_tap(&cookie, &prepared).await.assert_ok();
}

async fn confirm_with(
    harness: &Harness,
    cookie: &str,
    prepared: &Resp,
    change: impl FnOnce(&mut serde_json::Value),
) -> Resp {
    let mut body = json!({
        "prepared_id": prepared.body["prepared_id"],
        "action": prepared.body["action"],
        "target": prepared.body["target"],
        "params": prepared.body["params"],
        "confirmation": prepared.body["confirmation"],
    });
    change(&mut body);
    harness.post("/api/v1/actions/confirm", cookie, body).await
}

#[tokio::test]
async fn a_confirmation_is_bound_to_action_target_and_parameters() {
    let (harness, _auth, cookie) = Harness::signed_in().await;

    let prepared = harness.prepare(&cookie, "vm.start", "vm101", json!({})).await;
    let wrong_target = confirm_with(&harness, &cookie, &prepared, |b| b["target"] = json!("vm100")).await;
    wrong_target.assert_refused(StatusCode::CONFLICT, "binding_mismatch");

    let prepared = harness.prepare(&cookie, "pets.feed", "feeder", json!({ "grams": 10 })).await;
    let wrong_params = confirm_with(&harness, &cookie, &prepared, |b| b["params"] = json!({ "grams": 50 })).await;
    wrong_params.assert_refused(StatusCode::CONFLICT, "binding_mismatch");

    let prepared = harness.prepare(&cookie, "vm.start", "vm101", json!({})).await;
    let wrong_action = confirm_with(&harness, &cookie, &prepared, |b| b["action"] = json!("pets.clean")).await;
    wrong_action.assert_refused(StatusCode::CONFLICT, "binding_mismatch");

    assert_eq!(harness.devices.total_commands(), 0);
    assert_eq!(harness.audit_count("refused: binding_mismatch"), 3);
}

#[tokio::test]
async fn a_mismatch_cancels_the_prepared_action() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "vm.start", "vm101", json!({})).await;
    confirm_with(&harness, &cookie, &prepared, |b| b["target"] = json!("vm100")).await;
    harness.confirm_tap(&cookie, &prepared).await.assert_refused(StatusCode::CONFLICT, "already_used");
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn a_confirmation_is_bound_to_its_session() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let other = harness.login(&mut auth).await;
    let prepared = harness.prepare(&cookie, "pets.feed", "feeder", json!({ "grams": 10 })).await;
    harness.confirm_tap(&other, &prepared).await.assert_refused(StatusCode::FORBIDDEN, "wrong_session");
    assert_eq!(harness.devices.total_commands(), 0);
    // The attempt from the other session does not cancel it for its owner.
    harness.confirm_tap(&cookie, &prepared).await.assert_ok();
}

#[tokio::test]
async fn a_wrong_or_missing_token_is_refused() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "pets.feed", "feeder", json!({ "grams": 10 })).await;
    let wrong = confirm_with(&harness, &cookie, &prepared, |b| b["confirmation"] = json!("guess")).await;
    wrong.assert_refused(StatusCode::FORBIDDEN, "invalid_confirmation");
    let prepared = harness.prepare(&cookie, "vm.start", "vm100", json!({})).await;
    let missing = confirm_with(&harness, &cookie, &prepared, |b| {
        b.as_object_mut().unwrap().remove("confirmation").map(|_| ()).unwrap()
    })
    .await;
    missing.assert_refused(StatusCode::FORBIDDEN, "invalid_confirmation");
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn the_confirmation_token_is_stored_hashed() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "pets.feed", "feeder", json!({ "grams": 10 })).await;
    let token = prepared.body["confirmation"].as_str().unwrap();
    assert!(!harness.dump("prepared_actions").contains(token));
    assert!(!harness.dump("audit_log").contains(token));
}

#[tokio::test]
async fn unknown_prepared_id_is_refused() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let refused = harness
        .post(
            "/api/v1/actions/confirm",
            &cookie,
            json!({ "prepared_id": "nope", "action": "pets.feed", "target": "feeder", "params": {} }),
        )
        .await;
    refused.assert_refused(StatusCode::NOT_FOUND, "unknown_prepared");
}

// ----- tier 3

const TIER_3: &[(&str, &str)] = &[
    ("vm.stop", "vm101"),
    ("vm.restart", "vm100"),
    ("vm.force_stop", "vm103"),
    ("host.shutdown", "host"),
    ("host.reboot", "host"),
    ("router.wireguard_restart", "router"),
];

#[tokio::test]
async fn tier_3_cannot_be_executed_directly() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    for (action, target) in TIER_3 {
        let refused = harness.execute(&cookie, action, target, json!({})).await;
        refused.assert_refused(StatusCode::FORBIDDEN, "step_up_required");
    }
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn every_tier_3_action_runs_after_a_bound_passkey_assertion() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    for (action, target) in TIER_3 {
        let done = harness.critical(&cookie, &mut auth, action, target, json!({})).await;
        done.assert_ok();
        assert_eq!(done.phase(), "confirmed", "{action}: {}", done.text);
        assert_eq!(done.body["step_up"], json!("passkey"));
    }
    assert_eq!(harness.audit_count("confirmed"), TIER_3.len());
}

#[tokio::test]
async fn tier_3_gives_no_token_and_a_token_does_not_help() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    prepared.assert_ok();
    assert!(prepared.body["confirmation"].is_null());
    assert_eq!(prepared.body["needs"], json!("passkey"));

    // A valid tier 2 token from another action must not satisfy tier 3.
    let other = harness.prepare(&cookie, "vm.start", "vm100", json!({})).await;
    let forged =
        confirm_with(&harness, &cookie, &prepared, |b| b["confirmation"] = other.body["confirmation"].clone()).await;
    forged.assert_refused(StatusCode::PRECONDITION_REQUIRED, "step_up_required");
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn a_step_up_cannot_finish_without_being_started() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    let started = harness.login_start().await;
    let assertion = assert_with(&mut auth, &started.body["options"], origin_url());
    let refused = harness.confirm_assertion(&cookie, &prepared, &assertion).await;
    refused.assert_refused(StatusCode::PRECONDITION_REQUIRED, "step_up_required");
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn a_login_assertion_cannot_be_used_as_a_step_up() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    harness.step_up_start(&cookie, &prepared).await.assert_ok();
    let login = harness.login_start().await;
    let assertion = assert_with(&mut auth, &login.body["options"], origin_url());
    let refused = harness.confirm_assertion(&cookie, &prepared, &assertion).await;
    refused.assert_refused(StatusCode::UNAUTHORIZED, "assertion_rejected");
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn a_step_up_cannot_be_replayed() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    let started = harness.step_up_start(&cookie, &prepared).await;
    let assertion = assert_with(&mut auth, &started.body["options"], origin_url());
    harness.confirm_assertion(&cookie, &prepared, &assertion).await.assert_ok();
    let replay = harness.confirm_assertion(&cookie, &prepared, &assertion).await;
    replay.assert_refused(StatusCode::CONFLICT, "already_used");
    assert_eq!(harness.devices.command_count("vm101"), 1);
}

#[tokio::test]
async fn a_repeat_that_finds_the_challenge_taken_does_not_cancel_the_approval() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    let started = harness.step_up_start(&cookie, &prepared).await;
    let assertion = assert_with(&mut auth, &started.body["options"], origin_url());

    // The state a repeat finds while the first request is checking its signature.
    let id = prepared.body["prepared_id"].as_str().unwrap().to_string();
    harness
        .app
        .store
        .with(|conn| conn.execute("UPDATE ceremonies SET used_at = 1 WHERE prepared_id = ?1", [&id]))
        .unwrap();
    let repeat = harness.confirm_assertion(&cookie, &prepared, &assertion).await;
    repeat.assert_refused(StatusCode::CONFLICT, "challenge_used");

    let consumed: Option<i64> = harness
        .app
        .store
        .with(|conn| conn.query_row("SELECT consumed_at FROM prepared_actions WHERE id = ?1", [&id], |row| row.get(0)))
        .unwrap();
    assert_eq!(consumed, None, "the repeat cancelled the approval it repeated");
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn one_assertion_cannot_serve_a_second_prepared_action() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let first = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    let second = harness.prepare(&cookie, "vm.stop", "vm103", json!({})).await;
    let first_up = harness.step_up_start(&cookie, &first).await;
    harness.step_up_start(&cookie, &second).await.assert_ok();

    // Signed for the first, presented for the second.
    let assertion = assert_with(&mut auth, &first_up.body["options"], origin_url());
    let refused = harness.confirm_assertion(&cookie, &second, &assertion).await;
    refused.assert_refused(StatusCode::UNAUTHORIZED, "assertion_rejected");
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn the_step_up_challenge_commits_to_the_action() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let first = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    let second = harness.prepare(&cookie, "vm.stop", "vm103", json!({})).await;
    let a = harness.step_up_start(&cookie, &first).await;
    let b = harness.step_up_start(&cookie, &second).await;
    assert_ne!(a.body["options"]["publicKey"]["challenge"], b.body["options"]["publicKey"]["challenge"]);
    // The stored state holds exactly the challenge sent to the browser.
    let stored = harness.dump("ceremonies");
    assert!(stored.contains(a.body["options"]["publicKey"]["challenge"].as_str().unwrap()));
}

#[tokio::test]
async fn a_step_up_is_bound_to_the_exact_target() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    let started = harness.step_up_start(&cookie, &prepared).await;
    let assertion = assert_with(&mut auth, &started.body["options"], origin_url());
    let wrong = harness
        .post(
            "/api/v1/actions/confirm",
            &cookie,
            json!({
                "prepared_id": prepared.body["prepared_id"],
                "action": "vm.stop",
                "target": "vm103",
                "params": prepared.body["params"],
                "assertion": assertion,
            }),
        )
        .await;
    wrong.assert_refused(StatusCode::CONFLICT, "binding_mismatch");
    assert_eq!(harness.devices.total_commands(), 0);
    // The genuine request is cancelled too, since something tampered with it.
    let genuine = harness.confirm_assertion(&cookie, &prepared, &assertion).await;
    genuine.assert_refused(StatusCode::CONFLICT, "already_used");
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn a_step_up_is_bound_to_the_exact_action() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    let started = harness.step_up_start(&cookie, &prepared).await;
    let assertion = assert_with(&mut auth, &started.body["options"], origin_url());
    let wrong = harness
        .post(
            "/api/v1/actions/confirm",
            &cookie,
            json!({
                "prepared_id": prepared.body["prepared_id"],
                "action": "vm.force_stop",
                "target": "vm101",
                "params": prepared.body["params"],
                "assertion": assertion,
            }),
        )
        .await;
    wrong.assert_refused(StatusCode::CONFLICT, "binding_mismatch");
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn a_step_up_is_bound_to_the_exact_parameters() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let mut other = authenticator();
    harness.register_passkey(&cookie, &mut auth, &mut other, "iPad", false).await;

    let prepared = harness.prepare(&cookie, "passkey.register", "new", json!({ "name": "Key A" })).await;
    let started = harness.step_up_start(&cookie, &prepared).await;
    let assertion = assert_with(&mut auth, &started.body["options"], origin_url());
    let wrong = harness
        .post(
            "/api/v1/actions/confirm",
            &cookie,
            json!({
                "prepared_id": prepared.body["prepared_id"],
                "action": "passkey.register",
                "target": "new",
                "params": { "name": "Key B" },
                "assertion": assertion,
            }),
        )
        .await;
    wrong.assert_refused(StatusCode::CONFLICT, "binding_mismatch");
}

#[tokio::test]
async fn step_up_start_checks_the_restated_action() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    let refused = harness
        .post(
            "/api/v1/auth/step-up/start",
            &cookie,
            json!({
                "prepared_id": prepared.body["prepared_id"],
                "action": "vm.stop",
                "target": "vm100",
                "params": {},
            }),
        )
        .await;
    refused.assert_refused(StatusCode::CONFLICT, "binding_mismatch");
    harness.step_up_start(&cookie, &prepared).await.assert_refused(StatusCode::CONFLICT, "already_used");
}

#[tokio::test]
async fn a_step_up_challenge_expires_after_sixty_seconds() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    let started = harness.step_up_start(&cookie, &prepared).await;
    assert_eq!(started.body["expires_at"], json!(harness.now() + 60_000));
    let assertion = assert_with(&mut auth, &started.body["options"], origin_url());
    harness.advance(60_000);
    let late = harness.confirm_assertion(&cookie, &prepared, &assertion).await;
    late.assert_refused(StatusCode::GONE, "challenge_expired");
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn a_step_up_challenge_works_until_just_before_sixty_seconds() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    let started = harness.step_up_start(&cookie, &prepared).await;
    let assertion = assert_with(&mut auth, &started.body["options"], origin_url());
    harness.advance(59_999);
    harness.confirm_assertion(&cookie, &prepared, &assertion).await.assert_ok();
}

#[tokio::test]
async fn a_prepared_critical_action_must_start_its_step_up_in_time() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    harness.advance(120_000);
    harness.step_up_start(&cookie, &prepared).await.assert_refused(StatusCode::GONE, "expired");
}

#[tokio::test]
async fn only_one_step_up_is_issued_per_action() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    harness.step_up_start(&cookie, &prepared).await.assert_ok();
    harness.step_up_start(&cookie, &prepared).await.assert_refused(StatusCode::CONFLICT, "challenge_used");
}

#[tokio::test]
async fn a_failed_assertion_uses_up_the_step_up() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    let started = harness.step_up_start(&cookie, &prepared).await;
    let bad = assert_from_wrong_origin(&mut auth, &started.body["options"]);
    harness
        .confirm_assertion(&cookie, &prepared, &bad)
        .await
        .assert_refused(StatusCode::UNAUTHORIZED, "assertion_rejected");
    let good = assert_with(&mut auth, &started.body["options"], origin_url());
    harness.confirm_assertion(&cookie, &prepared, &good).await.assert_refused(StatusCode::CONFLICT, "already_used");
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn a_step_up_from_the_wrong_origin_is_refused() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    let started = harness.step_up_start(&cookie, &prepared).await;
    let assertion = assert_from_wrong_origin(&mut auth, &started.body["options"]);
    let refused = harness.confirm_assertion(&cookie, &prepared, &assertion).await;
    refused.assert_refused(StatusCode::UNAUTHORIZED, "assertion_rejected");
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn a_step_up_needs_user_verification() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    let started = harness.step_up_start(&cookie, &prepared).await;
    let assertion = assert_without_uv(&mut auth, &started.body["options"]);
    let refused = harness.confirm_assertion(&cookie, &prepared, &assertion).await;
    refused.assert_refused(StatusCode::UNAUTHORIZED, "assertion_rejected");
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn a_step_up_belongs_to_the_session_that_prepared_it() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let other = harness.login(&mut auth).await;
    let prepared = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    harness.step_up_start(&other, &prepared).await.assert_refused(StatusCode::FORBIDDEN, "wrong_session");
    let started = harness.step_up_start(&cookie, &prepared).await;
    let assertion = assert_with(&mut auth, &started.body["options"], origin_url());
    harness
        .confirm_assertion(&other, &prepared, &assertion)
        .await
        .assert_refused(StatusCode::FORBIDDEN, "wrong_session");
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn a_tier_2_action_has_no_step_up() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "vm.start", "vm100", json!({})).await;
    harness.step_up_start(&cookie, &prepared).await.assert_refused(StatusCode::CONFLICT, "step_up_not_applicable");
}

#[tokio::test]
async fn the_step_up_credential_is_recorded() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let mut second = authenticator();
    let second_id = harness.register_passkey(&cookie, &mut auth, &mut second, "iPad", false).await;
    // Signed in with the first passkey, approved with the second.
    let done = harness.critical(&cookie, &mut second, "vm.stop", "vm101", json!({})).await;
    done.assert_ok();
    let audit = harness.get("/api/v1/audit", &cookie).await;
    let row = audit.body["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["result"] == json!("confirmed") && r["action"] == json!("vm.stop"))
        .unwrap()
        .clone();
    assert_eq!(row["credential_ref"], json!(second_id));
    assert_eq!(row["device"], json!("iPad"));
    assert_eq!(row["step_up"], json!("passkey"));
}

// ----- the console VM

#[tokio::test]
async fn stopping_the_console_vm_needs_an_acknowledgement() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    for action in ["vm.stop", "vm.restart", "vm.force_stop"] {
        let refused = harness.prepare(&cookie, action, "vm102", json!({})).await;
        refused.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "acknowledgement_required");
        assert_eq!(refused.body["error"]["detail"]["field"], json!("acknowledge_console_offline"));
        assert!(refused.body["error"]["detail"]["warning"].as_str().unwrap().contains("console"));
        let refused = harness.prepare(&cookie, action, "vm102", json!({ "acknowledge_console_offline": false })).await;
        refused.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "acknowledgement_required");
    }

    let params = json!({ "acknowledge_console_offline": true });
    let prepared = harness.prepare(&cookie, "vm.stop", "vm102", params.clone()).await;
    prepared.assert_ok();
    assert!(prepared.body["warning"].as_str().unwrap().contains("goes offline"));
    harness.advance(1);
    let done = harness.critical(&cookie, &mut auth, "vm.stop", "vm102", params).await;
    done.assert_ok();
    assert_eq!(done.phase(), "confirmed");
}

#[tokio::test]
async fn the_acknowledgement_is_only_needed_for_the_configured_vm() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await.assert_ok();

    let (harness, _auth, cookie) = Harness::signed_in_with(|c| c.runs_on = Some("vm101".to_string())).await;
    harness.prepare(&cookie, "vm.stop", "vm102", json!({})).await.assert_ok();
    let refused = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    refused.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "acknowledgement_required");
}

#[tokio::test]
async fn starting_the_console_vm_needs_no_acknowledgement() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    harness.prepare(&cookie, "vm.start", "vm102", json!({})).await.assert_ok();
}

// ----- nothing outside the catalog

#[tokio::test]
async fn there_is_no_generic_command() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    for action in ["shell", "proxy", "raw", "exec", "command", "light.power; rm -rf /"] {
        let refused = harness.execute(&cookie, action, "bulb1", json!({})).await;
        refused.assert_refused(StatusCode::NOT_FOUND, "unknown_action");
    }
    for path in
        ["/api/v1/proxy", "/api/v1/shell", "/api/v1/exec", "/api/v1/raw", "/api/v1/device/raw", "/api/v1/actions/raw"]
    {
        let response = harness.post(path, &cookie, json!({ "command": "x" })).await;
        response.assert_refused(StatusCode::NOT_FOUND, "not_found");
    }
    assert_eq!(harness.devices.total_commands(), 0);
    // A made-up name is not copied into the log.
    assert!(!harness.dump("audit_log").contains("rm -rf"));
}

#[tokio::test]
async fn unexpected_fields_are_refused() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let refused = harness
        .post(
            "/api/v1/actions/execute",
            &cookie,
            json!({ "action": "light.power", "target": "bulb1", "params": { "on": true }, "url": "http://x" }),
        )
        .await;
    assert!(refused.status.is_client_error());
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn targets_outside_the_table_are_refused() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let cases = [
        ("light.power", "bulb5", json!({ "on": true })),
        ("light.power", "../etc/passwd", json!({ "on": true })),
        ("speaker.volume", "sub", json!({ "level": 10 })),
        ("state.read", "http://192.0.2.1/", json!({})),
    ];
    for (action, target, params) in cases {
        harness
            .execute(&cookie, action, target, params)
            .await
            .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
    }
    let refused = harness.prepare(&cookie, "vm.stop", "vm104", json!({})).await;
    refused.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
    assert_eq!(harness.devices.total_commands(), 0);
    assert!(!harness.dump("audit_log").contains("passwd"));
}

#[tokio::test]
async fn parameters_out_of_range_never_reach_an_adapter() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let cases = [
        ("light.brightness", "bulb1", json!({ "percent": 0 })),
        ("light.brightness", "bulb1", json!({ "percent": 101 })),
        ("light.brightness", "bulb1", json!({ "percent": "50" })),
        ("light.hue", "bulb1", json!({ "hue": 360 })),
        ("speaker.volume", "sonos", json!({ "level": 101 })),
        ("speaker.volume", "sonos", json!({ "level": -1 })),
        ("speaker.volume", "sonos", json!({ "level": 1.5 })),
        ("tv.input", "tv", json!({ "input": "HDMI 9" })),
        ("light.power", "bulb1", json!({ "on": true, "raw": "x" })),
        ("light.power", "bulb1", json!({})),
    ];
    for (action, target, params) in cases {
        let refused = harness.execute(&cookie, action, target, params.clone()).await;
        refused.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_params");
    }
    let refused = harness.prepare(&cookie, "pets.feed", "feeder", json!({ "grams": 500 })).await;
    refused.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_params");
    assert_eq!(harness.devices.total_commands(), 0);
    assert_eq!(harness.devices.total_reads(), 0);
}

#[tokio::test]
async fn clients_cannot_claim_the_voice_channel() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let refused = harness
        .post(
            "/api/v1/actions/execute",
            &cookie,
            json!({ "action": "light.power", "target": "bulb1", "params": { "on": true }, "channel": "voice" }),
        )
        .await;
    refused.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_channel");
}

#[tokio::test]
async fn the_catalog_describes_every_action() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let catalog = harness.get("/api/v1/catalog", &cookie).await;
    catalog.assert_ok();
    let actions = catalog.body["actions"].as_array().unwrap();
    // The standard table, then the actions of the plugins in this build.
    assert_eq!(actions.len(), swifty_hub::catalog::all().len());
    assert_eq!(actions.len(), swifty_hub::catalog::CATALOG.len() + swifty_hub::catalog::registered().len());
    let volume = actions.iter().find(|a| a["id"] == json!("speaker.volume")).unwrap();
    assert_eq!(volume["tier"], json!(1));
    assert_eq!(volume["params"][0]["schema"]["max"], json!(100));
    assert_eq!(actions.iter().find(|a| a["id"] == json!("print.estop")).unwrap()["tier"], json!("S"));
    let feed = actions.iter().find(|a| a["id"] == json!("pets.feed")).unwrap();
    assert_eq!(feed["flow"], json!("prepare_confirm"));
    assert_eq!(feed["idempotent"], json!(false));
    assert_eq!(actions.iter().find(|a| a["id"] == json!("vm.stop")).unwrap()["flow"], json!("prepare_step_up"));
}

#[tokio::test]
async fn devices_report_their_availability() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let devices = harness.get("/api/v1/devices", &cookie).await;
    devices.assert_ok();
    assert_eq!(devices.body["live"], json!(true));
    let list = devices.body["devices"].as_array().unwrap();
    assert!(list.iter().all(|d| d["enabled"] == json!(true)));
    let vm = list.iter().find(|d| d["target"] == json!("vm100")).unwrap();
    assert_eq!(vm["plugin"], json!("mock"), "the test house runs on the mock plugin");
    assert_eq!(vm["instance"], json!("proxmox"));
    assert_eq!(vm["class"], json!("vm"));
    assert_eq!(vm["audience"], json!("owner"));
    assert_eq!(vm.get("adapter"), None, "adapter became plugin");
}

// ----- the registry the real server starts with

/// Every first-party plugin, with the rig's instances: Petkit's waits for its sign-in.
async fn production(
    live: bool,
    tweak: impl FnOnce(&mut common::devices::Site),
) -> (common::devices::Rig, Harness, webauthn_authenticator_rs::softpasskey::SoftPasskey, String) {
    rig_harness(live, tweak).await
}

#[tokio::test]
async fn petkit_is_disabled_with_a_clear_reason_until_it_is_configured_in_both_modes() {
    for live in [false, true] {
        let (rig, harness, _auth, cookie) = production(live, |_| {}).await;
        let refused = harness.prepare(&cookie, "pets.feed", "feeder", json!({ "grams": 10 })).await;
        refused.assert_refused(StatusCode::CONFLICT, "device_unavailable");
        assert!(refused.body["error"]["message"].as_str().unwrap().contains("Petkit login is stored"));
        let refused = harness.prepare(&cookie, "pets.clean", "litter", json!({})).await;
        refused.assert_refused(StatusCode::CONFLICT, "device_unavailable");

        let state = harness.get("/api/v1/state/feeder", &cookie).await;
        assert_eq!(state.body["available"], json!(false));
        assert!(state.body["error"].as_str().unwrap().contains("Petkit login is stored"));
        assert_eq!(rig.traffic(), 0);
    }
}

#[tokio::test]
async fn an_instance_waiting_for_its_setup_cannot_run_live() {
    let (rig, harness, _auth, cookie) = production(true, |site| site.forget("webos", "client_key")).await;
    let refused = harness.execute(&cookie, "tv.input", "tv", json!({ "input": "HDMI 1" })).await;
    refused.assert_refused(StatusCode::CONFLICT, "device_unavailable");
    assert!(refused.body["error"]["message"].as_str().unwrap().contains("The TV is not paired yet"));
    assert_eq!(rig.tv.traffic(), 0);
}

#[tokio::test]
async fn an_unknown_cookie_on_a_public_route_changes_nothing() {
    let harness = Harness::new();
    let response = harness
        .send(
            Method::POST,
            "/api/v1/auth/login/start",
            Some("__Host-swifty-session=forged"),
            Some(ORIGIN),
            Some(json!({})),
        )
        .await;
    response.assert_refused(StatusCode::CONFLICT, "no_credentials");
}

#[tokio::test]
async fn a_disabled_device_is_also_checked_at_confirm_time() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "tv.power", "tv", json!({ "on": false })).await;
    prepared.assert_ok();
    harness.devices.set_disabled("webos", "The TV has not been paired.");
    let refused = harness.confirm_tap(&cookie, &prepared).await;
    refused.assert_refused(StatusCode::CONFLICT, "device_unavailable");
    assert_eq!(harness.devices.total_commands(), 0);
}
