//! What the tests of this plugin share: a grant, short timeouts, the instance config for
//! the test kit's mock cloud, and the driver built the way the hub builds it.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use swifty_net::{LiveGrant, Timeouts};
use swifty_plugin_api::{
    DeviceRecord, Directory, Driver, InstanceView, LiveContext, Op, Plugin, PluginError, Readiness, Secrets,
};
use swifty_plugin_testkit::devices::{MockToshiba, AC_IDS, AC_UNIQUE_IDS, TOSHIBA_EMAIL, TOSHIBA_PASSWORD};
use swifty_plugin_toshiba::protocol::encode_credentials;
use swifty_plugin_toshiba::{LOGIN_SLOT, PLUGIN};

/// The suffix the test kit's mock cloud knows this client by: an install's pinned value.
pub const PINNED_SUFFIX: &str = "6f6c64687562";

pub fn grant() -> LiveGrant {
    LiveGrant::when_live(true).unwrap()
}

pub fn timeouts() -> Timeouts {
    Timeouts { connect: Duration::from_millis(300), read: Duration::from_millis(300) }
}

/// The hub keeps no adopted devices for this plugin: its units come from the config.
pub struct NoDevices;

impl Directory for NoDevices {
    fn get(&self, _local_id: &str) -> Option<DeviceRecord> {
        None
    }

    fn all(&self) -> Vec<DeviceRecord> {
        Vec::new()
    }
}

/// The stored sign-in for the mock's account, as the setup writes it.
pub fn login() -> String {
    encode_credentials(TOSHIBA_EMAIL, TOSHIBA_PASSWORD)
}

/// Its URLs, the client id it knows, and both units.
pub fn config(cloud: &MockToshiba) -> Value {
    let units: Vec<Value> =
        AC_IDS.iter().zip(AC_UNIQUE_IDS).map(|(id, unique)| json!({ "id": id, "unique_id": unique })).collect();
    json!({
        "api_url": cloud.http.url,
        "hub_url": cloud.http.url,
        "timeout_ms": 500,
        "client_suffix": PINNED_SUFFIX,
        "units": units,
    })
}

/// The driver, built from a config and what the `login` slot holds, as a live hub does.
pub fn connect(config: Value, login: Option<&str>) -> Result<Arc<dyn Driver>, PluginError> {
    let secrets = Secrets::of(login.map(|value| (LOGIN_SLOT, value.to_string())));
    PLUGIN.connect(LiveContext {
        instance_id: "toshiba".to_string(),
        config,
        secrets,
        grant: grant(),
        timeouts: timeouts(),
        directory: Arc::new(NoDevices),
    })
}

/// What the pure calls see of an instance with this config and these filled slots.
pub fn pure<T>(config: &Value, slots: &[&str], call: impl FnOnce(&InstanceView<'_>) -> T) -> T {
    let present: BTreeSet<String> = slots.iter().map(|slot| slot.to_string()).collect();
    call(&InstanceView { id: "toshiba", config, secrets_present: &present, adopted: &[] })
}

pub fn refusal(config: &Value, target: &str, op: &Op) -> Option<String> {
    pure(config, &[LOGIN_SLOT], |view| PLUGIN.refusal(view, target, op))
}

pub fn readiness(config: &Value, slots: &[&str]) -> Readiness {
    pure(config, slots, |view| PLUGIN.readiness(view))
}
