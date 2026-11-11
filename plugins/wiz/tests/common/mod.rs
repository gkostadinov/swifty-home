//! What the WiZ tests share: a grant, short timeouts, and a driver built the way
//! the hub builds one.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use swifty_net::{LiveGrant, Timeouts};
use swifty_plugin_api::{DeviceRecord, Directory, Driver, InstanceView, LiveContext, Plugin, Secrets, SetupContext};
use swifty_plugin_wiz::PLUGIN;

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

pub fn driver(config: Value, adopted: Vec<DeviceRecord>) -> Arc<dyn Driver> {
    PLUGIN
        .connect(LiveContext {
            instance_id: "wiz".to_string(),
            config,
            secrets: Secrets::default(),
            grant: grant(),
            timeouts: timeouts(),
            directory: Arc::new(Adopted(adopted)),
        })
        .unwrap()
}

/// What `Plugin::refusal` says for this config and these adopted devices.
pub fn refusal(config: &Value, adopted: &[DeviceRecord], device: &str, op: &swifty_plugin_api::Op) -> Option<String> {
    let none = BTreeSet::new();
    PLUGIN.refusal(&InstanceView { id: "wiz", config, secrets_present: &none, adopted }, device, op)
}

pub fn setup_context(live: bool) -> SetupContext {
    SetupContext {
        instance_id: "wiz".to_string(),
        grant: LiveGrant::when_live(live),
        timeouts: timeouts(),
        secrets_present: BTreeSet::new(),
    }
}
