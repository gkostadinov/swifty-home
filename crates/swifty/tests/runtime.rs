//! The hub on its plugins: what it takes from the build's manifests, how it finds
//! the instances another process stored, what it does with devices of a plugin it
//! lacks, the devices an instance's config names, and which telemetry node feeds
//! the charts.

mod common;

use std::collections::BTreeSet;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{json, Value};
use swifty_hub::background::reload_if_moved;
use swifty_hub::store::Store;
use swifty_hub::{catalog, history, inventory, voice};
use swifty_plugin_api::{DeviceClass, Op};

fn listed_targets(devices: &Resp) -> BTreeSet<String> {
    devices.body["devices"].as_array().unwrap().iter().map(|d| d["target"].as_str().unwrap().to_string()).collect()
}

#[tokio::test]
async fn the_hub_takes_its_words_and_custom_actions_from_the_plugins_it_was_built_with() {
    let harness = Harness::new();
    let built: Vec<&str> = swifty::plugins::compiled_in().iter().map(|plugin| plugin.manifest().id).collect();
    let words: BTreeSet<&str> = harness.app.plugin_words.iter().map(|words| words.plugin).collect();
    assert_eq!(words, built.iter().copied().collect(), "every plugin of the build, once");

    let lexicon = voice::lexicon_for(&harness.app).unwrap();
    for (noun, class) in
        [("klipper", DeviceClass::Printer), ("sonos", DeviceClass::Speaker), ("gadget", DeviceClass::Other)]
    {
        assert_eq!(lexicon.nouns.get(noun), Some(&class), "{noun}");
    }
    assert_eq!(lexicon.normalize("clipper status"), "klipper status", "a manifest's mishearing");
    for dropped in &harness.app.dropped_words {
        assert!(built.contains(&dropped.plugin.as_str()), "{dropped:?}");
    }

    let custom = catalog::find(swifty_plugin_mock::CUSTOM_ACTION).expect("the mock plugin's action is in the catalog");
    assert!(custom.is_custom());
    assert!(catalog::registered().iter().any(|known| known.plugin == "mock"));
}

