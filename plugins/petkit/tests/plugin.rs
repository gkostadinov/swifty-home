//! What the hub learns about this plugin without a network: the manifest, readiness,
//! the devices with today's targets, and refusals. None of it opens a socket.

mod common;

use common::{pure, readiness, refusal};
use serde_json::json;
use swifty_plugin_api::{Capability, DeviceClass, Op, Plugin, Reach, Readiness};
use swifty_plugin_petkit::{LOGIN_SLOT, MANIFEST, PLUGIN};
use swifty_plugin_testkit::trap::Trap;

#[test]
fn the_manifest_says_this_is_an_unofficial_cloud_integration() {
    assert!(matches!(MANIFEST.reach, Reach::Cloud { vendor: "Petkit", official: false }));
    assert!(MANIFEST.secrets.iter().any(|slot| slot.slot == LOGIN_SLOT && slot.required));
    let commands = MANIFEST.config.iter().find(|field| field.key == "commands").unwrap();
    assert_eq!(commands.default, Some("false"), "commands start off");
    assert!(MANIFEST.summary.contains("Unofficial"));
}

#[tokio::test]
async fn the_feeder_and_the_litter_box_keep_their_targets() {
    let trap = Trap::start().await;
    let config = json!({ "region": "DE", "passport_url": format!("http://{}", trap.addr) });
    let devices = pure(&config, &[LOGIN_SLOT], |view| PLUGIN.devices(view));
    let found: Vec<(&str, Option<&str>, DeviceClass)> =
        devices.iter().map(|device| (device.local_id.as_str(), device.target_hint.as_deref(), device.class)).collect();
    assert_eq!(
        found,
        [("feeder", Some("feeder"), DeviceClass::Feeder), ("litter", Some("litter"), DeviceClass::Litter)]
    );
    assert_eq!(devices[0].capabilities, [Capability::Dispense { grams: vec![10, 20, 30, 40, 50] }]);
    assert_eq!(devices[1].capabilities, [Capability::Clean]);

    assert_eq!(readiness(&config, &[LOGIN_SLOT]), Readiness::Ready);
    assert!(matches!(readiness(&config, &[]), Readiness::NeedsSetup(_)), "no login stored yet");
    assert!(pure(&json!({}), &[], |view| PLUGIN.devices(view)).is_empty());
    assert!(PLUGIN.validate(&config).is_ok());
    let refused = refusal(&config, "feeder", &Op::PetFeed { grams: 10 }).unwrap();
    assert!(refused.contains("switched off"), "commands start off: {refused}");
    assert_eq!(trap.traffic(), 0, "the pure calls reach nothing");
}
