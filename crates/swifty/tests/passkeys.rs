//! Passkey ceremonies, signed by a software authenticator.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use swifty_hub::passkeys::{Passkeys, ENROLL_CEREMONY_TTL_MS, ENROLL_TOKEN_TTL_MS, MAX_OPEN_LOGINS};

#[tokio::test]
async fn first_enrollment_then_sign_in() {
    let harness = Harness::new();
    let mut auth = authenticator();
    let id = harness.enroll(&mut auth, "iPhone").await;

    let cookie = harness.login(&mut auth).await;
    let session = harness.get("/api/v1/session", &cookie).await;
    session.assert_ok();
    assert_eq!(session.body["session"]["credential_id"], json!(id));
    assert_eq!(session.body["session"]["device"], json!("iPhone"));
    assert!(harness.audit_has("enrolled"));
    assert!(harness.audit_has("signed_in"));
}

#[tokio::test]
async fn enrollment_needs_a_valid_token() {
    let harness = Harness::new();
    let refused = harness.post_public("/api/v1/auth/enroll/start", json!({ "token": "guess", "name": "x" })).await;
    refused.assert_refused(StatusCode::FORBIDDEN, "invalid_enrollment_token");
    assert!(harness.audit_has("refused: invalid_enrollment_token"));
}

#[tokio::test]
async fn enrollment_token_is_single_use() {
    let harness = Harness::new();
    let token = harness.enroll_token();
    let first = harness.post_public("/api/v1/auth/enroll/start", json!({ "token": token, "name": "x" })).await;
    first.assert_ok();
    let second = harness.post_public("/api/v1/auth/enroll/start", json!({ "token": token, "name": "x" })).await;
    second.assert_refused(StatusCode::FORBIDDEN, "invalid_enrollment_token");
}

#[tokio::test]
async fn enrollment_token_expires_after_fifteen_minutes() {
    assert_eq!(ENROLL_TOKEN_TTL_MS, 15 * MINUTE_MS);
    let harness = Harness::new();
    let token = harness.enroll_token();
    harness.advance(15 * MINUTE_MS);
    let late = harness.post_public("/api/v1/auth/enroll/start", json!({ "token": token, "name": "x" })).await;
    late.assert_refused(StatusCode::FORBIDDEN, "invalid_enrollment_token");

    let fresh = harness.enroll_token();
    harness.advance(15 * MINUTE_MS - 1);
    harness.post_public("/api/v1/auth/enroll/start", json!({ "token": fresh, "name": "x" })).await.assert_ok();
}

#[tokio::test]
async fn enrollment_token_is_stored_hashed() {
    let harness = Harness::new();
    let token = harness.enroll_token();
    let stored = harness.dump("enroll_tokens");
    assert!(!stored.contains(&token));
    assert!(stored.contains(&swifty_hub::crypto::sha256_hex(token.as_bytes())));
}

#[tokio::test]
async fn enrollment_is_only_for_the_first_passkey() {
    let harness = Harness::new();
    let mut auth = authenticator();
    let token = harness.enroll_token();
    // A second token issued before the first is used replaces it.
    let current = harness.enroll_token();
    let stale = harness.post_public("/api/v1/auth/enroll/start", json!({ "token": token, "name": "x" })).await;
    stale.assert_refused(StatusCode::FORBIDDEN, "invalid_enrollment_token");
    harness.enroll_with(&mut auth, &current, "First").await.assert_ok();

    // The local command refuses once a passkey exists.
    let error = Passkeys::issue_enroll_token(&harness.app.store, harness.now()).unwrap_err();
    assert_eq!(error.code, "already_enrolled");

    // A token that somehow exists does not help either.
    let refused = harness.post_public("/api/v1/auth/enroll/start", json!({ "token": current, "name": "x" })).await;
    refused.assert_refused(StatusCode::FORBIDDEN, "already_enrolled");
}

#[tokio::test]
async fn enrollment_finish_replay_is_refused() {
    let harness = Harness::new();
    let mut auth = authenticator();
    let token = harness.enroll_token();
    let started = harness.post_public("/api/v1/auth/enroll/start", json!({ "token": token, "name": "First" })).await;
    let credential = register(&mut auth, &started.body["options"], origin_url());
    let body = json!({ "ceremony_id": started.body["ceremony_id"], "credential": credential });
    harness.post_public("/api/v1/auth/enroll/finish", body.clone()).await.assert_ok();
    let replay = harness.post_public("/api/v1/auth/enroll/finish", body).await;
    replay.assert_refused(StatusCode::CONFLICT, "challenge_used");
}

