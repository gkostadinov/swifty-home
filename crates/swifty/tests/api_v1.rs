//! The API's one prefix, `/api/v1`: the old unversioned paths are gone,
//! every response says which API it is, and `meta` tells a client what the hub is
//! before it signs in.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{json, Value};
use swifty_hub::http::{API_MAJOR, API_MINOR, API_PREFIX};

#[test]
fn the_prefix_is_version_one() {
    assert_eq!(API_PREFIX, "/api/v1");
    assert_eq!(API_MAJOR, 1);
}

#[tokio::test]
async fn the_old_paths_are_gone_and_not_aliased() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    for path in ["/api/state", "/api/state/bulb1", "/api/session", "/api/catalog", "/api/devices"] {
        let old = harness.get(path, &cookie).await;
        old.assert_refused(StatusCode::NOT_FOUND, "not_found");
        let new = harness.get(&path.replacen("/api/", "/api/v1/", 1), &cookie).await;
        new.assert_ok();
    }
    // The stream would not end, so only the old path is asked for (tests/events.rs reads the new one).
    harness.get("/api/events", &cookie).await.assert_refused(StatusCode::NOT_FOUND, "not_found");
    let command = json!({ "action": "light.power", "target": "bulb1", "params": { "on": true } });
    harness.post("/api/actions/execute", &cookie, command).await.assert_refused(StatusCode::NOT_FOUND, "not_found");
    let login = harness.post_public("/api/auth/login/start", json!({})).await;
    login.assert_refused(StatusCode::NOT_FOUND, "not_found");
    assert_eq!(harness.devices.total_commands(), 0, "nothing reached a device through an old path");
    // The new ones work, and the page and the health check stay where they were.
    harness.get("/api/v1/state", &cookie).await.assert_ok();
    for path in ["/", "/app/main.js", "/healthz"] {
        harness.send(Method::GET, path, None, None, None).await.assert_ok();
    }
}

#[tokio::test]
async fn every_response_names_the_api_version() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let responses = [
        harness.get("/api/v1/me", &cookie).await,
        harness.send(Method::GET, "/api/v1/meta", None, None, None).await,
        harness.send(Method::GET, "/api/v1/catalog", None, None, None).await,
        harness
            .send(Method::POST, "/api/v1/auth/logout", Some(&cookie), Some("https://elsewhere.example.org"), None)
            .await,
        harness.send(Method::GET, "/api/state", None, None, None).await,
        harness.send(Method::GET, "/", None, None, None).await,
    ];
    let statuses: Vec<u16> = responses.iter().map(|response| response.status.as_u16()).collect();
    assert_eq!(statuses, [200, 200, 401, 403, 404, 200]);
    for response in responses {
        assert_eq!(response.headers["x-swifty-api"], "1", "{}", response.text);
    }
}

#[tokio::test]
async fn meta_says_what_the_hub_is_to_anyone() {
    let harness = Harness::new();
    let meta = harness.send(Method::GET, "/api/v1/meta", None, None, None).await;
    meta.assert_ok();
    assert_eq!(
        meta.body,
        json!({
            "product": "Swifty Home",
            "version": env!("CARGO_PKG_VERSION"),
            "api": { "major": API_MAJOR, "minor": API_MINOR },
            "plugin_api": swifty_plugin_api::API_VERSION,
            "live": true,
            "needs_owner": true,
            "auth": ["passkey"],
            "events": ["sse"],
        })
    );
    // Once the first passkey is in, the hub no longer waits for its owner.
    harness.sign_in().await;
    let meta = harness.send(Method::GET, "/api/v1/meta", None, None, None).await;
    assert_eq!(meta.body["needs_owner"], json!(false));
    // A dry-run hub says so.
    let dry = Harness::with(|config| config.live = false);
    let meta = dry.send(Method::GET, "/api/v1/meta", None, None, None).await;
    assert_eq!(meta.body["live"], json!(false));
}

#[tokio::test]
async fn meta_names_no_device_person_or_setting() {
    let (harness, _auth, _cookie) = Harness::signed_in().await;
    let meta = harness.send(Method::GET, "/api/v1/meta", None, None, None).await;
    let mut keys: Vec<&str> = meta.body.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["api", "auth", "events", "live", "needs_owner", "plugin_api", "product", "version"]);
    for leak in ["bulb", "iPhone", "Owner", "example.org", "vm10"] {
        assert!(!meta.text.contains(leak), "{leak} in {}", meta.text);
    }
}

#[tokio::test]
async fn a_state_entry_names_its_plugin_and_instance() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let one = harness.get("/api/v1/state/bulb1", &cookie).await;
    one.assert_ok();
    assert_eq!((&one.body["plugin"], &one.body["instance"]), (&json!("mock"), &json!("wiz")), "{}", one.text);
    assert_eq!(one.body.get("adapter"), None, "adapter became plugin and instance");
    let all = harness.get("/api/v1/state", &cookie).await;
    let entries = all.body["devices"].as_object().unwrap();
    assert!(!entries.is_empty());
    for (target, entry) in entries {
        assert!(entry["instance"].is_string() && entry["plugin"].is_string(), "{target}: {entry}");
        assert_eq!(entry.get("adapter"), None::<&Value>, "{target}");
    }
}
