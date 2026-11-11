//! Rooms, room-level actions, discovery of WiZ bulbs, the Hue Bridge and its lights,
//! and voice resolving room names, against mock devices on loopback.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::devices::*;
use common::*;
use serde_json::{json, Value};
use swifty_hub::secrets::{FileSecretStore, SecretStore};
use swifty_plugin_api::Secret as Credential;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;

async fn live() -> (Rig, Harness, SoftPasskey, String) {
    rig_harness(true, |_| {}).await
}

async fn room(harness: &Harness, cookie: &str, name: &str, devices: &[&str]) -> String {
    let created = harness.post("/api/v1/rooms", cookie, json!({ "name": name })).await;
    created.assert_ok();
    let id = created.body["room"]["id"].as_str().unwrap().to_string();
    if !devices.is_empty() {
        harness
            .send(
                Method::PUT,
                &format!("/api/v1/rooms/{id}/devices"),
                Some(cookie),
                Some(ORIGIN),
                Some(json!({ "targets": devices })),
            )
            .await
            .assert_ok();
    }
    id
}

fn names(list: &Value) -> Vec<String> {
    list["rooms"].as_array().unwrap().iter().map(|r| r["name"].as_str().unwrap().to_string()).collect()
}

// ----- arranging

#[tokio::test]
async fn rooms_can_be_created_renamed_ordered_and_deleted() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let a = room(&harness, &cookie, "Kitchen", &[]).await;
    let b = room(&harness, &cookie, "Hall", &[]).await;
    let c = room(&harness, &cookie, "Bedroom", &[]).await;
    assert_eq!(names(&harness.get("/api/v1/rooms", &cookie).await.body), ["Kitchen", "Hall", "Bedroom"]);

    let renamed = harness
        .send(
            Method::PATCH,
            &format!("/api/v1/rooms/{a}"),
            Some(&cookie),
            Some(ORIGIN),
            Some(json!({ "name": "Galley" })),
        )
        .await;
    assert_eq!(renamed.body["room"]["name"], json!("Galley"));

    let ordered = harness
        .send(Method::PUT, "/api/v1/rooms/order", Some(&cookie), Some(ORIGIN), Some(json!({ "rooms": [c, a, b] })))
        .await;
    assert_eq!(names(&ordered.body), ["Bedroom", "Galley", "Hall"]);

    harness.send(Method::DELETE, &format!("/api/v1/rooms/{c}"), Some(&cookie), Some(ORIGIN), None).await.assert_ok();
    assert_eq!(names(&harness.get("/api/v1/rooms", &cookie).await.body), ["Galley", "Hall"]);
    assert!(harness.dump("audit_log").contains("room.rename"));
}

#[tokio::test]
async fn a_name_has_to_be_usable_and_unique() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    room(&harness, &cookie, "Kitchen", &[]).await;
    harness
        .post("/api/v1/rooms", &cookie, json!({ "name": "KITCHEN" }))
        .await
        .assert_refused(StatusCode::CONFLICT, "name_taken");
    for bad in ["", " spaced ", "line\nbreak", &"x".repeat(41)] {
        harness
            .post("/api/v1/rooms", &cookie, json!({ "name": bad }))
            .await
            .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_name");
    }
    harness
        .post("/api/v1/rooms", &cookie, json!({ "name": "!!!" }))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_name");
    for reserved in ["TV", "Lights", "Off", "Stop"] {
        harness
            .post("/api/v1/rooms", &cookie, json!({ "name": reserved }))
            .await
            .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "reserved_name");
    }
}

#[tokio::test]
async fn ordering_needs_every_room_exactly_once() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let a = room(&harness, &cookie, "Kitchen", &[]).await;
    let b = room(&harness, &cookie, "Hall", &[]).await;
    for list in [json!([a]), json!([a, a]), json!([a, b, "nope"]), json!([a, "nope"])] {
        harness
            .send(Method::PUT, "/api/v1/rooms/order", Some(&cookie), Some(ORIGIN), Some(json!({ "rooms": list })))
            .await
            .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_order");
    }
}

#[tokio::test]
async fn a_device_is_in_one_room_at_a_time() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let a = room(&harness, &cookie, "Kitchen", &["bulb1", "bulb2"]).await;
    let b = room(&harness, &cookie, "Hall", &["bulb2", "sonos"]).await;
    let list = harness.get("/api/v1/rooms", &cookie).await;
    let targets = |id: &str| -> Vec<String> {
        list.body["rooms"].as_array().unwrap().iter().find(|r| r["id"] == json!(id)).unwrap()["devices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["target"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(targets(&a), ["bulb1"]);
    assert_eq!(targets(&b), ["bulb2", "sonos"]);
}

#[tokio::test]
async fn only_real_devices_can_be_put_in_a_room() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let a = room(&harness, &cookie, "Kitchen", &[]).await;
    for targets in [json!(["nope"]), json!(["bulb1", "bulb1"]), json!(["vm999"])] {
        harness
            .send(
                Method::PUT,
                &format!("/api/v1/rooms/{a}/devices"),
                Some(&cookie),
                Some(ORIGIN),
                Some(json!({ "targets": targets })),
            )
            .await
            .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_devices");
    }
}

#[tokio::test]
async fn a_device_can_be_renamed() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    harness
        .send(
            Method::PUT,
            "/api/v1/devices/bulb1/name",
            Some(&cookie),
            Some(ORIGIN),
            Some(json!({ "name": "Reading lamp" })),
        )
        .await
        .assert_ok();
    let devices = harness.get("/api/v1/devices", &cookie).await;
    assert!(devices.text.contains("Reading lamp"));
    harness
        .send(Method::PUT, "/api/v1/devices/nope/name", Some(&cookie), Some(ORIGIN), Some(json!({ "name": "x" })))
        .await
        .assert_refused(StatusCode::NOT_FOUND, "unknown_device");
}

#[tokio::test]
async fn family_cannot_arrange_rooms_but_can_see_and_use_them() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let (_family, family_cookie, _id) = harness.add_family(&cookie, &mut owner, "Alice").await;
    let id = room(&harness, &cookie, "Kitchen", &["bulb1", "vm100"]).await;
    harness
        .post("/api/v1/rooms", &family_cookie, json!({ "name": "Mine" }))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "owner_only");

    let list = harness.get("/api/v1/rooms", &family_cookie).await;
    let shown: Vec<&str> =
        list.body["rooms"][0]["devices"].as_array().unwrap().iter().map(|d| d["target"].as_str().unwrap()).collect();
    assert_eq!(shown, ["bulb1"], "a computer in the room stays hidden");

    let done = harness.execute(&family_cookie, "room.power", &id, json!({ "on": false })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
}

// ----- room-level actions

