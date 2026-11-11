//! The pure calls: validate, readiness, devices and refusal. None of them may
//! reach the network, so every config here points at a trap.

mod common;

use std::collections::BTreeSet;

use common::*;
use serde_json::json;
use swifty_plugin_api::{Capability, DeviceClass, NetUse, Op, Plugin, Readiness};
use swifty_plugin_glimdock::{DEFAULT_NAME, DEVICE, MANIFEST, PLUGIN};
use swifty_plugin_testkit::trap::Trap;

#[tokio::test]
async fn readiness_follows_the_config_and_the_token() {
    let trap = Trap::start().await;
    let config = pairing_config(&format!("http://{}", trap.addr), "/api/snapshot");
    let none = BTreeSet::new();
    assert!(
        matches!(PLUGIN.readiness(&view(&config, &none)), Readiness::NeedsSetup(reason) if reason.contains("token"))
    );
    assert_eq!(PLUGIN.readiness(&view(&config, &with_token())), Readiness::Ready);
    let broken = json!({ "url": "http://192.0.2.10/api", "snapshot_path": "/s" });
    assert!(
        matches!(PLUGIN.readiness(&view(&broken, &with_token())), Readiness::Disabled(reason) if reason.contains("url"))
    );
    assert_eq!(trap.traffic(), 0);
}

#[tokio::test]
async fn a_node_is_one_telemetry_device_that_keeps_its_target() {
    let trap = Trap::start().await;
    let config = config(&format!("http://{}", trap.addr), "/api/snapshot");
    let present = with_token();
    let devices = PLUGIN.devices(&view(&config, &present));
    assert_eq!(devices.len(), 1);
    let node = &devices[0];
    assert_eq!(node.local_id, DEVICE);
    assert_eq!(node.local_id, "collector", "the target an existing install uses");
    assert_eq!(node.target_hint.as_deref(), Some("collector"));
    assert_eq!(node.class, DeviceClass::Telemetry);
    assert_eq!(node.name, DEFAULT_NAME);
    assert_eq!(node.name, "Glimdock node");
    assert_eq!(node.capabilities, [Capability::Metrics, Capability::AgentPairing]);
    assert_eq!(node.attrs, json!({}), "a node's device holds nothing beyond its config");
    assert_eq!(trap.traffic(), 0);
}

#[tokio::test]
async fn every_operation_is_refused_and_nothing_is_contacted() {
    let trap = Trap::start().await;
    let config = pairing_config(&format!("http://{}", trap.addr), "/api/snapshot");
    let present = with_token();
    for op in [
        Op::Play,
        Op::Pause,
        Op::HostShutdown,
        Op::HostReboot,
        Op::VmStart,
        Op::WireguardRestart,
        Op::PlugPower { on: false },
        Op::Custom { action: "glimdock.restart".to_string(), args: Default::default() },
    ] {
        let reason = PLUGIN.refusal(&view(&config, &present), DEVICE, &op).expect("a node is read-only");
        assert!(reason.contains("read-only"), "{reason}");
    }
    assert!(PLUGIN.validate(&config).is_ok());
    assert_eq!(trap.traffic(), 0);
}

#[test]
fn the_manifest_describes_a_local_read_only_telemetry_plugin() {
    assert_eq!(MANIFEST.id, "glimdock");
    assert_eq!(MANIFEST.classes, [DeviceClass::Telemetry]);
    assert!(matches!(MANIFEST.reach, swifty_plugin_api::Reach::Local));
    assert!(MANIFEST.network.iter().all(|net| matches!(net, NetUse::Http { .. } | NetUse::Https { .. })));
    assert!(MANIFEST.actions.is_empty());
    assert!(MANIFEST.multi_instance, "one instance per node");
    let slots: Vec<_> = MANIFEST.secrets.iter().map(|slot| slot.slot).collect();
    assert_eq!(slots, ["token"]);
    let keys: Vec<_> = MANIFEST.config.iter().map(|field| field.key).collect();
    assert_eq!(keys, ["url", "snapshot_path", "pairing_path"]);
    let required: Vec<_> = MANIFEST.config.iter().filter(|field| field.required).map(|field| field.key).collect();
    assert_eq!(required, ["url", "snapshot_path"]);
    // The manifest serialises, so the app and `swifty plugin info` can show it.
    let shown = serde_json::to_value(&MANIFEST).unwrap();
    assert_eq!(shown["classes"], json!(["telemetry"]));
}

#[test]
fn the_plugin_offers_a_setup_flow() {
    assert!(PLUGIN.setup().is_some());
}
