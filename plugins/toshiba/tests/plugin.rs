//! What the hub learns about this plugin without a network: the manifest, readiness,
//! the devices with today's targets, and refusals. None of it opens a socket.

mod common;

use common::{pure, readiness, refusal, PINNED_SUFFIX};
use serde_json::{json, Value};
use swifty_plugin_api::{CapKind, DeviceClass, Op, Plugin, Reach, Readiness};
use swifty_plugin_testkit::devices::{AC_IDS, AC_UNIQUE_IDS};
use swifty_plugin_testkit::trap::Trap;
use swifty_plugin_toshiba::{LOGIN_SLOT, MANIFEST, PLUGIN};

fn config_at(url: &str, names: [Option<&str>; 2]) -> Value {
    let units: Vec<Value> = AC_IDS
        .iter()
        .zip(AC_UNIQUE_IDS)
        .zip(names)
        .map(|((id, unique), name)| {
            let mut row = json!({ "id": id, "unique_id": unique });
            if let Some(name) = name {
                row["name"] = json!(name);
            }
            row
        })
        .collect();
    json!({ "api_url": url, "hub_url": url, "client_suffix": PINNED_SUFFIX, "units": units })
}

#[test]
fn the_manifest_says_this_is_an_unofficial_cloud_integration() {
    assert!(matches!(MANIFEST.reach, Reach::Cloud { vendor: "Toshiba", official: false }));
    assert!(MANIFEST.secrets.iter().any(|slot| slot.slot == LOGIN_SLOT && slot.required));
    assert!(MANIFEST.config.iter().any(|field| field.key == "client_suffix" && !field.required));
    assert!(MANIFEST.summary.contains("Unofficial"));
}

#[tokio::test]
async fn each_unit_keeps_its_target_and_gets_a_plain_name() {
    let trap = Trap::start().await;
    let config = config_at(&format!("http://{}", trap.addr), [Some("Upstairs"), None]);
    let devices = pure(&config, &[LOGIN_SLOT], |view| PLUGIN.devices(view));
    let targets: Vec<String> = AC_IDS.iter().map(|id| format!("ac-{id}")).collect();
    assert_eq!(devices.iter().map(|device| device.local_id.clone()).collect::<Vec<_>>(), targets);
    assert!(devices.iter().all(|device| device.target_hint.as_ref() == Some(&device.local_id)));
    assert_eq!(
        devices.iter().map(|device| device.name.as_str()).collect::<Vec<_>>(),
        ["Upstairs", "Air conditioner 2"]
    );
    for device in &devices {
        assert_eq!(device.class, DeviceClass::Aircon);
        let kinds: Vec<CapKind> = device.capabilities.iter().map(|cap| cap.kind()).collect();
        assert_eq!(kinds, [CapKind::Power, CapKind::Mode, CapKind::Temperature, CapKind::Fan]);
    }
    let one = json!({ "units": [{ "id": AC_IDS[0], "unique_id": "UNIT-ONE" }] });
    assert_eq!(pure(&one, &[], |view| PLUGIN.devices(view))[0].name, "Air conditioner");

    assert_eq!(readiness(&config, &[LOGIN_SLOT]), Readiness::Ready);
    assert!(matches!(readiness(&config, &[]), Readiness::NeedsSetup(_)), "no sign-in stored yet");
    assert!(matches!(readiness(&json!({ "units": [] }), &[LOGIN_SLOT]), Readiness::Disabled(_)));
    assert!(pure(&json!({}), &[], |view| PLUGIN.devices(view)).is_empty());

    assert!(PLUGIN.validate(&config).is_ok());
    assert!(refusal(&config, &targets[0], &Op::AirconTemp { celsius: 21 }).is_none());
    assert!(refusal(&json!({}), &targets[0], &Op::AirconTemp { celsius: 21 }).is_some(), "no config, no unit");
    assert_eq!(trap.traffic(), 0, "the pure calls reach nothing");
}
