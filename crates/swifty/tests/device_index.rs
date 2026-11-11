//! The device index: what the registry routes a target by, and what `/api/v1/devices`
//! says each device is.

mod common;

use std::collections::BTreeSet;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{json, Value};
use swifty_plugin_api::{Audience, DeviceClass, Op, State};

use Audience::{Family, Owner};

/// The legacy house: each device, the instance that carries it, its class, and who may see it.
/// Every instance of the test house is one of the mock plugin.
const LEGACY_HOUSE: [(&str, &str, DeviceClass, Audience); 20] = [
    ("bulb1", "wiz", DeviceClass::Light, Family),
    ("bulb2", "wiz", DeviceClass::Light, Family),
    ("bulb3", "wiz", DeviceClass::Light, Family),
    ("bulb4", "wiz", DeviceClass::Light, Family),
    ("hue", "hue", DeviceClass::Bridge, Family),
    ("sonos", "sonos", DeviceClass::Speaker, Family),
    ("sub", "sonos", DeviceClass::Speaker, Family),
    ("tv", "webos", DeviceClass::Tv, Family),
    ("feeder", "petkit", DeviceClass::Feeder, Family),
    ("litter", "petkit", DeviceClass::Litter, Family),
    ("printer", "moonraker", DeviceClass::Printer, Owner),
    ("vm100", "proxmox", DeviceClass::Vm, Owner),
    ("vm101", "proxmox", DeviceClass::Vm, Owner),
    ("vm102", "proxmox", DeviceClass::Vm, Owner),
    ("vm103", "proxmox", DeviceClass::Vm, Owner),
    ("host", "proxmox", DeviceClass::Host, Owner),
    ("router", "router", DeviceClass::Router, Owner),
    ("camera", "camera", DeviceClass::Camera, Family),
    ("plex", "plex", DeviceClass::Service, Owner),
    ("collector", "collector", DeviceClass::Telemetry, Owner),
];

fn listed(response: &Resp, target: &str) -> Value {
    response.body["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|device| device["target"] == json!(target))
        .unwrap_or_else(|| panic!("{target} is not listed: {}", response.text))
        .clone()
}

fn targets(response: &Resp) -> BTreeSet<String> {
    response.body["devices"].as_array().unwrap().iter().map(|d| d["target"].as_str().unwrap().to_string()).collect()
}

#[tokio::test]
async fn the_legacy_house_is_twenty_devices_each_with_its_class_and_audience() {
    let harness = Harness::new();
    let index = harness.app.registry.index();
    assert_eq!(index.len(), 20);
    assert_eq!(index.active().count(), 20);
    for (target, instance, class, audience) in LEGACY_HOUSE {
        let device = index.get(target).unwrap_or_else(|| panic!("{target} is not in the index"));
        assert_eq!(device.class, class, "{target}");
        assert_eq!(device.audience, audience, "{target}");
        assert_eq!(device.instance, instance, "{target}");
        assert_eq!(device.plugin, "mock", "{target}");
        assert_eq!(device.local_id, target);
        assert!(!device.pending, "{target}");
        assert_eq!(index.class_of(target), Some(class));
        assert_eq!(index.family_may_see(target), audience == Family, "{target}");
    }
    let lights: Vec<&str> = index.of_class(DeviceClass::Light).map(|device| device.target.as_str()).collect();
    assert_eq!(lights, ["bulb1", "bulb2", "bulb3", "bulb4"]);
}

#[tokio::test]
async fn devices_say_what_they_are_and_a_family_member_sees_only_theirs() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let devices = harness.get("/api/v1/devices", &cookie).await;
    devices.assert_ok();
    assert_eq!(devices.body["devices"].as_array().unwrap().len(), 20, "{}", devices.text);
    for (target, instance, class, audience) in LEGACY_HOUSE {
        let device = listed(&devices, target);
        assert_eq!(device["plugin"], json!("mock"), "{target}");
        assert_eq!(device["instance"], json!(instance), "{target}");
        assert_eq!(device["class"], json!(class), "{target}");
        assert_eq!(device["audience"], json!(audience), "{target}");
        assert!(device["capabilities"].is_array(), "{target}");
        assert_eq!(device.get("adapter"), None, "{target}: adapter became plugin");
    }
    let capabilities = |target| listed(&devices, target)["capabilities"].clone();
    assert_eq!(
        capabilities("bulb1"),
        json!([{ "cap": "power" }, { "cap": "brightness", "min": 10 }, { "cap": "color" }])
    );
    assert_eq!(capabilities("sub"), json!([]), "the subwoofer is read-only");
    assert_eq!(capabilities("feeder"), json!([{ "cap": "dispense", "grams": [10, 20, 30, 40, 50] }]));
    assert_eq!(capabilities("host"), json!([{ "cap": "node_power" }, { "cap": "history" }]));

    let (_family, family_cookie, _user) = harness.add_family(&cookie, &mut owner, "Bob").await;
    let seen = harness.get("/api/v1/devices", &family_cookie).await;
    seen.assert_ok();
    let theirs: BTreeSet<String> = LEGACY_HOUSE
        .iter()
        .filter(|(_, _, _, audience)| *audience == Family)
        .map(|(target, ..)| target.to_string())
        .collect();
    assert_eq!(targets(&seen), theirs);
    assert!(seen.body["devices"].as_array().unwrap().iter().all(|device| device["audience"] == json!("family")));
}

