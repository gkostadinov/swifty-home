//! What the Proxmox plugin's tests share: an instance pointed at a mock, the four
//! VMs a test house has adopted, and the hub's side of a live context.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use swifty_plugin_api::net::{LiveGrant, Timeouts};
use swifty_plugin_api::{
    DeviceClass, DeviceRecord, Directory, Driver, InstanceView, LiveContext, MachineHost, Plugin, PluginError, Secrets,
    SetupContext,
};
use swifty_plugin_proxmox::PLUGIN;

/// The node name in tests and examples.
pub const NODE: &str = "pve";

pub fn grant() -> LiveGrant {
    LiveGrant::when_live(true).unwrap()
}

pub fn timeouts() -> Timeouts {
    Timeouts { connect: Duration::from_millis(300), read: Duration::from_millis(300) }
}

/// The adopted devices, as the hub would keep them.
pub struct Adopted(pub Vec<DeviceRecord>);

impl Directory for Adopted {
    fn get(&self, local_id: &str) -> Option<DeviceRecord> {
        self.0.iter().find(|record| record.local_id == local_id).cloned()
    }

    fn all(&self) -> Vec<DeviceRecord> {
        self.0.clone()
    }
}

/// An adopted VM.
pub fn vm(local_id: &str) -> DeviceRecord {
    DeviceRecord {
        target: local_id.to_string(),
        local_id: local_id.to_string(),
        class: DeviceClass::Vm,
        name: local_id.to_string(),
        attrs: json!({}),
    }
}

/// The four VMs the mock starts with, all adopted.
pub fn four_vms() -> Vec<DeviceRecord> {
    ["vm100", "vm101", "vm102", "vm103"].into_iter().map(vm).collect()
}

/// An instance config for `url`, with `vm102` on the console list.
pub fn config(url: &str) -> Value {
    json!({ "url": url, "node": NODE, "public_ca": false, "console_targets": [{ "vm": "vm102" }] })
}

/// A live context for this config, these secrets and these adopted devices.
pub fn context(config: Value, secrets: Secrets, adopted: Vec<DeviceRecord>, timeouts: Timeouts) -> LiveContext {
    LiveContext {
        instance_id: "proxmox".to_string(),
        config,
        secrets,
        grant: grant(),
        timeouts,
        directory: Arc::new(Adopted(adopted)),
    }
}

pub fn try_connect(
    config: Value,
    secrets: Secrets,
    adopted: Vec<DeviceRecord>,
    timeouts: Timeouts,
) -> Result<Arc<dyn Driver>, PluginError> {
    PLUGIN.connect(context(config, secrets, adopted, timeouts))
}

pub fn connect(config: Value, secrets: Secrets, adopted: Vec<DeviceRecord>) -> Arc<dyn Driver> {
    try_connect(config, secrets, adopted, timeouts()).map_err(|e| e.to_string()).unwrap()
}

/// The machine side of a driver.
pub fn machines(driver: &Arc<dyn Driver>) -> &dyn MachineHost {
    driver.machines().expect("the Proxmox driver has the machine extension")
}

/// Calls a pure function of the plugin as a dry-run hub would.
pub fn with_view<T>(config: &Value, adopted: &[DeviceRecord], call: impl FnOnce(&InstanceView<'_>) -> T) -> T {
    let present: BTreeSet<String> = ["token".to_string()].into();
    call(&InstanceView { id: "proxmox", config, secrets_present: &present, adopted })
}

/// A setup context, live or dry-run, with these slots already holding a value.
pub fn setup_context(live: bool, present: &[&str]) -> SetupContext {
    SetupContext {
        instance_id: "proxmox".to_string(),
        grant: LiveGrant::when_live(live),
        timeouts: timeouts(),
        secrets_present: present.iter().map(|slot| slot.to_string()).collect(),
    }
}

/// Writes a file a further host's credential or CA comes from, readable by its owner alone.
pub fn write_file(dir: &Path, name: &str, value: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("{value}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    path
}
