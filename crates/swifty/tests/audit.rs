//! The audit log: what it records, what it refuses to store, and tamper detection.

mod common;

use std::process::{Command, Stdio};

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use swifty_hub::audit::{self, Checkpoint, VerifyOptions};
use swifty_hub::store::AUDIT_TRIGGERS;

fn verify(harness: &Harness) -> audit::VerifyReport {
    harness.app.store.with(audit::verify).unwrap()
}

/// Removes the triggers, as someone editing the file by hand could.
fn drop_triggers(harness: &Harness) {
    harness
        .app
        .store
        .with(|conn| {
            conn.execute_batch(
                "DROP TRIGGER IF EXISTS audit_log_no_update; DROP TRIGGER IF EXISTS audit_log_no_delete;",
            )
        })
        .unwrap();
}

/// Puts the triggers back exactly, as someone covering their tracks would.
fn restore_triggers(harness: &Harness) {
    harness
        .app
        .store
        .with(|conn| {
            for (_, trigger) in AUDIT_TRIGGERS {
                conn.execute_batch(trigger)?;
            }
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
}

fn tamper(harness: &Harness, sql: &str) {
    drop_triggers(harness);
    harness.app.store.with(|conn| conn.execute_batch(sql)).unwrap();
}

async fn with_activity() -> (Harness, webauthn_authenticator_rs::softpasskey::SoftPasskey, String) {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await.assert_ok();
    let prepared = harness.prepare(&cookie, "vm.start", "vm100", json!({})).await;
    harness.confirm_tap(&cookie, &prepared).await.assert_ok();
    harness.critical(&cookie, &mut auth, "vm.stop", "vm101", json!({})).await.assert_ok();
    (harness, auth, cookie)
}

#[tokio::test]
async fn the_chain_verifies_after_real_activity() {
    let (harness, _auth, cookie) = with_activity().await;
    let report = verify(&harness);
    assert!(report.ok, "{report:?}");
    assert!(report.rows >= 8);

    let http = harness.get("/api/v1/audit/verify", &cookie).await;
    http.assert_ok();
    assert_eq!(http.body["ok"], json!(true));
    assert_eq!(http.body["rows"], json!(report.rows));
}

#[tokio::test]
async fn an_empty_log_is_reported_as_empty_and_not_as_verified() {
    let harness = Harness::new();
    let report = verify(&harness);
    assert!(!report.ok);
    assert!(report.empty);
    assert_eq!(report.rows, 0);
    assert_eq!(report.head, audit::GENESIS);
    assert!(report.problem.unwrap().contains("nothing to verify"));

    let allowed = harness
        .app
        .store
        .with(|conn| audit::verify_with(conn, VerifyOptions { allow_empty: true, ..Default::default() }))
        .unwrap();
    assert!(allowed.ok);
    assert!(allowed.empty);
}

#[tokio::test]
async fn a_log_with_all_its_rows_and_head_removed_is_not_verified() {
    let (harness, _auth, cookie) = with_activity().await;
    tamper(&harness, "DELETE FROM audit_log; DELETE FROM meta WHERE key = 'audit_head';");
    restore_triggers(&harness);
    let report = verify(&harness);
    assert!(!report.ok);
    assert!(report.empty);
    let http = harness.get("/api/v1/audit/verify", &cookie).await;
    assert_eq!(http.body["ok"], json!(false), "{}", http.text);
}

// ----- what the chain alone cannot catch, and what does catch it

#[tokio::test]
async fn verification_fails_if_an_append_only_trigger_is_missing_or_altered() {
    let (harness, _auth, _cookie) = with_activity().await;
    assert!(verify(&harness).ok);

    harness.app.store.with(|conn| conn.execute_batch("DROP TRIGGER audit_log_no_delete;")).unwrap();
    let report = verify(&harness);
    assert!(!report.ok);
    assert!(report.problem.unwrap().contains("audit_log_no_delete"));

    harness
        .app
        .store
        .with(|conn| {
            conn.execute_batch("CREATE TRIGGER audit_log_no_delete BEFORE DELETE ON audit_log BEGIN SELECT 1; END;")
        })
        .unwrap();
    let report = verify(&harness);
    assert!(!report.ok, "a trigger that does nothing passed");

    drop_triggers(&harness);
    restore_triggers(&harness);
    assert!(verify(&harness).ok);
}

/// Cuts the last rows and makes the recorded head match, leaving the triggers
/// as they were: the attack the unkeyed chain cannot see.
fn truncate_and_fix_the_head(harness: &Harness, keep: i64) {
    let hash: String = harness
        .app
        .store
        .with(|conn| conn.query_row("SELECT hash FROM audit_log WHERE seq = ?1", [keep], |row| row.get(0)))
        .unwrap();
    tamper(
        harness,
        &format!(
            "DELETE FROM audit_log WHERE seq > {keep};
             UPDATE meta SET value = '{keep}:{hash}' WHERE key = 'audit_head';"
        ),
    );
    restore_triggers(harness);
}

const AUDIT_KEY: &str = "audit-signing-key-for-tests-0123456789";

/// A harness whose audit head is signed, and the directory holding the key.
async fn keyed_activity() -> (Harness, webauthn_authenticator_rs::softpasskey::SoftPasskey, String, tempfile::TempDir) {
    let secrets = tempfile::tempdir().unwrap();
    let key = common::devices::write_secret(secrets.path(), "audit.key", AUDIT_KEY);
    let (harness, mut auth, cookie) = Harness::signed_in_with(move |c| c.audit.key_file = Some(key)).await;
    harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await.assert_ok();
    let prepared = harness.prepare(&cookie, "vm.start", "vm100", json!({})).await;
    harness.confirm_tap(&cookie, &prepared).await.assert_ok();
    harness.critical(&cookie, &mut auth, "vm.stop", "vm101", json!({})).await.assert_ok();
    (harness, auth, cookie, secrets)
}

fn keyed_verify(harness: &Harness) -> audit::VerifyReport {
    harness.app.store.with(|conn| harness.app.verify_audit(conn)).unwrap()
}

#[tokio::test]
async fn a_keyed_log_verifies_and_says_it_was_keyed() {
    let (harness, _auth, cookie, _secrets) = keyed_activity().await;
    let report = keyed_verify(&harness);
    assert!(report.ok, "{report:?}");
    assert!(report.keyed);
    let http = harness.get("/api/v1/audit/verify", &cookie).await;
    assert_eq!(http.body["ok"], json!(true), "{}", http.text);
    assert_eq!(http.body["keyed"], json!(true), "{}", http.text);
    assert!(!harness.dump("meta").contains(AUDIT_KEY));
    assert!(!harness.dump("audit_log").contains(AUDIT_KEY));
}

#[tokio::test]
async fn a_truncated_log_with_a_repaired_head_fails_with_a_key() {
    let (harness, _auth, _cookie, _secrets) = keyed_activity().await;
    let rows = keyed_verify(&harness).rows;
    truncate_and_fix_the_head(&harness, rows - 4);
    let report = keyed_verify(&harness);
    assert!(!report.ok, "{report:?}");
    assert!(report.problem.unwrap().contains("signature"));
}

#[tokio::test]
async fn rows_rewritten_with_the_chain_recomputed_fail_with_a_key() {
    let (harness, _auth, _cookie, _secrets) = keyed_activity().await;
    let good = keyed_verify(&harness).rows;
    drop_triggers(&harness);
    // Everything after the cut is thrown away and a forged row takes its place,
    // appended the way the service would but without the key.
    harness
        .app
        .store
        .with(|conn| {
            conn.execute_batch(&format!("DELETE FROM audit_log WHERE seq > {};", good - 3))?;
            let (seq, hash): (i64, String) =
                conn.query_row("SELECT seq, hash FROM audit_log ORDER BY seq DESC LIMIT 1", [], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?;
            conn.execute("UPDATE meta SET value = ?1 WHERE key = 'audit_head'", [format!("{seq}:{hash}")])?;
            audit::append(conn, &forged_event(), None)
        })
        .unwrap();
    restore_triggers(&harness);
    let report = keyed_verify(&harness);
    assert!(!report.ok, "{report:?}");
    assert!(report.problem.unwrap().contains("signature"));
}

fn forged_event() -> audit::Event {
    audit::Event {
        ts: 1,
        action: "vm.start".to_string(),
        target: "vm100".to_string(),
        params: json!({}),
        credential_ref: "forged".to_string(),
        session_ref: "forged".to_string(),
        user_ref: "forged".to_string(),
        device: "Forged".to_string(),
        channel: "panel".to_string(),
        step_up: "confirm".to_string(),
        result: "confirmed".to_string(),
        rtt_ms: None,
        run_id: None,
    }
}

#[tokio::test]
async fn the_service_does_not_sign_over_a_log_that_was_tampered_with() {
    let (harness, _auth, cookie, _secrets) = keyed_activity().await;
    let rows = keyed_verify(&harness).rows;
    truncate_and_fix_the_head(&harness, rows - 2);
    harness.execute(&cookie, "light.power", "bulb2", json!({ "on": false })).await.assert_ok();
    harness.execute(&cookie, "light.power", "bulb3", json!({ "on": true })).await.assert_ok();
    let report = keyed_verify(&harness);
    assert!(!report.ok, "new rows made a tampered log verify: {report:?}");
}

#[tokio::test]
async fn removing_the_signature_does_not_help() {
    let (harness, _auth, _cookie, _secrets) = keyed_activity().await;
    harness.app.store.with(|conn| conn.execute("DELETE FROM meta WHERE key = 'audit_head_signature'", [])).unwrap();
    assert!(!keyed_verify(&harness).ok);
}

#[tokio::test]
async fn a_short_or_missing_audit_key_stops_the_service() {
    use std::sync::Arc;
    use swifty_hub::app::App;
    use swifty_hub::clock::ManualClock;
    use swifty_plugin_mock::MockDevices;

    let dir = tempfile::tempdir().unwrap();
    let mut config = test_config(&dir);
    config.audit.key_file = Some(common::devices::write_secret(dir.path(), "short.key", "too short"));
    let clock = Arc::new(ManualClock::new(START_MS));
    assert!(App::new(config.clone(), clock.clone(), mock_registry(true, &MockDevices::new())).is_err());
    config.audit.key_file = Some(dir.path().join("absent.key"));
    assert!(App::new(config, clock, mock_registry(true, &MockDevices::new())).is_err());
}

#[tokio::test]
async fn a_checkpoint_catches_a_truncation_the_chain_cannot() {
    let (harness, _auth, _cookie) = with_activity().await;
    let before = verify(&harness);
    assert!(before.ok);
    let checkpoint: Checkpoint = before.checkpoint.parse().unwrap();
    let options = VerifyOptions { expected: Some(&checkpoint), ..Default::default() };
    assert!(harness.app.store.with(|conn| audit::verify_with(conn, options)).unwrap().ok);

    truncate_and_fix_the_head(&harness, before.rows - 4);
    assert!(verify(&harness).ok, "without a checkpoint or a key this cannot be seen");
    let report = harness.app.store.with(|conn| audit::verify_with(conn, options)).unwrap();
    assert!(!report.ok);
    assert_eq!(report.first_bad_seq, Some(before.rows));
    assert!(report.problem.unwrap().contains("removed"));
}

#[tokio::test]
async fn a_checkpoint_catches_a_chain_rebuilt_from_an_earlier_row() {
    let (harness, _auth, _cookie) = with_activity().await;
    let before = verify(&harness);
    let checkpoint: Checkpoint = before.checkpoint.parse().unwrap();
    let keep = before.rows - 2;
    truncate_and_fix_the_head(&harness, keep);
    harness.app.store.with(|conn| audit::append(conn, &forged_event(), None)).unwrap();
    harness.app.store.with(|conn| audit::append(conn, &forged_event(), None)).unwrap();
    harness.app.store.with(|conn| audit::append(conn, &forged_event(), None)).unwrap();
    assert!(verify(&harness).ok, "without a checkpoint or a key this cannot be seen");

    let options = VerifyOptions { expected: Some(&checkpoint), ..Default::default() };
    let report = harness.app.store.with(|conn| audit::verify_with(conn, options)).unwrap();
    assert!(!report.ok);
    assert_eq!(report.first_bad_seq, Some(checkpoint.seq));
    assert!(report.problem.unwrap().contains("checkpoint"));
}

#[test]
fn checkpoints_are_parsed_strictly() {
    let hash = "a".repeat(64);
    let parsed: Checkpoint = format!("12:{hash}").parse().unwrap();
    assert_eq!(parsed, Checkpoint { seq: 12, hash: hash.clone() });
    assert_eq!(format!("12:{}", hash.to_uppercase()).parse::<Checkpoint>().unwrap().hash, hash);
    for bad in [
        "",
        "12",
        "x:y",
        "0:aaa",
        &format!("0:{hash}"),
        &format!("-1:{hash}"),
        &format!("12:{}", "a".repeat(63)),
        &format!("12:{}", "g".repeat(64)),
    ] {
        assert!(bad.parse::<Checkpoint>().is_err(), "{bad}");
    }
}

#[tokio::test]
async fn each_row_has_every_required_field() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    harness.advance(5);
    harness.critical(&cookie, &mut auth, "vm.restart", "vm103", json!({})).await.assert_ok();
    let log = harness.get("/api/v1/audit", &cookie).await;
    log.assert_ok();
    let rows = log.body["rows"].as_array().unwrap();
    let row = rows.iter().find(|r| r["result"] == json!("confirmed")).unwrap();
    assert_eq!(row["ts"], json!(harness.now()));
    assert_eq!(row["action"], json!("vm.restart"));
    assert_eq!(row["target"], json!("vm103"));
    assert_eq!(row["params"], json!({}));
    assert!(row["credential_ref"].as_str().unwrap().len() > 8);
    assert!(row["session_ref"].as_str().unwrap().len() > 8);
    assert_eq!(row["device"], json!("iPhone"));
    assert_eq!(row["channel"], json!("panel"));
    assert_eq!(row["step_up"], json!("passkey"));
    assert!(row["rtt_ms"].is_i64());
    assert!(row["run_id"].is_string());
    assert_eq!(row["hash"].as_str().unwrap().len(), 64);
    assert_eq!(row["prev_hash"].as_str().unwrap().len(), 64);
}

#[tokio::test]
async fn the_log_pages_newest_first() {
    let (harness, _auth, cookie) = with_activity().await;
    let first = harness.get("/api/v1/audit?limit=2", &cookie).await;
    let rows = first.body["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows[0]["seq"].as_i64() > rows[1]["seq"].as_i64());
    let before = first.body["next_before"].as_i64().unwrap();
    let next = harness.get(&format!("/api/v1/audit?limit=2&before={before}"), &cookie).await;
    assert!(next.body["rows"][0]["seq"].as_i64().unwrap() < before);
}

#[tokio::test]
async fn updates_and_deletes_are_refused_by_the_database() {
    let (harness, _auth, _cookie) = with_activity().await;
    let update = harness.app.store.with(|conn| conn.execute("UPDATE audit_log SET result = 'x'", []));
    assert!(update.unwrap_err().to_string().contains("append-only"));
    let delete = harness.app.store.with(|conn| conn.execute("DELETE FROM audit_log", []));
    assert!(delete.unwrap_err().to_string().contains("append-only"));
    assert!(verify(&harness).ok);
}

#[tokio::test]
async fn a_changed_field_is_detected() {
    for (column, value) in [
        ("result", "'confirmed'"),
        ("action", "'pets.feed'"),
        ("target", "'vm999'"),
        ("params", "'{\"x\":1}'"),
        ("credential_ref", "'someone-else'"),
        ("session_ref", "'other'"),
        ("user_ref", "'someone-else'"),
        ("device", "'Forged'"),
        ("channel", "'voice'"),
        ("step_up", "'passkey'"),
        ("ts", "1"),
        ("rtt_ms", "1"),
        ("run_id", "'x'"),
    ] {
        let (harness, _auth, _cookie) = with_activity().await;
        tamper(&harness, &format!("UPDATE audit_log SET {column} = {value} WHERE seq = 3"));
        let report = verify(&harness);
        assert!(!report.ok, "changing {column} went unnoticed");
        assert_eq!(report.first_bad_seq, Some(3), "{column}");
    }
}

#[tokio::test]
async fn a_recomputed_hash_breaks_the_next_link() {
    let (harness, _auth, _cookie) = with_activity().await;
    tamper(
        &harness,
        "UPDATE audit_log SET hash = 'f000000000000000000000000000000000000000000000000000000000000000' WHERE seq = 2",
    );
    let report = verify(&harness);
    assert!(!report.ok);
    assert_eq!(report.first_bad_seq, Some(2));
}

#[tokio::test]
async fn a_deleted_row_is_detected() {
    let (harness, _auth, _cookie) = with_activity().await;
    tamper(&harness, "DELETE FROM audit_log WHERE seq = 3");
    let report = verify(&harness);
    assert!(!report.ok);
    assert_eq!(report.first_bad_seq, Some(3));
    assert!(report.problem.unwrap().contains("missing"));
}

#[tokio::test]
async fn cutting_rows_off_the_end_is_detected() {
    let (harness, _auth, _cookie) = with_activity().await;
    let before = verify(&harness).rows;
    tamper(&harness, &format!("DELETE FROM audit_log WHERE seq = {before}"));
    let report = verify(&harness);
    assert!(!report.ok);
    assert!(report.problem.unwrap().contains("removed from the end"));
}

#[tokio::test]
async fn an_inserted_row_is_detected() {
    let (harness, _auth, _cookie) = with_activity().await;
    let total = verify(&harness).rows;
    tamper(
        &harness,
        &format!(
            "INSERT INTO audit_log (seq, ts, action, target, params, credential_ref, session_ref, device, channel, step_up, result, prev_hash, hash)
             VALUES ({}, 1, 'a', 'b', 'null', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j')",
            total + 1
        ),
    );
    assert!(!verify(&harness).ok);
}

#[tokio::test]
async fn the_verify_endpoint_reports_tampering() {
    let (harness, _auth, cookie) = with_activity().await;
    tamper(&harness, "UPDATE audit_log SET result = 'confirmed' WHERE seq = 1");
    let http = harness.get("/api/v1/audit/verify", &cookie).await;
    http.assert_ok();
    assert_eq!(http.body["ok"], json!(false));
    assert_eq!(http.body["first_bad_seq"], json!(1));
}

#[tokio::test]
async fn refused_attempts_are_recorded() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    harness
        .execute(&cookie, "vm.stop", "vm101", json!({}))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "step_up_required");
    harness
        .execute(&cookie, "speaker.volume", "sonos", json!({ "level": 500 }))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_params");
    assert!(harness.audit_has("refused: step_up_required"));
    assert!(harness.audit_has("refused: invalid_params"));
    // An invalid value is not copied in.
    let log = harness.get("/api/v1/audit", &cookie).await;
    let row = log.body["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["result"] == json!("refused: invalid_params"))
        .unwrap()
        .clone();
    assert!(row["params"].is_null());
}

#[tokio::test]
async fn anonymous_refusals_cannot_flood_the_log() {
    use swifty_hub::app::MAX_ANONYMOUS_REFUSALS_PER_MINUTE as CAP;
    let harness = Harness::new();
    let attempt = json!({ "token": "guess", "name": "x" });
    for _ in 0..CAP + 20 {
        harness
            .post_public("/api/v1/auth/enroll/start", attempt.clone())
            .await
            .assert_refused(StatusCode::FORBIDDEN, "invalid_enrollment_token");
    }
    assert_eq!(harness.audit_count("refused: invalid_enrollment_token"), CAP as usize);
    harness.advance(60_001);
    harness.post_public("/api/v1/auth/enroll/start", attempt).await;
    assert_eq!(harness.audit_count("refused: invalid_enrollment_token"), CAP as usize + 1);
    assert!(verify(&harness).ok);
}

#[tokio::test]
async fn refusals_the_cap_keeps_out_are_counted_in_one_row_a_minute() {
    use swifty_hub::app::{DROPPED_REFUSALS_ACTION, MAX_ANONYMOUS_REFUSALS_PER_MINUTE as CAP};
    let harness = Harness::new();
    let attempt = json!({ "token": "guess", "name": "x" });
    for _ in 0..CAP + 26 {
        harness.post_public("/api/v1/auth/enroll/start", attempt.clone()).await;
    }
    assert_eq!(harness.audit_results().len(), CAP as usize, "the count waits until the minute is over");

    harness.advance(MINUTE_MS + 1);
    harness.post_public("/api/v1/auth/enroll/start", attempt.clone()).await;
    assert_eq!(harness.audit_count("dropped: 26"), 1, "{:?}", harness.audit_results());
    let log = harness.dump("audit_log");
    assert!(log.contains(DROPPED_REFUSALS_ACTION));
    assert!(log.contains("\"dropped\":26"), "{log}");

    // The count was written and cleared, so it is not written again.
    harness.advance(MINUTE_MS + 1);
    harness.post_public("/api/v1/auth/enroll/start", attempt).await;
    assert_eq!(harness.audit_results().iter().filter(|r| r.starts_with("dropped")).count(), 1);
    assert!(verify(&harness).ok);
}

#[tokio::test]
async fn a_dropped_count_appears_when_the_log_is_read_without_another_refusal() {
    use swifty_hub::app::MAX_ANONYMOUS_REFUSALS_PER_MINUTE as CAP;
    let (harness, _auth, cookie) = Harness::signed_in().await;
    harness.advance(MINUTE_MS + 1);
    for _ in 0..CAP + 3 {
        harness.post_public("/api/v1/auth/enroll/start", json!({ "token": "guess", "name": "x" })).await;
    }
    harness.advance(MINUTE_MS + 1);
    let log = harness.get("/api/v1/audit", &cookie).await;
    log.assert_ok();
    assert!(log.body["rows"].as_array().unwrap().iter().any(|r| r["result"] == json!("dropped: 3")), "{}", log.text);
}

#[tokio::test]
async fn a_restart_writes_a_dropped_count_that_was_still_waiting() {
    use std::sync::Arc;
    use swifty_hub::app::{App, MAX_ANONYMOUS_REFUSALS_PER_MINUTE as CAP};

    let harness = Harness::new();
    for _ in 0..CAP + 2 {
        harness.post_public("/api/v1/auth/enroll/start", json!({ "token": "guess", "name": "x" })).await;
    }
    harness.advance(MINUTE_MS + 1);
    App::new(harness.app.config.clone(), harness.clock.clone() as Arc<_>, mock_registry(true, &harness.devices))
        .unwrap();
    assert!(harness.audit_has("dropped: 2"));
}

#[tokio::test]
async fn no_secret_reaches_the_log() {
    let harness = Harness::new();
    let mut auth = authenticator();
    let token = harness.enroll_token();
    harness.enroll_with(&mut auth, &token, "iPhone").await.assert_ok();

    let started = harness.login_start().await;
    let assertion = assert_with(&mut auth, &started.body["options"], origin_url());
    let signature = serde_json::to_value(&assertion).unwrap()["response"]["signature"].as_str().unwrap().to_string();
    let done = harness.login_submit(&started.body["ceremony_id"], &assertion, None).await;
    let cookie = done.session_cookie();

    let prepared = harness.prepare(&cookie, "pets.feed", "feeder", json!({ "grams": 10 })).await;
    let confirmation = prepared.body["confirmation"].as_str().unwrap().to_string();
    harness.confirm_tap(&cookie, &prepared).await.assert_ok();

    let log = harness.dump("audit_log");
    let session_token = cookie.split_once('=').unwrap().1;
    for secret in [token.as_str(), session_token, confirmation.as_str(), signature.as_str()] {
        assert!(!log.contains(secret), "{secret} reached the audit log");
    }
}

fn write_config(harness: &Harness) -> std::path::PathBuf {
    let path = harness.dir.path().join("config.toml");
    let text = config_text(&harness.app.config.database_path(), "");
    std::fs::write(&path, text).unwrap();
    path
}

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_swifty"))
}

#[tokio::test]
async fn the_audit_verify_command_passes_and_fails_correctly() {
    let (harness, _auth, _cookie) = with_activity().await;
    let config = write_config(&harness);

    let output = binary().arg("--config").arg(&config).arg("audit-verify").output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], json!(true));

    tamper(&harness, "UPDATE audit_log SET device = 'Forged' WHERE seq = 2");
    let output = binary().arg("--config").arg(&config).arg("audit-verify").output().unwrap();
    assert!(!output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], json!(false));
    assert_eq!(report["first_bad_seq"], json!(2));
}

