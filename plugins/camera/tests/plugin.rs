//! The pure calls: validate, readiness, devices and refusal. None of them may
//! reach the network, so every config here points at a trap.

mod common;

use std::collections::BTreeSet;

use common::*;
use serde_json::json;
use swifty_plugin_api::{Audience, DeviceClass, NetUse, Op, Plugin, Readiness};
use swifty_plugin_camera::{DEFAULT_NAME, DEVICE, MANIFEST, PLUGIN};
use swifty_plugin_testkit::trap::Trap;

#[tokio::test]
async fn readiness_follows_the_config_and_the_login() {
    let trap = Trap::start().await;
    let config = camera_config(trap.addr.port());
    let none = BTreeSet::new();
    assert!(
        matches!(PLUGIN.readiness(&view(&config, &none)), Readiness::NeedsSetup(reason) if reason.contains("login"))
    );
    assert_eq!(PLUGIN.readiness(&view(&config, &with_login())), Readiness::Ready);
    let broken = json!({ "host": "rtsp://192.0.2.30" });
    assert!(
        matches!(PLUGIN.readiness(&view(&broken, &with_login())), Readiness::Disabled(reason) if reason.contains("host"))
    );
    assert_eq!(trap.traffic(), 0);
}

#[tokio::test]
async fn the_camera_is_one_family_device_that_keeps_its_target() {
    let trap = Trap::start().await;
    let config = camera_config(trap.addr.port());
    let present = with_login();
    let devices = PLUGIN.devices(&view(&config, &present));
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].local_id, DEVICE);
    assert_eq!(devices[0].target_hint.as_deref(), Some("camera"));
    assert_eq!(devices[0].class, DeviceClass::Camera);
    assert_eq!(devices[0].class.default_audience(), Audience::Family);
    assert_eq!(devices[0].name, DEFAULT_NAME);
    assert!(devices[0].capabilities.is_empty());
    for op in [Op::Play, Op::TvPower { on: false }, Op::PlugPower { on: true }] {
        assert!(PLUGIN.refusal(&view(&config, &present), DEVICE, &op).is_some(), "{op:?}");
    }
    assert!(PLUGIN.validate(&config).is_ok());
    assert_eq!(trap.traffic(), 0);
}

#[test]
fn the_manifest_describes_a_local_read_only_plugin() {
    assert_eq!(MANIFEST.id, "camera");
    assert_eq!(MANIFEST.classes, [DeviceClass::Camera]);
    assert!(matches!(MANIFEST.network, [NetUse::Tcp { port: 554 }]));
    assert!(MANIFEST.actions.is_empty());
    assert_eq!(MANIFEST.secrets.iter().map(|slot| slot.slot).collect::<Vec<_>>(), ["login"]);
    assert_eq!(MANIFEST.config.iter().map(|field| field.key).collect::<Vec<_>>(), ["host", "port"]);
    assert!(serde_json::to_value(&MANIFEST).is_ok());
    assert!(PLUGIN.setup().is_some());
}
