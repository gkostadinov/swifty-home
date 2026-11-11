//! The primary node's power, the stored machine list, further Proxmox hosts, and
//! agent pairing, against mock servers on loopback.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::devices::*;
use common::*;
use serde_json::{json, Value};

use swifty_hub::app::App;
use swifty_hub::catalog::{self, ACK_PARAM};
use swifty_hub::config::Config;
use swifty_hub::{audit, store};
use swifty_plugin_api::{DeviceClass, State};
use webauthn_authenticator_rs::softpasskey::SoftPasskey;

async fn live() -> (Rig, Harness, SoftPasskey, String) {
    rig_harness(true, |_| {}).await
}

// ----- node power

#[tokio::test]
async fn a_reboot_is_critical_and_says_what_goes_offline() {
    let (rig, harness, mut auth, cookie) = live().await;
    harness
        .execute(&cookie, "host.reboot", "host", json!({}))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "step_up_required");
    let prepared = harness.prepare(&cookie, "host.reboot", "host", json!({})).await;
    prepared.assert_ok();
    assert_eq!(prepared.body["needs"], json!("passkey"));
    assert!(prepared.body["warning"].as_str().unwrap().contains("offline"), "{}", prepared.text);
    assert!(rig.proxmox.node.lock().unwrap().commands.is_empty());

    let done = harness.critical(&cookie, &mut auth, "host.reboot", "host", json!({})).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert!(done.body["warning"].as_str().unwrap().contains("offline"), "{}", done.text);
    assert_eq!(rig.proxmox.node.lock().unwrap().commands, ["command=reboot"]);
    let node_status = format!("/nodes/{NODE}/status");
    let post = rig.proxmox.http.posts().into_iter().find(|r| r.path.ends_with(&node_status)).unwrap();
    assert_eq!(post.header("authorization"), format!("PVEAPIToken={PROXMOX_TOKEN}"));
    assert_eq!(post.header("content-type"), "application/x-www-form-urlencoded");
}

#[tokio::test]
async fn a_shutdown_warns_it_cannot_be_undone_from_the_page() {
    let (rig, harness, mut auth, cookie) = live().await;
    let prepared = harness.prepare(&cookie, "host.shutdown", "host", json!({})).await;
    let warning = prepared.body["warning"].as_str().unwrap();
    assert!(warning.contains("offline") && warning.contains("cannot be undone from this page"), "{warning}");

    let done = harness.critical(&cookie, &mut auth, "host.shutdown", "host", json!({})).await;
    assert_eq!(rig.proxmox.node.lock().unwrap().commands, ["command=shutdown"]);
    // A node that has gone away cannot confirm it, so this is never a success.
    assert_eq!(done.phase(), "no_response", "{}", done.text);
    assert!(done.body["detail"].as_str().unwrap().contains("nothing could confirm"), "{}", done.text);
    let warning = done.body["warning"].as_str().unwrap();
    assert!(warning.contains("cannot be undone from this page"), "{warning}");

    let run = harness.get(&format!("/api/v1/actions/runs/{}", done.body["run_id"].as_str().unwrap()), &cookie).await;
    assert!(run.body["warning"].as_str().unwrap().contains("cannot be undone"), "{}", run.text);
}

#[tokio::test]
async fn node_power_is_owner_only_and_never_by_voice_alone() {
    let speech = FakeSpeech::start().await;
    let config = speech.config();
    let (rig, harness, mut owner, cookie) = rig_harness(true, move |c| c.speech = Some(config)).await;
    let (_family, family_cookie, _id) = harness.add_family(&cookie, &mut owner, "Alice").await;
    for action in ["host.reboot", "host.shutdown"] {
        harness
            .prepare(&family_cookie, action, "host", json!({}))
            .await
            .assert_refused(StatusCode::FORBIDDEN, "owner_only");
    }
    speech.say("reboot the host");
    let spoken = post_voice(&harness, &cookie).await;
    assert_eq!(spoken.body["kind"], json!("prepared"), "{}", spoken.text);
    assert_eq!(spoken.body["needs"], json!("passkey"));
    assert!(spoken.text.contains("offline"), "the warning reaches voice too");
    assert!(rig.proxmox.node.lock().unwrap().commands.is_empty());
}

#[tokio::test]
async fn in_dry_run_node_power_sends_nothing() {
    let (rig, harness, mut auth, cookie) = rig_harness(false, |_| {}).await;
    for action in ["host.reboot", "host.shutdown"] {
        let done = harness.critical(&cookie, &mut auth, action, "host", json!({})).await;
        assert_eq!(done.phase(), "simulated", "{}", done.text);
        assert!(done.body["warning"].as_str().unwrap().contains("offline"));
        harness.advance(10_001);
    }
    assert_eq!(rig.traffic(), 0);
}

#[tokio::test]
async fn a_token_without_power_rights_gets_a_clear_refusal() {
    let (rig, harness, mut auth, cookie) = live().await;
    rig.proxmox.node.lock().unwrap().power_forbidden = true;
    let done = harness.critical(&cookie, &mut auth, "host.reboot", "host", json!({})).await;
    assert_eq!(done.phase(), "failed", "{}", done.text);
    assert!(done.body["detail"].as_str().unwrap().contains("not allowed"), "{}", done.text);
    assert!(rig.proxmox.node.lock().unwrap().commands.is_empty());
}

// ----- the stored list