#[tokio::test]
async fn the_enroll_token_command_prints_a_working_token_once() {
    let harness = Harness::new();
    let config = write_config(&harness);
    let output = binary().arg("--config").arg(&config).arg("enroll-token").output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let token = String::from_utf8(output.stdout).unwrap().trim().to_string();
    assert_eq!(token.len(), 43);
    assert!(!harness.dump("enroll_tokens").contains(&token));
    // Standard output is the token alone, for a script. Standard error has the link that opens the
    // app on the enrollment form with it, as `swifty init` prints, and where to paste it otherwise.
    let said = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(said.contains(&format!("{ORIGIN}/#enroll={token}")), "{said}");
    assert!(said.contains("paste the token"), "{said}");

    // The token created by the command really opens enrollment. Time here is
    // the manual clock, so it is moved to the wall clock used by the command.
    let wall = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    harness.advance(wall - harness.now());
    let mut auth = authenticator();
    harness.enroll_with(&mut auth, &token, "iPhone").await.assert_ok();

    // And it refuses once a passkey exists.
    let output = binary().arg("--config").arg(&config).arg("enroll-token").output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

#[tokio::test]
async fn audit_verify_refuses_a_database_that_does_not_exist() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.db");
    let config = dir.path().join("config.toml");
    std::fs::write(&config, config_text(&missing, "")).unwrap();
    let output = binary().arg("--config").arg(&config).arg("audit-verify").output().unwrap();
    assert!(!output.status.success());
    assert!(!missing.exists(), "checking must not create an empty database");
}

