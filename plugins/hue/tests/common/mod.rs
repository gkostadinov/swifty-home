//! What the Hue tests share: a grant, short timeouts, and a driver built the way
//! the hub builds one.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use swifty_net::{LiveGrant, Timeouts};
use swifty_plugin_api::{
    DeviceClass, DeviceRecord, Directory, Driver, InstanceView, LiveContext, Op, Plugin, PluginError, Secrets,
    SetupContext,
};
use swifty_plugin_hue::PLUGIN;

pub fn grant() -> LiveGrant {
    LiveGrant::when_live(true).unwrap()
}

pub fn timeouts() -> Timeouts {
    Timeouts { connect: Duration::from_millis(300), read: Duration::from_millis(300) }
}

/// The adopted devices, as the hub keeps them for a driver.
pub struct Adopted(pub Vec<DeviceRecord>);

impl Directory for Adopted {
    fn get(&self, local_id: &str) -> Option<DeviceRecord> {
        self.0.iter().find(|record| record.local_id == local_id).cloned()
    }

    fn all(&self) -> Vec<DeviceRecord> {
        self.0.clone()
    }
}

/// An adopted light that says nothing about colour, as a record from before
/// discovery noted it.
pub fn light(local_id: &str) -> DeviceRecord {
    DeviceRecord {
        target: local_id.to_string(),
        local_id: local_id.to_string(),
        class: DeviceClass::Light,
        name: "Light".to_string(),
        attrs: json!({}),
    }
}

pub fn config(url: &str) -> Value {
    json!({ "url": url })
}

/// `PLUGIN.connect` with this key in the `app_key` slot, or no key at all.
pub fn try_driver(url: &str, key: Option<&str>, adopted: Vec<DeviceRecord>) -> Result<Arc<dyn Driver>, PluginError> {
    PLUGIN.connect(LiveContext {
        instance_id: "hue".to_string(),
        config: config(url),
        secrets: Secrets::of(key.map(|key| ("app_key", key))),
        grant: grant(),
        timeouts: timeouts(),
        directory: Arc::new(Adopted(adopted)),
    })
}

pub fn driver(url: &str, key: &str, adopted: Vec<DeviceRecord>) -> Arc<dyn Driver> {
    try_driver(url, Some(key), adopted).unwrap()
}

/// What `Plugin::refusal` says, with the key present.
pub fn refusal(url: &str, adopted: &[DeviceRecord], device: &str, op: &Op) -> Option<String> {
    let present: BTreeSet<String> = ["app_key".to_string()].into();
    let config = config(url);
    PLUGIN.refusal(&InstanceView { id: "hue", config: &config, secrets_present: &present, adopted }, device, op)
}

pub fn setup_context(live: bool, paired: bool) -> SetupContext {
    SetupContext {
        instance_id: "hue".to_string(),
        grant: LiveGrant::when_live(live),
        timeouts: timeouts(),
        secrets_present: if paired { ["app_key".to_string()].into() } else { BTreeSet::new() },
    }
}
