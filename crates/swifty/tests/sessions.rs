//! Sessions, the origin check and the absence of CORS.

mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use common::*;
use serde_json::json;

fn set_cookie(response: &Resp) -> String {
    response.headers[header::SET_COOKIE].to_str().unwrap().to_string()
}

#[tokio::test]
async fn the_cookie_is_locked_down_and_host_only() {
    let harness = Harness::new();
    let mut auth = authenticator();
    harness.enroll(&mut auth, "iPhone").await;
    let started = harness.login_start().await;
    let assertion = assert_with(&mut auth, &started.body["options"], origin_url());
    let done = harness.login_submit(&started.body["ceremony_id"], &assertion, None).await;
    let cookie = set_cookie(&done);

    assert!(cookie.starts_with("__Host-swifty-session="));
    assert!(cookie.contains("; HttpOnly"));
    assert!(cookie.contains("; Secure"));
    assert!(cookie.contains("; SameSite=Strict"));
    assert!(cookie.contains("; Path=/"));
    assert!(!cookie.to_lowercase().contains("domain="));
    assert!(cookie.contains("Max-Age=28800"));
}

#[tokio::test]
async fn the_session_id_is_random_and_stored_hashed() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let second = harness.login(&mut auth).await;
    assert_ne!(cookie, second);

    let token = cookie.split_once('=').unwrap().1;
    let stored = harness.dump("sessions");
    assert!(!stored.contains(token));
    assert!(stored.contains(&swifty_hub::crypto::sha256_hex(token.as_bytes())));
    assert!(!harness.dump("audit_log").contains(token));
    assert!(token.len() >= 43);
}

#[tokio::test]
async fn a_session_ends_at_the_absolute_limit_however_active_it_is() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    for _ in 0..16 {
        harness.advance(29 * MINUTE_MS);
        harness.get("/api/v1/catalog", &cookie).await.assert_ok();
    }
    // 16 x 29 minutes is 7 h 44 min. Stay active up to just under 8 hours.
    harness.advance(15 * MINUTE_MS);
    harness.get("/api/v1/catalog", &cookie).await.assert_ok();
    harness.advance(MINUTE_MS);
    let expired = harness.get("/api/v1/catalog", &cookie).await;
    expired.assert_refused(StatusCode::UNAUTHORIZED, "session_expired");
    let again = harness.get("/api/v1/catalog", &cookie).await;
    again.assert_refused(StatusCode::UNAUTHORIZED, "session_ended");
}

#[tokio::test]
async fn a_session_ends_at_the_idle_limit() {
    let (harness, _auth, cookie) = Harness::signed_in_with(|c| c.session.idle_secs = 600).await;
    harness.advance(599_000);
    harness.get("/api/v1/catalog", &cookie).await.assert_ok();
    harness.advance(599_000);
    harness.get("/api/v1/catalog", &cookie).await.assert_ok();
    harness.advance(600_000);
    let idle = harness.get("/api/v1/catalog", &cookie).await;
    idle.assert_refused(StatusCode::UNAUTHORIZED, "session_idle");
}

#[tokio::test]
async fn passive_polling_does_not_keep_a_session_alive() {
    let (harness, _auth, cookie) = Harness::signed_in_with(|c| c.session.idle_secs = 600).await;
    for _ in 0..3 {
        harness.advance(180_000);
        harness.get("/api/v1/state", &cookie).await.assert_ok();
        harness.get("/api/v1/session", &cookie).await.assert_ok();
        harness.get("/api/v1/actions/runs", &cookie).await.assert_ok();
    }
    harness.advance(60_000);
    harness.get("/api/v1/state", &cookie).await.assert_refused(StatusCode::UNAUTHORIZED, "session_idle");
}

