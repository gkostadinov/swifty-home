//! The pure side of the plugin: readiness, devices and refusals, from config alone.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use swifty_plugin_api::{CapKind, DeviceClass, InstanceView, Op, Plugin, Readiness};
use swifty_plugin_sonos::{MANIFEST, PLUGIN};

fn config() -> Value {
    json!({ "speakers": [
        { "id": "sonos", "url": "http://192.0.2.30:1400" },
        { "id": "sub", "url": "http://192.0.2.31:1400", "read_only": true },
    ]})
}

fn view<'a>(config: &'a Value, present: &'a BTreeSet<String>) -> InstanceView<'a> {
    InstanceView { id: "sonos", config, secrets_present: present, adopted: &[] }
}

#[test]
fn today_s_targets_are_kept_and_the_subwoofer_offers_nothing() {
    let (config, present) = (config(), BTreeSet::new());
    let devices = PLUGIN.devices(&view(&config, &present));
    assert_eq!(devices.len(), 2);
    let (speaker, sub) = (&devices[0], &devices[1]);
    assert_eq!((speaker.local_id.as_str(), speaker.target_hint.as_deref()), ("sonos", Some("sonos")));
    assert_eq!((sub.local_id.as_str(), sub.target_hint.as_deref()), ("sub", Some("sub")));
    assert!(devices.iter().all(|device| device.class == DeviceClass::Speaker));
    let kinds: Vec<CapKind> = speaker.capabilities.iter().map(|cap| cap.kind()).collect();
    assert_eq!(kinds, [CapKind::Volume, CapKind::Mute, CapKind::NightMode, CapKind::Transport]);
    assert!(sub.capabilities.is_empty(), "no catalog action may admit the subwoofer");
    assert_eq!(sub.attrs["read_only"], json!(true));
}

#[test]
fn readiness_needs_only_a_valid_config() {
    let present = BTreeSet::new();
    assert_eq!(PLUGIN.readiness(&view(&config(), &present)), Readiness::Ready);
    let broken = json!({ "speakers": [] });
    assert!(matches!(PLUGIN.readiness(&view(&broken, &present)), Readiness::Disabled(_)));
    assert!(PLUGIN.devices(&view(&broken, &present)).is_empty());
    assert!(PLUGIN.refusal(&view(&broken, &present), "sonos", &Op::Play).is_some());
}

#[test]
fn refusals_are_the_adapter_s() {
    let (config, present) = (config(), BTreeSet::new());
    let refusal = |device: &str, op: &Op| PLUGIN.refusal(&view(&config, &present), device, op);
    for op in [
        Op::Volume { level: 0 },
        Op::Volume { level: 100 },
        Op::VolumeCap { max: 30 },
        Op::Mute { muted: true },
        Op::NightMode { on: false },
        Op::Play,
        Op::Pause,
    ] {
        assert_eq!(refusal("sonos", &op), None, "{op:?}");
        assert!(refusal("sub", &op).unwrap().contains("read-only"), "{op:?}");
    }
    assert!(refusal("sonos", &Op::LightPower { on: true }).is_some());
    assert!(refusal("tv", &Op::Play).is_some());
}

#[test]
fn the_manifest_names_the_brand_and_its_port() {
    assert_eq!(MANIFEST.id, "sonos");
    assert!(MANIFEST.nouns.contains(&("sonos", DeviceClass::Speaker)));
    assert!(MANIFEST.secrets.is_empty());
    let json = serde_json::to_value(&MANIFEST).unwrap();
    assert_eq!(json["network"], json!([{ "use": "http", "port": 1400 }]));
}