#[tokio::test]
async fn the_list_starts_with_the_four_machines() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let list = harness.get("/api/v1/machines", &cookie).await;
    let rows = list.body["machines"].as_array().unwrap();
    let targets: Vec<&str> = rows.iter().map(|m| m["target"].as_str().unwrap()).collect();
    assert_eq!(targets, ["vm100", "vm101", "vm102", "vm103"]);
    assert!(rows.iter().all(|m| m["status"] == json!("approved") && m["host"] == json!("primary")));
}

#[tokio::test]
async fn a_new_vm_is_pending_and_cannot_be_controlled() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.proxmox.node.lock().unwrap().listed.push((150, "new-vm".to_string(), false));
    rig.proxmox.vms.lock().unwrap().insert(150, VmState { status: "stopped".to_string(), uptime: 0 });

    let scan = harness.post("/api/v1/machines/scan", &cookie, json!({})).await;
    scan.assert_ok();
    assert_eq!(scan.body["added"], json!(["vm150"]));
    let list = harness.get("/api/v1/machines", &cookie).await;
    let row = list.body["machines"].as_array().unwrap().iter().find(|m| m["target"] == json!("vm150")).unwrap().clone();
    assert_eq!(row["status"], json!("pending"));

    rig.proxmox.http.seen.lock().unwrap().clear();
    harness
        .prepare(&cookie, "vm.start", "vm150", json!({}))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "machine_not_approved");
    harness
        .prepare(&cookie, "vm.stop", "vm150", json!({}))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "machine_not_approved");
    harness
        .execute(&cookie, "state.read", "vm150", json!({}))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "machine_not_approved");
    harness.get("/api/v1/state/vm150", &cookie).await.assert_refused(StatusCode::NOT_FOUND, "unknown_device");
    assert!(rig.proxmox.http.requests().is_empty(), "a pending machine must not be contacted");
    let devices = harness.get("/api/v1/devices", &cookie).await;
    assert!(!devices.text.contains("vm150"));
}

#[tokio::test]
async fn approving_takes_a_step_up_and_then_it_can_be_controlled() {
    let (rig, harness, mut auth, cookie) = live().await;
    rig.proxmox.node.lock().unwrap().listed.push((150, "new-vm".to_string(), false));
    rig.proxmox.vms.lock().unwrap().insert(150, VmState { status: "stopped".to_string(), uptime: 0 });
    harness.post("/api/v1/machines/scan", &cookie, json!({})).await.assert_ok();

    harness
        .execute(&cookie, "machine.approve", "vm150", json!({}))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "step_up_required");
    let approved = harness.critical(&cookie, &mut auth, "machine.approve", "vm150", json!({})).await;
    assert_eq!(approved.phase(), "confirmed", "{}", approved.text);
    assert_eq!(approved.body["step_up"], json!("passkey"));

    let prepared = harness.prepare(&cookie, "vm.start", "vm150", json!({})).await;
    prepared.assert_ok();
    let done = harness.confirm_tap(&cookie, &prepared).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert!(rig.proxmox.http.posts().iter().any(|r| r.path.ends_with("/qemu/150/status/start")));
    let state = harness.get("/api/v1/state/vm150", &cookie).await;
    assert_eq!(state.body["available"], json!(true), "{}", state.text);
    assert!(harness.get("/api/v1/devices", &cookie).await.text.contains("vm150"));
    assert!(harness.audit_has("confirmed"));
}

#[tokio::test]
async fn only_the_owner_can_approve() {
    let (rig, harness, mut owner, cookie) = live().await;
    let (_family, family_cookie, _id) = harness.add_family(&cookie, &mut owner, "Alice").await;
    rig.proxmox.node.lock().unwrap().listed.push((150, "new-vm".to_string(), false));
    harness.post("/api/v1/machines/scan", &cookie, json!({})).await.assert_ok();
    harness
        .prepare(&family_cookie, "machine.approve", "vm150", json!({}))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "owner_only");
    let list = harness.get("/api/v1/machines", &cookie).await;
    assert!(list.text.contains("\"pending\""));
}

#[tokio::test]
async fn an_approved_machine_is_not_approved_again_and_an_unseen_one_cannot_be() {
    let (_rig, harness, _auth, cookie) = live().await;
    harness
        .prepare(&cookie, "machine.approve", "vm100", json!({}))
        .await
        .assert_refused(StatusCode::CONFLICT, "already_approved");
    harness
        .prepare(&cookie, "machine.approve", "vm999", json!({}))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
}

#[tokio::test]
async fn a_scan_ignores_ids_proxmox_could_not_have() {
    let (rig, harness, _auth, cookie) = live().await;
    {
        let mut node = rig.proxmox.node.lock().unwrap();
        node.listed.push((1, "too low".to_string(), false));
        node.listed.push((4_000_000_000, "too high".to_string(), false));
    }
    let scan = harness.post("/api/v1/machines/scan", &cookie, json!({})).await;
    assert_eq!(scan.body["added"], json!([]), "{}", scan.text);
}