#[tokio::test]
async fn locking_stops_everything_until_a_passkey_signs_in() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    harness.post("/api/v1/session/lock", &cookie, json!({})).await.assert_ok();
    assert!(harness.audit_has("locked"));

    let locked = StatusCode::LOCKED;
    harness.get("/api/v1/catalog", &cookie).await.assert_refused(locked, "session_locked");
    harness.get("/api/v1/state", &cookie).await.assert_refused(locked, "session_locked");
    harness
        .execute(&cookie, "light.power", "bulb1", json!({ "on": false }))
        .await
        .assert_refused(locked, "session_locked");
    harness
        .prepare(&cookie, "pets.feed", "feeder", json!({ "grams": 10 }))
        .await
        .assert_refused(locked, "session_locked");
    assert_eq!(harness.devices.total_commands(), 0);

    // The session is still known to be locked.
    let info = harness.get("/api/v1/session", &cookie).await;
    info.assert_ok();
    assert_eq!(info.body["session"]["locked"], json!(true));

    // Signing in with a passkey replaces the locked session.
    let started = harness.login_start().await;
    let assertion = assert_with(&mut auth, &started.body["options"], origin_url());
    let unlocked = harness.login_submit(&started.body["ceremony_id"], &assertion, Some(&cookie)).await;
    unlocked.assert_ok();
    let fresh = unlocked.session_cookie();
    harness.get("/api/v1/catalog", &fresh).await.assert_ok();
    harness.get("/api/v1/catalog", &cookie).await.assert_refused(StatusCode::UNAUTHORIZED, "session_ended");
}

#[tokio::test]
async fn a_prepared_action_cannot_be_confirmed_while_locked() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let prepared = harness.prepare(&cookie, "pets.feed", "feeder", json!({ "grams": 10 })).await;
    prepared.assert_ok();
    harness.post("/api/v1/session/lock", &cookie, json!({})).await.assert_ok();
    harness.confirm_tap(&cookie, &prepared).await.assert_refused(StatusCode::LOCKED, "session_locked");
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn sessions_can_be_listed_and_ended() {
    let (harness, mut auth, phone) = Harness::signed_in().await;
    let laptop = harness.login(&mut auth).await;

    let list = harness.get("/api/v1/sessions", &phone).await;
    list.assert_ok();
    let rows = list.body["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows.iter().filter(|r| r["current"] == json!(true)).count(), 1);
    assert!(!list.text.contains(phone.split_once('=').unwrap().1));

    let other = rows.iter().find(|r| r["current"] == json!(false)).unwrap();
    let path = format!("/api/v1/sessions/{}", other["session_ref"].as_str().unwrap());
    harness.send(Method::DELETE, &path, Some(&phone), Some(ORIGIN), None).await.assert_ok();
    harness.get("/api/v1/catalog", &laptop).await.assert_refused(StatusCode::UNAUTHORIZED, "session_ended");
    harness.get("/api/v1/catalog", &phone).await.assert_ok();

    let repeat = harness.send(Method::DELETE, &path, Some(&phone), Some(ORIGIN), None).await;
    repeat.assert_refused(StatusCode::NOT_FOUND, "unknown_session");
}

#[tokio::test]
async fn signing_out_ends_the_session_and_clears_the_cookie() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let done = harness.post("/api/v1/auth/logout", &cookie, json!({})).await;
    done.assert_ok();
    assert!(set_cookie(&done).contains("Max-Age=0"));
    harness.get("/api/v1/catalog", &cookie).await.assert_refused(StatusCode::UNAUTHORIZED, "session_ended");
}

const STATE_CHANGING: &[(&str, &str)] = &[
    ("POST", "/api/v1/auth/enroll/start"),
    ("POST", "/api/v1/auth/enroll/finish"),
    ("POST", "/api/v1/auth/login/start"),
    ("POST", "/api/v1/auth/login/finish"),
    ("POST", "/api/v1/auth/logout"),
    ("POST", "/api/v1/auth/step-up/start"),
    ("POST", "/api/v1/session/lock"),
    ("DELETE", "/api/v1/sessions/abc"),
    ("POST", "/api/v1/actions/execute"),
    ("POST", "/api/v1/actions/prepare"),
    ("POST", "/api/v1/actions/confirm"),
    ("PATCH", "/api/v1/passkeys/abc"),
    ("POST", "/api/v1/passkeys/register/finish"),
    ("POST", "/api/v1/voice"),
];

