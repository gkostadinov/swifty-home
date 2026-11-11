//! What the tests of this plugin share.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use swifty_plugin_api::net::{LiveGrant, Timeouts};
use swifty_plugin_api::{
    DeviceRecord, Directory, InstanceView, LiveContext, Scalar, Secrets, SetupContext, SetupInput,
};

/// A port where nothing listens on loopback: port 1, which no test binds.
pub const NOTHING_LISTENS: u16 = 1;

pub fn grant() -> LiveGrant {
    LiveGrant::when_live(true).unwrap()
}

pub fn timeouts() -> Timeouts {
    Timeouts { connect: Duration::from_millis(300), read: Duration::from_millis(300) }
}

/// A camera on loopback at `port`.
pub fn camera_config(port: u16) -> Value {
    json!({ "host": "127.0.0.1", "port": port })
}

/// No adopted devices: the camera's one device comes from its config.
pub struct NoDevices;

impl Directory for NoDevices {
    fn get(&self, _local_id: &str) -> Option<DeviceRecord> {
        None
    }

    fn all(&self) -> Vec<DeviceRecord> {
        Vec::new()
    }
}

/// What a live hub hands `connect`.
pub fn live(config: Value, login: &str) -> LiveContext {
    LiveContext {
        instance_id: "camera".to_string(),
        config,
        secrets: Secrets::of([("login", login)]),
        grant: grant(),
        timeouts: timeouts(),
        directory: Arc::new(NoDevices),
    }
}

/// The login slot as present.
pub fn with_login() -> BTreeSet<String> {
    BTreeSet::from(["login".to_string()])
}

/// What the pure calls see.
pub fn view<'a>(config: &'a Value, secrets: &'a BTreeSet<String>) -> InstanceView<'a> {
    InstanceView { id: "camera", config, secrets_present: secrets, adopted: &[] }
}

/// What a setup flow sees.
pub fn setup_context(grant: Option<LiveGrant>, secrets_present: BTreeSet<String>) -> SetupContext {
    SetupContext { instance_id: "camera".to_string(), grant, timeouts: timeouts(), secrets_present }
}

/// The form's values.
pub fn values(pairs: &[(&str, Scalar)]) -> SetupInput {
    let values: BTreeMap<String, Scalar> = pairs.iter().map(|(key, value)| (key.to_string(), value.clone())).collect();
    SetupInput::Values { values }
}

pub fn text(value: &str) -> Scalar {
    Scalar::Text(value.to_string())
}