#[tokio::test]
async fn registration_from_the_wrong_origin_is_refused() {
    use webauthn_authenticator_rs::AuthenticatorBackend;
    use webauthn_rs::prelude::CreationChallengeResponse;

    let harness = Harness::new();
    let mut auth = authenticator();
    let token = harness.enroll_token();
    let started = harness.post_public("/api/v1/auth/enroll/start", json!({ "token": token, "name": "x" })).await;
    let options: CreationChallengeResponse = serde_json::from_value(started.body["options"].clone()).unwrap();
    let credential = AuthenticatorBackend::perform_register(&mut auth, evil_url(), options.public_key, 60_000).unwrap();
    let refused = harness
        .post_public(
            "/api/v1/auth/enroll/finish",
            json!({ "ceremony_id": started.body["ceremony_id"], "credential": credential }),
        )
        .await;
    refused.assert_refused(StatusCode::UNAUTHORIZED, "assertion_rejected");
    let list = harness.app.store.with(swifty_hub::passkeys::list_credentials).unwrap();
    assert!(list.is_empty());
}

#[tokio::test]
async fn login_requires_a_passkey_to_exist() {
    let harness = Harness::new();
    let refused = harness.post_public("/api/v1/auth/login/start", json!({})).await;
    refused.assert_refused(StatusCode::CONFLICT, "no_credentials");
}

#[tokio::test]
async fn login_from_the_wrong_origin_is_refused() {
    let harness = Harness::new();
    let mut auth = authenticator();
    harness.enroll(&mut auth, "iPhone").await;
    let started = harness.login_start().await;
    let assertion = assert_from_wrong_origin(&mut auth, &started.body["options"]);
    let refused = harness.login_submit(&started.body["ceremony_id"], &assertion, None).await;
    refused.assert_refused(StatusCode::UNAUTHORIZED, "assertion_rejected");
    assert!(refused.headers.get("set-cookie").is_none());
}

#[tokio::test]
async fn login_without_user_verification_is_refused() {
    let harness = Harness::new();
    let mut auth = authenticator();
    harness.enroll(&mut auth, "iPhone").await;
    let started = harness.login_start().await;
    let assertion = assert_without_uv(&mut auth, &started.body["options"]);
    let refused = harness.login_submit(&started.body["ceremony_id"], &assertion, None).await;
    refused.assert_refused(StatusCode::UNAUTHORIZED, "assertion_rejected");
}

#[tokio::test]
async fn login_assertion_cannot_be_replayed() {
    let harness = Harness::new();
    let mut auth = authenticator();
    harness.enroll(&mut auth, "iPhone").await;
    let started = harness.login_start().await;
    let assertion = assert_with(&mut auth, &started.body["options"], origin_url());
    harness.login_submit(&started.body["ceremony_id"], &assertion, None).await.assert_ok();
    let replay = harness.login_submit(&started.body["ceremony_id"], &assertion, None).await;
    replay.assert_refused(StatusCode::CONFLICT, "challenge_used");
}

#[tokio::test]
async fn login_assertion_for_another_challenge_is_refused() {
    let harness = Harness::new();
    let mut auth = authenticator();
    harness.enroll(&mut auth, "iPhone").await;
    let first = harness.login_start().await;
    let second = harness.login_start().await;
    let assertion = assert_with(&mut auth, &first.body["options"], origin_url());
    let refused = harness.login_submit(&second.body["ceremony_id"], &assertion, None).await;
    refused.assert_refused(StatusCode::UNAUTHORIZED, "assertion_rejected");
}

#[tokio::test]
async fn login_challenge_expires() {
    let harness = Harness::new();
    let mut auth = authenticator();
    harness.enroll(&mut auth, "iPhone").await;
    let started = harness.login_start().await;
    let assertion = assert_with(&mut auth, &started.body["options"], origin_url());
    harness.advance(120_000);
    let late = harness.login_submit(&started.body["ceremony_id"], &assertion, None).await;
    late.assert_refused(StatusCode::GONE, "challenge_expired");
}

