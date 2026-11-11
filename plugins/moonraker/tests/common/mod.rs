//! What the Moonraker plugin's tests share: an instance pointed at a mock, and
//! the hub's side of a live context.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use swifty_plugin_api::net::{LiveGrant, Timeouts};
use swifty_plugin_api::{DeviceRecord, Directory, Driver, InstanceView, LiveContext, Plugin, Secrets, SetupContext};
use swifty_plugin_moonraker::PLUGIN;

pub fn grant() -> LiveGrant {
    LiveGrant::when_live(true).unwrap()
}

pub fn timeouts() -> Timeouts {
    Timeouts { connect: Duration::from_millis(300), read: Duration::from_millis(300) }
}

/// No adopted devices: the printer comes from the config.
pub struct Nothing;

impl Directory for Nothing {
    fn get(&self, _local_id: &str) -> Option<DeviceRecord> {
        None
    }

    fn all(&self) -> Vec<DeviceRecord> {
        Vec::new()
    }
}

pub fn config(url: &str) -> Value {
    json!({ "url": url })
}

/// The plugin pointed at `url`, with these secrets and timeouts.
pub fn connect_with(url: &str, secrets: Secrets, timeouts: Timeouts) -> Arc<dyn Driver> {
    let ctx = LiveContext {
        instance_id: "moonraker".to_string(),
        config: config(url),
        secrets,
        grant: grant(),
        timeouts,
        directory: Arc::new(Nothing),
    };
    PLUGIN.connect(ctx).map_err(|e| e.to_string()).unwrap()
}

/// The plugin pointed at `url`, with no API key.
pub fn connect(url: &str) -> Arc<dyn Driver> {
    connect_with(url, Secrets::default(), timeouts())
}

/// Calls a pure function of the plugin as a dry-run hub would.
pub fn with_view<T>(config: &Value, call: impl FnOnce(&InstanceView<'_>) -> T) -> T {
    let present = BTreeSet::new();
    call(&InstanceView { id: "moonraker", config, secrets_present: &present, adopted: &[] })
}

/// A setup context, live or dry-run, with these slots already holding a value.
pub fn setup_context(live: bool, present: &[&str]) -> SetupContext {
    SetupContext {
        instance_id: "moonraker".to_string(),
        grant: LiveGrant::when_live(live),
        timeouts: timeouts(),
        secrets_present: present.iter().map(|slot| slot.to_string()).collect(),
    }
}