#[tokio::test]
async fn an_added_light_is_indexed_named_and_reached_through_its_own_instance() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    harness.app.store.with(|conn| store_light(conn, "hue-1", "hue-1", "hue-bridge", "Strip")).unwrap();
    assert!(harness.app.registry.index().get("hue-1").is_none(), "not until the store is read again");
    harness.app.sync_directory().unwrap();
    let index = harness.app.registry.index();
    let light = index.get("hue-1").unwrap();
    assert_eq!(
        (light.class, light.audience, light.plugin, light.name.as_str()),
        (DeviceClass::Light, Family, "mock", "Strip")
    );
    assert_eq!(light.instance, "hue");
    assert_eq!(
        light.capabilities,
        [
            swifty_plugin_api::Capability::Power,
            swifty_plugin_api::Capability::Brightness { min: 10 },
            swifty_plugin_api::Capability::Color
        ],
        "what the mock gives an adopted light"
    );

    harness.devices.add_state("hue-1", State::Light { on: false, bri: 50, hue: 0, color: true });
    let done = harness.execute(&cookie, "light.power", "hue-1", json!({ "on": true })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(harness.devices.commands(), [("hue-1".to_string(), Op::LightPower { on: true })]);

    // Switching the bridge's instance off reaches its light and nothing else.
    harness.devices.set_disabled("hue", "The bridge is off for the test.");
    harness
        .execute(&cookie, "light.power", "hue-1", json!({ "on": false }))
        .await
        .assert_refused(StatusCode::CONFLICT, "device_unavailable");
    let bulb = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    assert_eq!(bulb.phase(), "confirmed", "{}", bulb.text);
    let devices = harness.get("/api/v1/devices", &cookie).await;
    assert_eq!(listed(&devices, "hue-1")["enabled"], json!(false));
    assert_eq!(listed(&devices, "hue-1")["reason"], json!("The bridge is off for the test."));

    let renamed = harness
        .send(
            Method::PUT,
            "/api/v1/devices/hue-1/name",
            Some(&cookie),
            Some(ORIGIN),
            Some(json!({ "name": "Desk strip" })),
        )
        .await;
    renamed.assert_ok();
    assert_eq!(harness.app.registry.index().get("hue-1").unwrap().name, "Desk strip");
}

#[tokio::test]
async fn a_scanned_machine_waits_in_the_index_until_it_is_approved() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    harness.devices.add_seen_machine("primary", 104, "builder");
    let scan = harness.post("/api/v1/machines/scan", &cookie, json!({})).await;
    scan.assert_ok();
    assert_eq!(scan.body["added"], json!(["vm104"]));

    let index = harness.app.registry.index();
    let pending = index.get("vm104").unwrap();
    assert!(pending.pending);
    assert_eq!((pending.class, pending.audience), (DeviceClass::Vm, Owner));
    assert!(index.active().all(|device| device.target != "vm104"));
    assert!(!targets(&harness.get("/api/v1/devices", &cookie).await).contains("vm104"));

    let approved = harness.critical(&cookie, &mut auth, "machine.approve", "vm104", json!({})).await;
    assert_eq!(approved.phase(), "confirmed", "{}", approved.text);
    let machine = harness.app.registry.index().get("vm104").unwrap().clone();
    assert!(!machine.pending);
    assert_eq!(machine.name, "builder · VM 104");
    let listed = listed(&harness.get("/api/v1/devices", &cookie).await, "vm104");
    assert_eq!((listed["plugin"].clone(), listed["instance"].clone()), (json!("mock"), json!("proxmox")));
}

#[tokio::test]
async fn configured_sockets_and_air_conditioners_are_indexed_from_the_start() {
    // The sockets and the air conditioner are named in their instances' configs,
    // and nothing else stores them: the hub does so when it starts.
    let socket = "plug-bf0123456789abcdef";
    let rows = |rows: Value| json!({ "devices": rows });
    let harness = Harness::with_house(
        Vec::new(),
        |house| {
            for instance in &mut house.instances {
                let aircon = "ac-0a1b2c3d-1111-2222-3333-444455556666";
                match instance.id.as_str() {
                    "tuya" => {
                        instance.config = rows(json!([
                            { "id": socket, "class": "plug", "name": "Heater", "target": socket },
                            { "id": format!("{socket}-usb"), "class": "plug", "name": "Heater USB", "target": format!("{socket}-usb") },
                        ]))
                    }
                    "toshiba" => {
                        instance.config = rows(json!([
                            { "id": aircon, "class": "aircon", "name": "Air conditioner", "target": aircon }
                        ]))
                    }
                    _ => {}
                }
            }
        },
        |_| {},
    );
    let (_auth, cookie) = harness.sign_in().await;
    let index = harness.app.registry.index();
    let socket = index.get("plug-bf0123456789abcdef").unwrap();
    assert_eq!(
        (socket.class, socket.audience, socket.instance.as_str(), socket.name.as_str()),
        (DeviceClass::Plug, Owner, "tuya", "Heater")
    );
    assert_eq!(index.get("plug-bf0123456789abcdef-usb").unwrap().name, "Heater USB");
    let aircon = "ac-0a1b2c3d-1111-2222-3333-444455556666";
    assert_eq!(index.class_of(aircon), Some(DeviceClass::Aircon));
    assert_eq!(index.get(aircon).unwrap().instance, "toshiba");
    assert_eq!(index.len(), 23);

    let on = harness.execute(&cookie, "plug.power", "plug-bf0123456789abcdef", json!({ "on": true })).await;
    assert_eq!(on.phase(), "confirmed", "{}", on.text);
    let cool = harness.execute(&cookie, "ac.power", aircon, json!({ "on": true })).await;
    assert_eq!(cool.phase(), "confirmed", "{}", cool.text);
    let devices = harness.get("/api/v1/devices", &cookie).await;
    assert_eq!(listed(&devices, aircon)["class"], json!("aircon"));
    assert_eq!(listed(&devices, "plug-bf0123456789abcdef-usb")["capabilities"], json!([{ "cap": "power" }]));
}