#[tokio::test]
async fn signature_counter_is_stored_and_a_regression_is_refused() {
    let harness = Harness::new();
    let mut auth = authenticator();
    harness.enroll(&mut auth, "YubiKey").await;

    // Two ceremonies opened together, signed in order, submitted out of order:
    // the second signature carries a lower counter than the one already seen.
    let first = harness.login_start().await;
    let second = harness.login_start().await;
    let older = assert_with(&mut auth, &first.body["options"], origin_url());
    let newer = assert_with(&mut auth, &second.body["options"], origin_url());

    harness.login_submit(&second.body["ceremony_id"], &newer, None).await.assert_ok();
    let stored: i64 = harness
        .app
        .store
        .with(|conn| conn.query_row("SELECT sign_count FROM credentials", [], |row| row.get(0)))
        .unwrap();
    assert_eq!(stored, 2);

    let refused = harness.login_submit(&first.body["ceremony_id"], &older, None).await;
    refused.assert_refused(StatusCode::UNAUTHORIZED, "credential_counter_regressed");
    assert!(harness.audit_has("refused: credential_counter_regressed"));
}

fn credential_count(harness: &Harness) -> usize {
    harness.app.store.with(swifty_hub::passkeys::list_credentials).unwrap().len()
}

// ----- a flood of sign-ins cannot lock the owner out

#[tokio::test]
async fn opening_more_sign_ins_than_the_cap_drops_the_oldest_and_refuses_nobody() {
    let harness = Harness::new();
    let mut auth = authenticator();
    harness.enroll(&mut auth, "iPhone").await;

    let oldest = harness.login_start().await;
    for _ in 0..MAX_OPEN_LOGINS {
        harness.login_start().await;
    }
    let open: i64 = harness
        .app
        .store
        .with(|conn| {
            conn.query_row("SELECT COUNT(*) FROM ceremonies WHERE kind = 'login' AND used_at IS NULL", [], |row| {
                row.get(0)
            })
        })
        .unwrap();
    assert_eq!(open, MAX_OPEN_LOGINS);

    let assertion = assert_with(&mut auth, &oldest.body["options"], origin_url());
    let late = harness.login_submit(&oldest.body["ceremony_id"], &assertion, None).await;
    late.assert_refused(StatusCode::NOT_FOUND, "unknown_challenge");
}

#[tokio::test]
async fn the_owner_can_sign_in_while_a_flood_holds_the_cap() {
    let harness = Harness::new();
    let mut auth = authenticator();
    harness.enroll(&mut auth, "iPhone").await;
    for _ in 0..MAX_OPEN_LOGINS {
        harness.login_start().await;
    }
    // The owner starts last, and more arrive while the passkey is being touched.
    let mine = harness.login_start().await;
    let assertion = assert_with(&mut auth, &mine.body["options"], origin_url());
    for _ in 0..MAX_OPEN_LOGINS - 1 {
        harness.login_start().await;
    }
    harness.login_submit(&mine.body["ceremony_id"], &assertion, None).await.assert_ok();
}

// ----- enrollment tokens

#[tokio::test]
async fn a_replacement_token_cancels_an_enrollment_opened_with_the_old_one() {
    let harness = Harness::new();
    let mut leaked = authenticator();
    let old = harness.enroll_token();
    let started = harness.post_public("/api/v1/auth/enroll/start", json!({ "token": old, "name": "Attacker" })).await;
    started.assert_ok();
    let credential = register(&mut leaked, &started.body["options"], origin_url());

    let replacement = harness.enroll_token();
    let refused = harness
        .post_public(
            "/api/v1/auth/enroll/finish",
            json!({ "ceremony_id": started.body["ceremony_id"], "credential": credential }),
        )
        .await;
    refused.assert_refused(StatusCode::CONFLICT, "challenge_used");
    assert_eq!(credential_count(&harness), 0);

    let mut owner = authenticator();
    harness.enroll_with(&mut owner, &replacement, "iPhone").await.assert_ok();
}

#[tokio::test]
async fn an_enrollment_cannot_outlive_its_token() {
    let harness = Harness::new();
    let mut auth = authenticator();
    let token = harness.enroll_token();
    harness.advance(ENROLL_TOKEN_TTL_MS - 1);
    let started = harness.post_public("/api/v1/auth/enroll/start", json!({ "token": token, "name": "x" })).await;
    started.assert_ok();
    let credential = register(&mut auth, &started.body["options"], origin_url());
    harness.advance(1);
    let late = harness
        .post_public(
            "/api/v1/auth/enroll/finish",
            json!({ "ceremony_id": started.body["ceremony_id"], "credential": credential }),
        )
        .await;
    late.assert_refused(StatusCode::GONE, "challenge_expired");
    assert_eq!(credential_count(&harness), 0);
}

