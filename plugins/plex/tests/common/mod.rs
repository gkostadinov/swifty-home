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

/// An address where nothing listens: port 1 on loopback, which no test binds.
pub const NOTHING_LISTENS: &str = "http://127.0.0.1:1";

pub fn grant() -> LiveGrant {
    LiveGrant::when_live(true).unwrap()
}

pub fn timeouts() -> Timeouts {
    Timeouts { connect: Duration::from_millis(300), read: Duration::from_millis(300) }
}

pub fn config(url: &str) -> Value {
    json!({ "url": url })
}

/// No adopted devices: the server's one device comes from its config.
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
pub fn live(config: Value, token: &str) -> LiveContext {
    LiveContext {
        instance_id: "plex".to_string(),
        config,
        secrets: Secrets::of([("token", token)]),
        grant: grant(),
        timeouts: timeouts(),
        directory: Arc::new(NoDevices),
    }
}

/// The token slot as present.
pub fn with_token() -> BTreeSet<String> {
    BTreeSet::from(["token".to_string()])
}

/// What the pure calls see.
pub fn view<'a>(config: &'a Value, secrets: &'a BTreeSet<String>) -> InstanceView<'a> {
    InstanceView { id: "plex", config, secrets_present: secrets, adopted: &[] }
}

/// What a setup flow sees.
pub fn setup_context(grant: Option<LiveGrant>, secrets_present: BTreeSet<String>) -> SetupContext {
    SetupContext { instance_id: "plex".to_string(), grant, timeouts: timeouts(), secrets_present }
}

/// The form's values, all as text.
pub fn values(pairs: &[(&str, &str)]) -> SetupInput {
    let values: BTreeMap<String, Scalar> =
        pairs.iter().map(|(key, value)| (key.to_string(), Scalar::Text(value.to_string()))).collect();
    SetupInput::Values { values }
}