#[tokio::test]
async fn a_bad_config_stops_the_binary() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.toml");
    std::fs::write(&path, "database = 'x.db'\nsecret = 'inline'\n").unwrap();
    let output = binary().arg("--config").arg(&path).arg("audit-verify").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid configuration"));
}

#[tokio::test]
async fn a_secret_pasted_into_the_config_is_not_printed() {
    const PASTED: &str = "swifty@pve!home=PASTED-PROXMOX-SECRET";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pasted.toml");
    std::fs::write(
        &path,
        format!(
            "config_version = 2\ndatabase = 'x.db'\n[network]\nconnect_timeout_ms = 1500\nread_timeout_ms = 2500\ntoken = \"{PASTED}\"\n\
             [webauthn]\norigin = \"https://home.example.org\"\nrp_id = \"home.example.org\"\nrp_name = \"Swifty Home\"\n"
        ),
    )
    .unwrap();
    let output = binary().arg("--config").arg(&path).arg("serve").output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stderr.contains("line 6"), "{stderr}");
    assert!(stderr.contains("token"), "{stderr}");
    assert!(!stderr.contains("PASTED"), "{stderr}");
    assert!(!stdout.contains("PASTED"), "{stdout}");
}

fn write_keyed_config(harness: &Harness, key: &std::path::Path) -> std::path::PathBuf {
    let path = harness.dir.path().join("keyed.toml");
    let text = config_text(&harness.app.config.database_path(), &format!("[audit]\nkey_file = '{}'", key.display()));
    std::fs::write(&path, text).unwrap();
    path
}