#[tokio::test]
async fn an_enrollment_started_early_still_gets_its_five_minutes() {
    let harness = Harness::new();
    let mut auth = authenticator();
    let token = harness.enroll_token();
    let started = harness.post_public("/api/v1/auth/enroll/start", json!({ "token": token, "name": "x" })).await;
    let credential = register(&mut auth, &started.body["options"], origin_url());
    harness.advance(ENROLL_CEREMONY_TTL_MS - 1);
    harness
        .post_public(
            "/api/v1/auth/enroll/finish",
            json!({ "ceremony_id": started.body["ceremony_id"], "credential": credential }),
        )
        .await
        .assert_ok();
}

// ----- revocation and the sessions it ends

#[tokio::test]
async fn a_session_is_created_in_the_transaction_that_checks_its_credential() {
    let harness = Harness::new();
    let mut auth = authenticator();
    harness.enroll(&mut auth, "iPhone").await;
    let started = harness.login_start().await;
    let assertion = assert_with(&mut auth, &started.body["options"], origin_url());
    let ceremony = started.body["ceremony_id"].as_str().unwrap();

    // A second connection sees only what has been committed. If it can already
    // see the credential marked as used, the check and the session creation are
    // separate transactions, and a revocation could land between them.
    let observer = rusqlite::Connection::open(harness.app.config.database_path()).unwrap();
    let last_used = |conn: &rusqlite::Connection| -> Option<i64> {
        conn.query_row("SELECT last_used_at FROM credentials", [], |row| row.get(0)).unwrap()
    };
    let seen_during = harness
        .app
        .passkeys
        .login_finish(&harness.app.store, harness.now(), ceremony, &assertion, |_, _| Ok(last_used(&observer)))
        .unwrap();
    assert_eq!(seen_during, None, "the credential check had already committed");
    assert_eq!(last_used(&observer), Some(harness.now()));
}

#[tokio::test]
async fn a_session_is_dead_the_moment_its_passkey_is_revoked_however_that_happened() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let mut second = authenticator();
    let second_id = harness.register_passkey(&cookie, &mut auth, &mut second, "iPad", false).await;
    let second_cookie = harness.login(&mut second).await;

    // The credential is revoked with the session left open, the shape of the
    // overlap that the sign-in transaction now prevents.
    harness
        .app
        .store
        .with(|conn| conn.execute("UPDATE credentials SET revoked_at = 1 WHERE id = ?1", [&second_id]))
        .unwrap();

    let list = harness.get("/api/v1/sessions", &cookie).await;
    assert_eq!(list.body["sessions"].as_array().unwrap().len(), 1, "{}", list.text);
    harness.get("/api/v1/session", &second_cookie).await.assert_refused(StatusCode::UNAUTHORIZED, "session_ended");
    harness
        .execute(&second_cookie, "light.power", "bulb1", json!({ "on": false }))
        .await
        .assert_refused(StatusCode::UNAUTHORIZED, "session_ended");
    assert_eq!(harness.devices.total_commands(), 0);
    let reason: String = harness
        .app
        .store
        .with(|conn| {
            conn.query_row("SELECT end_reason FROM sessions WHERE credential_id = ?1", [&second_id], |row| row.get(0))
        })
        .unwrap();
    assert_eq!(reason, "credential_revoked");
}

/// Opens a registration window and returns what finishing it needs.
async fn registration_window(
    harness: &Harness,
    auth: &mut webauthn_authenticator_rs::softpasskey::SoftPasskey,
    cookie: &str,
) -> (String, String, webauthn_rs::prelude::RegisterPublicKeyCredential) {
    let confirmed = harness.critical(cookie, auth, "passkey.register", "new", json!({ "name": "Late" })).await;
    confirmed.assert_ok();
    let registration = &confirmed.body["registration"];
    let mut new_key = authenticator();
    let credential = register(&mut new_key, &registration["options"], origin_url());
    let session = harness.get("/api/v1/session", cookie).await;
    (
        session.body["session"]["session_ref"].as_str().unwrap().to_string(),
        registration["ceremony_id"].as_str().unwrap().to_string(),
        credential,
    )
}

/// Finishes a registration past the request's own session check, which is where
/// a revocation landing mid-request would leave it.
fn finish_directly(
    harness: &Harness,
    session_ref: &str,
    ceremony_id: &str,
    credential: &webauthn_rs::prelude::RegisterPublicKeyCredential,
) -> Result<swifty_hub::passkeys::CredentialView, swifty_hub::error::ApiError> {
    harness.app.passkeys.register_finish(
        &harness.app.store,
        harness.now(),
        harness.app.limits(),
        session_ref,
        ceremony_id,
        credential,
        |_, _| Ok(()),
    )
}

