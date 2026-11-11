//! What a device is comes from the device index: its class, its capabilities and
//! who may see it. Nothing is decided from the shape of a target's name, so a
//! device can be called anything.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{json, Value};
use swifty_plugin_api::{Op, State};

/// A light whose name no rule about names would take for one.
const LAMP: &str = "reading-nook";

/// Adds a light called [`LAMP`] to the `wiz` instance's config, which the mock
/// plugin carries. Nothing else in the hub has heard of it.
fn add_mock_light(harness: &Harness) {
    harness.devices.add_state(LAMP, State::Light { on: false, bri: 50, hue: 30, color: true });
    let mut rows: Vec<Value> = (1..=4)
        .map(|n| json!({ "id": format!("bulb{n}"), "class": "light", "name": format!("Bulb {n}"), "target": format!("bulb{n}") }))
        .collect();
    rows.push(json!({ "id": LAMP, "class": "light", "name": "Reading nook", "target": LAMP }));
    harness.set_instance_config("wiz", json!({ "devices": rows }));
}

fn lamp_is_on(harness: &Harness) -> bool {
    matches!(harness.devices.state(LAMP), Some(State::Light { on: true, .. }))
}

fn step_targets(done: &Resp) -> Vec<String> {
    done.body["steps"].as_array().unwrap().iter().map(|step| step["target"].as_str().unwrap().to_string()).collect()
}

