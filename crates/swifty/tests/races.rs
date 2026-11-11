//! Requests that really do overlap, on a multi-threaded runtime. Each test runs
//! many rounds and checks something that must hold however they interleave.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use common::*;
use serde_json::json;

const ROUNDS: usize = 40;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sign_in_racing_a_revocation_never_leaves_a_live_session() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let harness = Arc::new(harness);
    let mut signed_in_during_revocation = 0;

    for round in 0..ROUNDS {
        let mut stolen = authenticator();
        let id = harness.register_passkey(&cookie, &mut owner, &mut stolen, &format!("Key {round}"), false).await;

        let login = harness.login_start().await;
        let login_assertion = assert_with(&mut stolen, &login.body["options"], origin_url());
        let prepared = harness.prepare(&cookie, "passkey.revoke", &id, json!({})).await;
        prepared.assert_ok();
        let up = harness.step_up_start(&cookie, &prepared).await;
        let revoke_assertion = assert_with(&mut owner, &up.body["options"], origin_url());

        // The revocation has more work to do before it lands, so the sign-in is
        // held back by a different amount each round to sweep across it.
        let hold_back = Duration::from_micros(round as u64 * 300);
        let signing_in = {
            let harness = harness.clone();
            let ceremony = login.body["ceremony_id"].clone();
            tokio::spawn(async move {
                let until = Instant::now() + hold_back;
                while Instant::now() < until {
                    std::hint::spin_loop();
                }
                harness.login_submit(&ceremony, &login_assertion, None).await
            })
        };
        let revoking = {
            let harness = harness.clone();
            let cookie = cookie.clone();
            tokio::spawn(async move { harness.confirm_assertion(&cookie, &prepared, &revoke_assertion).await })
        };
        let signed_in = signing_in.await.unwrap();
        let revoked = revoking.await.unwrap();
        assert_eq!(revoked.phase(), "confirmed", "round {round}: {}", revoked.text);

        // Read before anything uses the cookie, so this sees what the two
        // transactions left, without the session check tidying up behind them.
        let alive: i64 = harness
            .app
            .store
            .with(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM sessions WHERE credential_id = ?1 AND ended_at IS NULL",
                    [&id],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(alive, 0, "round {round}: a session outlived the revocation of its passkey");

        if signed_in.status.is_success() {
            signed_in_during_revocation += 1;
            let session = harness.get("/api/v1/session", &signed_in.session_cookie()).await;
            session.assert_refused(StatusCode::UNAUTHORIZED, "session_ended");
        } else {
            signed_in.assert_refused(StatusCode::UNAUTHORIZED, "credential_not_active");
        }
    }
    // Not asserted, because it depends on the scheduler. Printed for anyone
    // checking the race is really being exercised.
    println!("{signed_in_during_revocation} of {ROUNDS} sign-ins succeeded before the revocation landed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_double_submitted_step_up_runs_exactly_once() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let harness = Arc::new(harness);

    for round in 0..ROUNDS {
        let prepared = Arc::new(harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await);
        prepared.assert_ok();
        let up = harness.step_up_start(&cookie, &prepared).await;
        let assertion = Arc::new(assert_with(&mut auth, &up.body["options"], origin_url()));

        let submit = || {
            let (harness, cookie, prepared, assertion) =
                (harness.clone(), cookie.clone(), prepared.clone(), assertion.clone());
            tokio::spawn(async move { harness.confirm_assertion(&cookie, &prepared, &assertion).await })
        };
        let (first, second) = (submit(), submit());
        let replies = [first.await.unwrap(), second.await.unwrap()];

        let ran: Vec<&Resp> = replies.iter().filter(|r| r.status.is_success()).collect();
        assert_eq!(ran.len(), 1, "round {round}: {} / {}", replies[0].text, replies[1].text);
        assert_eq!(ran[0].phase(), "confirmed", "round {round}: {}", ran[0].text);
        let refused = replies.iter().find(|r| !r.status.is_success()).unwrap();
        assert_eq!(refused.status, StatusCode::CONFLICT, "round {round}: {}", refused.text);
        assert!(matches!(refused.code(), "challenge_used" | "already_used"), "round {round}: {}", refused.text);
        assert_eq!(harness.devices.command_count("vm101"), round + 1, "round {round}");
    }
}
