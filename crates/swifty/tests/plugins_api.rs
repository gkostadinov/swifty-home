//! The plugin routes: the plugins of the build and the stored
//! instances, one manifest, one instance, and generic discovery, candidates,
//! adoption and removal of a device. All of them are the owner's.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{json, Value};
use swifty_plugin_api::{Capability, DeviceClass, DeviceDecl};

fn entry<'a>(list: &'a Value, key: &str, id: &str) -> &'a Value {
    list[key].as_array().unwrap().iter().find(|item| item["id"] == json!(id)).unwrap_or_else(|| panic!("no {id}"))
}

fn targets_of(devices: &Value) -> Vec<String> {
    devices["devices"].as_array().unwrap().iter().filter_map(|d| d["target"].as_str()).map(str::to_string).collect()
}

fn lamp(id: &str) -> DeviceDecl {
    DeviceDecl {
        local_id: id.to_string(),
        target_hint: None,
        class: DeviceClass::Light,
        name: "Found lamp".to_string(),
        capabilities: vec![Capability::Power],
        attrs: json!({}),
    }
}

async fn delete(harness: &Harness, cookie: &str, path: &str) -> Resp {
    harness.send(Method::DELETE, path, Some(cookie), Some(ORIGIN), None).await
}

#[tokio::test]
async fn the_owner_sees_every_plugin_of_the_build_and_every_instance() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let listed = harness.get("/api/v1/plugins", &cookie).await;
    listed.assert_ok();
    assert_eq!(listed.body["live"], json!(true));
    assert_eq!(listed.body["setups_open"], json!(0));

    let ids: Vec<&str> = listed.body["plugins"].as_array().unwrap().iter().filter_map(|p| p["id"].as_str()).collect();
    for id in ["hue", "wiz", "mock", "proxmox", "toshiba"] {
        assert!(ids.contains(&id), "{id} is in the build: {ids:?}");
    }
    let hue = entry(&listed.body, "plugins", "hue");
    assert_eq!(hue["name"], json!("Philips Hue"));
    assert_eq!(hue["reach"], json!({ "reach": "local" }));
    assert_eq!(hue["setup"], json!(true));
    assert_eq!(hue["can_add"], json!(true));
    let toshiba = entry(&listed.body, "plugins", "toshiba");
    let said = toshiba["reach_text"].to_string();
    assert!(said.contains("does not document this") && said.contains("terms"), "{said}");

    // The test house's instances all run on the mock, under the old kinds' names.
    let instance = entry(&listed.body, "instances", "hue");
    assert_eq!(instance["plugin"], json!("mock"));
    assert_eq!(instance["in_build"], json!(true));
    assert_eq!(instance["readiness"], json!({ "status": "ready" }));
    assert_eq!(instance["devices"][0]["target"], json!("hue"));
    assert_eq!(instance["setup_open"], Value::Null);
    let machines = entry(&listed.body, "instances", "proxmox");
    assert!(machines["devices"].as_array().unwrap().len() >= 5, "the host and the test VMs");
}

#[tokio::test]
async fn one_manifest_in_full_with_the_tiers_its_actions_really_have() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let hue = harness.get("/api/v1/plugins/hue", &cookie).await;
    hue.assert_ok();
    assert_eq!(hue.body["id"], json!("hue"));
    assert_eq!(hue.body["secrets"][0]["slot"], json!("app_key"));
    assert_eq!(hue.body["config"][0]["key"], json!("url"));
    assert_eq!(hue.body["instances"], json!([]), "the test house's `hue` is a mock");
    assert!(hue.body["dropped_words"].is_array());

    let mock = harness.get("/api/v1/plugins/mock", &cookie).await;
    let action = &mock.body["actions"][0];
    assert_eq!(action["id"], json!("mock.set_level"));
    assert!(!action["tier_effective"].is_null(), "{}", mock.text);
    assert!(mock.body["instances"].as_array().unwrap().contains(&json!("hue")));

    harness.get("/api/v1/plugins/toaster", &cookie).await.assert_refused(StatusCode::NOT_FOUND, "unknown_plugin");
}

#[tokio::test]
async fn an_instance_shows_its_config_and_which_slots_hold_a_value_never_the_value() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    harness.add_instance(
        "demo",
        "mock",
        json!({ "devices": [{ "id": "lamp", "class": "light" }], "require_code": true }),
    );
    let waiting = harness.get("/api/v1/instances/demo", &cookie).await;
    waiting.assert_ok();
    assert_eq!(waiting.body["readiness"]["status"], json!("needs_setup"), "{}", waiting.text);
    assert_eq!(waiting.body["secret_slots"][0]["set"], json!(false));

    const CODE: &str = "instance-code-31337";
    harness.app.registry.secret_store().write("demo", "access_code", &CODE.to_string().into()).unwrap();
    harness.app.sync_directory().unwrap();
    let ready = harness.get("/api/v1/instances/demo", &cookie).await;
    assert_eq!(ready.body["readiness"], json!({ "status": "ready" }), "{}", ready.text);
    assert_eq!(ready.body["secrets"], json!(["access_code"]));
    assert_eq!(ready.body["secret_slots"][0]["slot"], json!("access_code"));
    assert_eq!(ready.body["secret_slots"][0]["set"], json!(true));
    assert_eq!(ready.body["config"]["require_code"], json!(true));
    assert_eq!(ready.body["devices"][0]["target"], json!("demo-lamp"));
    assert!(ready.body["created_at"].is_i64());
    assert!(!ready.text.contains(CODE));

    harness.get("/api/v1/instances/nothing", &cookie).await.assert_refused(StatusCode::NOT_FOUND, "unknown_instance");
}