#[tokio::test]
async fn a_scan_cannot_approve_anything_or_touch_an_approved_machine() {
    let (rig, harness, _auth, cookie) = live().await;
    let scan = harness.post("/api/v1/machines/scan", &cookie, json!({})).await;
    assert_eq!(scan.body["added"], json!([]));
    let list = harness.get("/api/v1/machines", &cookie).await;
    let row = list.body["machines"].as_array().unwrap().iter().find(|m| m["target"] == json!("vm100")).unwrap().clone();
    assert_eq!(row["name"], json!("alpha"), "an approved name is not overwritten by what Proxmox says");
    // Scanning again does not add it twice.
    rig.proxmox.node.lock().unwrap().listed.push((150, "new".to_string(), false));
    harness.post("/api/v1/machines/scan", &cookie, json!({})).await.assert_ok();
    let again = harness.post("/api/v1/machines/scan", &cookie, json!({})).await;
    assert_eq!(again.body["added"], json!([]));
}

#[tokio::test]
async fn a_template_is_not_a_machine() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.proxmox.node.lock().unwrap().listed.push((9000, "template".to_string(), true));
    let scan = harness.post("/api/v1/machines/scan", &cookie, json!({})).await;
    assert_eq!(scan.body["added"], json!([]));
}

#[tokio::test]
async fn in_dry_run_a_scan_sends_nothing() {
    let (rig, harness, _auth, cookie) = rig_harness(false, |_| {}).await;
    let scan = harness.post("/api/v1/machines/scan", &cookie, json!({})).await;
    scan.assert_ok();
    assert_eq!(scan.body["simulated"], json!(true));
    assert_eq!(scan.body["added"], json!([]));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.traffic(), 0);
}

#[tokio::test]
async fn a_removed_machine_cannot_be_controlled_until_it_is_approved_again() {
    let (rig, harness, mut auth, cookie) = live().await;
    let done = harness.critical(&cookie, &mut auth, "machine.remove", "vm103", json!({})).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    harness
        .prepare(&cookie, "vm.start", "vm103", json!({}))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
    assert!(!harness.get("/api/v1/devices", &cookie).await.text.contains("vm103"));
    // The next scan lists it as pending, not approved.
    let scan = harness.post("/api/v1/machines/scan", &cookie, json!({})).await;
    assert_eq!(scan.body["added"], json!(["vm103"]));
    harness
        .prepare(&cookie, "vm.start", "vm103", json!({}))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "machine_not_approved");
    assert_eq!(rig.proxmox.http.posts().len(), 0);
}

#[tokio::test]
async fn the_vm_this_console_runs_on_cannot_be_removed() {
    let (_rig, harness, _auth, cookie) = live().await;
    harness
        .prepare(&cookie, "machine.remove", "vm102", json!({}))
        .await
        .assert_refused(StatusCode::CONFLICT, "console_machine");
}

#[tokio::test]
async fn stopping_the_console_vm_still_needs_its_acknowledgement() {
    let (_rig, harness, _auth, cookie) = live().await;
    harness
        .prepare(&cookie, "vm.stop", "vm102", json!({}))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "acknowledgement_required");
    harness.prepare(&cookie, "vm.stop", "vm102", json!({ ACK_PARAM: true })).await.assert_ok();
}

#[tokio::test]
async fn the_catalog_lists_the_machines_that_are_approved_now() {
    let (rig, harness, mut auth, cookie) = live().await;
    rig.proxmox.node.lock().unwrap().listed.push((150, "new".to_string(), false));
    harness.post("/api/v1/machines/scan", &cookie, json!({})).await.assert_ok();
    let targets_of = |catalog: &Value| {
        catalog["actions"].as_array().unwrap().iter().find(|a| a["id"] == json!("vm.start")).unwrap()["targets"].clone()
    };
    let before = harness.get("/api/v1/catalog", &cookie).await;
    assert!(!targets_of(&before.body).as_array().unwrap().contains(&json!("vm150")));
    harness.critical(&cookie, &mut auth, "machine.approve", "vm150", json!({})).await.assert_ok();
    let after = harness.get("/api/v1/catalog", &cookie).await;
    assert!(targets_of(&after.body).as_array().unwrap().contains(&json!("vm150")));
}

#[tokio::test]
async fn reading_all_vms_follows_the_stored_list() {
    let (rig, harness, mut auth, cookie) = live().await;
    harness.critical(&cookie, &mut auth, "machine.remove", "vm103", json!({})).await.assert_ok();
    let read = harness.execute(&cookie, "state.read", "all_vms", json!({})).await;
    assert_eq!(read.phase(), "confirmed", "{}", read.text);
    assert!(read.body["state"].get("vm100").is_some());
    assert!(read.body["state"].get("vm103").is_none());
    assert!(!rig.proxmox.http.requests().iter().any(|r| r.path.contains("/qemu/103/")));
}

// ----- further Proxmox hosts

fn host_params(id: &str, address: &str, token_file: &str, ca_file: &str) -> Value {
    json!({ "id": id, "address": address, "node": "pve2", "token_file": token_file, "ca_file": ca_file })
}

fn write_host_secrets(rig: &Rig, ca_pem: &str) {
    write_secret(rig.dir.path(), "pve2.token", PROXMOX_TOKEN);
    write_secret(rig.dir.path(), "pve2-ca.pem", ca_pem);
}

async fn with_further_host() -> (Rig, MockTlsPve, Harness, SoftPasskey, String) {
    let (rig, harness, mut auth, cookie) = live().await;
    let pve = MockTlsPve::start("pve2", 200).await;
    write_host_secrets(&rig, &pve.ca_pem);
    let done = harness
        .critical(
            &cookie,
            &mut auth,
            "proxmox.add_host",
            "new",
            host_params("nas2", &pve.address, "pve2.token", "pve2-ca.pem"),
        )
        .await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    (rig, pve, harness, auth, cookie)
}