#[tokio::test]
async fn every_state_changing_route_checks_the_origin() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    for (method, path) in STATE_CHANGING {
        let method: Method = method.parse().unwrap();
        for origin in [
            Some(EVIL_ORIGIN),
            None,
            Some("null"),
            Some("https://home.example.org.evil.example"),
            Some("http://home.example.org"),
        ] {
            let response = harness.send(method.clone(), path, Some(&cookie), origin, Some(json!({}))).await;
            response.assert_refused(StatusCode::FORBIDDEN, "bad_origin");
        }
    }
    assert_eq!(harness.devices.total_commands(), 0);
    harness.get("/api/v1/session", &cookie).await.assert_ok();
}

#[tokio::test]
async fn the_right_origin_passes_the_check() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let response = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    response.assert_ok();
}

#[tokio::test]
async fn origin_is_configurable_for_tests() {
    let harness = Harness::with(|config| {
        config.webauthn.rp_id = "localhost".to_string();
        config.webauthn.origin = "http://localhost:8790".to_string();
    });
    let allowed = harness
        .send(Method::POST, "/api/v1/auth/login/start", None, Some("http://localhost:8790"), Some(json!({})))
        .await;
    allowed.assert_refused(StatusCode::CONFLICT, "no_credentials");
    let refused = harness.send(Method::POST, "/api/v1/auth/login/start", None, Some(ORIGIN), Some(json!({}))).await;
    refused.assert_refused(StatusCode::FORBIDDEN, "bad_origin");
}

#[tokio::test]
async fn there_is_no_cors() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let preflight = Request::builder()
        .method(Method::OPTIONS)
        .uri("/api/v1/actions/execute")
        .header(header::ORIGIN, EVIL_ORIGIN)
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
        .body(Body::empty())
        .unwrap();
    let response = harness.dispatch(preflight).await;
    assert!(response.status.is_client_error());
    let responses = [
        response,
        harness.get("/api/v1/catalog", &cookie).await,
        harness.send(Method::GET, "/api/v1/catalog", Some(&cookie), Some(EVIL_ORIGIN), None).await,
        harness.execute(&cookie, "light.power", "bulb1", json!({ "on": true })).await,
    ];
    for response in responses {
        let found: Vec<_> =
            response.headers.keys().filter(|name| name.as_str().starts_with("access-control-")).collect();
        assert!(found.is_empty(), "found CORS headers: {found:?}");
    }
}

const SESSION_ROUTES: &[(&str, &str)] = &[
    ("POST", "/api/v1/auth/logout"),
    ("POST", "/api/v1/auth/step-up/start"),
    ("GET", "/api/v1/session"),
    ("POST", "/api/v1/session/lock"),
    ("GET", "/api/v1/sessions"),
    ("DELETE", "/api/v1/sessions/abc"),
    ("GET", "/api/v1/catalog"),
    ("GET", "/api/v1/devices"),
    ("GET", "/api/v1/state"),
    ("GET", "/api/v1/state/bulb1"),
    ("POST", "/api/v1/actions/execute"),
    ("POST", "/api/v1/actions/prepare"),
    ("POST", "/api/v1/actions/confirm"),
    ("GET", "/api/v1/actions/runs"),
    ("GET", "/api/v1/actions/runs/abc"),
    ("GET", "/api/v1/audit"),
    ("GET", "/api/v1/audit/verify"),
    ("GET", "/api/v1/passkeys"),
    ("PATCH", "/api/v1/passkeys/abc"),
    ("POST", "/api/v1/passkeys/register/finish"),
    ("POST", "/api/v1/voice"),
];

#[tokio::test]
async fn every_protected_route_needs_a_session() {
    let harness = Harness::new();
    for (method, path) in SESSION_ROUTES {
        let method: Method = method.parse().unwrap();
        let body = (method != Method::GET).then(|| json!({}));
        let response = harness.send(method.clone(), path, None, Some(ORIGIN), body.clone()).await;
        assert_eq!(response.status, StatusCode::UNAUTHORIZED, "{method} {path}: {}", response.text);
        let forged = harness.send(method.clone(), path, Some("__Host-swifty-session=forged"), Some(ORIGIN), body).await;
        assert_eq!(forged.status, StatusCode::UNAUTHORIZED, "{method} {path}");
    }
}