#[tokio::test]
async fn an_instance_another_process_stored_is_loaded_once_the_revision_moves() {
    let harness = Harness::new();
    let mut seen = harness.app.store.with(inventory::instances_rev).unwrap();
    assert!(!reload_if_moved(&harness.app, &mut seen).unwrap(), "nothing moved yet");

    // What `swifty plugin add` does beside the running hub: its own connection.
    let other = Store::open(&harness.app.config.database_path()).unwrap();
    other
        .tx(|tx| -> rusqlite::Result<()> {
            tx.execute(
                "INSERT INTO plugin_instances (id, plugin, config, config_version, enabled, created_at, created_by, updated_at)
                 VALUES ('kitchen', 'mock', ?1, 1, 1, 0, 'cli', 0)",
                [json!({ "devices": [{ "id": "lamp", "class": "light", "name": "Kitchen lamp" }] }).to_string()],
            )?;
            inventory::bump_instances_rev(tx).map(|_| ())
        })
        .unwrap();
    assert!(harness.app.registry.index().get("kitchen-lamp").is_none());

    assert!(reload_if_moved(&harness.app, &mut seen).unwrap());
    let lamp = harness.app.registry.index().get("kitchen-lamp").cloned().expect("the new instance's lamp");
    assert_eq!(
        (lamp.instance.as_str(), lamp.local_id.as_str(), lamp.name.as_str()),
        ("kitchen", "lamp", "Kitchen lamp")
    );
    assert!(!reload_if_moved(&harness.app, &mut seen).unwrap(), "loaded once");

    let (_auth, cookie) = harness.sign_in().await;
    let done = harness.execute(&cookie, "light.power", "kitchen-lamp", json!({ "on": true })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(harness.devices.commands(), [("lamp".to_string(), Op::LightPower { on: true })]);
}

#[tokio::test]
async fn a_device_of_a_plugin_this_build_lacks_is_not_listed_and_nothing_else_stops() {
    let harness = Harness::new();
    harness
        .app
        .store
        .with(|conn| {
            conn.execute(
                "INSERT INTO plugin_instances (id, plugin, config, config_version, enabled, created_at, created_by, updated_at)
                 VALUES ('blinds', 'acme', '{}', 1, 1, 0, 'test', 0)",
                [],
            )?;
            conn.execute(
                "INSERT INTO devices (target, instance_id, local_id, class, name, attrs, origin, status, first_seen)
                 VALUES ('blind1', 'blinds', 'blind1', 'other', 'Blind', '{}', 'config', 'active', 0)",
                [],
            )
        })
        .unwrap();
    harness.app.sync_directory().unwrap();
    assert!(harness.app.registry.instance("blinds").is_none());
    assert!(harness.app.registry.index().get("blind1").is_none());

    let (_auth, cookie) = harness.sign_in().await;
    assert!(!listed_targets(&harness.get("/api/v1/devices", &cookie).await).contains("blind1"));
    let done = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    // It is still stored, for a build that has the plugin.
    let kept: bool = harness
        .app
        .store
        .with(|conn| {
            conn.query_row("SELECT EXISTS(SELECT 1 FROM devices WHERE target = 'blind1')", [], |row| row.get(0))
        })
        .unwrap();
    assert!(kept);
}

#[tokio::test]
async fn the_devices_an_instance_config_names_follow_it() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    // A second instance names a light the house already has a `bulb1` for.
    harness.add_instance(
        "porch",
        "mock",
        json!({ "devices": [{ "id": "bulb1", "class": "light", "name": "Porch light", "target": "bulb1" }] }),
    );
    let index = harness.app.registry.index();
    assert_eq!(index.get("bulb1").unwrap().instance, "wiz", "a target never moves");
    assert_eq!(index.get("porch-bulb1").unwrap().name, "Porch light", "the hint was taken");

    let created = harness.post("/api/v1/rooms", &cookie, json!({ "name": "Porch" })).await;
    created.assert_ok();
    let id = created.body["room"]["id"].as_str().unwrap().to_string();
    harness
        .send(
            Method::PUT,
            &format!("/api/v1/rooms/{id}/devices"),
            Some(&cookie),
            Some(ORIGIN),
            Some(json!({ "targets": ["porch-bulb1", "bulb2"] })),
        )
        .await
        .assert_ok();

    // Renamed in the config: the same device, under the same target, with the new name.
    harness.set_instance_config(
        "porch",
        json!({ "devices": [{ "id": "bulb1", "class": "light", "name": "Front light", "target": "bulb1" }] }),
    );
    assert_eq!(harness.app.registry.index().get("porch-bulb1").unwrap().name, "Front light");

    // Taken out of the config: gone, and gone from its room.
    harness.set_instance_config("porch", json!({ "devices": [] }));
    assert!(harness.app.registry.index().get("porch-bulb1").is_none());
    let rooms = harness.get("/api/v1/rooms", &cookie).await;
    let room = rooms.body["rooms"].as_array().unwrap().iter().find(|room| room["id"] == json!(id)).unwrap().clone();
    let members: Vec<&str> =
        room["devices"].as_array().unwrap().iter().map(|device| device["target"].as_str().unwrap()).collect();
    assert_eq!(members, ["bulb2"], "{}", rooms.text);
    harness
        .execute(&cookie, "light.power", "porch-bulb1", json!({ "on": true }))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
}

#[tokio::test]
async fn the_charts_are_fed_by_the_telemetry_node_the_configuration_names() {
    let second = |house: &mut swifty_plugin_mock::House| {
        let row = json!({ "id": "node2", "class": "telemetry", "name": "Second node", "target": "node2" });
        let config = json!({ "devices": [row] });
        house.instances.push(swifty_plugin_mock::HouseInstance {
            id: "lab".to_string(),
            plugin: "mock".to_string(),
            config,
            config_version: 1,
            enabled: true,
        });
    };
    let harness = Harness::with_house(Vec::new(), second, |_| {});
    assert_eq!(history::telemetry_source(&harness.app).as_deref(), Some("collector"), "the first in target order");

    let harness = Harness::with_house(Vec::new(), second, |config| config.health.telemetry = Some("node2".to_string()));
    assert_eq!(history::telemetry_source(&harness.app).as_deref(), Some("node2"));
    harness.devices.set_collector(json!({ "pkg_temp_c": 99 }));
    harness
        .devices
        .add_state("node2", swifty_plugin_api::State::Telemetry { data: json!({ "pkg_temp_c": 41, "pkg_watts": 70 }) });
    assert_eq!(history::sample_collector(&harness.app).await.unwrap(), 2, "the named node's two readings");

    let unknown = Harness::with(|config| config.health.telemetry = Some("nowhere".to_string()));
    assert_eq!(history::telemetry_source(&unknown.app), None, "a node that is not here feeds nothing");
}

#[tokio::test]
async fn the_device_list_is_the_index() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let devices = harness.get("/api/v1/devices", &cookie).await;
    let index = harness.app.registry.index();
    let active: BTreeSet<String> = index.active().map(|device| device.target.clone()).collect();
    assert_eq!(listed_targets(&devices), active);
    let order: Vec<Value> = devices.body["devices"].as_array().unwrap().iter().map(|d| d["target"].clone()).collect();
    let mut sorted = order.clone();
    sorted.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    assert_eq!(order, sorted, "in target order");
}