fn run_verify(config: &std::path::Path, extra: &[&str]) -> (bool, serde_json::Value, String) {
    let output = binary().arg("--config").arg(config).arg("audit-verify").args(extra).output().unwrap();
    let report = serde_json::from_slice(&output.stdout).unwrap_or(serde_json::Value::Null);
    (output.status.success(), report, String::from_utf8_lossy(&output.stderr).to_string())
}

#[tokio::test]
async fn the_verify_command_checks_an_exported_checkpoint() {
    let (harness, _auth, _cookie) = with_activity().await;
    let config = write_config(&harness);
    let (ok, report, _) = run_verify(&config, &[]);
    assert!(ok);
    let checkpoint = report["checkpoint"].as_str().unwrap().to_string();

    let (ok, report, _) = run_verify(&config, &["--expect-head", &checkpoint]);
    assert!(ok, "{report}");

    let rows = report["rows"].as_i64().unwrap();
    truncate_and_fix_the_head(&harness, rows - 3);
    assert!(run_verify(&config, &[]).0, "only the checkpoint can see this without a key");
    let (ok, report, _) = run_verify(&config, &["--expect-head", &checkpoint]);
    assert!(!ok);
    assert_eq!(report["ok"], json!(false));
    assert!(report["problem"].as_str().unwrap().contains("removed"), "{report}");

    let (ok, _, stderr) = run_verify(&config, &["--expect-head", "not-a-checkpoint"]);
    assert!(!ok);
    assert!(stderr.contains("seq:hash"), "{stderr}");
}

