//! What the mock plugin's tests share: a directory, a live context and the
//! pure view of an instance.
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use swifty_plugin_api::net::{LiveGrant, Timeouts};
use swifty_plugin_api::{DeviceRecord, Directory, Driver, InstanceView, LiveContext, Plugin, Secrets};
use swifty_plugin_mock::House;

/// The adopted devices of one instance, as the hub would keep them.
#[derive(Default)]
pub struct Adopted(pub Mutex<Vec<DeviceRecord>>);

impl Adopted {
    pub fn of(records: Vec<DeviceRecord>) -> Arc<Self> {
        Arc::new(Self(Mutex::new(records)))
    }

    pub fn add(&self, record: DeviceRecord) {
        self.0.lock().unwrap().push(record);
    }
}

impl Directory for Adopted {
    fn get(&self, local_id: &str) -> Option<DeviceRecord> {
        self.0.lock().unwrap().iter().find(|record| record.local_id == local_id).cloned()
    }

    fn all(&self) -> Vec<DeviceRecord> {
        self.0.lock().unwrap().clone()
    }
}

pub fn timeouts() -> Timeouts {
    Timeouts { connect: Duration::from_millis(200), read: Duration::from_millis(200) }
}

/// A live context for one instance. Only tests build a grant like this.
pub fn context(instance: &str, config: &Value, secrets: Secrets, directory: Arc<dyn Directory>) -> LiveContext {
    LiveContext {
        instance_id: instance.to_string(),
        config: config.clone(),
        secrets,
        grant: LiveGrant::when_live(true).unwrap(),
        timeouts: timeouts(),
        directory,
    }
}

/// Connects one instance with no secrets and no adopted devices.
pub fn connect(plugin: &dyn Plugin, instance: &str, config: &Value) -> Arc<dyn Driver> {
    plugin.connect(context(instance, config, Secrets::default(), Adopted::of(Vec::new()))).unwrap()
}

/// Connects one instance of a house, with its adopted devices.
pub fn connect_house(plugin: &dyn Plugin, house: &House, instance: &str) -> Arc<dyn Driver> {
    let spec = house.instance(instance).unwrap();
    plugin.connect(context(instance, &spec.config, Secrets::default(), Adopted::of(house.adopted(instance)))).unwrap()
}

/// Calls `f` with the pure view of an instance.
pub fn with_view<T>(
    id: &str,
    config: &Value,
    secrets: &[&str],
    adopted: &[DeviceRecord],
    f: impl FnOnce(&InstanceView<'_>) -> T,
) -> T {
    let present: BTreeSet<String> = secrets.iter().map(|slot| slot.to_string()).collect();
    f(&InstanceView { id, config, secrets_present: &present, adopted })
}