#[tokio::test]
async fn a_room_action_touches_only_that_rooms_lights() {
    let (rig, harness, _auth, cookie) = live().await;
    let kitchen = room(&harness, &cookie, "Kitchen", &["bulb1", "bulb2", "sonos"]).await;
    room(&harness, &cookie, "Hall", &["bulb3", "bulb4"]).await;

    let done = harness.execute(&cookie, "room.power", &kitchen, json!({ "on": false })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert!(!rig.bulbs[0].state.lock().unwrap().on);
    assert!(!rig.bulbs[1].state.lock().unwrap().on);
    assert_eq!(rig.bulbs[2].traffic(), 0);
    assert_eq!(rig.bulbs[3].traffic(), 0);
    assert_eq!(rig.sonos.http.traffic(), 0, "a speaker in the room is not a light");

    let done = harness.execute(&cookie, "room.brightness", &kitchen, json!({ "percent": 55 })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(rig.bulbs[0].state.lock().unwrap().dimming, 55);
    assert_eq!(rig.bulbs[1].state.lock().unwrap().dimming, 55);
    assert_eq!(rig.bulbs[2].traffic(), 0);
}

#[tokio::test]
async fn a_room_with_no_lights_runs_nothing() {
    let (rig, harness, _auth, cookie) = live().await;
    let empty = room(&harness, &cookie, "Hall", &["sonos"]).await;
    harness
        .execute(&cookie, "room.power", &empty, json!({ "on": true }))
        .await
        .assert_refused(StatusCode::CONFLICT, "room_has_no_lights");
    assert_eq!(rig.traffic(), 0);
}

#[tokio::test]
async fn a_room_action_checks_its_values_and_room() {
    let (rig, harness, _auth, cookie) = live().await;
    let id = room(&harness, &cookie, "Kitchen", &["bulb1"]).await;
    harness
        .execute(&cookie, "room.brightness", &id, json!({ "percent": 500 }))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_params");
    harness
        .execute(&cookie, "room.power", "nosuchroom", json!({ "on": true }))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
    assert_eq!(rig.traffic(), 0);
}

#[tokio::test]
async fn a_room_scene_sets_only_lights_in_its_room() {
    let (rig, harness, _auth, cookie) = live().await;
    let kitchen = room(&harness, &cookie, "Kitchen", &["bulb1", "bulb2"]).await;
    room(&harness, &cookie, "Hall", &["bulb3"]).await;
    let scene = |name: &str, settings: Value| json!({ "name": name, "settings": settings });
    let path = format!("/api/v1/rooms/{kitchen}/scenes");

    let saved = harness
        .post(
            &path,
            &cookie,
            scene("Breakfast", json!([{ "target": "bulb1", "percent": 80, "hue": 40 }, { "target": "bulb2" }])),
        )
        .await;
    saved.assert_ok();
    assert_eq!(saved.body["room"]["scenes"][0]["name"], json!("Breakfast"));

    for (bad, code) in [
        (json!([{ "target": "bulb3", "percent": 80 }]), "invalid_scene"),
        (json!([{ "target": "nope", "percent": 80 }]), "invalid_scene"),
        (json!([{ "target": "bulb1", "percent": 101 }]), "invalid_scene"),
        (json!([{ "target": "bulb1", "percent": 50, "hue": 360 }]), "invalid_scene"),
        (json!([{ "target": "bulb1" }, { "target": "bulb1" }]), "invalid_scene"),
        (json!([]), "invalid_scene"),
    ] {
        harness.post(&path, &cookie, scene("Other", bad)).await.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, code);
    }

    let done = harness.execute(&cookie, "room.scene", &kitchen, json!({ "scene": "Breakfast" })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(rig.bulbs[0].state.lock().unwrap().dimming, 80);
    assert!(!rig.bulbs[1].state.lock().unwrap().on);
    assert_eq!(rig.bulbs[2].traffic(), 0);
    assert_eq!(rig.bulbs[3].traffic(), 0);

    harness
        .execute(&cookie, "room.scene", &kitchen, json!({ "scene": "Nothing" }))
        .await
        .assert_refused(StatusCode::NOT_FOUND, "unknown_scene");
}

#[tokio::test]
async fn a_light_that_left_the_room_is_left_alone_by_its_scene() {
    let (rig, harness, _auth, cookie) = live().await;
    let kitchen = room(&harness, &cookie, "Kitchen", &["bulb1", "bulb2"]).await;
    harness
        .post(
            &format!("/api/v1/rooms/{kitchen}/scenes"),
            &cookie,
            json!({ "name": "Breakfast", "settings": [{ "target": "bulb1", "percent": 80, "hue": 40 }, { "target": "bulb2", "percent": 80, "hue": 40 }] }),
        )
        .await
        .assert_ok();
    harness
        .send(
            Method::PUT,
            &format!("/api/v1/rooms/{kitchen}/devices"),
            Some(&cookie),
            Some(ORIGIN),
            Some(json!({ "targets": ["bulb1"] })),
        )
        .await
        .assert_ok();
    harness.execute(&cookie, "room.scene", &kitchen, json!({ "scene": "Breakfast" })).await.assert_ok();
    assert_eq!(rig.bulbs[1].traffic(), 0);
}

#[tokio::test]
async fn in_dry_run_a_room_action_reaches_no_light() {
    let (rig, harness, _auth, cookie) = rig_harness(false, |_| {}).await;
    let id = room(&harness, &cookie, "Kitchen", &["bulb1"]).await;
    let done = harness.execute(&cookie, "room.power", &id, json!({ "on": false })).await;
    assert_eq!(done.phase(), "simulated", "{}", done.text);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.traffic(), 0);
}

// ----- discovery

#[tokio::test]
async fn discovery_finds_bulbs_on_the_network() {
    let (rig, harness, _auth, cookie) = live().await;
    let found = harness.post("/api/v1/lights/discover", &cookie, json!({})).await;
    found.assert_ok();
    assert_eq!(found.body["simulated"], json!(false));
    let list = found.body["found"].as_array().unwrap();
    assert_eq!(list.len(), 2, "{}", found.text);
    assert_eq!(list[0]["mac"], json!(DISCOVERED_MACS[0]));
    assert_eq!(list[0]["added"], json!(false));
    assert_eq!(rig.broadcast.datagrams.lock().unwrap()[0]["method"], json!("getPilot"));
    assert_eq!(rig.unknown_bulbs[0].traffic(), 0, "discovery changes nothing on a bulb");
    let results = harness.audit_results();
    let at = |what: &str| results.iter().position(|r| r == what).unwrap();
    assert!(at("commanded") < at("done"), "the row is written before anything is sent");
}

#[tokio::test]
async fn a_discovered_light_can_be_added_and_then_used() {
    let (rig, harness, _auth, cookie) = live().await;
    harness.post("/api/v1/lights/discover", &cookie, json!({})).await.assert_ok();
    let added =
        harness.post("/api/v1/lights", &cookie, json!({ "mac": DISCOVERED_MACS[0], "name": "Hall lamp" })).await;
    added.assert_ok();
    let target = added.body["light"]["target"].as_str().unwrap().to_string();
    assert_eq!(target, format!("wiz-{}", DISCOVERED_MACS[0]));

    let done = harness.execute(&cookie, "light.power", &target, json!({ "on": false })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert!(!rig.unknown_bulbs[0].state.lock().unwrap().on);
    assert_eq!(rig.unknown_bulbs[1].traffic(), 0);

    let id = room(&harness, &cookie, "Hall", &[&target]).await;
    let done = harness.execute(&cookie, "room.brightness", &id, json!({ "percent": 30 })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(rig.unknown_bulbs[0].state.lock().unwrap().dimming, 30);
    assert!(harness.get("/api/v1/devices", &cookie).await.text.contains("Hall lamp"));
}

/// What a discovery found is a candidate of the instance that found it, and adding
/// one stores it as that instance's device with what was found about it.
#[tokio::test]
async fn found_and_added_lights_are_stored_under_their_instance() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.hue_bridge.lock().unwrap().lights.insert("1".to_string(), HueLight::colour("Lightstrip"));
    harness.post("/api/v1/lights/discover", &cookie, json!({})).await.assert_ok();
    let candidates: Vec<(String, String, Value)> = harness
        .app
        .store
        .with(|conn| {
            let mut statement = conn
                .prepare("SELECT instance_id, local_id, decl FROM device_candidates ORDER BY instance_id, local_id")?;
            let rows = statement.query_map([], |row| {
                let decl: String = row.get(2)?;
                Ok((row.get(0)?, row.get(1)?, serde_json::from_str(&decl).unwrap()))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap();
    let ids: Vec<(&str, &str)> = candidates.iter().map(|(i, l, _)| (i.as_str(), l.as_str())).collect();
    let wiz_ids: Vec<String> = DISCOVERED_MACS.iter().map(|mac| format!("wiz-{mac}")).collect();
    assert_eq!(ids, [("hue", "hue-1"), ("wiz", wiz_ids[0].as_str()), ("wiz", wiz_ids[1].as_str())]);
    assert_eq!(candidates[0].2["name"], json!("Lightstrip"), "the bridge's name is kept");
    assert_eq!(candidates[1].2["attrs"]["mac"], json!(DISCOVERED_MACS[0]));

    for (mac, name) in [(DISCOVERED_MACS[0], "Hall lamp"), ("hue-1", "Strip")] {
        harness.post("/api/v1/lights", &cookie, json!({ "mac": mac, "name": name })).await.assert_ok();
    }
    let stored: Vec<(String, String, String, String, Value)> = harness
        .app
        .store
        .with(|conn| {
            let mut statement = conn.prepare(
                "SELECT target, instance_id, origin, status, attrs FROM devices
                 WHERE class = 'light' AND origin = 'adopted' ORDER BY target",
            )?;
            let rows = statement.query_map([], |row| {
                let attrs: String = row.get(4)?;
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, serde_json::from_str(&attrs).unwrap()))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap();
    let wiz_address = candidates[1].2["attrs"]["address"].clone();
    assert_eq!(
        stored,
        [
            (
                "hue-1".to_string(),
                "hue".to_string(),
                "adopted".to_string(),
                "active".to_string(),
                json!({ "mac": "hue-1", "address": "hue-bridge", "light": "1", "colour": true })
            ),
            (
                wiz_ids[0].clone(),
                "wiz".to_string(),
                "adopted".to_string(),
                "active".to_string(),
                json!({ "mac": DISCOVERED_MACS[0], "address": wiz_address })
            ),
        ]
    );
    let instances = harness.dump("plugin_instances");
    assert!(instances.contains("\nhue|hue|") || instances.starts_with("hue|hue|"), "{instances}");
    assert!(instances.contains("\nwiz|wiz|") || instances.starts_with("wiz|wiz|"), "{instances}");
}

#[tokio::test]
async fn a_light_cannot_be_added_without_a_real_discovery() {
    let (rig, harness, _auth, cookie) = live().await;
    harness
        .post("/api/v1/lights", &cookie, json!({ "mac": "a8bb50aaaaaa", "name": "Forged" }))
        .await
        .assert_refused(StatusCode::CONFLICT, "light_not_discovered");
    for bad in ["../../etc", "a8bb50", "zzzzzzzzzzzz"] {
        harness
            .post("/api/v1/lights", &cookie, json!({ "mac": bad, "name": "x" }))
            .await
            .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_mac");
    }
    // The address cannot be given, because the field does not exist.
    harness
        .post("/api/v1/lights", &cookie, json!({ "mac": DISCOVERED_MACS[0], "name": "x", "address": "192.0.2.1" }))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_body");
    assert_eq!(rig.traffic(), 0);
}

#[tokio::test]
async fn an_unknown_light_target_is_refused() {
    let (rig, harness, _auth, cookie) = live().await;
    harness
        .execute(&cookie, "light.power", "wiz-a8bb50aaaaaa", json!({ "on": true }))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
    assert_eq!(rig.traffic(), 0);
}

#[tokio::test]
async fn adding_the_same_light_twice_is_refused() {
    let (_rig, harness, _auth, cookie) = live().await;
    harness.post("/api/v1/lights/discover", &cookie, json!({})).await.assert_ok();
    harness.post("/api/v1/lights", &cookie, json!({ "mac": DISCOVERED_MACS[0], "name": "One" })).await.assert_ok();
    harness
        .post("/api/v1/lights", &cookie, json!({ "mac": DISCOVERED_MACS[0], "name": "Two" }))
        .await
        .assert_refused(StatusCode::CONFLICT, "light_exists");
}

async fn accept_address(harness: &Harness, cookie: &str, target: &str) -> Resp {
    harness.send(Method::PUT, &format!("/api/v1/lights/{target}/address"), Some(cookie), Some(ORIGIN), None).await
}

#[tokio::test]
async fn a_rediscovered_light_moves_only_when_the_owner_accepts() {
    let (rig, harness, _auth, cookie) = live().await;
    harness.post("/api/v1/lights/discover", &cookie, json!({})).await.assert_ok();
    harness.post("/api/v1/lights", &cookie, json!({ "mac": DISCOVERED_MACS[0], "name": "One" })).await.assert_ok();
    let target = format!("wiz-{}", DISCOVERED_MACS[0]);
    harness
        .app
        .store
        .with(|conn| conn.execute("UPDATE devices SET attrs = json_set(attrs, '$.address', '127.0.0.1:9')", []))
        .unwrap();
    harness.app.sync_directory().unwrap();

    let found = harness.post("/api/v1/lights/discover", &cookie, json!({})).await;
    assert_eq!(found.body["found"][0]["moved"], json!(true), "{}", found.text);
    assert_eq!(found.body["found"][1]["moved"], json!(false), "{}", found.text);
    let not_yet = harness.execute(&cookie, "light.power", &target, json!({ "on": false })).await;
    assert_ne!(not_yet.phase(), "confirmed", "{}", not_yet.text);
    assert!(rig.unknown_bulbs[0].state.lock().unwrap().on, "the light moved without being accepted");

    let accepted = accept_address(&harness, &cookie, &target).await;
    accepted.assert_ok();
    assert_eq!(accepted.body["address"], json!(rig.unknown_bulbs[0].addr.to_string()));
    assert!(harness.audit_has("done"));
    let done = harness.execute(&cookie, "light.power", &target, json!({ "on": false })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert!(!rig.unknown_bulbs[0].state.lock().unwrap().on);

    let again = harness.post("/api/v1/lights/discover", &cookie, json!({})).await;
    assert_eq!(again.body["found"][0]["moved"], json!(false), "{}", again.text);
}

#[tokio::test]
async fn something_else_answering_with_a_lights_mac_cannot_move_it() {
    let (rig, harness, _auth, cookie) = live().await;
    harness.post("/api/v1/lights/discover", &cookie, json!({})).await.assert_ok();
    harness.post("/api/v1/lights", &cookie, json!({ "mac": DISCOVERED_MACS[0], "name": "One" })).await.assert_ok();
    let target = format!("wiz-{}", DISCOVERED_MACS[0]);

    // Inside the network and on the right port, so only the owner can tell.
    let impostor = MockBulb::start_at(&format!("127.0.0.77:{}", rig.broadcast.addr.port()), DISCOVERED_MACS[0]).await;
    rig.broadcast.answered_only_by(vec![impostor.addr]);
    let found = harness.post("/api/v1/lights/discover", &cookie, json!({})).await;
    assert_eq!(found.body["found"][0]["address"], json!(impostor.addr.to_string()), "{}", found.text);
    assert_eq!(found.body["found"][0]["moved"], json!(true), "{}", found.text);

    let done = harness.execute(&cookie, "light.power", &target, json!({ "on": false })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert!(!rig.unknown_bulbs[0].state.lock().unwrap().on);
    assert_eq!(impostor.traffic(), 0, "a command went to whoever answered last");
}

#[tokio::test]
async fn a_reply_that_cannot_be_a_bulb_is_not_believed() {
    let (rig, harness, _auth, cookie) = live().await;
    let other_port = MockBulb::start_at("127.0.0.78:0", "a8bb500000aa").await;
    let other_network = MockBulb::start_at(&format!("127.0.1.78:{}", rig.broadcast.addr.port()), "a8bb500000bb").await;
    rig.broadcast.also_answered_by(other_port.addr);
    rig.broadcast.also_answered_by(other_network.addr);

    let found = harness.post("/api/v1/lights/discover", &cookie, json!({})).await;
    let macs: Vec<&str> =
        found.body["found"].as_array().unwrap().iter().map(|bulb| bulb["mac"].as_str().unwrap()).collect();
    assert_eq!(macs, DISCOVERED_MACS, "{}", found.text);
    harness
        .post("/api/v1/lights", &cookie, json!({ "mac": "a8bb500000aa", "name": "x" }))
        .await
        .assert_refused(StatusCode::CONFLICT, "light_not_discovered");
}

#[tokio::test]
async fn a_new_address_can_only_be_accepted_for_a_light_a_discovery_found() {
    let (_rig, harness, _auth, cookie) = live().await;
    accept_address(&harness, &cookie, "wiz-a8bb50aaaaaa").await.assert_refused(StatusCode::NOT_FOUND, "unknown_device");
    harness.post("/api/v1/lights/discover", &cookie, json!({})).await.assert_ok();
    harness.post("/api/v1/lights", &cookie, json!({ "mac": DISCOVERED_MACS[0], "name": "One" })).await.assert_ok();
    let target = format!("wiz-{}", DISCOVERED_MACS[0]);
    harness.app.store.with(|conn| conn.execute("DELETE FROM device_candidates", [])).unwrap();
    accept_address(&harness, &cookie, &target).await.assert_refused(StatusCode::CONFLICT, "light_not_discovered");
    // The address is never taken from the request.
    let given = harness
        .send(
            Method::PUT,
            &format!("/api/v1/lights/{target}/address"),
            Some(&cookie),
            Some(ORIGIN),
            Some(json!({ "address": "192.0.2.1:38899" })),
        )
        .await;
    given.assert_refused(StatusCode::CONFLICT, "light_not_discovered");
}

#[tokio::test]
async fn in_dry_run_discovery_sends_nothing_and_adds_nothing() {
    let (rig, harness, _auth, cookie) = rig_harness(false, |_| {}).await;
    let found = harness.post("/api/v1/lights/discover", &cookie, json!({})).await;
    found.assert_ok();
    assert_eq!(found.body["simulated"], json!(true));
    assert_eq!(found.body["found"], json!([]));
    harness
        .post("/api/v1/lights", &cookie, json!({ "mac": DISCOVERED_MACS[0], "name": "x" }))
        .await
        .assert_refused(StatusCode::CONFLICT, "light_not_discovered");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.traffic(), 0);
}

#[tokio::test]
async fn discovery_without_a_configured_broadcast_says_so() {
    let (rig, harness, _auth, cookie) = rig_harness(true, |site| {
        site.instance("wiz").as_object_mut().unwrap().remove("broadcast");
    })
    .await;
    let refused = harness.post("/api/v1/lights/discover", &cookie, json!({})).await;
    refused.assert_refused(StatusCode::CONFLICT, "discovery_failed");
    assert!(refused.text.contains("broadcast"), "{}", refused.text);
    assert_eq!(rig.broadcast.traffic(), 0);
}

// ----- pairing the Hue Bridge

struct PairingBridge {
    http: MockHttp,
    button: Arc<AtomicBool>,
}

const NEW_HUE_KEY: &str = "fresh-hue-application-key-123";

async fn pairing_bridge() -> PairingBridge {
    let button = Arc::new(AtomicBool::new(false));
    let pressed = button.clone();
    let http = MockHttp::start(move |seen| {
        if seen.method == "POST" && seen.path == "/api" {
            return if pressed.load(Ordering::SeqCst) {
                (200, json!([{ "success": { "username": NEW_HUE_KEY } }]).to_string())
            } else {
                (
                    200,
                    json!([{ "error": { "type": 101, "address": "", "description": "link button not pressed" } }])
                        .to_string(),
                )
            };
        }
        if seen.path == format!("/api/{NEW_HUE_KEY}/groups/0") {
            return (200, json!({ "name": "Group 0" }).to_string());
        }
        // The setup lists the bridge's lights once it has a key. It has none.
        if seen.path == format!("/api/{NEW_HUE_KEY}/lights") {
            return (200, json!({}).to_string());
        }
        (200, json!([{ "error": { "type": 1 } }]).to_string())
    })
    .await;
    PairingBridge { http, button }
}

/// The rig, with its Hue instance on a bridge that has not been paired: its key
/// slot is empty. Also gives the file that slot is kept in.
async fn with_bridge(
    live: bool,
) -> (Rig, PairingBridge, FileSecretStore, std::path::PathBuf, Harness, SoftPasskey, String) {
    let bridge = pairing_bridge().await;
    let url = bridge.http.url.clone();
    let (rig, harness, auth, cookie) = rig_harness(live, move |site| {
        *site.instance("hue") = json!({ "url": url });
        site.forget("hue", "app_key");
    })
    .await;
    let secrets = harness.app.config.secrets_path();
    let key_file = secrets.join("hue").join("app_key");
    (rig, bridge, FileSecretStore::new(secrets), key_file, harness, auth, cookie)
}

#[tokio::test]
async fn pairing_needs_the_link_button_and_then_keeps_the_key_out_of_sight() {
    let (_rig, bridge, _dir, key_file, harness, _auth, cookie) = with_bridge(true).await;
    let early = harness.post("/api/v1/lights/hue/pair", &cookie, json!({})).await;
    early.assert_refused(StatusCode::CONFLICT, "link_button_not_pressed");
    assert!(early.text.contains("link button"), "{}", early.text);
    assert!(!key_file.exists());

    bridge.button.store(true, Ordering::SeqCst);
    let paired = harness.post("/api/v1/lights/hue/pair", &cookie, json!({})).await;
    paired.assert_ok();
    assert_eq!(paired.body["paired"], json!(true));
    assert!(!paired.text.contains(NEW_HUE_KEY), "{}", paired.text);
    assert_eq!(std::fs::read_to_string(&key_file).unwrap().trim(), NEW_HUE_KEY);
    assert!(!harness.dump("audit_log").contains(NEW_HUE_KEY));

    // The running service starts using it straight away.
    let state = harness.get("/api/v1/state/hue", &cookie).await;
    assert_eq!(state.body["available"], json!(true), "{}", state.text);
}

#[tokio::test]
async fn an_existing_key_is_never_replaced() {
    let (_rig, bridge, store, key_file, harness, _auth, cookie) = with_bridge(true).await;
    bridge.button.store(true, Ordering::SeqCst);
    store.write("hue", "app_key", &Credential::new("existing-key")).unwrap();
    harness
        .post("/api/v1/lights/hue/pair", &cookie, json!({}))
        .await
        .assert_refused(StatusCode::CONFLICT, "already_paired");
    assert_eq!(std::fs::read_to_string(&key_file).unwrap().trim(), "existing-key");
    assert!(bridge.http.posts().is_empty());
}

#[tokio::test]
async fn pairing_is_owner_only() {
    let (_rig, bridge, _dir, key_file, harness, mut owner, cookie) = with_bridge(true).await;
    let (_family, family_cookie, _id) = harness.add_family(&cookie, &mut owner, "Alice").await;
    bridge.button.store(true, Ordering::SeqCst);
    harness
        .post("/api/v1/lights/hue/pair", &family_cookie, json!({}))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "owner_only");
    assert!(bridge.http.posts().is_empty());
    assert!(!key_file.exists());
}

#[tokio::test]
async fn in_dry_run_pairing_sends_nothing_and_writes_nothing() {
    let (_rig, bridge, _dir, key_file, harness, _auth, cookie) = with_bridge(false).await;
    bridge.button.store(true, Ordering::SeqCst);
    let simulated = harness.post("/api/v1/lights/hue/pair", &cookie, json!({})).await;
    simulated.assert_ok();
    assert_eq!(simulated.body["simulated"], json!(true));
    assert_eq!(simulated.body["paired"], json!(false));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(bridge.http.traffic(), 0);
    assert!(!key_file.exists());
}

#[tokio::test]
async fn the_hue_bridge_itself_stays_read_only_after_pairing() {
    let (_rig, bridge, _dir, _key_file, harness, _auth, cookie) = with_bridge(true).await;
    bridge.button.store(true, Ordering::SeqCst);
    harness.post("/api/v1/lights/hue/pair", &cookie, json!({})).await.assert_ok();
    let before = bridge.http.posts().len();
    let catalog = harness.get("/api/v1/catalog", &cookie).await;
    let writable: Vec<&Value> = catalog.body["actions"]
        .as_array()
        .unwrap()
        .iter()
        // The integration called `hue` is set up again or removed by its id; that aims at
        // the instance, not at the bridge device that shares the name.
        .filter(|a| {
            a["id"] != json!("state.read")
                && a["target_kind"] != json!("instance")
                && a["targets"].as_array().is_some_and(|t| t.contains(&json!("hue")))
        })
        .collect();
    assert!(writable.is_empty(), "{writable:?}");
    assert_eq!(bridge.http.posts().len(), before);
}

// ----- lights on the Hue Bridge

/// A live service with one colour light on the bridge, found and added.
async fn with_hue_light() -> (Rig, Harness, SoftPasskey, String, String) {
    let (rig, harness, auth, cookie) = live().await;
    rig.hue_bridge.lock().unwrap().lights.insert("1".to_string(), HueLight::colour("Lightstrip"));
    harness.post("/api/v1/lights/discover", &cookie, json!({})).await.assert_ok();
    let added = harness.post("/api/v1/lights", &cookie, json!({ "mac": "hue-1", "name": "Lightstrip" })).await;
    added.assert_ok();
    let target = added.body["light"]["target"].as_str().unwrap().to_string();
    (rig, harness, auth, cookie, target)
}

#[tokio::test]
async fn discovery_lists_the_bridges_lights_next_to_the_bulbs() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.hue_bridge.lock().unwrap().lights.insert("1".to_string(), HueLight::colour("Lightstrip\u{7}"));
    let found = harness.post("/api/v1/lights/discover", &cookie, json!({})).await;
    found.assert_ok();
    let list = found.body["found"].as_array().unwrap();
    assert_eq!(list.len(), 3, "{}", found.text);
    assert_eq!(list[2]["mac"], json!("hue-1"));
    assert_eq!(list[2]["name"], json!("Lightstrip"));
    assert_eq!(list[2]["kind"], json!("hue"));
    assert_eq!(list[2]["added"], json!(false));
    assert_eq!(found.body["hue_error"], Value::Null);
    assert!(!found.text.contains(HUE_KEY), "{}", found.text);
}

#[tokio::test]
async fn a_light_from_the_bridge_is_added_the_same_way_and_says_so_after() {
    let (rig, harness, _auth, cookie, target) = with_hue_light().await;
    assert_eq!(target, "hue-1");
    let devices = harness.get("/api/v1/devices", &cookie).await;
    assert!(devices.text.contains("Lightstrip"), "{}", devices.text);
    let found = harness.post("/api/v1/lights/discover", &cookie, json!({})).await;
    assert_eq!(found.body["found"][2]["added"], json!(true), "{}", found.text);
    harness
        .post("/api/v1/lights", &cookie, json!({ "mac": "hue-1", "name": "Twice" }))
        .await
        .assert_refused(StatusCode::CONFLICT, "light_exists");
    assert!(rig.hue.requests().iter().all(|r| r.method != "PUT"), "adding a light changes nothing on it");
}

#[tokio::test]
async fn a_light_the_bridge_does_not_have_cannot_be_added() {
    let (rig, harness, _auth, cookie) = live().await;
    harness.post("/api/v1/lights/discover", &cookie, json!({})).await.assert_ok();
    harness
        .post("/api/v1/lights", &cookie, json!({ "mac": "hue-7", "name": "Forged" }))
        .await
        .assert_refused(StatusCode::CONFLICT, "light_not_discovered");
    for bad in ["hue-", "hue-0", "hue-01", "hue-1/../2", "hue-a", "hue-123456", "hue"] {
        harness
            .post("/api/v1/lights", &cookie, json!({ "mac": bad, "name": "x" }))
            .await
            .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_mac");
    }
    assert!(rig.hue.requests().iter().all(|r| r.method == "GET"));
}

#[tokio::test]
async fn a_bridge_that_cannot_be_read_does_not_spoil_the_bulbs_that_were_found() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.hue.hang.store(true, Ordering::SeqCst);
    let found = harness.post("/api/v1/lights/discover", &cookie, json!({})).await;
    found.assert_ok();
    assert_eq!(found.body["found"].as_array().unwrap().len(), 2, "{}", found.text);
    assert!(found.body["hue_error"].is_string(), "{}", found.text);
    assert!(!found.text.contains(HUE_KEY), "{}", found.text);
}

#[tokio::test]
async fn a_hue_light_is_used_through_the_same_actions_as_a_bulb() {
    let (rig, harness, _auth, cookie, target) = with_hue_light().await;
    let done = harness.execute(&cookie, "light.power", &target, json!({ "on": true })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    let done = harness.execute(&cookie, "light.brightness", &target, json!({ "percent": 42 })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    let done = harness.execute(&cookie, "light.hue", &target, json!({ "hue": 200 })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);

    let state = harness.get(&format!("/api/v1/state/{target}"), &cookie).await;
    assert_eq!(state.body["available"], json!(true), "{}", state.text);
    assert_eq!(state.body["plugin"], json!("hue"), "{}", state.text);
    assert_eq!(state.body["instance"], json!("hue"), "{}", state.text);
    assert_eq!(state.body["state"]["kind"], json!("light"), "{}", state.text);
    assert_eq!(state.body["state"]["on"], json!(true), "{}", state.text);
    assert_eq!(state.body["state"]["bri"], json!(42), "{}", state.text);
    assert_eq!(state.body["state"]["hue"], json!(200), "{}", state.text);
    assert_eq!(state.body["state"]["color"], json!(true), "{}", state.text);
    assert_eq!(rig.broadcast.traffic(), 1, "only the discovery touched the WiZ side");
}

#[tokio::test]
async fn the_bare_hue_target_is_still_the_bridges_reachability() {
    let (_rig, harness, _auth, cookie, _target) = with_hue_light().await;
    let state = harness.get("/api/v1/state/hue", &cookie).await;
    assert_eq!(state.body["available"], json!(true), "{}", state.text);
    assert_eq!(state.body["state"]["kind"], json!("bridge"), "{}", state.text);
    harness
        .execute(&cookie, "light.power", "hue", json!({ "on": true }))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
}

#[tokio::test]
async fn a_light_that_was_never_added_is_refused() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.hue_bridge.lock().unwrap().lights.insert("1".to_string(), HueLight::colour("Lightstrip"));
    harness
        .execute(&cookie, "light.power", "hue-1", json!({ "on": true }))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
    assert_eq!(rig.hue.traffic(), 0);
}

#[tokio::test]
async fn a_light_the_bridge_cannot_reach_is_no_response_and_unavailable() {
    let (rig, harness, _auth, cookie, target) = with_hue_light().await;
    rig.hue_bridge.lock().unwrap().lights.get_mut("1").unwrap().reachable = false;
    let done = harness.execute(&cookie, "light.power", &target, json!({ "on": true })).await;
    assert_eq!(done.phase(), "no_response", "{}", done.text);
    assert!(!harness.audit_has("confirmed"));
    let state = harness.get(&format!("/api/v1/state/{target}"), &cookie).await;
    assert_eq!(state.body["available"], json!(false), "{}", state.text);
    assert!(state.body.get("state").is_none(), "{}", state.text);
}

#[tokio::test]
async fn a_light_that_ignores_the_bridge_is_not_confirmed() {
    let (rig, harness, _auth, cookie, target) = with_hue_light().await;
    rig.hue_bridge.lock().unwrap().ignores = true;
    let done = harness.execute(&cookie, "light.power", &target, json!({ "on": true })).await;
    assert_eq!(done.phase(), "no_response", "{}", done.text);
}

#[tokio::test]
async fn an_error_from_the_bridge_is_a_failure_and_the_key_stays_hidden() {
    let (rig, harness, _auth, cookie, target) = with_hue_light().await;
    rig.hue_bridge.lock().unwrap().error = Some(7);
    let done = harness.execute(&cookie, "light.power", &target, json!({ "on": true })).await;
    assert_eq!(done.phase(), "failed", "{}", done.text);
    assert!(!done.text.contains(HUE_KEY), "{}", done.text);

    rig.hue_bridge.lock().unwrap().lights.clear();
    let gone = harness.get(&format!("/api/v1/state/{target}"), &cookie).await;
    assert_eq!(gone.body["available"], json!(false), "{}", gone.text);
    assert!(!gone.text.contains(HUE_KEY), "{}", gone.text);
    let read = harness.execute(&cookie, "state.read", &target, json!({})).await;
    assert_eq!(read.phase(), "failed", "{}", read.text);
    assert!(!read.text.contains(HUE_KEY), "{}", read.text);

    for table in ["audit_log", "action_runs", "devices", "device_candidates"] {
        assert!(!harness.dump(table).contains(HUE_KEY), "the key reached {table}");
    }
    assert!(!harness.get("/api/v1/audit", &cookie).await.text.contains(HUE_KEY));
    assert!(!harness.get("/api/v1/lights", &cookie).await.text.contains(HUE_KEY));
}

#[tokio::test]
async fn a_colour_on_a_light_without_colour_says_so() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.hue_bridge.lock().unwrap().lights.insert("3".to_string(), HueLight::white("Hall"));
    harness.post("/api/v1/lights/discover", &cookie, json!({})).await.assert_ok();
    harness.post("/api/v1/lights", &cookie, json!({ "mac": "hue-3", "name": "Hall" })).await.assert_ok();
    // The bridge said it is white when it was found, so it has no colour to set.
    let devices = harness.get("/api/v1/devices", &cookie).await;
    let hall =
        devices.body["devices"].as_array().unwrap().iter().find(|d| d["target"] == json!("hue-3")).unwrap().clone();
    assert!(!hall["capabilities"].as_array().unwrap().contains(&json!({ "cap": "color" })), "{hall}");
    harness
        .execute(&cookie, "light.hue", "hue-3", json!({ "hue": 10 }))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
    assert!(rig.hue.requests().iter().all(|r| r.method == "GET"));
}

#[tokio::test]
async fn a_room_and_its_scene_cover_bulbs_and_hue_lights_together() {
    let (rig, harness, _auth, cookie, target) = with_hue_light().await;
    let id = room(&harness, &cookie, "Hall", &["bulb1", &target]).await;
    let done = harness.execute(&cookie, "room.brightness", &id, json!({ "percent": 30 })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(rig.bulbs[0].state.lock().unwrap().dimming, 30);
    assert_eq!(rig.hue_bridge.lock().unwrap().lights["1"].bri, 75);

    let path = format!("/api/v1/rooms/{id}/scenes");
    harness
        .post(&path, &cookie, json!({ "name": "Glow", "settings": [{ "target": "bulb1" }, { "target": target, "percent": 80, "hue": 120 }] }))
        .await
        .assert_ok();
    let done = harness.execute(&cookie, "room.scene", &id, json!({ "scene": "Glow" })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert!(!rig.bulbs[0].state.lock().unwrap().on);
    assert_eq!(rig.hue_bridge.lock().unwrap().lights["1"].hue, Some(21845));
}

#[tokio::test]
async fn a_hue_light_that_does_not_answer_leaves_the_room_a_success_with_a_note() {
    let (rig, harness, _auth, cookie, target) = with_hue_light().await;
    let id = room(&harness, &cookie, "Hall", &["bulb1", &target]).await;
    rig.hue_bridge.lock().unwrap().lights.get_mut("1").unwrap().reachable = false;
    let done = harness.execute(&cookie, "room.power", &id, json!({ "on": false })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(done.body["detail"], json!("1 of 2 lights did not answer."));
}

#[tokio::test]
async fn family_can_use_a_light_from_the_bridge() {
    let (_rig, harness, mut owner, cookie, target) = with_hue_light().await;
    let (_family, family_cookie, _id) = harness.add_family(&cookie, &mut owner, "Alice").await;
    let done = harness.execute(&family_cookie, "light.power", &target, json!({ "on": true })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    harness.get(&format!("/api/v1/state/{target}"), &family_cookie).await.assert_ok();
}

#[tokio::test]
async fn in_dry_run_a_hue_light_is_simulated_and_the_bridge_hears_nothing() {
    let (rig, harness, _auth, cookie) = rig_harness(false, |_| {}).await;
    rig.hue_bridge.lock().unwrap().lights.insert("1".to_string(), HueLight::colour("Lightstrip"));
    let found = harness.post("/api/v1/lights/discover", &cookie, json!({})).await;
    assert_eq!(found.body["found"], json!([]), "{}", found.text);
    harness
        .post("/api/v1/lights", &cookie, json!({ "mac": "hue-1", "name": "x" }))
        .await
        .assert_refused(StatusCode::CONFLICT, "light_not_discovered");

    harness.app.store.with(|conn| store_light(conn, "hue-1", "hue-1", "hue-bridge", "Strip")).unwrap();
    harness.app.sync_directory().unwrap();
    for (action, params) in [
        ("light.power", json!({ "on": true })),
        ("light.brightness", json!({ "percent": 5 })),
        ("light.hue", json!({ "hue": 5 })),
    ] {
        let done = harness.execute(&cookie, action, "hue-1", params).await;
        assert_eq!(done.phase(), "simulated", "{action}: {}", done.text);
    }
    harness.get("/api/v1/state", &cookie).await.assert_ok();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.hue.traffic(), 0, "dry-run reached the bridge");
    assert_eq!(rig.traffic(), 0, "dry-run reached a device");
}

// ----- what "All" reaches, and what a room does not

/// A Hue light and a stored WiZ light, on top of the four fixed bulbs.
async fn with_every_kind_of_light() -> (Rig, Harness, SoftPasskey, String, String, String) {
    let (rig, harness, auth, cookie, hue) = with_hue_light().await;
    let added =
        harness.post("/api/v1/lights", &cookie, json!({ "mac": DISCOVERED_MACS[0], "name": "Hall lamp" })).await;
    added.assert_ok();
    let wiz = added.body["light"]["target"].as_str().unwrap().to_string();
    (rig, harness, auth, cookie, hue, wiz)
}

#[tokio::test]
async fn all_the_lights_means_a_hue_light_and_a_stored_wiz_light_too() {
    let (rig, harness, _auth, cookie, hue, _wiz) = with_every_kind_of_light().await;
    let done = harness.execute(&cookie, "lights.power", "all_bulbs", json!({ "on": true })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(done.body["steps"].as_array().unwrap().len(), 6, "{}", done.text);
    assert!(rig.hue_bridge.lock().unwrap().lights["1"].on);
    assert!(rig.unknown_bulbs[0].state.lock().unwrap().on);

    let done = harness.execute(&cookie, "lights.brightness", "all_bulbs", json!({ "percent": 33 })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(rig.unknown_bulbs[0].state.lock().unwrap().dimming, 33);
    assert_eq!(harness.get(&format!("/api/v1/state/{hue}"), &cookie).await.body["state"]["bri"], json!(33));

    let done = harness.execute(&cookie, "lights.power", "all_bulbs", json!({ "on": false })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert!(!rig.hue_bridge.lock().unwrap().lights["1"].on);
    assert!(!rig.unknown_bulbs[0].state.lock().unwrap().on);
}

#[tokio::test]
async fn a_scene_sets_a_light_with_no_level_of_its_own_like_its_first_bulb() {
    let (rig, harness, _auth, cookie, hue, _wiz) = with_every_kind_of_light().await;
    let done = harness.execute(&cookie, "scene.apply", "evening", json!({})).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    let state = harness.get(&format!("/api/v1/state/{hue}"), &cookie).await;
    assert_eq!(state.body["state"]["bri"], json!(40), "{}", state.text);
    assert_eq!(state.body["state"]["hue"], json!(34), "{}", state.text);
    assert_eq!(rig.unknown_bulbs[0].state.lock().unwrap().dimming, 40);
    assert!(!rig.bulbs[2].state.lock().unwrap().on, "bulb 3 keeps its own level");

    let done = harness.execute(&cookie, "scene.apply", "all_off", json!({})).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert!(!rig.hue_bridge.lock().unwrap().lights["1"].on);
    assert!(!rig.unknown_bulbs[0].state.lock().unwrap().on);
}

#[tokio::test]
async fn movie_reaches_the_added_lights_and_then_the_tv() {
    let (rig, harness, _auth, cookie, hue, wiz) = with_every_kind_of_light().await;
    let prepared = harness.prepare(&cookie, "scene.movie", "movie", json!({})).await;
    let done = harness.confirm_tap(&cookie, &prepared).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    let targets: Vec<&str> =
        done.body["steps"].as_array().unwrap().iter().map(|s| s["target"].as_str().unwrap()).collect();
    assert_eq!(targets, ["bulb1", "bulb2", "bulb3", "bulb4", hue.as_str(), wiz.as_str(), "tv", "tv", "sonos"]);
    assert_eq!(rig.tv.state.lock().unwrap().app, "com.webos.app.hdmi1");
}

#[tokio::test]
async fn movie_changes_only_the_lights_in_the_living_room() {
    let (rig, harness, _auth, cookie, hue, wiz) = with_every_kind_of_light().await;
    room(&harness, &cookie, "Living room", &[&hue]).await;
    room(&harness, &cookie, "Bedroom", &["bulb1", "bulb2", &wiz]).await;
    let bedroom = |rig: &Rig| rig.bulbs[0].traffic() + rig.bulbs[1].traffic() + rig.unknown_bulbs[0].traffic();
    let before = bedroom(&rig);
    let prepared = harness.prepare(&cookie, "scene.movie", "movie", json!({})).await;
    let done = harness.confirm_tap(&cookie, &prepared).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    let targets: Vec<&str> =
        done.body["steps"].as_array().unwrap().iter().map(|s| s["target"].as_str().unwrap()).collect();
    assert_eq!(targets, [hue.as_str(), "tv", "tv", "sonos"]);
    assert_eq!(bedroom(&rig), before, "Movie reached the bedroom");
    assert_eq!(rig.tv.state.lock().unwrap().app, "com.webos.app.hdmi1");
}

#[tokio::test]
async fn movie_follows_the_tv_to_the_room_it_is_in() {
    let (_rig, harness, _auth, cookie, hue, wiz) = with_every_kind_of_light().await;
    room(&harness, &cookie, "Living room", &[&hue]).await;
    room(&harness, &cookie, "Den", &["tv", &wiz]).await;
    let prepared = harness.prepare(&cookie, "scene.movie", "movie", json!({})).await;
    let done = harness.confirm_tap(&cookie, &prepared).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    let targets: Vec<&str> =
        done.body["steps"].as_array().unwrap().iter().map(|s| s["target"].as_str().unwrap()).collect();
    assert_eq!(targets, [wiz.as_str(), "tv", "tv", "sonos"]);
}

#[tokio::test]
async fn anything_can_be_put_in_a_room_without_it_counting_as_a_light() {
    let (_rig, harness, _auth, cookie, hue, _wiz) = with_every_kind_of_light().await;
    let devices = ["host", "printer", "camera", "feeder", "litter", "sonos", "tv"];
    let office = room(&harness, &cookie, "Office", &devices).await;
    let rooms = harness.get("/api/v1/rooms", &cookie).await;
    let entry = rooms.body["rooms"].as_array().unwrap().iter().find(|r| r["id"] == json!(office)).unwrap();
    let placed: Vec<&str> =
        entry["devices"].as_array().unwrap().iter().map(|d| d["target"].as_str().unwrap()).collect();
    for device in devices {
        assert!(placed.contains(&device), "{device} in {placed:?}");
    }
    harness
        .execute(&cookie, "room.power", &office, json!({ "on": true }))
        .await
        .assert_refused(StatusCode::CONFLICT, "room_has_no_lights");
    let _ = hue;
}

#[tokio::test]
async fn a_room_never_touches_a_light_in_another_room() {
    let (rig, harness, _auth, cookie, hue, wiz) = with_every_kind_of_light().await;
    let living = room(&harness, &cookie, "Living room", &[&hue]).await;
    let bedroom = room(&harness, &cookie, "Bedroom", &["bulb1", &wiz]).await;

    let puts = |rig: &Rig| rig.hue.requests().iter().filter(|r| r.method == "PUT").count();
    let done = harness.execute(&cookie, "room.power", &bedroom, json!({ "on": true })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(done.body["steps"].as_array().unwrap().len(), 2, "{}", done.text);
    assert_eq!(puts(&rig), 0, "the bedroom reached the living room's light");
    assert!(!rig.hue_bridge.lock().unwrap().lights["1"].on);
    assert_eq!(rig.bulbs[1].traffic(), 0, "a bulb in no room was touched");

    let before = rig.unknown_bulbs[0].traffic() + rig.bulbs[0].traffic();
    let done = harness.execute(&cookie, "room.brightness", &living, json!({ "percent": 50 })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(done.body["steps"].as_array().unwrap().len(), 1, "{}", done.text);
    assert_eq!(rig.unknown_bulbs[0].traffic() + rig.bulbs[0].traffic(), before, "the living room reached the bedroom");
    assert_eq!(puts(&rig), 1);
}

#[tokio::test]
async fn voice_turning_off_the_lights_reaches_a_hue_light() {
    let speech = FakeSpeech::start().await;
    let config = speech.config();
    let (rig, harness, _auth, cookie) = rig_harness(true, move |c| c.speech = Some(config)).await;
    rig.hue_bridge
        .lock()
        .unwrap()
        .lights
        .insert("1".to_string(), HueLight { on: true, ..HueLight::colour("Lightstrip") });
    harness.post("/api/v1/lights/discover", &cookie, json!({})).await.assert_ok();
    harness.post("/api/v1/lights", &cookie, json!({ "mac": "hue-1", "name": "Lightstrip" })).await.assert_ok();

    speech.say("lights off");
    let done = post_voice(&harness, &cookie).await;
    assert_eq!(done.body["kind"], json!("executed"), "{}", done.text);
    assert!(!rig.hue_bridge.lock().unwrap().lights["1"].on);
}

// ----- voice

async fn with_voice() -> (Rig, FakeSpeech, Harness, SoftPasskey, String, String) {
    let speech = FakeSpeech::start().await;
    let config = speech.config();
    let (rig, harness, auth, cookie) = rig_harness(true, move |c| c.speech = Some(config)).await;
    let kitchen = room(&harness, &cookie, "Kitchen", &["bulb1", "bulb2"]).await;
    room(&harness, &cookie, "Hall", &["bulb3"]).await;
    (rig, speech, harness, auth, cookie, kitchen)
}

#[tokio::test]
async fn voice_resolves_a_room_and_touches_only_it() {
    let (rig, speech, harness, _auth, cookie, _kitchen) = with_voice().await;
    speech.say("turn off the kitchen lights");
    let done = post_voice(&harness, &cookie).await;
    assert_eq!(done.body["kind"], json!("executed"), "{}", done.text);
    assert_eq!(done.body["run"]["action"], json!("room.power"));
    assert!(!rig.bulbs[0].state.lock().unwrap().on);
    assert!(!rig.bulbs[1].state.lock().unwrap().on);
    assert_eq!(rig.bulbs[2].traffic(), 0);
    assert_eq!(rig.bulbs[3].traffic(), 0);
}

#[tokio::test]
async fn voice_finds_a_rooms_scene() {
    let (rig, speech, harness, _auth, cookie, kitchen) = with_voice().await;
    harness
        .post(
            &format!("/api/v1/rooms/{kitchen}/scenes"),
            &cookie,
            json!({ "name": "Late Dinner", "settings": [{ "target": "bulb1", "percent": 20, "hue": 30 }] }),
        )
        .await
        .assert_ok();
    speech.say("late dinner in the kitchen");
    let done = post_voice(&harness, &cookie).await;
    assert_eq!(done.body["kind"], json!("executed"), "{}", done.text);
    assert_eq!(rig.bulbs[0].state.lock().unwrap().dimming, 20);
    assert_eq!(rig.bulbs[2].traffic(), 0);
}

#[tokio::test]
async fn a_room_scene_can_share_a_name_with_a_whole_house_scene() {
    let (rig, speech, harness, _auth, cookie, kitchen) = with_voice().await;
    harness
        .post(
            &format!("/api/v1/rooms/{kitchen}/scenes"),
            &cookie,
            json!({ "name": "Evening", "settings": [{ "target": "bulb1", "percent": 35, "hue": 30 }] }),
        )
        .await
        .assert_ok();
    speech.say("evening in the kitchen");
    let done = post_voice(&harness, &cookie).await;
    assert_eq!(done.body["run"]["action"], json!("room.scene"), "{}", done.text);
    assert_eq!(rig.bulbs[0].state.lock().unwrap().dimming, 35);
    for bulb in &rig.bulbs[1..] {
        assert_eq!(bulb.traffic(), 0, "the whole-house scene must not have run");
    }
}

#[tokio::test]
async fn a_scene_cannot_take_a_command_word_for_its_name() {
    let (_rig, harness, _auth, cookie) = live().await;
    let id = room(&harness, &cookie, "Kitchen", &["bulb1"]).await;
    harness
        .post(
            &format!("/api/v1/rooms/{id}/scenes"),
            &cookie,
            json!({ "name": "Off", "settings": [{ "target": "bulb1" }] }),
        )
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "reserved_name");
}

#[tokio::test]
async fn voice_runs_nothing_when_two_rooms_are_named() {
    let (rig, speech, harness, _auth, cookie, _kitchen) = with_voice().await;
    speech.say("turn off the kitchen and the hall");
    let reply = post_voice(&harness, &cookie).await;
    assert_eq!(reply.body["kind"], json!("choices"), "{}", reply.text);
    assert_eq!(reply.body["choices"].as_array().unwrap().len(), 2);
    assert_eq!(rig.traffic(), 0);
}

#[tokio::test]
async fn a_renamed_room_answers_to_its_new_name() {
    let (rig, speech, harness, _auth, cookie, kitchen) = with_voice().await;
    harness
        .send(
            Method::PATCH,
            &format!("/api/v1/rooms/{kitchen}"),
            Some(&cookie),
            Some(ORIGIN),
            Some(json!({ "name": "Galley" })),
        )
        .await
        .assert_ok();
    speech.say("turn off the galley lights");
    let done = post_voice(&harness, &cookie).await;
    assert_eq!(done.body["kind"], json!("executed"), "{}", done.text);
    assert!(!rig.bulbs[0].state.lock().unwrap().on);
}

#[tokio::test]
async fn family_can_use_a_room_by_voice() {
    let (_rig, speech, harness, mut owner, cookie, _kitchen) = with_voice().await;
    let (_family, family_cookie, _id) = harness.add_family(&cookie, &mut owner, "Alice").await;
    speech.say("kitchen lights on");
    let done = post_voice(&harness, &family_cookie).await;
    assert_eq!(done.body["kind"], json!("executed"), "{}", done.text);
}

#[tokio::test]
async fn in_dry_run_voice_on_a_room_reaches_no_light() {
    let speech = FakeSpeech::start().await;
    let config = speech.config();
    let (rig, harness, _auth, cookie) = rig_harness(false, move |c| c.speech = Some(config)).await;
    room(&harness, &cookie, "Kitchen", &["bulb1"]).await;
    speech.say("turn off the kitchen lights");
    let done = post_voice(&harness, &cookie).await;
    assert_eq!(done.body["kind"], json!("executed"), "{}", done.text);
    assert_eq!(done.body["phase"], json!("simulated"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.traffic(), 0);
}