#[tokio::test]
async fn the_verify_command_uses_the_audit_key() {
    let (harness, _auth, _cookie, secrets) = keyed_activity().await;
    let key = secrets.path().join("audit.key");
    let config = write_keyed_config(&harness, &key);
    let (ok, report, _) = run_verify(&config, &[]);
    assert!(ok, "{report}");
    assert_eq!(report["keyed"], json!(true));

    let rows = report["rows"].as_i64().unwrap();
    truncate_and_fix_the_head(&harness, rows - 3);
    let (ok, report, _) = run_verify(&config, &[]);
    assert!(!ok, "{report}");
    assert!(report["problem"].as_str().unwrap().contains("signature"), "{report}");
}

#[tokio::test]
async fn the_verify_command_refuses_a_zero_byte_database_and_leaves_it_alone() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("home.db");
    std::fs::write(&database, b"").unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(&config, config_text(&database, "")).unwrap();

    for extra in [&[][..], &["--allow-empty"][..]] {
        let (ok, report, stderr) = run_verify(&config, extra);
        assert!(!ok, "{report}");
        assert!(report.is_null(), "{report}");
        assert!(stderr.contains("not a database this service has run on"), "{stderr}");
        assert_eq!(std::fs::metadata(&database).unwrap().len(), 0, "the check wrote to the file");
    }
}