#[tokio::test]
async fn a_passkey_cannot_be_added_by_a_session_that_has_just_ended() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let (session_ref, ceremony_id, credential) = registration_window(&harness, &mut auth, &cookie).await;
    let before = credential_count(&harness);
    harness
        .app
        .store
        .with(|conn| swifty_hub::sessions::end(conn, harness.now(), &session_ref, "credential_revoked"))
        .unwrap();
    let refused = finish_directly(&harness, &session_ref, &ceremony_id, &credential).unwrap_err();
    assert_eq!((refused.status, refused.code), (StatusCode::UNAUTHORIZED, "session_ended"));
    assert_eq!(credential_count(&harness), before);
}

#[tokio::test]
async fn a_passkey_cannot_be_added_by_a_session_whose_passkey_was_revoked() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let (session_ref, ceremony_id, credential) = registration_window(&harness, &mut auth, &cookie).await;
    let before = credential_count(&harness);
    harness.app.store.with(|conn| conn.execute("UPDATE credentials SET revoked_at = 1", [])).unwrap();
    let refused = finish_directly(&harness, &session_ref, &ceremony_id, &credential).unwrap_err();
    assert_eq!(refused.code, "session_ended");
    assert_eq!(credential_count(&harness), before);
}

#[tokio::test]
async fn a_passkey_cannot_be_added_by_a_locked_session() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let (session_ref, ceremony_id, credential) = registration_window(&harness, &mut auth, &cookie).await;
    harness.app.store.with(|conn| swifty_hub::sessions::lock(conn, harness.now(), &session_ref)).unwrap();
    let refused = finish_directly(&harness, &session_ref, &ceremony_id, &credential).unwrap_err();
    assert_eq!(refused.code, "session_ended");
}

// ----- a new passkey and its audit row

fn block_audit_inserts(harness: &Harness) {
    harness
        .app
        .store
        .with(|conn| {
            conn.execute_batch(
                "CREATE TRIGGER block_audit BEFORE INSERT ON audit_log
                 BEGIN SELECT RAISE(ABORT, 'blocked'); END;",
            )
        })
        .unwrap();
}

#[tokio::test]
async fn no_passkey_is_added_if_its_audit_row_cannot_be_written() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let confirmed = harness.critical(&cookie, &mut auth, "passkey.register", "new", json!({ "name": "iPad" })).await;
    let registration = &confirmed.body["registration"];
    let mut new_key = authenticator();
    let credential = register(&mut new_key, &registration["options"], origin_url());
    let before = credential_count(&harness);

    block_audit_inserts(&harness);
    let failed = harness
        .post(
            "/api/v1/passkeys/register/finish",
            &cookie,
            json!({ "ceremony_id": registration["ceremony_id"], "credential": credential }),
        )
        .await;
    assert_eq!(failed.status, StatusCode::INTERNAL_SERVER_ERROR, "{}", failed.text);
    assert_eq!(credential_count(&harness), before);
}

#[tokio::test]
async fn a_removed_passkey_does_not_stop_its_device_adding_a_new_one() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let mut device = authenticator();
    let removed = harness.register_passkey(&cookie, &mut auth, &mut device, "iPad", false).await;
    harness.critical(&cookie, &mut auth, "passkey.revoke", &removed, json!({})).await.assert_ok();

    let confirmed =
        harness.critical(&cookie, &mut auth, "passkey.register", "new", json!({ "name": "iPad again" })).await;
    let registration = &confirmed.body["registration"];
    let excluded = registration["options"]["publicKey"]["excludeCredentials"].as_array().unwrap();
    assert_eq!(excluded.len(), 1, "only the passkey still in use should be listed: {excluded:?}");

    let credential = register(&mut device, &registration["options"], origin_url());
    let finished = harness
        .post(
            "/api/v1/passkeys/register/finish",
            &cookie,
            json!({ "ceremony_id": registration["ceremony_id"], "credential": credential }),
        )
        .await;
    assert!(finished.status.is_success(), "{}", finished.text);
}

#[tokio::test]
async fn no_first_passkey_is_added_if_its_audit_row_cannot_be_written() {
    let harness = Harness::new();
    let mut auth = authenticator();
    let token = harness.enroll_token();
    let started = harness.post_public("/api/v1/auth/enroll/start", json!({ "token": token, "name": "x" })).await;
    let credential = register(&mut auth, &started.body["options"], origin_url());

    block_audit_inserts(&harness);
    let failed = harness
        .post_public(
            "/api/v1/auth/enroll/finish",
            json!({ "ceremony_id": started.body["ceremony_id"], "credential": credential }),
        )
        .await;
    assert_eq!(failed.status, StatusCode::INTERNAL_SERVER_ERROR, "{}", failed.text);
    assert_eq!(credential_count(&harness), 0);
}

