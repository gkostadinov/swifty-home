//! What the tests of this plugin share: a grant, short timeouts, the instance config for
//! the test kit's mock cloud, and the driver.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use swifty_net::{LiveGrant, Timeouts};
use swifty_plugin_api::{
    DeviceRecord, Directory, Driver, InstanceView, LiveContext, Op, Plugin, PluginError, Readiness, Secrets,
};
use swifty_plugin_petkit::protocol::parse_credentials;
use swifty_plugin_petkit::{config, PetkitDriver, LOGIN_SLOT, PLUGIN};
use swifty_plugin_testkit::devices::{MockPetkit, PETKIT_ACCOUNT, PETKIT_PASSWORD_MD5};

pub fn grant() -> LiveGrant {
    LiveGrant::when_live(true).unwrap()
}

pub fn timeouts() -> Timeouts {
    Timeouts { connect: Duration::from_millis(300), read: Duration::from_millis(300) }
}

/// The hub keeps no adopted devices for this plugin: its two devices come from the config.
pub struct NoDevices;

impl Directory for NoDevices {
    fn get(&self, _local_id: &str) -> Option<DeviceRecord> {
        None
    }

    fn all(&self) -> Vec<DeviceRecord> {
        Vec::new()
    }
}

/// What the `login` slot holds for the mock's account.
pub fn login() -> String {
    format!("{PETKIT_ACCOUNT}:{PETKIT_PASSWORD_MD5}")
}

/// The mock lists its server under the region `FR`, two hours east of UTC.
pub fn petkit_config(mock: &MockPetkit, commands: bool) -> Value {
    json!({
        "region": "FR",
        "timezone": "Europe/Paris",
        "utc_offset_minutes": 120,
        "commands": commands,
        "passport_url": mock.http.url,
        "poll_secs": 60,
    })
}

/// The driver itself, with the short timeouts the hub's tests gave the adapter.
pub fn driver(config: &Value) -> PetkitDriver {
    let petkit = config::parse(config).unwrap();
    PetkitDriver::new(grant(), &petkit, parse_credentials(&login()).unwrap(), timeouts()).unwrap()
}

/// The driver as a live hub builds it, from a config and what the `login` slot holds.
pub fn connect(config: Value, login: Option<&str>) -> Result<Arc<dyn Driver>, PluginError> {
    let secrets = Secrets::of(login.map(|value| (LOGIN_SLOT, value.to_string())));
    PLUGIN.connect(LiveContext {
        instance_id: "petkit".to_string(),
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
    call(&InstanceView { id: "petkit", config, secrets_present: &present, adopted: &[] })
}

pub fn refusal(config: &Value, target: &str, op: &Op) -> Option<String> {
    pure(config, &[LOGIN_SLOT], |view| PLUGIN.refusal(view, target, op))
}

pub fn readiness(config: &Value, slots: &[&str]) -> Readiness {
    pure(config, slots, |view| PLUGIN.readiness(view))
}