#[tokio::test]
async fn the_verify_command_refuses_a_file_that_is_not_a_database() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("home.db");
    std::fs::write(&database, b"this is not a database, and it is long enough to read a header from").unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(&config, config_text(&database, "")).unwrap();

    assert!(!run_verify(&config, &[]).0);
}

#[tokio::test]
async fn the_verify_command_refuses_a_database_with_no_owner() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("home.db");
    swifty_hub::store::Store::open(&database).unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(&config, config_text(&database, "")).unwrap();

    let (ok, _, stderr) = run_verify(&config, &[]);
    assert!(!ok);
    assert!(stderr.contains("not a database this service has run on"), "{stderr}");
}

#[tokio::test]
async fn the_verify_command_treats_an_empty_log_as_unverified_unless_told_otherwise() {
    let harness = Harness::new();
    let config = write_config(&harness);
    let (ok, report, _) = run_verify(&config, &[]);
    assert!(!ok);
    assert_eq!(report["empty"], json!(true));
    assert_eq!(report["ok"], json!(false));

    let (ok, report, _) = run_verify(&config, &["--allow-empty"]);
    assert!(ok, "{report}");
    assert_eq!(report["empty"], json!(true));
}

#[tokio::test]
async fn the_verify_command_changes_nothing_in_the_database() {
    let (harness, _auth, _cookie) = with_activity().await;
    let config = write_config(&harness);
    let before = harness.dump("audit_log") + &harness.dump("meta");
    assert!(run_verify(&config, &[]).0);
    assert_eq!(harness.dump("audit_log") + &harness.dump("meta"), before);
}

