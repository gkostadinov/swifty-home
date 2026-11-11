//! One configured instance of a plugin, as the registry holds it while the hub runs.
//!
//! An instance keeps what its plugin's pure calls look at (its config, which secret
//! slots hold a value, its adopted devices) and, in a live hub whose instance is
//! ready, the driver `Plugin::connect` built. Its [`InstanceDirectory`] is the list of
//! its active devices the driver checks on its own; the registry keeps it current
//! without building the driver again.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde_json::Value;
use swifty_plugin_api::{DeviceDecl, DeviceRecord, Directory, Driver, Health, InstanceView, Op, Plugin, Readiness};

/// How a stored instance is configured: a row of `plugin_instances`.
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceSpec {
    /// `[a-z][a-z0-9]{0,15}`.
    pub id: String,
    /// The id of the plugin it is an instance of.
    pub plugin: String,
    /// Its config, as the plugin's `validate` accepted it.
    pub config: Value,
    /// The plugin's config version that config was written for.
    pub config_version: u32,
    /// Whether it is loaded at all.
    pub enabled: bool,
    /// When its row was last written (`plugin_instances.updated_at`). Whoever changes
    /// an instance moves it forward, a secret replaced in the same slot included, so a
    /// hub that loads the instances again builds this one anew even when its config
    /// and its filled slots look the same.
    pub updated_at: i64,
}

/// Where a stored device came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Named by its instance's config.
    Config,
    /// Discovered and then adopted, or approved.
    Adopted,
}

/// A row of `devices`, with the name it is shown under.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredDevice {
    pub target: String,
    pub instance: String,
    pub local_id: String,
    pub class: swifty_plugin_api::DeviceClass,
    /// The name a person gave it, or the name it was stored with.
    pub name: String,
    pub attrs: Value,
    pub origin: Origin,
    /// Waiting for the owner's approval.
    pub pending: bool,
}

impl StoredDevice {
    /// The record a plugin gets back for it.
    pub fn record(&self) -> DeviceRecord {
        DeviceRecord {
            target: self.target.clone(),
            local_id: self.local_id.clone(),
            class: self.class,
            name: self.name.clone(),
            attrs: self.attrs.clone(),
        }
    }
}

/// The active devices of one instance, by local id. The driver reads it; the
/// registry replaces what is in it after every change in the store.
#[derive(Default)]
pub struct InstanceDirectory {
    records: RwLock<BTreeMap<String, DeviceRecord>>,
}

impl InstanceDirectory {
    pub fn replace(&self, records: Vec<DeviceRecord>) {
        let map = records.into_iter().map(|record| (record.local_id.clone(), record)).collect();
        *self.records.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = map;
    }
}

impl Directory for InstanceDirectory {
    fn get(&self, local_id: &str) -> Option<DeviceRecord> {
        self.records.read().unwrap_or_else(|poisoned| poisoned.into_inner()).get(local_id).cloned()
    }

    fn all(&self) -> Vec<DeviceRecord> {
        self.records.read().unwrap_or_else(|poisoned| poisoned.into_inner()).values().cloned().collect()
    }
}

/// How many panics in how long mark an instance failed.
pub const PANICS_ALLOWED: usize = 3;
pub const PANIC_WINDOW: Duration = Duration::from_secs(300);
/// What a panic inside a plugin is called, wherever it is reported.
pub const PLUGIN_FAILED: &str = "the plugin failed";
/// Why the devices of an instance that is switched off cannot be used.
pub const SWITCHED_OFF: &str = "This integration is switched off.";

/// Runs one of a plugin's pure calls. A panic in it is the plugin's failure, not
/// the hub's.
pub fn contained<T>(call: impl FnOnce() -> T) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(call)).map_err(|_| PLUGIN_FAILED.to_string())
}

/// One loaded instance.
pub struct Instance {
    pub spec: InstanceSpec,
    pub plugin: &'static dyn Plugin,
    /// The slots of the secret store that hold a value for it. Never the values.
    pub secrets_present: BTreeSet<String>,
    /// Its adopted, active devices: what `InstanceView::adopted` shows the plugin.
    pub adopted: Vec<DeviceRecord>,
    /// What its plugin declared for it at load.
    pub declared: Vec<DeviceDecl>,
    /// What its plugin would declare for its devices that wait for approval, once
    /// approved: what the index says they will be able to do.
    pub declared_pending: Vec<DeviceDecl>,
    pub directory: Arc<InstanceDirectory>,
    /// The live driver. `None` in dry-run, before setup, or when connecting failed.
    pub driver: Option<Arc<dyn Driver>>,
    /// Why there is no driver in a live hub, when connecting failed.
    connect_failure: Option<String>,
    /// When the plugin panicked lately.
    panics: Mutex<Vec<Instant>>,
    /// Set once it panicked too often. Cleared only by loading it again.
    failed: Mutex<Option<String>>,
    /// What the last health poll said.
    last_health: Mutex<Health>,
}

