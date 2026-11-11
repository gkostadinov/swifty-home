//! The pure side of the plugin: readiness, devices and refusals, from config and
//! the names of the secret slots alone.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use swifty_plugin_api::{CapKind, Capability, DeviceClass, InstanceView, Op, Plugin, Readiness};
use swifty_plugin_webos::{CLIENT_KEY, MANIFEST, NOT_PAIRED, PLUGIN};

fn view<'a>(config: &'a Value, present: &'a BTreeSet<String>) -> InstanceView<'a> {
    InstanceView { id: "webos", config, secrets_present: present, adopted: &[] }
}

#[test]
fn the_tv_needs_setup_until_its_key_is_stored() {
    let config = json!({ "url": "ws://192.0.2.40:3000" });
    let none = BTreeSet::new();
    assert_eq!(PLUGIN.readiness(&view(&config, &none)), Readiness::NeedsSetup(NOT_PAIRED.to_string()));
    let paired = BTreeSet::from([CLIENT_KEY.to_string()]);
    assert_eq!(PLUGIN.readiness(&view(&config, &paired)), Readiness::Ready);
    let broken = json!({ "url": "wss://192.0.2.40:3001" });
    assert!(matches!(PLUGIN.readiness(&view(&broken, &paired)), Readiness::Disabled(_)));
    assert!(PLUGIN.devices(&view(&broken, &paired)).is_empty());
}

#[test]
fn the_tv_keeps_today_s_target_and_lists_its_inputs_and_keys() {
    let (config, present) = (json!({ "url": "ws://192.0.2.40:3000" }), BTreeSet::new());
    let devices = PLUGIN.devices(&view(&config, &present));
    assert_eq!(devices.len(), 1, "listed even before it is paired");
    let tv = &devices[0];
    assert_eq!((tv.local_id.as_str(), tv.target_hint.as_deref()), ("tv", Some("tv")));
    assert_eq!(tv.class, DeviceClass::Tv);
    let kinds: Vec<CapKind> = tv.capabilities.iter().map(|cap| cap.kind()).collect();
    assert_eq!(kinds, [CapKind::Power, CapKind::Inputs, CapKind::Keys]);
    let Capability::Inputs { inputs } = &tv.capabilities[1] else { panic!() };
    assert_eq!(inputs, &["HDMI 1", "HDMI 2", "HDMI 3", "HDMI 4", "Live TV"]);
    let Capability::Keys { keys } = &tv.capabilities[2] else { panic!() };
    assert_eq!(keys.len(), 15);
    assert!(tv.attrs.to_string().contains("192.0.2.40"));
}

#[test]
fn refusals_are_the_adapter_s_and_wake_on_lan_lifts_one() {
    let present = BTreeSet::new();
    let without = json!({ "url": "ws://192.0.2.40:3000" });
    let with = json!({ "url": "ws://192.0.2.40:3000", "wol_mac": "02:00:00:00:00:01" });
    let on = Op::TvPower { on: true };
    assert!(PLUGIN.refusal(&view(&without, &present), "tv", &on).unwrap().contains("wol_mac"));
    assert_eq!(PLUGIN.refusal(&view(&with, &present), "tv", &on), None);
    for op in [Op::TvPower { on: false }, Op::TvInput { input: "Live TV".into() }, Op::TvKey { key: "ok".into() }] {
        assert_eq!(PLUGIN.refusal(&view(&without, &present), "tv", &op), None, "{op:?}");
    }
    assert!(PLUGIN.refusal(&view(&without, &present), "tv", &Op::Volume { level: 3 }).is_some());
    assert!(PLUGIN.refusal(&view(&without, &present), "tv2", &Op::TvPower { on: false }).is_some());
}

#[test]
fn the_manifest_declares_the_key_slot_and_the_brand() {
    assert_eq!(MANIFEST.secrets.len(), 1);
    assert_eq!(MANIFEST.secrets[0].slot, CLIENT_KEY);
    assert!(MANIFEST.config.iter().all(|field| field.key != CLIENT_KEY));
    assert!(MANIFEST.nouns.contains(&("lg", DeviceClass::Tv)));
    assert!(MANIFEST.vocabulary.contains(&("lgtv", "lg tv")));
}