#[tokio::test]
async fn a_session_and_its_audit_row_commit_together() {
    let harness = Harness::new();
    let mut auth = authenticator();
    harness.enroll(&mut auth, "iPhone").await;
    let started = harness.login_start().await;
    let assertion = assert_with(&mut auth, &started.body["options"], origin_url());
    block_audit_inserts(&harness);
    let failed = harness.login_submit(&started.body["ceremony_id"], &assertion, None).await;
    assert_eq!(failed.status, StatusCode::INTERNAL_SERVER_ERROR, "{}", failed.text);
    assert!(failed.headers.get("set-cookie").is_none());
    assert!(!harness.dump("sessions").contains('|'), "a session was created");
}

// ----- what a flood of junk cannot keep out of the log

#[tokio::test]
async fn a_flood_of_junk_cannot_hide_a_cloned_passkey() {
    use swifty_hub::app::MAX_ANONYMOUS_REFUSALS_PER_MINUTE as CAP;
    let harness = Harness::new();
    let mut auth = authenticator();
    let id = harness.enroll(&mut auth, "YubiKey").await;

    let first = harness.login_start().await;
    let second = harness.login_start().await;
    let older = assert_with(&mut auth, &first.body["options"], origin_url());
    let newer = assert_with(&mut auth, &second.body["options"], origin_url());
    harness.login_submit(&second.body["ceremony_id"], &newer, None).await.assert_ok();

    for _ in 0..CAP {
        harness.post_public("/api/v1/auth/enroll/start", json!({ "token": "junk", "name": "x" })).await;
    }
    let refused = harness.login_submit(&first.body["ceremony_id"], &older, None).await;
    refused.assert_refused(StatusCode::UNAUTHORIZED, "credential_counter_regressed");

    assert_eq!(harness.audit_count("refused: credential_counter_regressed"), 1, "the alarm did not reach the log");
    let log = harness.dump("audit_log");
    let row = log.lines().find(|line| line.contains("credential_counter_regressed")).unwrap();
    assert!(row.contains(&id), "the row should say which passkey: {row}");
}

#[tokio::test]
async fn a_flood_of_junk_cannot_hide_a_revoked_passkey_being_used() {
    use swifty_hub::app::MAX_ANONYMOUS_REFUSALS_PER_MINUTE as CAP;
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let mut second = authenticator();
    let second_id = harness.register_passkey(&cookie, &mut auth, &mut second, "iPad", false).await;
    let started = harness.login_start().await;
    let assertion = assert_with(&mut second, &started.body["options"], origin_url());
    harness.critical(&cookie, &mut auth, "passkey.revoke", &second_id, json!({})).await.assert_ok();

    for _ in 0..CAP {
        harness.post_public("/api/v1/auth/enroll/start", json!({ "token": "junk", "name": "x" })).await;
    }
    harness
        .login_submit(&started.body["ceremony_id"], &assertion, None)
        .await
        .assert_refused(StatusCode::UNAUTHORIZED, "credential_not_active");
    assert_eq!(harness.audit_count("refused: credential_not_active"), 1);
}

#[tokio::test]
async fn further_passkeys_need_a_session_and_a_step_up() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;

    // No session.
    let anonymous = harness
        .send(
            axum::http::Method::POST,
            "/api/v1/actions/prepare",
            None,
            Some(ORIGIN),
            Some(json!({ "action": "passkey.register", "target": "new", "params": { "name": "x" } })),
        )
        .await;
    anonymous.assert_refused(StatusCode::UNAUTHORIZED, "not_signed_in");

    // A session alone is not enough.
    let direct = harness.execute(&cookie, "passkey.register", "new", json!({ "name": "x" })).await;
    direct.assert_refused(StatusCode::FORBIDDEN, "step_up_required");

    let prepared = harness.prepare(&cookie, "passkey.register", "new", json!({ "name": "iPad" })).await;
    prepared.assert_ok();
    assert_eq!(prepared.body["needs"], json!("passkey"));
    assert!(prepared.body["confirmation"].is_null());
    let bare = harness
        .post(
            "/api/v1/actions/confirm",
            &cookie,
            json!({
                "prepared_id": prepared.body["prepared_id"],
                "action": "passkey.register",
                "target": "new",
                "params": prepared.body["params"],
            }),
        )
        .await;
    bare.assert_refused(StatusCode::PRECONDITION_REQUIRED, "step_up_required");

    // With the step-up it works, and the new key can sign in.
    let mut new_key = authenticator();
    let id = harness.register_passkey(&cookie, &mut auth, &mut new_key, "iPad", false).await;
    let signed_in = harness.login(&mut new_key).await;
    let session = harness.get("/api/v1/session", &signed_in).await;
    assert_eq!(session.body["session"]["credential_id"], json!(id));
    assert!(harness.audit_has("registered"));
}