#[tokio::test]
async fn an_instance_of_a_plugin_this_build_lacks_is_listed_and_can_be_removed() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    harness
        .app
        .store
        .with(|conn| {
            conn.execute(
                "INSERT INTO plugin_instances (id, plugin, config, config_version, enabled, created_at, created_by, updated_at)
                 VALUES ('gone', 'toaster', '{}', 1, 1, 0, 'test', 0)",
                [],
            )
        })
        .unwrap();
    harness.app.sync_directory().unwrap();
    let listed = harness.get("/api/v1/plugins", &cookie).await;
    let gone = entry(&listed.body, "instances", "gone");
    assert_eq!(gone["in_build"], json!(false));
    assert_eq!(gone["readiness"]["status"], json!("disabled"));
    assert!(gone["readiness"]["reason"].as_str().unwrap().contains("no plugin toaster"));

    harness
        .prepare(&cookie, "plugin.reconfigure", "gone", json!({}))
        .await
        .assert_refused(StatusCode::CONFLICT, "plugin_not_in_build");
    let catalog = harness.get("/api/v1/catalog", &cookie).await;
    let remove =
        catalog.body["actions"].as_array().unwrap().iter().find(|a| a["id"] == json!("plugin.remove")).unwrap();
    assert!(remove["targets"].as_array().unwrap().contains(&json!("gone")), "the store's instances are its targets");

    let removed = harness.critical(&cookie, &mut owner, "plugin.remove", "gone", json!({})).await;
    assert_eq!(removed.phase(), "confirmed", "{}", removed.text);
    let listed = harness.get("/api/v1/plugins", &cookie).await;
    assert!(!listed.text.contains("\"gone\""), "{}", listed.text);
}

#[tokio::test]
async fn the_plugin_routes_are_the_owners() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let (_family, family, _id) = harness.add_family(&cookie, &mut owner, "Alice").await;
    let before = harness.audit_count("refused: owner_only");
    for (method, path, body) in [
        (Method::GET, "/api/v1/plugins", None),
        (Method::GET, "/api/v1/plugins/hue", None),
        (Method::GET, "/api/v1/instances/hue", None),
        (Method::POST, "/api/v1/instances/hue/discover", Some(json!({}))),
        (Method::GET, "/api/v1/instances/hue/candidates", None),
        (Method::POST, "/api/v1/devices/adopt", Some(json!({ "instance": "hue", "local_id": "x" }))),
        (Method::DELETE, "/api/v1/devices/bulb1", None),
        (Method::GET, "/api/v1/setup/anything", None),
    ] {
        let origin = (method != Method::GET).then_some(ORIGIN);
        harness
            .send(method, path, Some(&family), origin, body)
            .await
            .assert_refused(StatusCode::FORBIDDEN, "owner_only");
    }
    assert_eq!(harness.audit_count("refused: owner_only"), before + 8);
    // A family member's catalog offers none of the lifecycle actions.
    let catalog = harness.get("/api/v1/catalog", &family).await;
    assert!(!catalog.text.contains("plugin."), "{}", catalog.text);
}

#[tokio::test]
async fn a_found_device_is_adopted_from_its_candidate_and_removed_again() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    harness.devices.add_discoverable("hue", lamp("lamp9"));

    let found = harness.post("/api/v1/instances/hue/discover", &cookie, json!({})).await;
    found.assert_ok();
    assert_eq!(found.body["simulated"], json!(false));
    let first = &found.body["found"][0];
    assert_eq!(first["local_id"], json!("lamp9"));
    assert_eq!(first["adopted"], json!(false));
    let candidates = harness.get("/api/v1/instances/hue/candidates", &cookie).await;
    assert_eq!(candidates.body["candidates"][0]["local_id"], json!("lamp9"), "{}", candidates.text);
    assert!(harness.audit_has("done"));

    let adopted = harness
        .post("/api/v1/devices/adopt", &cookie, json!({ "instance": "hue", "local_id": "lamp9", "name": "Desk lamp" }))
        .await;
    adopted.assert_ok();
    let target = adopted.body["device"]["target"].as_str().unwrap().to_string();
    assert_eq!(target, "hue-lamp9");
    let devices = harness.get("/api/v1/devices", &cookie).await;
    assert!(targets_of(&devices.body).contains(&target), "{}", devices.text);
    let on = harness.execute(&cookie, "light.power", &target, json!({ "on": true })).await;
    assert_eq!(on.phase(), "confirmed", "an adopted light works at once: {}", on.text);

    harness
        .post("/api/v1/devices/adopt", &cookie, json!({ "instance": "hue", "local_id": "lamp9" }))
        .await
        .assert_refused(StatusCode::CONFLICT, "device_exists");
    harness
        .post("/api/v1/devices/adopt", &cookie, json!({ "instance": "hue", "local_id": "lamp7" }))
        .await
        .assert_refused(StatusCode::NOT_FOUND, "unknown_candidate");
    harness
        .post(
            "/api/v1/devices/adopt",
            &cookie,
            json!({ "instance": "hue", "local_id": "lamp9", "address": "192.0.2.9" }),
        )
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_body");

    delete(&harness, &cookie, &format!("/api/v1/devices/{target}")).await.assert_ok();
    let devices = harness.get("/api/v1/devices", &cookie).await;
    assert!(!targets_of(&devices.body).contains(&target));
    assert!(harness.audit_has("done"));
    // Its candidate stays, so it can be adopted again.
    harness.post("/api/v1/devices/adopt", &cookie, json!({ "instance": "hue", "local_id": "lamp9" })).await.assert_ok();
}