fn catalog_entry(catalog: &Resp, id: &str) -> Value {
    catalog.body["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["id"] == json!(id))
        .unwrap_or_else(|| panic!("{id} is not listed: {}", catalog.text))
        .clone()
}

fn listed_targets(catalog: &Resp, id: &str) -> Vec<String> {
    catalog_entry(catalog, id)["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|target| target.as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn a_light_with_any_name_can_be_switched_by_family_and_placed_in_a_room() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let (_family, family_cookie, _id) = harness.add_family(&cookie, &mut owner, "Alice").await;
    harness
        .execute(&family_cookie, "light.power", LAMP, json!({ "on": true }))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");

    add_mock_light(&harness);

    // Family switches it, because the index says it is a light and lights are theirs.
    let done = harness.execute(&family_cookie, "light.power", LAMP, json!({ "on": true })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert!(lamp_is_on(&harness));
    let dimmed = harness.execute(&family_cookie, "light.brightness", LAMP, json!({ "percent": 40 })).await;
    assert_eq!(dimmed.phase(), "confirmed", "{}", dimmed.text);
    assert!(harness.devices.commands().contains(&(LAMP.to_string(), Op::LightBrightness { percent: 40 })));
    let read = harness.get(&format!("/api/v1/state/{LAMP}"), &family_cookie).await;
    read.assert_ok();
    assert_eq!(read.body["state"]["bri"], json!(40), "{}", read.text);

    // The owner places it in a room, under the name the index has for it.
    let created = harness.post("/api/v1/rooms", &cookie, json!({ "name": "Reading room" })).await;
    created.assert_ok();
    let room = created.body["room"]["id"].as_str().unwrap().to_string();
    let placed = harness
        .send(
            Method::PUT,
            &format!("/api/v1/rooms/{room}/devices"),
            Some(&cookie),
            Some(ORIGIN),
            Some(json!({ "targets": [LAMP, "bulb2"] })),
        )
        .await;
    placed.assert_ok();
    assert_eq!(placed.body["room"]["devices"][0], json!({ "target": LAMP, "name": "Reading nook" }), "{}", placed.text);

    // Family sees it there, and the room's own actions reach it as one of its lights.
    let rooms = harness.get("/api/v1/rooms", &family_cookie).await;
    let listed = rooms.body["rooms"].as_array().unwrap().iter().find(|r| r["id"] == json!(room)).unwrap().clone();
    assert_eq!(listed["devices"].as_array().unwrap().len(), 2, "{listed}");
    let off = harness.execute(&family_cookie, "room.power", &room, json!({ "on": false })).await;
    assert_eq!(off.phase(), "confirmed", "{}", off.text);
    assert_eq!(step_targets(&off), [LAMP, "bulb2"]);
    assert!(!lamp_is_on(&harness));

    // "Every light" means it too, and the catalog offers it to family.
    let all = harness.execute(&family_cookie, "lights.power", "all_bulbs", json!({ "on": true })).await;
    assert_eq!(all.phase(), "confirmed", "{}", all.text);
    assert!(step_targets(&all).contains(&LAMP.to_string()), "{}", all.text);
    assert!(lamp_is_on(&harness));
    let catalog = harness.get("/api/v1/catalog", &family_cookie).await;
    assert!(listed_targets(&catalog, "light.power").contains(&LAMP.to_string()));
    assert!(listed_targets(&catalog, "state.read").contains(&LAMP.to_string()));

    // Being a light gives it a light's actions and nothing else.
    harness
        .execute(&cookie, "plug.power", LAMP, json!({ "on": false }))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
    harness
        .prepare(&cookie, "vm.start", LAMP, json!({}))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
}

#[tokio::test]
async fn a_name_that_looks_like_a_device_is_not_one() {
    let (harness, _owner, cookie) = Harness::signed_in().await;
    for (action, target) in [
        ("light.power", "wiz-a8bb50aabbcc"),
        ("light.power", "hue-7"),
        ("plug.power", "plug-bf0123456789abcdef"),
        ("ac.power", "ac-0a1b2c3d"),
    ] {
        harness
            .execute(&cookie, action, target, json!({ "on": true }))
            .await
            .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
    }
    harness
        .prepare(&cookie, "vm.start", "vm777", json!({}))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn the_catalog_says_what_each_action_takes_and_what_may_be_used_now() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let (_family, family_cookie, family_id) = harness.add_family(&cookie, &mut owner, "Alice").await;
    let room = harness.post("/api/v1/rooms", &cookie, json!({ "name": "Hall" })).await;
    let room = room.body["room"]["id"].as_str().unwrap().to_string();
    harness.devices.add_seen_machine("primary", 150, "new-vm");
    harness.post("/api/v1/machines/scan", &cookie, json!({})).await.assert_ok();

    let catalog = harness.get("/api/v1/catalog", &cookie).await;
    catalog.assert_ok();
    let volume = catalog_entry(&catalog, "speaker.volume");
    assert_eq!(volume["target_kind"], json!("capable"));
    assert_eq!(volume["class"], json!("speaker"));
    assert_eq!(volume["requires"], json!("volume"));
    assert_eq!(volume["targets"], json!(["sonos"]), "the subwoofer has no volume of its own");
    let remove = catalog_entry(&catalog, "machine.remove");
    assert_eq!((remove["target_kind"].clone(), remove["class"].clone()), (json!("class"), json!("vm")));
    assert_eq!(remove["requires"], Value::Null);
    let read = catalog_entry(&catalog, "state.read");
    assert_eq!((read["target_kind"].clone(), read["class"].clone()), (json!("readable"), Value::Null));

    assert_eq!(listed_targets(&catalog, "vm.start"), ["vm100", "vm101", "vm102", "vm103"]);
    assert_eq!(listed_targets(&catalog, "machine.approve"), ["vm150"]);
    assert_eq!(listed_targets(&catalog, "light.power"), ["bulb1", "bulb2", "bulb3", "bulb4"]);
    assert_eq!(listed_targets(&catalog, "scene.apply"), ["evening", "focus", "all_off"]);
    assert_eq!(listed_targets(&catalog, "room.power"), std::slice::from_ref(&room));
    assert_eq!(listed_targets(&catalog, "user.remove"), [family_id]);
    assert_eq!(listed_targets(&catalog, "passkey.register"), ["new"]);
    assert_eq!(listed_targets(&catalog, "passkey.revoke").len(), 2, "the owner's passkey and the family member's");
    assert_eq!(catalog_entry(&catalog, "proxmox.remove_host")["targets"], json!([]));
    assert_eq!(catalog_entry(&catalog, "plug.power")["targets"], json!([]), "no socket is configured");
    let readable = listed_targets(&catalog, "state.read");
    for target in ["all_bulbs", "all_vms", "sub", "collector", "vm100"] {
        assert!(readable.contains(&target.to_string()), "{target}");
    }
    assert!(!readable.contains(&"vm150".to_string()), "a pending machine cannot be read");

    let family = harness.get("/api/v1/catalog", &family_cookie).await;
    let readable = listed_targets(&family, "state.read");
    assert!(readable.contains(&"all_bulbs".to_string()) && readable.contains(&"camera".to_string()));
    for hidden in ["all_vms", "vm100", "collector", "printer"] {
        assert!(!readable.contains(&hidden.to_string()), "{hidden}");
    }
    assert_eq!(listed_targets(&family, "room.power"), [room]);
    assert_eq!(listed_targets(&family, "tv.key"), ["tv"]);
}