/// The old database is checked before `swifty migrate` copies it, so the command
/// reads one an earlier version left as it is, and leaves it at its version.
#[tokio::test]
async fn the_verify_command_checks_a_database_an_earlier_version_left() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("home.db");
    swifty_hub::store::create_version(&database, 3).unwrap();
    let expected = {
        let conn = rusqlite::Connection::open(&database).unwrap();
        conn.execute("INSERT INTO meta (key, value) VALUES ('owner_id', '11111111-2222-3333-4444-555555555555')", [])
            .unwrap();
        for n in 0..3 {
            audit::append(&conn, &appended(n, 0), None).unwrap();
        }
        audit::verify(&conn).unwrap()
    };
    let config = dir.path().join("config.toml");
    std::fs::write(&config, config_text(&database, "")).unwrap();

    let (ok, report, stderr) = run_verify(&config, &["--expect-head", &expected.checkpoint]);
    assert!(ok, "{report} {stderr}");
    assert_eq!(report["checkpoint"], json!(expected.checkpoint));
    let version: i64 =
        rusqlite::Connection::open(&database).unwrap().query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
    assert_eq!(version, 3, "checking upgraded the file");
}

// ----- two processes writing one log

/// Set in a child process of the test below to the database it appends to.
const CHILD_DATABASE: &str = "SWIFTY_TEST_AUDIT_DATABASE";
/// Which child it is, and the wall-clock millisecond both start at.
const CHILD_NUMBER: &str = "SWIFTY_TEST_AUDIT_CHILD";
const CHILD_START: &str = "SWIFTY_TEST_AUDIT_START";
const CHILD_ROWS: usize = 150;