#[tokio::test]
async fn registration_window_is_bound_to_its_session_and_single_use() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let other_cookie = harness.login(&mut auth).await;

    let confirmed = harness.critical(&cookie, &mut auth, "passkey.register", "new", json!({ "name": "iPad" })).await;
    confirmed.assert_ok();
    assert_eq!(confirmed.phase(), "confirmed");
    let registration = &confirmed.body["registration"];
    let mut new_key = authenticator();
    let credential = register(&mut new_key, &registration["options"], origin_url());
    let body = json!({ "ceremony_id": registration["ceremony_id"], "credential": credential });

    let stolen = harness.post("/api/v1/passkeys/register/finish", &other_cookie, body.clone()).await;
    stolen.assert_refused(StatusCode::FORBIDDEN, "wrong_session");
    let spent = harness.post("/api/v1/passkeys/register/finish", &cookie, body).await;
    spent.assert_refused(StatusCode::CONFLICT, "challenge_used");
}

#[tokio::test]
async fn registration_window_expires() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let confirmed = harness.critical(&cookie, &mut auth, "passkey.register", "new", json!({ "name": "iPad" })).await;
    let registration = &confirmed.body["registration"];
    let mut new_key = authenticator();
    let credential = register(&mut new_key, &registration["options"], origin_url());
    harness.advance(120_000);
    // The session is still within its idle window.
    let late = harness
        .post(
            "/api/v1/passkeys/register/finish",
            &cookie,
            json!({ "ceremony_id": registration["ceremony_id"], "credential": credential }),
        )
        .await;
    late.assert_refused(StatusCode::GONE, "challenge_expired");
}

#[tokio::test]
async fn passkeys_can_be_listed_and_renamed() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let mut key = authenticator();
    let id = harness.register_passkey(&cookie, &mut auth, &mut key, "iPad", false).await;

    let list = harness.get("/api/v1/passkeys", &cookie).await;
    list.assert_ok();
    let rows = list.body["passkeys"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows.iter().filter(|r| r["in_use"] == json!(true)).count(), 1);
    assert_eq!(rows[1]["name"], json!("iPad"));
    assert!(!list.text.contains("cred_id") && !list.text.contains("passkey\":{"));

    let path = format!("/api/v1/passkeys/{id}");
    harness
        .send(axum::http::Method::PATCH, &path, Some(&cookie), Some(ORIGIN), Some(json!({ "name": "Tablet" })))
        .await
        .assert_ok();
    let list = harness.get("/api/v1/passkeys", &cookie).await;
    assert_eq!(list.body["passkeys"][1]["name"], json!("Tablet"));

    let bad = harness
        .send(axum::http::Method::PATCH, &path, Some(&cookie), Some(ORIGIN), Some(json!({ "name": " spaced " })))
        .await;
    bad.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_name");
    let unknown = harness
        .send(
            axum::http::Method::PATCH,
            "/api/v1/passkeys/nope",
            Some(&cookie),
            Some(ORIGIN),
            Some(json!({ "name": "x" })),
        )
        .await;
    unknown.assert_refused(StatusCode::NOT_FOUND, "unknown_credential");
}

#[tokio::test]
async fn revoking_a_passkey_ends_its_sessions_and_stops_it_signing_in() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let mut second = authenticator();
    let second_id = harness.register_passkey(&cookie, &mut auth, &mut second, "iPad", false).await;
    let second_cookie = harness.login(&mut second).await;
    harness.get("/api/v1/session", &second_cookie).await.assert_ok();

    let revoked = harness.critical(&cookie, &mut auth, "passkey.revoke", &second_id, json!({})).await;
    revoked.assert_ok();
    assert_eq!(revoked.phase(), "confirmed");

    let ended = harness.get("/api/v1/session", &second_cookie).await;
    ended.assert_refused(StatusCode::UNAUTHORIZED, "session_ended");

    // It is left out of new challenges, and it does not count if presented.
    let started = harness.login_start().await;
    assert_eq!(started.body["options"]["publicKey"]["allowCredentials"].as_array().unwrap().len(), 1);

    let list = harness.get("/api/v1/passkeys", &cookie).await;
    let row = list.body["passkeys"].as_array().unwrap().iter().find(|r| r["id"] == json!(second_id)).unwrap().clone();
    assert!(row["revoked_at"].is_i64());
}