#[tokio::test]
async fn a_further_host_is_reached_only_with_a_valid_certificate() {
    let (_rig, pve, harness, mut auth, cookie) = with_further_host().await;
    let scan = harness.post("/api/v1/machines/scan", &cookie, json!({})).await;
    assert_eq!(scan.body["added"], json!(["nas2-vm200"]), "{}", scan.text);
    {
        let requests = pve.requests.lock().unwrap();
        assert!(requests.iter().all(|r| r.contains(&format!("PVEAPIToken={PROXMOX_TOKEN}"))));
    }
    harness
        .prepare(&cookie, "vm.start", "nas2-vm200", json!({}))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "machine_not_approved");

    harness.critical(&cookie, &mut auth, "machine.approve", "nas2-vm200", json!({})).await.assert_ok();
    let prepared = harness.prepare(&cookie, "vm.start", "nas2-vm200", json!({})).await;
    let done = harness.confirm_tap(&cookie, &prepared).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert!(pve
        .requests
        .lock()
        .unwrap()
        .iter()
        .any(|r| r.starts_with("POST /api2/json/nodes/pve2/qemu/200/status/start ")));
    let list = harness.get("/api/v1/machines", &cookie).await;
    assert_eq!(list.body["hosts"][0]["id"], json!("nas2"));
    assert!(!list.text.contains("pve2.token"), "the file reference is not handed back");
}

#[tokio::test]
async fn a_further_host_takes_a_step_up_and_the_owner() {
    let (rig, harness, mut owner, cookie) = live().await;
    let pve = MockTlsPve::start("pve2", 200).await;
    write_host_secrets(&rig, &pve.ca_pem);
    let params = host_params("nas2", &pve.address, "pve2.token", "pve2-ca.pem");
    harness
        .execute(&cookie, "proxmox.add_host", "new", params.clone())
        .await
        .assert_refused(StatusCode::FORBIDDEN, "step_up_required");
    let (_family, family_cookie, _id) = harness.add_family(&cookie, &mut owner, "Alice").await;
    harness
        .prepare(&family_cookie, "proxmox.add_host", "new", params)
        .await
        .assert_refused(StatusCode::FORBIDDEN, "owner_only");
}

#[tokio::test]
async fn a_certificate_from_another_ca_gets_no_token() {
    let (rig, harness, mut auth, cookie) = live().await;
    let pve = MockTlsPve::start("pve2", 200).await;
    let other = MockTlsPve::start("pve2", 200).await;
    write_host_secrets(&rig, &other.ca_pem);
    harness
        .critical(
            &cookie,
            &mut auth,
            "proxmox.add_host",
            "new",
            host_params("nas2", &pve.address, "pve2.token", "pve2-ca.pem"),
        )
        .await
        .assert_ok();
    let scan = harness.post("/api/v1/machines/scan", &cookie, json!({})).await;
    assert_eq!(scan.body["added"], json!([]), "{}", scan.text);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(pve.requests.lock().unwrap().is_empty(), "the token reached an untrusted server");
}

#[tokio::test]
async fn a_further_host_comes_back_after_a_restart() {
    let (_rig, pve, harness, _auth, cookie) = with_further_host().await;
    harness.post("/api/v1/machines/scan", &cookie, json!({})).await.assert_ok();
    pve.requests.lock().unwrap().clear();

    let config = harness.app.config.clone();
    let restarted = App::new(config.clone(), harness.clock.clone() as Arc<_>, production_registry(&config)).unwrap();
    swifty_hub::background::scan_machines(&restarted).await.unwrap();
    assert!(!pve.requests.lock().unwrap().is_empty(), "the host was not loaded again");
}

async fn after_a_restart_with(harness: &Harness, config: Config) -> App {
    App::new(config.clone(), harness.clock.clone() as Arc<_>, production_registry(&config)).unwrap()
}

#[tokio::test]
async fn a_stored_host_is_checked_again_at_every_start() {
    let (_rig, pve, harness, _auth, cookie) = with_further_host().await;
    harness.post("/api/v1/machines/scan", &cookie, json!({})).await.assert_ok();

    let mut narrowed = harness.app.config.clone();
    narrowed.network.allowed_host_networks = vec!["192.168.0.0/16".to_string()];
    // Then the networks as they were, and no directory for further hosts' files.
    for (config, files) in [(narrowed, true), (harness.app.config.clone(), false)] {
        if !files {
            let drop_files =
                "UPDATE plugin_instances SET config = json_remove(config, '$.host_files_dir') WHERE id = 'proxmox'";
            harness.app.store.with(|conn| conn.execute(drop_files, [])).unwrap();
        }
        pve.requests.lock().unwrap().clear();
        let restarted = after_a_restart_with(&harness, config).await;
        swifty_hub::background::scan_machines(&restarted).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(pve.requests.lock().unwrap().is_empty(), "a host the configuration no longer allows was contacted");
    }
}