fn appended(n: usize, child: usize) -> audit::Event {
    audit::Event {
        ts: n as i64,
        action: "test.append".to_string(),
        target: "-".to_string(),
        params: json!({ "n": n }),
        credential_ref: format!("process-{child}"),
        session_ref: "-".to_string(),
        user_ref: "-".to_string(),
        device: "-".to_string(),
        channel: "panel".to_string(),
        step_up: "none".to_string(),
        result: "done".to_string(),
        rtt_ms: None,
        run_id: None,
    }
}

fn wall_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()
}

/// What each child process does: wait for the agreed moment, then append, half the
/// rows on their own and half inside a transaction that reads first, as the CLI
/// and the hub do.
fn append_as_child(database: &str) {
    let child: usize = std::env::var(CHILD_NUMBER).unwrap().parse().unwrap();
    let start: u128 = std::env::var(CHILD_START).unwrap().parse().unwrap();
    let store = swifty_hub::store::Store::open(std::path::Path::new(database)).unwrap();
    while wall_ms() < start {
        std::thread::yield_now();
    }
    for n in 0..CHILD_ROWS {
        if (n + child).is_multiple_of(2) {
            store.with(|conn| audit::append(conn, &appended(n, child), None)).unwrap();
        } else {
            store
                .tx(|tx| {
                    tx.query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get::<_, i64>(0))?;
                    audit::append(tx, &appended(n, child), None)
                })
                .unwrap();
        }
    }
}

/// The hub and the CLI append to the same log. This test runs itself twice more as
/// separate processes that append at the same moment.
#[test]
fn two_processes_appending_audit_rows_at_once_both_succeed() {
    if let Ok(database) = std::env::var(CHILD_DATABASE) {
        return append_as_child(&database);
    }
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("home.db");
    let store = swifty_hub::store::Store::open(&database).unwrap();
    store.with(|conn| audit::append(conn, &appended(0, 9), None)).unwrap();
    drop(store);

    let start = wall_ms() + 1_500;
    let children: Vec<_> = (0..2)
        .map(|child| {
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "two_processes_appending_audit_rows_at_once_both_succeed", "--test-threads=1"])
                .env(CHILD_DATABASE, &database)
                .env(CHILD_NUMBER, child.to_string())
                .env(CHILD_START, start.to_string())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for (child, output) in children.into_iter().enumerate() {
        let output = output.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "process {child} failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"), "process {child} ran nothing");
    }

    let store = swifty_hub::store::Store::open(&database).unwrap();
    let report = store.with(audit::verify).unwrap();
    assert!(report.ok, "{report:?}");
    assert_eq!(report.rows, 1 + 2 * CHILD_ROWS as i64);
    for child in 0..2 {
        let rows: i64 = store
            .with(|conn| {
                conn.query_row(
                    "SELECT COUNT(DISTINCT params) FROM audit_log WHERE credential_ref = ?1",
                    [format!("process-{child}")],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(rows, CHILD_ROWS as i64, "every row of process {child} is there once");
    }
}