impl Instance {
    pub fn new(
        spec: InstanceSpec,
        plugin: &'static dyn Plugin,
        secrets_present: BTreeSet<String>,
        adopted: Vec<DeviceRecord>,
        pending: Vec<DeviceRecord>,
        directory: Arc<InstanceDirectory>,
    ) -> Self {
        let mut instance = Self {
            spec,
            plugin,
            secrets_present,
            adopted,
            declared: Vec::new(),
            declared_pending: Vec::new(),
            directory,
            driver: None,
            connect_failure: None,
            panics: Mutex::new(Vec::new()),
            failed: Mutex::new(None),
            last_health: Mutex::new(Health::unknown()),
        };
        match contained(|| instance.plugin.devices(&instance.view())) {
            Ok(declared) => instance.declared = declared,
            Err(reason) => *instance.failed.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(reason),
        }
        if !pending.is_empty() {
            let waiting: Vec<DeviceRecord> = instance.adopted.iter().cloned().chain(pending.iter().cloned()).collect();
            let view = InstanceView { adopted: &waiting, ..instance.view() };
            if let Ok(declared) = contained(|| instance.plugin.devices(&view)) {
                instance.declared_pending = declared
                    .into_iter()
                    .filter(|decl| pending.iter().any(|record| record.local_id == decl.local_id))
                    .collect();
            }
        }
        instance
    }

    /// What the pure calls may look at.
    pub fn view(&self) -> InstanceView<'_> {
        InstanceView {
            id: &self.spec.id,
            config: &self.spec.config,
            secrets_present: &self.secrets_present,
            adopted: &self.adopted,
        }
    }

    /// Whether it can work now, from the plugin's own pure call. Asked every time,
    /// because a plugin may know of a change the store has not seen.
    pub fn readiness(&self) -> Readiness {
        if !self.spec.enabled {
            return Readiness::Disabled(SWITCHED_OFF.to_string());
        }
        contained(|| self.plugin.readiness(&self.view())).unwrap_or_else(Readiness::Disabled)
    }

    /// Why its devices cannot be used, if they cannot.
    pub fn unavailable(&self, live: bool) -> Option<String> {
        if let Some(reason) = self.failure() {
            return Some(reason);
        }
        match self.readiness() {
            Readiness::Ready => {}
            Readiness::NeedsSetup(reason) | Readiness::Disabled(reason) => return Some(reason),
        }
        if live && self.driver.is_none() {
            return Some(
                self.connect_failure.clone().unwrap_or_else(|| "The integration is not connected.".to_string()),
            );
        }
        None
    }

    /// The plugin's own refusal of `op` on one of its devices. Never a socket.
    pub fn refusal(&self, local_id: &str, op: &Op) -> Option<String> {
        contained(|| self.plugin.refusal(&self.view(), local_id, op)).unwrap_or_else(Some)
    }

    /// Takes over what the same instance had before it was loaded again with the
    /// same spec: its driver, why connecting failed, its panics and its health.
    pub fn carry_over(&mut self, old: &Instance) {
        self.driver = old.driver.clone();
        self.connect_failure = old.connect_failure.clone();
        if let Some(reason) = old.failure() {
            *self.failed.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(reason);
        }
        *self.panics.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
            old.panics.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        self.set_health(old.last_health());
    }

    pub fn set_driver(&mut self, driver: Result<Arc<dyn Driver>, String>) {
        match driver {
            Ok(driver) => self.driver = Some(driver),
            Err(reason) => self.connect_failure = Some(reason),
        }
    }

    /// Why connecting failed, if it did.
    pub fn connect_failure(&self) -> Option<&str> {
        self.connect_failure.as_deref()
    }

    /// Why it was marked failed, if it was.
    pub fn failure(&self) -> Option<String> {
        self.failed.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    /// Notes one panic. The third in five minutes marks the instance failed.
    pub fn record_panic(&self) {
        let now = Instant::now();
        let mut panics = self.panics.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        panics.retain(|at| now.duration_since(*at) < PANIC_WINDOW);
        panics.push(now);
        if panics.len() >= PANICS_ALLOWED {
            tracing::error!(instance = %self.spec.id, plugin = %self.spec.plugin, "the plugin keeps failing; its instance is stopped");
            *self.failed.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(PLUGIN_FAILED.to_string());
        }
    }

    pub fn set_health(&self, health: Health) {
        *self.last_health.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = health;
    }

    /// What the last health poll said.
    pub fn last_health(&self) -> Health {
        self.last_health.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }
}