#[tokio::test]
async fn a_found_computer_waits_for_approval_and_is_never_adopted_or_removed_here() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    harness.devices.add_discoverable(
        "proxmox",
        DeviceDecl {
            local_id: "vm150".to_string(),
            target_hint: Some("vm150".to_string()),
            class: DeviceClass::Vm,
            name: "Builder".to_string(),
            capabilities: vec![Capability::Lifecycle],
            attrs: json!({ "host": "primary", "vmid": 150 }),
        },
    );
    let found = harness.post("/api/v1/instances/proxmox/discover", &cookie, json!({})).await;
    found.assert_ok();
    assert_eq!(found.body["found"][0]["pending"], json!(true), "{}", found.text);
    assert_eq!(found.body["found"][0]["target"], json!("vm150"));
    let machines = harness.get("/api/v1/machines", &cookie).await;
    assert!(machines.text.contains("vm150"), "{}", machines.text);

    harness
        .post("/api/v1/devices/adopt", &cookie, json!({ "instance": "proxmox", "local_id": "vm150" }))
        .await
        .assert_refused(StatusCode::CONFLICT, "needs_approval");
    for target in ["vm150", "vm100", "host"] {
        delete(&harness, &cookie, &format!("/api/v1/devices/{target}"))
            .await
            .assert_refused(StatusCode::CONFLICT, "needs_approval");
    }
    let approved = harness.critical(&cookie, &mut owner, "machine.approve", "vm150", json!({})).await;
    assert_eq!(approved.phase(), "confirmed", "{}", approved.text);
}

#[tokio::test]
async fn a_device_its_config_names_or_that_does_not_exist_is_not_removed_here() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    delete(&harness, &cookie, "/api/v1/devices/bulb1").await.assert_refused(StatusCode::CONFLICT, "declared_by_config");
    delete(&harness, &cookie, "/api/v1/devices/nothing").await.assert_refused(StatusCode::NOT_FOUND, "unknown_device");
    harness
        .post("/api/v1/instances/nothing/discover", &cookie, json!({}))
        .await
        .assert_refused(StatusCode::NOT_FOUND, "unknown_instance");
    harness
        .get("/api/v1/instances/nothing/candidates", &cookie)
        .await
        .assert_refused(StatusCode::NOT_FOUND, "unknown_instance");
    let devices = harness.get("/api/v1/devices", &cookie).await;
    assert!(targets_of(&devices.body).contains(&"bulb1".to_string()));
}

#[tokio::test]
async fn in_dry_run_discovery_contacts_nothing() {
    let (harness, _auth, cookie) = Harness::signed_in_with(|config| config.live = false).await;
    harness.devices.add_discoverable("hue", lamp("lamp9"));
    let found = harness.post("/api/v1/instances/hue/discover", &cookie, json!({})).await;
    found.assert_ok();
    assert_eq!(found.body, json!({ "simulated": true, "found": [] }));
    assert_eq!(harness.devices.total_reads(), 0);
    let candidates = harness.get("/api/v1/instances/hue/candidates", &cookie).await;
    assert_eq!(candidates.body["candidates"], json!([]));
}

#[tokio::test]
async fn removing_an_instance_named_like_its_device_tells_only_what_goes() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    // The test house's `hue` instance carries one device, also called `hue`, with a note.
    harness
        .app
        .store
        .with(|conn| swifty_hub::notes::set(conn, "hue", &["The hall lights hang off it.".to_string()]))
        .unwrap();
    let prepared = harness.prepare(&cookie, "plugin.remove", "hue", json!({})).await;
    prepared.assert_ok();
    assert_eq!(prepared.body["label"], json!("Remove hue"));
    assert_eq!(prepared.body["target_name"], json!("hue"));
    assert_eq!(
        prepared.body["consequences"],
        json!(["Its device goes: Light bridge.", "The audit log keeps every row about it."]),
        "{}",
        prepared.text
    );
    assert_eq!(prepared.body["affected"], json!(["Light bridge"]));
}
