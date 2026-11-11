//! The TV's secure port: a self-signed certificate, accepted only by its pinned SHA-256.
//!
//! Ported from the hub's former `webos_tls.rs`. The TLS server is the test kit's TV on
//! its secure port instead of a bare websocket server, so a successful
//! connection is shown by the TV answering a registration.

mod common;

use std::time::Duration;

use serde_json::{json, Value};
use swifty_plugin_api::net::{ws_connect, LiveGrant, Timeouts};
use swifty_plugin_api::{DeviceError, Plugin};
use swifty_plugin_testkit::devices::{MockTv, TV_KEY};
use swifty_plugin_webos::PLUGIN;
use url::Url;

const TIMEOUTS: Timeouts = Timeouts { connect: Duration::from_secs(3), read: Duration::from_secs(3) };

fn grant() -> LiveGrant {
    LiveGrant::when_live(true).unwrap()
}

/// A TV on a self-signed certificate. Returns it, its URL and the pin.
async fn wss_server() -> (MockTv, Url, String) {
    let tv = MockTv::start_tls().await;
    let url = Url::parse(&tv.url).unwrap();
    let pin = tv.pin.clone().unwrap();
    (tv, url, pin)
}

fn register() -> String {
    json!({ "type": "register", "id": "register_0", "payload": { "client-key": TV_KEY } }).to_string()
}

#[tokio::test]
async fn the_pinned_certificate_is_accepted() {
    let (_tv, url, pin) = wss_server().await;
    let Ok(mut ws) = ws_connect(grant(), &url, Some(&pin), TIMEOUTS).await else {
        panic!("the pinned certificate should connect");
    };
    ws.send(register()).await.unwrap();
    let reply: Value = serde_json::from_str(&ws.recv().await.unwrap()).unwrap();
    assert_eq!(reply["type"], "registered");
}

#[tokio::test]
async fn the_pin_ignores_letter_case() {
    let (_tv, url, pin) = wss_server().await;
    assert!(ws_connect(grant(), &url, Some(&pin.to_ascii_uppercase()), TIMEOUTS).await.is_ok());
}

#[tokio::test]
async fn any_other_certificate_is_refused() {
    let (tv, url, _) = wss_server().await;
    assert!(ws_connect(grant(), &url, Some(&"0".repeat(64)), TIMEOUTS).await.is_err());
    assert!(tv.received.lock().unwrap().is_empty(), "nothing reached the TV over the refused connection");
}

#[tokio::test]
async fn wss_without_a_pin_is_refused() {
    let (_tv, url, _) = wss_server().await;
    let refused = ws_connect(grant(), &url, None, TIMEOUTS).await;
    assert!(matches!(refused, Err(DeviceError::Disabled(_))));
}

#[test]
fn the_config_requires_a_pin_for_wss_and_only_for_wss() {
    let config = |body: Value| PLUGIN.validate(&body);
    let pin = "a".repeat(64);
    assert!(config(json!({ "url": "wss://tv:3001", "cert_sha256": pin })).is_ok());
    assert!(config(json!({ "url": "wss://tv:3001" })).is_err());
    assert!(config(json!({ "url": "wss://tv:3001", "cert_sha256": "not-hex" })).is_err());
    assert!(config(json!({ "url": "ws://tv:3000", "cert_sha256": pin })).is_err());
    assert!(config(json!({ "url": "ws://tv:3000" })).is_ok());
}

// ----- new with the plugin: the driver on the secure port

#[tokio::test]
async fn the_driver_reads_a_tv_on_its_pinned_certificate_and_nothing_else() {
    let (tv, _, pin) = wss_server().await;
    let config = json!({ "url": tv.url, "cert_sha256": pin });
    let paired = common::Instance::connect(config);
    assert!(paired.read_state("tv").await.is_ok());

    let wrong = common::Instance::connect(json!({ "url": tv.url, "cert_sha256": "f".repeat(64) }));
    let before = tv.received.lock().unwrap().len();
    assert!(wrong.read_state("tv").await.unwrap_err().is_silence());
    assert_eq!(tv.received.lock().unwrap().len(), before, "the key never went over an unpinned connection");
}
