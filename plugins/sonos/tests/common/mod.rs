//! What the tests share: a live instance built through the plugin's own API.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use swifty_plugin_api::net::{LiveGrant, Timeouts};
use swifty_plugin_api::{
    CommandAck, DeviceError, DeviceRecord, Directory, Driver, InstanceView, LiveContext, Op, Plugin, Secrets, State,
};
use swifty_plugin_sonos::PLUGIN;

pub fn grant() -> LiveGrant {
    LiveGrant::when_live(true).unwrap()
}

pub fn timeouts() -> Timeouts {
    Timeouts { connect: Duration::from_millis(300), read: Duration::from_millis(300) }
}

/// No adopted devices: every speaker comes from the config.
pub struct NoneAdopted;

impl Directory for NoneAdopted {
    fn get(&self, _local_id: &str) -> Option<DeviceRecord> {
        None
    }

    fn all(&self) -> Vec<DeviceRecord> {
        Vec::new()
    }
}

/// One live instance, called as the hub would: the driver, and the plugin's pure refusal.
pub struct Instance {
    pub config: Value,
    pub driver: Arc<dyn Driver>,
}

impl Instance {
    pub fn connect(config: Value) -> Self {
        let ctx = LiveContext {
            instance_id: "sonos".to_string(),
            config: config.clone(),
            secrets: Secrets::default(),
            grant: grant(),
            timeouts: timeouts(),
            directory: Arc::new(NoneAdopted),
        };
        let driver = PLUGIN.connect(ctx).unwrap();
        Self { config, driver }
    }

    pub fn refusal(&self, device: &str, op: &Op) -> Option<String> {
        let present = BTreeSet::new();
        let view = InstanceView { id: "sonos", config: &self.config, secrets_present: &present, adopted: &[] };
        PLUGIN.refusal(&view, device, op)
    }

    pub async fn command(&self, device: &str, op: &Op) -> Result<CommandAck, DeviceError> {
        self.driver.command(device, op).await
    }

    pub async fn read_state(&self, device: &str) -> Result<State, DeviceError> {
        self.driver.read_state(device).await
    }
}
