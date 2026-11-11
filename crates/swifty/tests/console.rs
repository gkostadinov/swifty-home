//! Console sessions, end to end: a real server and a real websocket on loopback,
//! the Proxmox plugin, and a mock of the Proxmox VNC proxy. Which VMs have a
//! console is the plugin's `console_targets`: the rig lists vm101 to vm103.

mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::devices::*;
use common::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use swifty_hub::console::{TICKET_PROTOCOL_PREFIX, TICKET_TTL_MS};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;

type Socket = WebSocketStream<TcpStream>;

async fn live() -> (Rig, Harness, SoftPasskey, String) {
    rig_harness(true, |_| {}).await
}

/// Opens the websocket the way a browser does.
async fn connect(served: &Served, cookie: &str, origin: Option<&str>, ticket: &str) -> Result<Socket, String> {
    let url = format!("ws://{}/api/v1/console/connect", served.addr);
    let mut request = url.into_client_request().unwrap();
    let headers = request.headers_mut();
    headers.insert("cookie", HeaderValue::from_str(cookie).unwrap());
    if let Some(origin) = origin {
        headers.insert("origin", HeaderValue::from_str(origin).unwrap());
    }
    let protocols = format!("binary, {TICKET_PROTOCOL_PREFIX}{ticket}");
    headers.insert("sec-websocket-protocol", HeaderValue::from_str(&protocols).unwrap());
    let stream = TcpStream::connect(served.addr).await.unwrap();
    match tokio_tungstenite::client_async(request, stream).await {
        Ok((socket, _)) => Ok(socket),
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => Err(response.status().to_string()),
        Err(error) => Err(error.to_string()),
    }
}

async fn open(harness: &Harness, cookie: &str, auth: &mut SoftPasskey, target: &str) -> Resp {
    harness.critical(cookie, auth, "console.open", target, json!({})).await
}

fn ticket(done: &Resp) -> String {
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    done.body["console"]["ticket"].as_str().unwrap().to_string()
}

async fn next_binary(socket: &mut Socket) -> Vec<u8> {
    let message = tokio::time::timeout(Duration::from_secs(5), socket.next()).await.unwrap().unwrap().unwrap();
    match message {
        Message::Binary(data) => data.to_vec(),
        other => panic!("expected binary, got {other:?}"),
    }
}

async fn assert_closes(socket: &mut Socket) {
    let waited = tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            match socket.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(waited.is_ok(), "the console stayed open");
}