#[tokio::test]
async fn a_challenge_issued_before_revocation_cannot_be_used_after_it() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let mut second = authenticator();
    let second_id = harness.register_passkey(&cookie, &mut auth, &mut second, "iPad", false).await;
    let started = harness.login_start().await;
    let assertion = assert_with(&mut second, &started.body["options"], origin_url());

    harness.critical(&cookie, &mut auth, "passkey.revoke", &second_id, json!({})).await.assert_ok();
    let refused = harness.login_submit(&started.body["ceremony_id"], &assertion, None).await;
    refused.assert_refused(StatusCode::UNAUTHORIZED, "credential_not_active");
}

#[tokio::test]
async fn the_last_usable_passkey_cannot_be_revoked() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let mut other = authenticator();
    let other_id = harness.register_passkey(&cookie, &mut auth, &mut other, "iPad", false).await;
    let other_cookie = harness.login(&mut other).await;

    // The first key revokes the second, leaving one.
    harness.critical(&cookie, &mut auth, "passkey.revoke", &other_id, json!({})).await.assert_ok();
    let _ = other_cookie;

    // The recovery key is not "usable" for daily sign-in, so it cannot stand in.
    let mut recovery = authenticator();
    harness.register_passkey(&cookie, &mut auth, &mut recovery, "YubiKey", true).await;
    let recovery_cookie = harness.login(&mut recovery).await;

    let list = harness.get("/api/v1/passkeys", &recovery_cookie).await;
    let daily = list.body["passkeys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["recovery"] == json!(false) && r["revoked_at"].is_null())
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let refused = harness.prepare(&recovery_cookie, "passkey.revoke", &daily, json!({})).await;
    refused.assert_refused(StatusCode::CONFLICT, "last_usable_credential");
}

#[tokio::test]
async fn the_recovery_passkey_must_remain() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let mut recovery = authenticator();
    let recovery_id = harness.register_passkey(&cookie, &mut auth, &mut recovery, "YubiKey", true).await;

    let refused = harness.prepare(&cookie, "passkey.revoke", &recovery_id, json!({})).await;
    refused.assert_refused(StatusCode::CONFLICT, "last_recovery_credential");

    // Once another recovery key exists, the old one can go.
    let mut replacement = authenticator();
    harness.register_passkey(&cookie, &mut auth, &mut replacement, "New YubiKey", true).await;
    harness.critical(&cookie, &mut auth, "passkey.revoke", &recovery_id, json!({})).await.assert_ok();
}

#[tokio::test]
async fn the_passkey_in_use_cannot_be_revoked_from_its_own_session() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let mut other = authenticator();
    harness.register_passkey(&cookie, &mut auth, &mut other, "iPad", false).await;
    let list = harness.get("/api/v1/passkeys", &cookie).await;
    let own = list.body["passkeys"].as_array().unwrap().iter().find(|r| r["in_use"] == json!(true)).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let refused = harness.prepare(&cookie, "passkey.revoke", &own, json!({})).await;
    refused.assert_refused(StatusCode::CONFLICT, "credential_in_use");
}

#[tokio::test]
async fn revoke_is_rechecked_when_it_runs() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let mut second = authenticator();
    let second_id = harness.register_passkey(&cookie, &mut auth, &mut second, "iPad", false).await;
    let mut third = authenticator();
    let third_id = harness.register_passkey(&cookie, &mut auth, &mut third, "MacBook", false).await;

    // Both are allowed while there are three keys.
    let a = harness.prepare(&cookie, "passkey.revoke", &second_id, json!({})).await;
    a.assert_ok();
    let b = harness.prepare(&cookie, "passkey.revoke", &third_id, json!({})).await;
    b.assert_ok();
    let a_up = harness.step_up_start(&cookie, &a).await;
    let b_up = harness.step_up_start(&cookie, &b).await;
    let a_sig = assert_with(&mut auth, &a_up.body["options"], origin_url());
    let b_sig = assert_with(&mut auth, &b_up.body["options"], origin_url());

    harness.confirm_assertion(&cookie, &a, &a_sig).await.assert_ok();
    // Still allowed, because the first key remains.
    harness.confirm_assertion(&cookie, &b, &b_sig).await.assert_ok();
    let list = harness.get("/api/v1/passkeys", &cookie).await;
    let active = list.body["passkeys"].as_array().unwrap().iter().filter(|r| r["revoked_at"].is_null()).count();
    assert_eq!(active, 1);
}
