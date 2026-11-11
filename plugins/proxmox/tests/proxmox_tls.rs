//! The Proxmox token must only ever reach a server whose certificate checks out.
//! An HTTPS stand-in on loopback, with its own private CA, records whatever a
//! client manages to send once the handshake is complete.
//!
//! These are the hub's TLS tests, under the same names, now driven through the
//! plugin. The stand-ins are the test kit's TLS servers, each on a certificate a
//! private CA signed for a chosen name.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use serde_json::json;
use swifty_plugin_api::net::{console_connect, Roots, Timeouts};
use swifty_plugin_api::{ConsoleGrant, DeviceError, Driver, ExtraHost, Secrets, State};
use swifty_plugin_testkit::devices::{MockTlsHttp, MockTlsWs, PROXMOX_TOKEN};

/// The name a certificate for some other machine carries.
const ANOTHER_HOST: &str = "elsewhere.example.org";

const TLS_TIMEOUTS: Timeouts = Timeouts { connect: Duration::from_millis(800), read: Duration::from_millis(800) };

const REPLY_BODY: &str = r#"{"data":{"status":"running","uptime":77}}"#;

/// `name` is what the server's certificate says it is.
async fn start(name: &str) -> MockTlsHttp {
    MockTlsHttp::start(name, REPLY_BODY).await
}

/// The plugin pointed at `server`, trusting `trust` as its CA, or only the public
/// roots when there is none.
fn driver(server: &MockTlsHttp, trust: Option<&str>) -> Arc<dyn Driver> {
    let mut config = config(&server.url);
    let secrets = match trust {
        Some(pem) => Secrets::of([("token", PROXMOX_TOKEN), ("ca", pem)]),
        None => {
            config["public_ca"] = json!(true);
            Secrets::of([("token", PROXMOX_TOKEN)])
        }
    };
    try_connect(config, secrets, four_vms(), TLS_TIMEOUTS).map_err(|e| e.to_string()).unwrap()
}

#[tokio::test]
async fn the_private_ca_is_trusted_and_the_token_arrives() {
    let server = start("127.0.0.1").await;
    let state = driver(&server, Some(&server.ca_pem)).read_state("vm100").await.unwrap();
    assert_eq!(state, State::Vm { state: "running".to_string(), uptime_s: 77 });
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains(&format!("PVEAPIToken={PROXMOX_TOKEN}")));
}

