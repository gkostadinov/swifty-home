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

/// Where the rig's node answers pairing requests.
pub const PAIRING_PATH: &str = "/api/agents/pair";

/// An address where nothing listens: port 1 on loopback, which no test binds.
pub const NOTHING_LISTENS: &str = "http://127.0.0.1:1";

pub fn grant() -> LiveGrant {
    LiveGrant::when_live(true).unwrap()
}

pub fn timeouts() -> Timeouts {
    Timeouts { connect: Duration::from_millis(300), read: Duration::from_millis(300) }
}

/// A node's config without a pairing path.
pub fn config(url: &str, snapshot_path: &str) -> Value {
    json!({ "url": url, "snapshot_path": snapshot_path })
}

/// A node's config with a pairing path.
pub fn pairing_config(url: &str, snapshot_path: &str) -> Value {
    json!({ "url": url, "snapshot_path": snapshot_path, "pairing_path": PAIRING_PATH })
}

/// No adopted devices: a node's one device comes from its config.
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
        instance_id: "glimdock".to_string(),
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
    InstanceView { id: "glimdock", config, secrets_present: secrets, adopted: &[] }
}

/// What a setup flow sees.
pub fn setup_context(grant: Option<LiveGrant>, secrets_present: BTreeSet<String>) -> SetupContext {
    SetupContext { instance_id: "glimdock".to_string(), grant, timeouts: timeouts(), secrets_present }
}

/// The form's values, all as text.
pub fn values(pairs: &[(&str, &str)]) -> SetupInput {
    let values: BTreeMap<String, Scalar> =
        pairs.iter().map(|(key, value)| (key.to_string(), Scalar::Text(value.to_string()))).collect();
    SetupInput::Values { values }
}