#[tokio::test]
async fn a_further_host_can_be_removed_and_is_not_contacted_again() {
    let (_rig, pve, harness, mut owner, cookie) = with_further_host().await;
    harness.post("/api/v1/machines/scan", &cookie, json!({})).await.assert_ok();
    harness.critical(&cookie, &mut owner, "machine.approve", "nas2-vm200", json!({})).await.assert_ok();

    let (_family, family_cookie, _id) = harness.add_family(&cookie, &mut owner, "Alice").await;
    harness
        .prepare(&family_cookie, "proxmox.remove_host", "nas2", json!({}))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "owner_only");
    harness
        .execute(&cookie, "proxmox.remove_host", "nas2", json!({}))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "step_up_required");
    assert_eq!(harness.get("/api/v1/machines", &cookie).await.body["hosts"][0]["id"], json!("nas2"));

    let done = harness.critical(&cookie, &mut owner, "proxmox.remove_host", "nas2", json!({})).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(done.body["step_up"], json!("passkey"));
    let list = harness.get("/api/v1/machines", &cookie).await;
    assert_eq!(list.body["hosts"], json!([]));
    assert!(!list.text.contains("nas2-vm200"), "{}", list.text);

    pve.requests.lock().unwrap().clear();
    let scan = harness.post("/api/v1/machines/scan", &cookie, json!({})).await;
    assert_eq!(scan.body["added"], json!([]), "{}", scan.text);
    harness
        .prepare(&cookie, "vm.start", "nas2-vm200", json!({}))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");

    let restarted = after_a_restart_with(&harness, harness.app.config.clone()).await;
    swifty_hub::background::scan_machines(&restarted).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(pve.requests.lock().unwrap().is_empty(), "a removed host was contacted");
}

#[tokio::test]
async fn a_removed_host_can_be_added_again() {
    let (_rig, pve, harness, mut auth, cookie) = with_further_host().await;
    harness.critical(&cookie, &mut auth, "proxmox.remove_host", "nas2", json!({})).await.assert_ok();
    let done = harness
        .critical(
            &cookie,
            &mut auth,
            "proxmox.add_host",
            "new",
            host_params("nas2", &pve.address, "pve2.token", "pve2-ca.pem"),
        )
        .await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    let scan = harness.post("/api/v1/machines/scan", &cookie, json!({})).await;
    assert_eq!(scan.body["added"], json!(["nas2-vm200"]), "{}", scan.text);
}

#[tokio::test]
async fn only_a_further_host_that_was_added_can_be_removed() {
    let (_rig, harness, _auth, cookie) = live().await;
    for target in ["primary", "nope", "Bad", "a/b", "vm100"] {
        harness
            .prepare(&cookie, "proxmox.remove_host", target, json!({}))
            .await
            .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
    }
}

#[tokio::test]
async fn in_dry_run_removing_a_host_sends_nothing() {
    let (rig, harness, mut auth, cookie) = rig_harness(false, |_| {}).await;
    harness
        .critical(
            &cookie,
            &mut auth,
            "proxmox.add_host",
            "new",
            host_params("nas2", "127.0.0.1:9", "absent.token", "absent.pem"),
        )
        .await
        .assert_ok();
    let done = harness.critical(&cookie, &mut auth, "proxmox.remove_host", "nas2", json!({})).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(harness.get("/api/v1/machines", &cookie).await.body["hosts"], json!([]));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.traffic(), 0);
}

#[tokio::test]
async fn a_token_pasted_into_a_field_never_reaches_the_log() {
    const SECRET: &str = "11111111-2222-3333-4444-555555555555";
    assert!(PROXMOX_TOKEN.contains(SECRET));
    let (_rig, harness, mut owner, cookie) = live().await;
    let (_family, family_cookie, _id) = harness.add_family(&cookie, &mut owner, "Alice").await;
    for params in [
        host_params("nas2", "127.0.0.1:8006", PROXMOX_TOKEN, "pve2-ca.pem"),
        host_params("nas2", "127.0.0.1:8006", "pve2.token", PROXMOX_TOKEN),
        host_params("nas2", PROXMOX_TOKEN, "pve2.token", "pve2-ca.pem"),
        // Proxmox shows the secret on a line of its own, and it passes for a file name.
        host_params("nas2", "127.0.0.1:8006", SECRET, "pve2-ca.pem"),
        host_params("nas2", "127.0.0.1:8006", "pve2.token", SECRET),
    ] {
        for refused in [
            harness.prepare(&cookie, "proxmox.add_host", "new", params.clone()).await,
            harness.execute(&cookie, "proxmox.add_host", "new", params.clone()).await,
        ] {
            refused.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_params");
            assert!(!refused.text.contains(SECRET), "{}", refused.text);
        }
        harness
            .prepare(&family_cookie, "proxmox.add_host", "new", params)
            .await
            .assert_refused(StatusCode::FORBIDDEN, "owner_only");
    }
    assert_eq!(harness.audit_count("refused: invalid_params"), 10);
    for table in ["audit_log", "prepared_actions", "action_runs"] {
        assert!(!harness.dump(table).contains(SECRET), "{table} holds the token");
    }
    assert!(!harness.get("/api/v1/audit?limit=500", &cookie).await.text.contains(SECRET));
}

/// The proxmox instance without a directory for further hosts' files.
fn no_files_dir(site: &mut common::devices::Site) {
    site.instance("proxmox").as_object_mut().unwrap().remove("host_files_dir");
}