#[tokio::test]
async fn an_unknown_certificate_gets_no_token() {
    let server = start("127.0.0.1").await;
    let error = driver(&server, None).read_state("vm100").await.unwrap_err();
    assert!(matches!(error, DeviceError::Unreachable(_)), "{error:?}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(server.requests.lock().unwrap().is_empty(), "the token reached an untrusted server");
}

#[tokio::test]
async fn a_certificate_from_another_ca_gets_no_token() {
    let server = start("127.0.0.1").await;
    let other = start("127.0.0.1").await;
    let error = driver(&server, Some(&other.ca_pem)).read_state("vm100").await.unwrap_err();
    assert!(matches!(error, DeviceError::Unreachable(_)), "{error:?}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(server.requests.lock().unwrap().is_empty(), "the token reached an untrusted server");
}

#[tokio::test]
async fn a_certificate_for_another_host_gets_no_token() {
    let server = start(ANOTHER_HOST).await;
    let error = driver(&server, Some(&server.ca_pem)).read_state("vm100").await.unwrap_err();
    assert!(matches!(error, DeviceError::Unreachable(_)), "{error:?}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(server.requests.lock().unwrap().is_empty(), "the token reached the wrong host");
}

// ----- the VNC websocket, which has its own TLS client

async fn start_console(name: &str) -> MockTlsWs {
    MockTlsWs::start(name).await
}

/// A console grant as `vncproxy` would hand it out.
fn grant(target: &str) -> ConsoleGrant {
    ConsoleGrant { target: target.to_string(), port: 5901, ticket: "t".to_string().into() }
}

/// The plugin with `server` as its configured host, `vm102` on the console list.
fn console_driver(server: &MockTlsWs, trust: Option<&str>) -> Arc<dyn Driver> {
    let mut config = config(&format!("https://{}", server.address));
    let secrets = match trust {
        Some(pem) => Secrets::of([("token", PROXMOX_TOKEN), ("ca", pem)]),
        None => {
            config["public_ca"] = json!(true);
            Secrets::of([("token", PROXMOX_TOKEN)])
        }
    };
    try_connect(config, secrets, four_vms(), TLS_TIMEOUTS).map_err(|e| e.to_string()).unwrap()
}

/// The plugin with `server` as a further host `two`, trusting `ca_pem` for it, and
/// `two-vm200` on the console list. The configured host is never asked.
fn further_console(server: &MockTlsWs, ca_pem: &str, files: &tempfile::TempDir) -> Arc<dyn Driver> {
    let config = json!({
        "url": "https://127.0.0.1:9",
        "node": NODE,
        "public_ca": true,
        "console_targets": [{ "vm": "two-vm200" }],
        "host_files_dir": files.path(),
    });
    let driver = try_connect(config, Secrets::of([("token", PROXMOX_TOKEN)]), vec![vm("two-vm200")], TLS_TIMEOUTS)
        .map_err(|e| e.to_string())
        .unwrap();
    let spec = ExtraHost {
        id: "two".to_string(),
        address: server.address.parse().unwrap(),
        node: "pve2".to_string(),
        token_file: write_file(files.path(), "two.credential", PROXMOX_TOKEN),
        ca_file: write_file(files.path(), "two-ca.crt", ca_pem),
    };
    machines(&driver).add_host(&spec).unwrap();
    driver
}

#[tokio::test]
async fn the_console_relay_trusts_the_private_ca_and_presents_the_token() {
    let server = start_console("127.0.0.1").await;
    let driver = console_driver(&server, Some(&server.ca_pem));
    let mut link = machines(&driver).connect_console(&grant("vm102")).await.map_err(|e| e.to_string()).unwrap();
    link.send(b"over-tls".to_vec()).await.unwrap();
    assert_eq!(link.recv().await.unwrap(), b"over-tls");
    let seen = server.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].contains("/vncwebsocket?port=5901&vncticket=t"), "{}", seen[0]);
    assert!(seen[0].ends_with(&format!("PVEAPIToken={PROXMOX_TOKEN}")), "{}", seen[0]);
}

#[tokio::test]
async fn the_console_relay_gives_no_token_to_an_unknown_certificate() {
    let server = start_console("127.0.0.1").await;
    let driver = console_driver(&server, None);
    assert!(machines(&driver).connect_console(&grant("vm102")).await.is_err());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(server.seen.lock().unwrap().is_empty(), "the token reached an untrusted server");
}

#[tokio::test]
async fn the_console_relay_gives_no_token_to_another_cas_certificate() {
    let server = start_console("127.0.0.1").await;
    let other = start_console("127.0.0.1").await;
    let driver = console_driver(&server, Some(&other.ca_pem));
    assert!(machines(&driver).connect_console(&grant("vm102")).await.is_err());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(server.seen.lock().unwrap().is_empty(), "the token reached an untrusted server");
}

#[tokio::test]
async fn the_console_relay_gives_no_token_to_a_certificate_for_another_host() {
    let server = start_console(ANOTHER_HOST).await;
    let driver = console_driver(&server, Some(&server.ca_pem));
    assert!(machines(&driver).connect_console(&grant("vm102")).await.is_err());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(server.seen.lock().unwrap().is_empty(), "the token reached the wrong host");
}

#[tokio::test]
async fn a_relay_limited_to_the_named_ca_still_trusts_that_ca() {
    let server = start_console("127.0.0.1").await;
    let files = tempfile::tempdir().unwrap();
    let driver = further_console(&server, &server.ca_pem, &files);
    let mut link = machines(&driver).connect_console(&grant("two-vm200")).await.map_err(|e| e.to_string()).unwrap();
    link.send(b"over-tls".to_vec()).await.unwrap();
    assert_eq!(link.recv().await.unwrap(), b"over-tls");
    let seen = server.seen.lock().unwrap();
    assert!(seen[0].starts_with("/api2/json/nodes/pve2/qemu/200/vncwebsocket?port=5901&vncticket=t "), "{}", seen[0]);
}

#[tokio::test]
async fn a_relay_limited_to_the_named_ca_trusts_nothing_else() {
    let server = start_console("127.0.0.1").await;
    let other = start_console("127.0.0.1").await;
    // A further host always names a CA, so "no CA at all" is asked of the relay itself.
    let url = url::Url::parse(&format!(
        "wss://{}/api2/json/nodes/pve2/qemu/200/vncwebsocket?port=5901&vncticket=t",
        server.address
    ))
    .unwrap();
    let authorization = http::HeaderValue::from_str(&format!("PVEAPIToken={PROXMOX_TOKEN}")).unwrap();
    let grant_to_relay = swifty_plugin_api::net::LiveGrant::when_live(true).unwrap();
    assert!(console_connect(grant_to_relay, &url, &authorization, None, Roots::NamedCaOnly, TLS_TIMEOUTS)
        .await
        .is_err());
    // And a further host whose named CA is another one.
    let files = tempfile::tempdir().unwrap();
    let driver = further_console(&server, &other.ca_pem, &files);
    assert!(machines(&driver).connect_console(&grant("two-vm200")).await.is_err());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(server.seen.lock().unwrap().is_empty(), "the token reached an untrusted server");
}