#[tokio::test]
async fn an_approved_console_relays_bytes_both_ways() {
    let (rig, harness, mut auth, cookie) = live().await;
    let served = harness.serve().await;
    let done = open(&harness, &cookie, &mut auth, "vm102").await;
    assert_eq!(done.body["step_up"], json!("passkey"));
    let ticket = ticket(&done);
    assert_eq!(done.body["console"]["vnc_credential"], json!(VNC_TICKET));
    assert_eq!(done.body["console"]["path"], json!("/api/v1/console/connect"));
    assert_eq!(done.body["console"]["subprotocols"][0], json!("binary"));

    let mut socket = connect(&served, &cookie, Some(ORIGIN), &ticket).await.unwrap();
    assert_eq!(next_binary(&mut socket).await, b"RFB 003.008\n");
    socket.send(Message::binary(b"key-press".to_vec())).await.unwrap();
    assert_eq!(next_binary(&mut socket).await, b"vm:key-press");

    let log = rig.proxmox.console.lock().unwrap().clone();
    assert_eq!(log.connections, 1);
    assert_eq!(log.authorization, [format!("PVEAPIToken={PROXMOX_TOKEN}")]);
    assert!(log.uri.contains(&("port".to_string(), VNC_PORT.to_string())), "{:?}", log.uri);
    assert!(log.uri.contains(&("vncticket".to_string(), VNC_TICKET.to_string())), "{:?}", log.uri);
    assert_eq!(log.from_browser, [b"key-press".to_vec()]);
    let requests = rig.proxmox.node.lock().unwrap().vncproxy_requests.clone();
    assert_eq!(requests, ["102:websocket=1"]);

    socket.close(None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(harness.audit_has("connected"));
    assert!(harness.audit_has("browser_closed"));
}

#[tokio::test]
async fn every_console_session_is_audited_with_its_person_and_vm() {
    let (_rig, harness, mut auth, cookie) = live().await;
    let served = harness.serve().await;
    let owner = harness.get("/api/v1/me", &cookie).await.body["session"]["user_id"].as_str().unwrap().to_string();
    let ticket = ticket(&open(&harness, &cookie, &mut auth, "vm103").await);
    let mut socket = connect(&served, &cookie, Some(ORIGIN), &ticket).await.unwrap();
    next_binary(&mut socket).await;
    socket.close(None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let rows = harness
        .app
        .store
        .with(|conn| {
            let mut statement = conn.prepare(
                "SELECT action, target, result, user_ref, step_up FROM audit_log WHERE action LIKE 'console.%' ORDER BY seq",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, String>(4)?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap();
    let results: Vec<(&str, &str)> = rows.iter().map(|r| (r.0.as_str(), r.2.as_str())).collect();
    assert_eq!(
        results,
        [
            ("console.open", "prepared"),
            ("console.open", "commanded"),
            ("console.open", "confirmed"),
            ("console.connect", "connected"),
            ("console.disconnect", "browser_closed"),
        ]
    );
    assert!(rows.iter().all(|r| r.1 == "vm103" && r.3 == owner));
    assert!(rows.iter().any(|r| r.4 == "passkey"));
}

#[tokio::test]
async fn nothing_secret_about_a_console_is_kept() {
    let (_rig, harness, mut auth, cookie) = live().await;
    let done = open(&harness, &cookie, &mut auth, "vm102").await;
    let ticket = ticket(&done);
    for table in ["audit_log", "action_runs", "prepared_actions", "ceremonies"] {
        let dump = harness.dump(table);
        assert!(!dump.contains(&ticket), "{table} holds the ticket");
        assert!(!dump.contains(VNC_TICKET), "{table} holds the Proxmox ticket");
    }
}

#[tokio::test]
async fn a_ticket_works_once() {
    let (rig, harness, mut auth, cookie) = live().await;
    let served = harness.serve().await;
    let ticket = ticket(&open(&harness, &cookie, &mut auth, "vm102").await);
    let mut first = connect(&served, &cookie, Some(ORIGIN), &ticket).await.unwrap();
    next_binary(&mut first).await;
    let second = connect(&served, &cookie, Some(ORIGIN), &ticket).await.unwrap_err();
    assert!(second.contains("403"), "{second}");
    assert_eq!(rig.proxmox.console.lock().unwrap().connections, 1);
    assert!(harness.audit_has("refused: invalid_console_ticket"));
}

#[tokio::test]
async fn a_ticket_expires() {
    let (rig, harness, mut auth, cookie) = live().await;
    let served = harness.serve().await;
    let ticket = ticket(&open(&harness, &cookie, &mut auth, "vm102").await);
    harness.advance(TICKET_TTL_MS);
    let refused = connect(&served, &cookie, Some(ORIGIN), &ticket).await.unwrap_err();
    assert!(refused.contains("403"), "{refused}");
    assert_eq!(rig.proxmox.console.lock().unwrap().connections, 0);
}

#[tokio::test]
async fn a_ticket_belongs_to_the_session_that_asked() {
    let (rig, harness, mut auth, cookie) = live().await;
    let served = harness.serve().await;
    let ticket = ticket(&open(&harness, &cookie, &mut auth, "vm102").await);
    let other_session = harness.login(&mut auth).await;
    let refused = connect(&served, &other_session, Some(ORIGIN), &ticket).await.unwrap_err();
    assert!(refused.contains("403"), "{refused}");
    // It is spent by the attempt as well.
    let again = connect(&served, &cookie, Some(ORIGIN), &ticket).await.unwrap_err();
    assert!(again.contains("403"), "{again}");
    assert_eq!(rig.proxmox.console.lock().unwrap().connections, 0);
}

#[tokio::test]
async fn the_websocket_checks_its_origin_itself() {
    let (rig, harness, mut auth, cookie) = live().await;
    let served = harness.serve().await;
    let ticket = ticket(&open(&harness, &cookie, &mut auth, "vm102").await);
    for origin in [Some(EVIL_ORIGIN), None] {
        let refused = connect(&served, &cookie, origin, &ticket).await.unwrap_err();
        assert!(refused.contains("403"), "{refused}");
    }
    assert_eq!(rig.proxmox.console.lock().unwrap().connections, 0);
}

#[tokio::test]
async fn no_session_means_no_console() {
    let (rig, harness, mut auth, cookie) = live().await;
    let served = harness.serve().await;
    let ticket = ticket(&open(&harness, &cookie, &mut auth, "vm102").await);
    let refused = connect(&served, "__Host-swifty-session=nothing", Some(ORIGIN), &ticket).await.unwrap_err();
    assert!(refused.contains("401"), "{refused}");
    assert_eq!(rig.proxmox.console.lock().unwrap().connections, 0);
}

#[tokio::test]
async fn a_console_needs_a_step_up_every_time() {
    let (rig, harness, mut auth, cookie) = live().await;
    harness
        .execute(&cookie, "console.open", "vm102", json!({}))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "step_up_required");
    let prepared = harness.prepare(&cookie, "console.open", "vm102", json!({})).await;
    assert_eq!(prepared.body["needs"], json!("passkey"));
    assert!(prepared.body["confirmation"].is_null());
    assert!(rig.proxmox.node.lock().unwrap().vncproxy_requests.is_empty());

    ticket(&open(&harness, &cookie, &mut auth, "vm102").await);
    ticket(&open(&harness, &cookie, &mut auth, "vm102").await);
    assert_eq!(rig.proxmox.node.lock().unwrap().vncproxy_requests.len(), 2);
}

#[tokio::test]
async fn a_step_up_for_one_vm_cannot_open_another() {
    let (rig, harness, mut auth, cookie) = live().await;
    let prepared = harness.prepare(&cookie, "console.open", "vm102", json!({})).await;
    let started = harness.step_up_start(&cookie, &prepared).await;
    let assertion = assert_with(&mut auth, &started.body["options"], origin_url());
    let swapped = harness
        .post(
            "/api/v1/actions/confirm",
            &cookie,
            json!({
                "prepared_id": prepared.body["prepared_id"],
                "action": "console.open",
                "target": "vm103",
                "params": {},
                "assertion": assertion,
            }),
        )
        .await;
    swapped.assert_refused(StatusCode::CONFLICT, "binding_mismatch");
    assert!(rig.proxmox.node.lock().unwrap().vncproxy_requests.is_empty());
}

fn console_list(site: &mut common::devices::Site, vms: &[&str]) {
    let list: Vec<serde_json::Value> = vms.iter().map(|vm| json!({ "vm": vm })).collect();
    site.instance("proxmox")["console_targets"] = json!(list);
}

#[tokio::test]
async fn a_vm_off_the_console_list_has_no_console() {
    let (rig, harness, _auth, cookie) = live().await;
    let refused = harness.prepare(&cookie, "console.open", "vm100", json!({})).await;
    refused.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
    assert!(harness.app.registry.index().has("vm101", swifty_plugin_api::CapKind::Console));
    assert!(!harness.app.registry.index().has("vm100", swifty_plugin_api::CapKind::Console));
    assert_eq!(rig.traffic(), 0);
}

#[tokio::test]
async fn the_console_list_is_the_plugins_to_widen() {
    let (rig, harness, _auth, cookie) = rig_harness(true, |site| console_list(site, &["vm100", "vm101"])).await;
    harness.prepare(&cookie, "console.open", "vm100", json!({})).await.assert_ok();
    assert!(rig.proxmox.node.lock().unwrap().vncproxy_requests.is_empty(), "preparing asks Proxmox nothing");
}

#[tokio::test]
async fn the_console_list_can_be_narrowed() {
    let (rig, harness, _auth, cookie) = rig_harness(true, |site| console_list(site, &["vm102"])).await;
    harness
        .prepare(&cookie, "console.open", "vm103", json!({}))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
    harness.prepare(&cookie, "console.open", "vm102", json!({})).await.assert_ok();
    assert!(rig.proxmox.node.lock().unwrap().vncproxy_requests.is_empty());
}

#[tokio::test]
async fn there_is_no_console_on_the_node_itself() {
    let (rig, harness, _auth, cookie) = live().await;
    for target in ["host", NODE, "vm102/../../status", "router"] {
        harness
            .prepare(&cookie, "console.open", target, json!({}))
            .await
            .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
    }
    assert_eq!(rig.traffic(), 0);
}

#[test]
fn the_source_never_names_a_node_shell_endpoint() {
    let text = [
        include_str!("../../../plugins/proxmox/src/protocol.rs"),
        include_str!("../../../plugins/proxmox/src/driver.rs"),
    ]
    .concat();
    // The protocol module's own comment names two of them once to say they are not spoken.
    for endpoint in ["vncshell", "termproxy", "spiceshell"] {
        assert!(text.matches(endpoint).count() <= 1, "{endpoint} is used");
        assert!(!text.contains(&format!("{endpoint}\"")), "{endpoint} is used");
        assert!(!text.contains(&format!("/{endpoint}")), "{endpoint} is used");
    }
}

#[tokio::test]
async fn family_can_never_open_a_console() {
    let (rig, harness, mut owner, cookie) = live().await;
    let (_family, family_cookie, _id) = harness.add_family(&cookie, &mut owner, "Alice").await;
    let served = harness.serve().await;
    harness
        .prepare(&family_cookie, "console.open", "vm102", json!({}))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "owner_only");
    let ticket = ticket(&open(&harness, &cookie, &mut owner, "vm102").await);
    let refused = connect(&served, &family_cookie, Some(ORIGIN), &ticket).await.unwrap_err();
    assert!(refused.contains("403"), "{refused}");
    assert_eq!(rig.proxmox.console.lock().unwrap().connections, 0);
}

#[tokio::test]
async fn a_pending_or_removed_machine_has_no_console() {
    let (rig, harness, _auth, cookie) =
        rig_harness(true, |site| console_list(site, &["vm101", "vm102", "vm103", "vm150"])).await;
    rig.proxmox.node.lock().unwrap().listed.push((150, "new".to_string(), false));
    harness.post("/api/v1/machines/scan", &cookie, json!({})).await.assert_ok();
    harness
        .prepare(&cookie, "console.open", "vm150", json!({}))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "machine_not_approved");
    assert!(rig.proxmox.node.lock().unwrap().vncproxy_requests.is_empty());
}

/// What the log says about consoles of one kind, as (target, result).
fn console_rows(harness: &Harness, action: &str) -> Vec<(String, String)> {
    harness
        .app
        .store
        .with(|conn| {
            let mut statement = conn.prepare("SELECT target, result FROM audit_log WHERE action = ?1 ORDER BY seq")?;
            let rows = statement.query_map([action], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<(String, String)>>>()
        })
        .unwrap()
}

#[tokio::test]
async fn removing_a_machine_closes_its_open_console_and_no_other() {
    let (_rig, harness, mut auth, cookie) = live().await;
    let served = harness.serve().await;
    let removed = ticket(&open(&harness, &cookie, &mut auth, "vm103").await);
    let other = ticket(&open(&harness, &cookie, &mut auth, "vm101").await);
    let mut removed = connect(&served, &cookie, Some(ORIGIN), &removed).await.unwrap();
    let mut other = connect(&served, &cookie, Some(ORIGIN), &other).await.unwrap();
    next_binary(&mut removed).await;
    next_binary(&mut other).await;

    let done = harness.critical(&cookie, &mut auth, "machine.remove", "vm103", json!({})).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_closes(&mut removed).await;

    // Past the next check, the other console is still carrying bytes.
    tokio::time::sleep(Duration::from_millis(1_300)).await;
    other.send(Message::binary(b"still-here".to_vec())).await.unwrap();
    assert_eq!(next_binary(&mut other).await, b"vm:still-here");
    assert_eq!(console_rows(&harness, "console.disconnect"), [("vm103".to_string(), "machine_removed".to_string())]);
}

#[tokio::test]
async fn a_refusal_after_the_ticket_is_spent_names_the_vm() {
    let (rig, harness, mut auth, cookie) = live().await;
    let served = harness.serve().await;
    let granted = ticket(&open(&harness, &cookie, &mut auth, "vm103").await);
    harness.critical(&cookie, &mut auth, "machine.remove", "vm103", json!({})).await.assert_ok();
    let refused = connect(&served, &cookie, Some(ORIGIN), &granted).await.unwrap_err();
    assert!(refused.contains("502"), "{refused}");
    assert_eq!(rig.proxmox.console.lock().unwrap().connections, 0);

    // Until a ticket is spent there is no VM to name.
    let refused = connect(&served, &cookie, Some(ORIGIN), "no-such-ticket").await.unwrap_err();
    assert!(refused.contains("403"), "{refused}");
    assert_eq!(
        console_rows(&harness, "console.connect"),
        [
            ("vm103".to_string(), "refused: console_unreachable".to_string()),
            ("-".to_string(), "refused: invalid_console_ticket".to_string()),
        ]
    );
}

#[tokio::test]
async fn locking_the_session_closes_the_console() {
    let (_rig, harness, mut auth, cookie) = live().await;
    let served = harness.serve().await;
    let ticket = ticket(&open(&harness, &cookie, &mut auth, "vm102").await);
    let mut socket = connect(&served, &cookie, Some(ORIGIN), &ticket).await.unwrap();
    next_binary(&mut socket).await;
    harness.post("/api/v1/session/lock", &cookie, json!({})).await.assert_ok();
    assert_closes(&mut socket).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(harness.audit_has("session_ended"));
}

#[tokio::test]
async fn ending_the_session_closes_the_console() {
    let (_rig, harness, mut auth, cookie) = live().await;
    let served = harness.serve().await;
    let ticket = ticket(&open(&harness, &cookie, &mut auth, "vm102").await);
    let mut socket = connect(&served, &cookie, Some(ORIGIN), &ticket).await.unwrap();
    next_binary(&mut socket).await;
    harness.send(Method::POST, "/api/v1/auth/logout", Some(&cookie), Some(ORIGIN), None).await.assert_ok();
    assert_closes(&mut socket).await;
}

#[tokio::test]
async fn the_vm_side_closing_closes_the_console() {
    let (_rig, harness, mut auth, cookie) = live().await;
    let served = harness.serve().await;
    let ticket = ticket(&open(&harness, &cookie, &mut auth, "vm102").await);
    let mut socket = connect(&served, &cookie, Some(ORIGIN), &ticket).await.unwrap();
    next_binary(&mut socket).await;
    socket.send(Message::binary(b"__vm_goes_away".to_vec())).await.unwrap();
    assert_closes(&mut socket).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(harness.audit_has("vm_closed"));
}

#[tokio::test]
async fn a_refusal_from_proxmox_gives_no_ticket() {
    let (rig, harness, mut auth, cookie) = live().await;
    rig.proxmox.node.lock().unwrap().down = true;
    let done = open(&harness, &cookie, &mut auth, "vm102").await;
    assert_eq!(done.phase(), "failed", "{}", done.text);
    assert!(done.body.get("console").is_none());
}

#[tokio::test]
async fn in_dry_run_a_console_contacts_nothing_and_gives_no_ticket() {
    let (rig, harness, mut auth, cookie) = rig_harness(false, |_| {}).await;
    let done = open(&harness, &cookie, &mut auth, "vm102").await;
    assert_eq!(done.phase(), "simulated", "{}", done.text);
    assert_eq!(done.body["simulated"], json!(true));
    assert!(done.body.get("console").is_none());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.traffic(), 0);
}