#[tokio::test]
async fn a_token_pasted_with_no_secrets_directory_is_kept_out_too() {
    let (_rig, harness, _auth, cookie) = rig_harness(true, no_files_dir).await;
    harness
        .prepare(&cookie, "proxmox.add_host", "new", host_params("nas2", "127.0.0.1:8006", PROXMOX_TOKEN, "c"))
        .await
        .assert_refused(StatusCode::CONFLICT, "hosts_not_configured");
    assert!(!harness.dump("audit_log").contains(PROXMOX_TOKEN));
}

#[tokio::test]
async fn a_refusal_keeps_the_parameters_that_passed_the_checks() {
    let (_rig, pve, harness, _auth, cookie) = with_further_host().await;
    harness
        .prepare(&cookie, "proxmox.add_host", "new", host_params("other", &pve.address, "pve2.token", "pve2-ca.pem"))
        .await
        .assert_refused(StatusCode::CONFLICT, "host_exists");
    let params: String = harness
        .app
        .store
        .with(|conn| {
            conn.query_row("SELECT params FROM audit_log WHERE result = 'refused: host_exists'", [], |row| row.get(0))
        })
        .unwrap();
    assert!(params.contains("pve2.token"), "{params}");
}

#[tokio::test]
async fn an_address_can_only_be_an_ip_on_an_allowed_network() {
    let (rig, harness, _auth, cookie) = live().await;
    write_host_secrets(&rig, "not used");
    for address in [
        "https://127.0.0.1:8006",
        "http://127.0.0.1:8006",
        "127.0.0.1:8006/api2/json",
        "127.0.0.1:8006?x=1",
        "127.0.0.1:8006#x",
        "user@127.0.0.1",
        "user:x@127.0.0.1:8006",
        "pve.example.invalid",
        "localhost",
        "8.8.8.8",
        "1.1.1.1:8006",
        "169.254.169.254",
        "0.0.0.0",
        "224.0.0.1",
        "255.255.255.255",
        "[fe80::1]:8006",
        "::1",
        "127.0.0.1:0",
    ] {
        let refused = harness
            .prepare(&cookie, "proxmox.add_host", "new", host_params("nas2", address, "pve2.token", "pve2-ca.pem"))
            .await;
        refused.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_params");
    }
    assert_eq!(rig.traffic(), 0);
    assert_eq!(harness.get("/api/v1/machines", &cookie).await.body["hosts"], json!([]));
}

#[tokio::test]
async fn files_can_only_come_from_the_secrets_directory() {
    let (rig, harness, _auth, cookie) = live().await;
    for (token, ca) in [
        ("../pve2.token", "ca.pem"),
        ("pve2.token", "../ca.pem"),
        ("/etc/shadow", "ca.pem"),
        ("C:\\Windows\\win.ini", "ca.pem"),
        ("sub/dir", "ca.pem"),
        (".hidden", "ca.pem"),
        ("a..b", "ca.pem"),
    ] {
        harness
            .prepare(&cookie, "proxmox.add_host", "new", host_params("nas2", "127.0.0.1:8006", token, ca))
            .await
            .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_params");
    }
    assert_eq!(rig.traffic(), 0);
}

#[tokio::test]
async fn the_id_and_node_are_plain_names() {
    let (_rig, harness, _auth, cookie) = live().await;
    for (id, node) in
        [("primary", "pve2"), ("Bad", "pve2"), ("a-b", "pve2"), ("1a", "pve2"), ("two", "a/b"), ("two", "a?b")]
    {
        let mut params = host_params(id, "127.0.0.1:8006", "pve2.token", "pve2-ca.pem");
        params["node"] = json!(node);
        harness
            .prepare(&cookie, "proxmox.add_host", "new", params)
            .await
            .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_params");
    }
}

#[tokio::test]
async fn a_file_that_does_not_hold_a_token_is_refused() {
    let (rig, harness, mut auth, cookie) = live().await;
    let pve = MockTlsPve::start("pve2", 200).await;
    write_secret(rig.dir.path(), "notes.txt", "this is not a proxmox token");
    write_secret(rig.dir.path(), "pve2-ca.pem", &pve.ca_pem);
    let done = harness
        .critical(
            &cookie,
            &mut auth,
            "proxmox.add_host",
            "new",
            host_params("nas2", &pve.address, "notes.txt", "pve2-ca.pem"),
        )
        .await;
    done.assert_refused(StatusCode::CONFLICT, "host_unusable");
    assert!(!done.text.contains("this is not a proxmox token"), "{}", done.text);
    assert_eq!(harness.get("/api/v1/machines", &cookie).await.body["hosts"], json!([]));
    assert!(pve.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_same_host_cannot_be_added_twice() {
    let (_rig, pve, harness, _auth, cookie) = with_further_host().await;
    for params in [
        host_params("nas2", "127.0.0.1:9", "pve2.token", "pve2-ca.pem"),
        host_params("other", &pve.address, "pve2.token", "pve2-ca.pem"),
    ] {
        harness
            .prepare(&cookie, "proxmox.add_host", "new", params)
            .await
            .assert_refused(StatusCode::CONFLICT, "host_exists");
    }
}

#[tokio::test]
async fn without_a_secrets_directory_no_host_can_be_added() {
    let (_rig, harness, _auth, cookie) = rig_harness(true, no_files_dir).await;
    harness
        .prepare(&cookie, "proxmox.add_host", "new", host_params("nas2", "127.0.0.1:8006", "t", "c"))
        .await
        .assert_refused(StatusCode::CONFLICT, "hosts_not_configured");
}

#[tokio::test]
async fn in_dry_run_adding_a_host_reads_no_credential_and_sends_nothing() {
    let (rig, harness, mut auth, cookie) = rig_harness(false, |_| {}).await;
    let done = harness
        .critical(
            &cookie,
            &mut auth,
            "proxmox.add_host",
            "new",
            host_params("nas2", "127.0.0.1:9", "absent.token", "absent.pem"),
        )
        .await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.traffic(), 0);
}

// ----- agents

#[tokio::test]
async fn pairing_goes_through_the_collectors_own_flow() {
    let (rig, harness, mut auth, cookie) = live().await;
    harness
        .execute(&cookie, "agent.pair", "new", json!({ "name": "Windows box" }))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "step_up_required");
    let done = harness.critical(&cookie, &mut auth, "agent.pair", "new", json!({ "name": "Windows box" })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(done.body["pairing"]["code"], json!("PAIR-1234"));
    let posts = rig.collector.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].path, PAIRING_PATH);
    assert_eq!(posts[0].body, json!({ "name": "Windows box" }).to_string());
    assert_eq!(posts[0].header("authorization"), format!("Bearer {COLLECTOR_TOKEN}"));
    for table in ["audit_log", "action_runs"] {
        assert!(!harness.dump(table).contains("PAIR-1234"), "{table} holds the code");
    }
}

