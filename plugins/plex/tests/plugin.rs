//! The pure calls: validate, readiness, devices and refusal. None of them may
//! reach the network, so every config here points at a trap.

mod common;

use std::collections::BTreeSet;

use common::*;
use serde_json::json;
use swifty_plugin_api::{DeviceClass, Op, Plugin, Readiness};
use swifty_plugin_plex::{DEFAULT_NAME, DEVICE, MANIFEST, PLUGIN};
use swifty_plugin_testkit::trap::Trap;

#[tokio::test]
async fn readiness_follows_the_config_and_the_token() {
    let trap = Trap::start().await;
    let config = config(&format!("http://{}", trap.addr));
    let none = BTreeSet::new();
    assert!(
        matches!(PLUGIN.readiness(&view(&config, &none)), Readiness::NeedsSetup(reason) if reason.contains("token"))
    );
    assert_eq!(PLUGIN.readiness(&view(&config, &with_token())), Readiness::Ready);
    let broken = json!({ "url": "plex" });
    assert!(matches!(PLUGIN.readiness(&view(&broken, &with_token())), Readiness::Disabled(_)));
    assert_eq!(trap.traffic(), 0);
}

#[tokio::test]
async fn the_server_is_one_service_device_that_keeps_its_target() {
    let trap = Trap::start().await;
    let config = config(&format!("http://{}", trap.addr));
    let present = with_token();
    let devices = PLUGIN.devices(&view(&config, &present));
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].local_id, DEVICE);
    assert_eq!(devices[0].target_hint.as_deref(), Some("plex"));
    assert_eq!(devices[0].class, DeviceClass::Service);
    assert_eq!(devices[0].name, DEFAULT_NAME);
    assert!(devices[0].capabilities.is_empty(), "reading is all it offers");
    for op in [Op::Play, Op::Pause, Op::Volume { level: 1 }] {
        assert!(PLUGIN.refusal(&view(&config, &present), DEVICE, &op).is_some(), "{op:?}");
    }
    assert_eq!(trap.traffic(), 0);
}

#[test]
fn the_manifest_describes_a_local_read_only_plugin() {
    assert_eq!(MANIFEST.id, "plex");
    assert_eq!(MANIFEST.classes, [DeviceClass::Service]);
    assert!(matches!(MANIFEST.reach, swifty_plugin_api::Reach::Local));
    assert!(MANIFEST.actions.is_empty());
    assert_eq!(MANIFEST.secrets.iter().map(|slot| slot.slot).collect::<Vec<_>>(), ["token"]);
    assert_eq!(MANIFEST.config.iter().map(|field| field.key).collect::<Vec<_>>(), ["url"]);
    assert_eq!(MANIFEST.nouns, [("plex", DeviceClass::Service)]);
    assert!(serde_json::to_value(&MANIFEST).is_ok());
    assert!(PLUGIN.setup().is_some());
}
