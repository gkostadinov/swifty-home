//! The pure calls: what a dry-run hub sees of the plugin.

mod common;

use std::collections::BTreeSet;

use common::*;
use serde_json::{json, Value};
use swifty_plugin_api::{Capability, DeviceClass, InstanceView, Op, Plugin, Readiness};
use swifty_plugin_testkit::trap::Traps;
use swifty_plugin_tuya::{MANIFEST, PLUGIN};

const A: &str = "bf0123456789abcdef";
const B: &str = "bf9876543210fedcba";

fn config() -> Value {
    json!({ "plugs": [
        { "id": A, "address": "192.0.2.40" },
        { "id": B, "address": "192.0.2.41", "port": 7000, "name": "Desk lamp", "usb_dp": 7 },
    ] })
}

fn view<'a>(config: &'a Value, present: &'a BTreeSet<String>) -> InstanceView<'a> {
    InstanceView { id: "tuya", config, secrets_present: present, adopted: &[] }
}

fn slots(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|id| format!("local_key/{id}")).collect()
}

#[test]
fn every_socket_keeps_todays_target_and_default_name() {
    let config = config();
    let present = slots(&[A, B]);
    let devices = PLUGIN.devices(&view(&config, &present));
    let summary: Vec<(&str, Option<&str>, &str)> =
        devices.iter().map(|d| (d.local_id.as_str(), d.target_hint.as_deref(), d.name.as_str())).collect();
    assert_eq!(
        summary,
        [
            ("plug-bf0123456789abcdef", Some("plug-bf0123456789abcdef"), "Socket 1"),
            ("plug-bf9876543210fedcba", Some("plug-bf9876543210fedcba"), "Desk lamp"),
            ("plug-bf9876543210fedcba-usb", Some("plug-bf9876543210fedcba-usb"), "Desk lamp USB"),
        ]
    );
    for device in &devices {
        assert_eq!(device.class, DeviceClass::Plug);
        assert_eq!(device.capabilities, [Capability::Power]);
        assert!(device.attrs.to_string().find("key").is_none(), "no key near the attributes: {}", device.attrs);
    }
    assert_eq!(devices[2].attrs["data_point"], json!(7));
    assert!(PLUGIN.devices(&view(&json!({ "plugs": [] }), &present)).is_empty(), "an invalid config offers nothing");
}

#[test]
fn readiness_follows_the_config_and_the_filled_slots() {
    let config = config();
    assert_eq!(PLUGIN.readiness(&view(&config, &slots(&[A, B]))), Readiness::Ready);
    assert_eq!(PLUGIN.readiness(&view(&config, &slots(&[B]))), Readiness::Ready, "one socket without a key");
    assert!(matches!(PLUGIN.readiness(&view(&config, &slots(&[]))), Readiness::NeedsSetup(_)));
    let broken = json!({ "plugs": [{ "id": "SHORT", "address": "192.0.2.40" }] });
    match PLUGIN.readiness(&view(&broken, &slots(&[]))) {
        Readiness::Disabled(reason) => assert!(reason.contains("plugs[0].id"), "{reason}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn refusal_is_todays_plus_an_empty_slot() {
    let config = config();
    let present = vec![format!("local_key/{A}")];
    let on = Op::PlugPower { on: true };
    assert_eq!(refusal(&config, &present, "plug-bf0123456789abcdef", &on), None);
    assert_eq!(refusal(&config, &present, "plug-bf0123456789abcdef", &Op::PlugPower { on: false }), None);
    assert_eq!(
        refusal(&config, &present, "plug-bf9876543210fedcba", &on).as_deref(),
        Some("The local key for plug-bf9876543210fedcba is not set.")
    );
    assert_eq!(
        refusal(&config, &present, "plug-bf9876543210fedcba-usb", &on).as_deref(),
        Some("The local key for plug-bf9876543210fedcba is not set."),
        "the USB port shares the socket's key"
    );
    assert_eq!(
        refusal(&config, &present, "plug-bf0123456789abcdef-usb", &on).as_deref(),
        Some("plug-bf0123456789abcdef-usb is not a known socket."),
        "no USB data point, no USB switch"
    );
    assert_eq!(refusal(&config, &present, "bulb1", &on).as_deref(), Some("bulb1 is not a known socket."));
    for op in [Op::Play, Op::LightPower { on: true }, Op::TvPower { on: true }] {
        assert_eq!(
            refusal(&config, &present, "plug-bf0123456789abcdef", &op).as_deref(),
            Some("A socket cannot do that.")
        );
    }
}

/// With a trap on every address the config names, the pure calls reach none of them.
#[tokio::test]
async fn the_pure_calls_open_nothing() {
    let traps = Traps::start(2).await;
    let rows: Vec<Value> = traps
        .addrs()
        .iter()
        .enumerate()
        .map(|(n, addr)| json!({ "id": format!("bf00000000000000{n:02}"), "address": addr.ip().to_string(), "port": addr.port(), "usb_dp": 7 }))
        .collect();
    let config = json!({ "plugs": rows });
    let present = slots(&["bf0000000000000000", "bf0000000000000001"]);
    let view = view(&config, &present);
    PLUGIN.validate(&config).unwrap();
    assert_eq!(PLUGIN.readiness(&view), Readiness::Ready);
    for device in PLUGIN.devices(&view) {
        for op in [Op::PlugPower { on: true }, Op::PlugPower { on: false }, Op::Play] {
            let _ = PLUGIN.refusal(&view, &device.local_id, &op);
        }
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(traps.traffic(), 0);
}

#[test]
fn the_manifest_says_what_the_plugin_touches() {
    assert_eq!(MANIFEST.id, "tuya");
    assert_eq!(MANIFEST.classes, [DeviceClass::Plug]);
    let manifest = serde_json::to_value(&MANIFEST).unwrap();
    assert_eq!(manifest["reach"], json!({ "reach": "local" }));
    assert_eq!(manifest["network"][0], json!({ "use": "tcp", "port": 6668 }));
    let hosts: Vec<&str> = swifty_plugin_tuya::cloud::REGIONS.iter().map(|region| region.host).collect::<Vec<_>>();
    for host in hosts {
        assert!(
            manifest["network"].as_array().unwrap().contains(&json!({ "use": "cloud_host", "host": host })),
            "{host}"
        );
    }
    assert_eq!(manifest["secrets"][0]["slot"], json!("local_key/{id}"));
    assert!(PLUGIN.setup().is_some());
}