#[tokio::test]
async fn pairing_is_owner_only_and_sends_nothing_in_dry_run() {
    let (rig, harness, mut owner, cookie) = rig_harness(false, |_| {}).await;
    let (_family, family_cookie, _id) = harness.add_family(&cookie, &mut owner, "Alice").await;
    harness
        .prepare(&family_cookie, "agent.pair", "new", json!({ "name": "x" }))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "owner_only");
    let done = harness.critical(&cookie, &mut owner, "agent.pair", "new", json!({ "name": "x" })).await;
    assert_eq!(done.phase(), "simulated", "{}", done.text);
    assert!(done.body.get("pairing").is_none());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.traffic(), 0);
}

#[tokio::test]
async fn pairing_without_a_configured_path_says_so() {
    let (rig, harness, mut auth, cookie) = rig_harness(true, |site| {
        site.instance("glimdock").as_object_mut().unwrap().remove("pairing_path");
    })
    .await;
    let done = harness.critical(&cookie, &mut auth, "agent.pair", "new", json!({ "name": "x" })).await;
    assert_eq!(done.phase(), "failed", "{}", done.text);
    assert!(done.body["detail"].as_str().unwrap().contains("pairing_path"));
    assert!(rig.collector.posts().is_empty());
}

#[tokio::test]
async fn agents_stay_monitoring_only() {
    let ids: Vec<&str> = catalog::CATALOG.iter().map(|def| def.id).filter(|id| id.starts_with("agent.")).collect();
    assert_eq!(ids, ["agent.pair"], "pairing is the only thing that can be asked about an agent");
    assert!(legacy_house().device("collector").is_some());
    let (rig, harness, _auth, cookie) = live().await;
    let index = harness.app.registry.index();
    for action in catalog::CATALOG.iter().filter(|def| def.admitted(&index).iter().any(|target| target == "collector"))
    {
        assert_eq!(action.id, "state.read", "only reading reaches the collector");
    }
    let posts_before = rig.collector.posts().len();
    harness.execute(&cookie, "state.read", "collector", json!({})).await.assert_ok();
    assert_eq!(rig.collector.posts().len(), posts_before, "a reading sends no command");
    let refused =
        harness.send(Method::POST, "/api/v1/agents/anything", Some(&cookie), Some(ORIGIN), Some(json!({}))).await;
    refused.assert_refused(StatusCode::NOT_FOUND, "not_found");
}

// ----- the device store of schema 4

#[tokio::test]
async fn a_new_database_has_no_machines() {
    let harness = Harness::on_database(|_| {});
    let (_auth, cookie) = harness.sign_in().await;
    let list = harness.get("/api/v1/machines", &cookie).await;
    list.assert_ok();
    assert_eq!(list.body["machines"], json!([]), "{}", list.text);
    assert_eq!(harness.app.registry.index().of_class(DeviceClass::Vm).count(), 0);
    harness
        .prepare(&cookie, "vm.start", "vm100", json!({}))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
    let devices = harness.get("/api/v1/devices", &cookie).await;
    assert!(!devices.text.contains("\"vm10"), "{}", devices.text);
}

/// A database as schema version 3 left it: the four machines it seeded and one
/// waiting for approval, a light and a light that was only found, a room, a name, a
/// phrase learned under an old action id, and audit rows naming those devices.
fn version_3_database(path: &std::path::Path) -> audit::Checkpoint {
    store::create_version(path, 3).unwrap();
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        r#"
        INSERT INTO machines (target, host, vmid, name, status, first_seen, approved_at, approved_by) VALUES
            ('vm100', 'primary', 100, 'alpha', 'approved', 0, 0, 'seed'),
            ('vm101', 'primary', 101, 'bravo', 'approved', 0, 0, 'seed'),
            ('vm102', 'primary', 102, 'charlie', 'approved', 0, 0, 'seed'),
            ('vm103', 'primary', 103, 'delta', 'approved', 0, 0, 'seed'),
            ('vm150', 'primary', 150, 'new-vm', 'pending', 5, NULL, NULL);
        INSERT INTO lights (target, mac, address, name, added_at, added_by)
            VALUES ('wiz-a8bb50aaaaaa', 'a8bb50aaaaaa', '192.0.2.21:38899', 'Hall lamp', 10, 'x');
        INSERT INTO light_candidates (mac, address, seen_at) VALUES
            ('a8bb50aaaaaa', '192.0.2.21:38899', 11),
            ('a8bb50bbbbbb', '192.0.2.22:38899', 12);
        INSERT INTO rooms (id, name, name_key, position, created_at) VALUES ('r-hall', 'Hall', 'hall', 0, 1);
        INSERT INTO room_devices (target, room_id, position) VALUES ('wiz-a8bb50aaaaaa', 'r-hall', 0);
        INSERT INTO device_names (target, name) VALUES ('vm101', 'Storage');
        INSERT INTO learned_phrases (id, phrase, action, target, params, label, learned_by, created_at, uses)
            VALUES ('p-quiet', 'make it quieter', 'sonos.volume', 'sonos', '{"level":10}', 'Volume 10', 'x', 1, 0);
        "#,
    )
    .unwrap();
    let rows = [("light.add", "wiz-a8bb50aaaaaa"), ("vm.start", "vm100"), ("sonos.volume", "sonos")];
    for (n, (action, target)) in rows.into_iter().enumerate() {
        let event = audit::Event {
            ts: n as i64,
            action: action.to_string(),
            target: target.to_string(),
            params: json!({}),
            credential_ref: "c".to_string(),
            session_ref: "s".to_string(),
            user_ref: "u".to_string(),
            device: "Phone".to_string(),
            channel: "panel".to_string(),
            step_up: "none".to_string(),
            result: "confirmed".to_string(),
            rtt_ms: None,
            run_id: None,
        };
        audit::append(&conn, &event, None).unwrap();
    }
    let report = audit::verify(&conn).unwrap();
    assert!(report.ok, "{report:?}");
    report.checkpoint.parse().unwrap()
}

#[tokio::test]
async fn an_upgraded_house_keeps_its_devices_rooms_phrases_and_audit_chain() {
    let mut checkpoint = None;
    let harness = Harness::on_database(|path| checkpoint = Some(version_3_database(path)));
    let checkpoint = checkpoint.unwrap();
    let (mut auth, cookie) = harness.sign_in().await;

    let list = harness.get("/api/v1/machines", &cookie).await;
    let machines: Vec<(&str, &str)> = list.body["machines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["target"].as_str().unwrap(), m["status"].as_str().unwrap()))
        .collect();
    assert_eq!(
        machines,
        [
            ("vm100", "approved"),
            ("vm101", "approved"),
            ("vm102", "approved"),
            ("vm103", "approved"),
            ("vm150", "pending")
        ]
    );
    harness
        .prepare(&cookie, "vm.start", "vm150", json!({}))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "machine_not_approved");
    harness.critical(&cookie, &mut auth, "machine.approve", "vm150", json!({})).await.assert_ok();
    harness.prepare(&cookie, "vm.start", "vm150", json!({})).await.assert_ok();

    // The light, its room and a name someone gave are where they were.
    let lights = harness.get("/api/v1/lights", &cookie).await;
    assert_eq!(lights.body["lights"][0]["target"], json!("wiz-a8bb50aaaaaa"), "{}", lights.text);
    assert_eq!(lights.body["lights"][0]["address"], json!("192.0.2.21:38899"), "{}", lights.text);
    harness.devices.add_state("wiz-a8bb50aaaaaa", State::Light { on: false, bri: 50, hue: 0, color: true });
    let done = harness.execute(&cookie, "light.power", "wiz-a8bb50aaaaaa", json!({ "on": true })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    let rooms = harness.get("/api/v1/rooms", &cookie).await;
    assert_eq!(rooms.body["rooms"][0]["devices"][0]["target"], json!("wiz-a8bb50aaaaaa"), "{}", rooms.text);
    let devices = harness.get("/api/v1/devices", &cookie).await;
    let storage = devices.body["devices"].as_array().unwrap().iter().find(|d| d["target"] == json!("vm101")).unwrap();
    assert_eq!(storage["name"], json!("Storage"));

    // A light the last discovery found before the upgrade can still be added.
    let added = harness.post("/api/v1/lights", &cookie, json!({ "mac": "a8bb50bbbbbb", "name": "Porch" })).await;
    added.assert_ok();
    assert_eq!(added.body["light"]["target"], json!("wiz-a8bb50bbbbbb"));

    // The phrase reads the new id.
    let action: String = harness
        .app
        .store
        .with(|conn| conn.query_row("SELECT action FROM learned_phrases WHERE id = 'p-quiet'", [], |row| row.get(0)))
        .unwrap();
    assert_eq!(action, "speaker.volume");

    // The chain still holds the rows from before, and went on from them.
    let options = audit::VerifyOptions { expected: Some(&checkpoint), ..Default::default() };
    let report = harness.app.store.with(|conn| audit::verify_with(conn, options)).unwrap();
    assert!(report.ok, "{report:?}");
    assert!(report.rows > checkpoint.seq);
    let first: (String, String) = harness
        .app
        .store
        .with(|conn| {
            conn.query_row("SELECT action, target FROM audit_log WHERE seq = 1", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
        })
        .unwrap();
    assert_eq!(first, ("light.add".to_string(), "wiz-a8bb50aaaaaa".to_string()));
}
