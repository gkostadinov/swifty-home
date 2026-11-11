//! The plugin runtime: the registry of configured instances and the index of every
//! device by target.
//!
//! [`Registry`] is the only code that calls a plugin, and it is where dry-run is
//! enforced. It holds the one [`LiveGrant`] the hub ever makes, and only when the
//! configuration says `live = true`. In dry-run no instance is connected: each one
//! answers from its plugin's pure calls (readiness, devices, refusals), and every
//! call that would reach a device returns before anything is looked up
//! ([`simulated`]). A live instance gets a driver from `Plugin::connect`, built from
//! its config, its secrets (read with the grant), the hub's timeouts and its
//! [`InstanceDirectory`]. A failure there disables that one instance, with its reason.
//!
//! Callers name a device by its target. The registry finds the instance that carries
//! it through the [`DeviceIndex`] and asks the driver about the local id.
//!
//! Every driver call runs in a task of its own. A panic in a plugin becomes
//! "the plugin failed" for that call, and three in five minutes stop the instance
//! until it is loaded again. That limits shared fate; it is not isolation.
//!
//! Once the app hands it the event bus ([`Registry::publish_to`]), the registry says
//! what it notices there: `devices` after every rebuild of the index,
//! `plugin` for an instance whose readiness or health changed, and `state` for a read
//! that returned something different from the last read of that device. It polls
//! nothing to find out; it only compares what it was going to do anyway.

pub mod health;
pub mod index;
pub mod instance;
pub mod simulated;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use swifty_net::{ConsoleLink, LiveGrant, Timeouts};
use swifty_plugin_api::{
    CommandAck, ConsoleGrant, DeviceDecl, DeviceError, DeviceRecord, Directory, Driver, ExtraHost, Health,
    HealthStatus, HistoryRange, InstanceView, LiveContext, Op, Plugin, Reach, Secrets, SeenMachine, SetupContext,
    State,
};

use self::health::InstanceStatus;
pub use self::index::DeviceIndex;
pub use self::instance::{contained, Instance, InstanceDirectory, InstanceSpec, Origin, StoredDevice, PLUGIN_FAILED};
use crate::config::v2::NetworkConfig;
use crate::events::{Event, EventBus};
use crate::secrets::SecretStore;

impl From<&NetworkConfig> for Timeouts {
    fn from(config: &NetworkConfig) -> Self {
        Self {
            connect: Duration::from_millis(config.connect_timeout_ms),
            read: Duration::from_millis(config.read_timeout_ms),
        }
    }
}

/// The shortest waits an instance of a cloud plugin gets. A vendor's servers are
/// further away than a device on the network, and the hub's own defaults are made
/// for the latter.
pub const CLOUD_CONNECT: Duration = Duration::from_secs(5);
pub const CLOUD_READ: Duration = Duration::from_secs(10);
/// The longest a discovery listens.
const DISCOVERY_WINDOW: Duration = Duration::from_secs(3);

/// Whether a device can be used now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", content = "reason", rename_all = "snake_case")]
pub enum Availability {
    Enabled,
    Disabled(String),
}

/// What a dispatch produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dispatched {
    /// A real device took the command.
    Sent(CommandAck),
    /// Dry-run: no driver was called and no socket was opened.
    Simulated,
}

/// Every configured instance and every device, and the only way to a driver.
pub struct Registry {
    live: bool,
    grant: Option<LiveGrant>,
    plugins: Vec<&'static dyn Plugin>,
    secrets: Arc<dyn SecretStore>,
    timeouts: Timeouts,
    instances: RwLock<BTreeMap<String, Arc<Instance>>>,
    index: RwLock<Arc<DeviceIndex>>,
    /// Where what the registry notices is published, once the app has said.
    events: OnceLock<Arc<EventBus>>,
    /// What the last read of each device returned, to tell a change from a repeat.
    last_reads: Mutex<HashMap<String, State>>,
    /// Each instance as it was last announced, to tell when it changed.
    announced: Mutex<BTreeMap<String, InstanceStatus>>,
}

fn unknown_device(target: &str) -> String {
    format!("{target} is not a device here.")
}

/// The instance that carries a target, its driver and the target's local id there.
struct Carrier {
    instance: Arc<Instance>,
    driver: Arc<dyn Driver>,
    local_id: String,
}

/// Aborts a spawned driver call when whoever waits for it stops waiting, as
/// dropping the call itself would.
struct Call<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for Call<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Registry {
    /// `plugins` is the build's compiled-in list, or the mock in tests. Only
    /// `live = true` makes the grant, here and nowhere else in the hub.
    pub fn new(
        live: bool,
        plugins: Vec<&'static dyn Plugin>,
        secrets: Arc<dyn SecretStore>,
        timeouts: Timeouts,
    ) -> Self {
        let grant = LiveGrant::when_live(live);
        Self {
            live: grant.is_some(),
            grant,
            plugins,
            secrets,
            timeouts,
            instances: RwLock::new(BTreeMap::new()),
            index: RwLock::new(Arc::new(DeviceIndex::default())),
            events: OnceLock::new(),
            last_reads: Mutex::new(HashMap::new()),
            announced: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn is_live(&self) -> bool {
        self.live
    }

    /// The hub's one grant, for the app's own outside connections (the assistant).
    /// `None` in dry-run.
    pub fn grant(&self) -> Option<LiveGrant> {
        self.grant
    }

    /// From now on, publishes what the registry notices on this bus. Until then it
    /// only keeps track, so the first load and the first reads set what later ones
    /// are compared with. A second bus is ignored.
    pub fn publish_to(&self, events: Arc<EventBus>) {
        if self.events.set(events).is_err() {
            tracing::warn!("the registry already publishes to an event bus");
        }
    }

    fn publish(&self, event: Event) {
        if let Some(events) = self.events.get() {
            events.publish(event);
        }
    }

    /// Keeps what a read of `target` returned and publishes it when it differs from
    /// the last read. The first read after a start only sets what is compared with.
    fn remember_read(&self, target: &str, state: &State) {
        let mut reads = self.last_reads.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // Published under the lock, so two reads of one device are told in the order
        // they were kept.
        if reads.insert(target.to_string(), state.clone()).is_some_and(|last| last != *state) {
            self.publish(Event::State { target: target.to_string(), state: state.clone() });
        }
    }

    /// Publishes `plugin` for every instance whose readiness, availability or health
    /// is not what it was when last announced, and forgets the instances that are gone.
    fn announce_instances(&self) {
        // Looked at under the lock, so a load and a health poll at once cannot
        // announce an older status after a newer one.
        let mut announced = self.announced.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = health::statuses(self);
        for status in &now {
            if announced.get(&status.id) != Some(status) {
                self.publish(Event::Plugin {
                    instance: status.id.clone(),
                    readiness: status.readiness.clone(),
                    health: status.health.clone(),
                });
            }
        }
        *announced = now.into_iter().map(|status| (status.id.clone(), status)).collect();
    }

    /// Every plugin in this build.
    pub fn plugins(&self) -> impl Iterator<Item = &'static dyn Plugin> + '_ {
        self.plugins.iter().copied()
    }

    /// The plugin with this id, if this build has it.
    pub fn plugin(&self, id: &str) -> Option<&'static dyn Plugin> {
        self.plugins.iter().copied().find(|plugin| plugin.manifest().id == id)
    }

    /// The secret store the instances' credentials are in.
    pub fn secret_store(&self) -> &Arc<dyn SecretStore> {
        &self.secrets
    }

    /// Every device there is right now.
    pub fn index(&self) -> Arc<DeviceIndex> {
        self.index.read().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    /// Every loaded instance, by id.
    pub fn instances(&self) -> Vec<Arc<Instance>> {
        self.instances.read().unwrap_or_else(|poisoned| poisoned.into_inner()).values().cloned().collect()
    }

    pub fn instance(&self, id: &str) -> Option<Arc<Instance>> {
        self.instances.read().unwrap_or_else(|poisoned| poisoned.into_inner()).get(id).cloned()
    }

    fn slots(&self, instance: &str) -> BTreeSet<String> {
        self.secrets.slots(instance).unwrap_or_else(|error| {
            tracing::warn!(%instance, kind = ?error.kind(), "the secret store could not list an instance's slots");
            BTreeSet::new()
        })
    }

    /// What an instance's plugin declares for it now, to store the devices its config
    /// names. `None` when this build has no such plugin or its config does not
    /// validate: then what is stored is left as it is.
    pub fn declared(&self, spec: &InstanceSpec, adopted: &[DeviceRecord]) -> Option<Vec<DeviceDecl>> {
        let plugin = self.plugin(&spec.plugin)?;
        contained(|| plugin.validate(&spec.config)).ok()?.ok()?;
        let secrets_present = self.slots(&spec.id);
        let view = InstanceView { id: &spec.id, config: &spec.config, secrets_present: &secrets_present, adopted };
        contained(|| plugin.devices(&view)).ok()
    }

    /// Loads these instances and their stored devices, and rebuilds the index. An
    /// instance whose spec and filled slots are what they were keeps its driver; the
    /// others are built again. An instance of a plugin this build does not have is
    /// left out, with its devices.
    pub fn load(&self, specs: Vec<InstanceSpec>, devices: Vec<StoredDevice>) {
        self.load_forcing(specs, devices, None);
    }

    /// As [`Registry::load`], building this one instance again in any case: after its
    /// secrets changed, for example.
    pub fn reload_instance(&self, id: &str, specs: Vec<InstanceSpec>, devices: Vec<StoredDevice>) {
        self.load_forcing(specs, devices, Some(id));
    }

    fn load_forcing(&self, specs: Vec<InstanceSpec>, devices: Vec<StoredDevice>, rebuild: Option<&str>) {
        let previous = self.instances.read().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        let mut next = BTreeMap::new();
        for spec in specs {
            let Some(plugin) = self.plugin(&spec.plugin) else {
                tracing::warn!(instance = %spec.id, plugin = %spec.plugin, "this build has no such plugin; the instance is not loaded");
                continue;
            };
            let secrets_present = self.slots(&spec.id);
            let mine = || devices.iter().filter(|device| device.instance == spec.id && !device.pending);
            let adopted: Vec<DeviceRecord> =
                mine().filter(|device| device.origin == Origin::Adopted).map(StoredDevice::record).collect();
            let pending: Vec<DeviceRecord> = devices
                .iter()
                .filter(|device| device.instance == spec.id && device.pending)
                .map(StoredDevice::record)
                .collect();
            let active: Vec<DeviceRecord> = mine().map(StoredDevice::record).collect();
            let kept = previous.get(&spec.id).filter(|old| {
                rebuild != Some(spec.id.as_str()) && old.spec == spec && old.secrets_present == secrets_present
            });
            let directory = kept.map(|old| old.directory.clone()).unwrap_or_default();
            directory.replace(active);
            let id = spec.id.clone();
            let mut instance = Instance::new(spec, plugin, secrets_present, adopted, pending, directory);
            match kept {
                Some(old) => instance.carry_over(old),
                None => self.connect(&mut instance),
            }
            next.insert(id, Arc::new(instance));
        }
        let list: Vec<Arc<Instance>> = next.values().cloned().collect();
        let index = index::build(&list, &devices);
        self.last_reads
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|target, _| index.get(target).is_some());
        *self.instances.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = next;
        *self.index.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::new(index);
        self.announce_instances();
        self.publish(Event::Devices);
    }

    /// The waits an instance of this plugin gets.
    pub fn timeouts_for(&self, plugin: &dyn Plugin) -> Timeouts {
        match plugin.manifest().reach {
            Reach::Cloud { .. } => {
                Timeouts { connect: self.timeouts.connect.max(CLOUD_CONNECT), read: self.timeouts.read.max(CLOUD_READ) }
            }
            Reach::Local => self.timeouts,
        }
    }

    /// Builds the driver of a ready instance, in a live hub only.
    fn connect(&self, instance: &mut Instance) {
        let Some(grant) = self.grant else {
            return;
        };
        if instance.failure().is_some() || instance.readiness() != swifty_plugin_api::Readiness::Ready {
            return;
        }
        let secrets = match self.read_secrets(grant, &instance.spec.id, &instance.secrets_present) {
            Ok(secrets) => secrets,
            Err(reason) => {
                instance.set_driver(Err(reason));
                return;
            }
        };
        let ctx = LiveContext {
            instance_id: instance.spec.id.clone(),
            config: instance.spec.config.clone(),
            secrets,
            grant,
            timeouts: self.timeouts_for(instance.plugin),
            directory: instance.directory.clone() as Arc<dyn Directory>,
        };
        let plugin = instance.plugin;
        let built = contained(|| plugin.connect(ctx)).and_then(|built| built.map_err(|error| error.0));
        if let Err(reason) = &built {
            tracing::warn!(instance = %instance.spec.id, plugin = %instance.spec.plugin, %reason, "an instance could not be connected");
        }
        instance.set_driver(built);
    }

    /// The values of the filled slots. Only a live hub can read one.
    fn read_secrets(&self, grant: LiveGrant, instance: &str, slots: &BTreeSet<String>) -> Result<Secrets, String> {
        let mut pairs = Vec::new();
        for slot in slots {
            match self.secrets.read(grant, instance, slot) {
                Ok(Some(secret)) => pairs.push((slot.clone(), secret.expose().to_string())),
                Ok(None) => {}
                Err(error) => {
                    return Err(format!("The secret {slot} could not be read ({:?}). Set it again.", error.kind()));
                }
            }
        }
        Ok(Secrets::of(pairs))
    }

    /// The instance that carries this target, and its local id there.
    fn locate(&self, target: &str) -> Result<(Arc<Instance>, String), DeviceError> {
        let index = self.index();
        let device = index.get(target).ok_or_else(|| DeviceError::Disabled(unknown_device(target)))?;
        let instance = self.instance(&device.instance).ok_or_else(|| DeviceError::Disabled(unknown_device(target)))?;
        Ok((instance, device.local_id.clone()))
    }

    /// The driver of the instance that carries this target, when it can be used.
    fn driver_for(&self, target: &str) -> Result<Carrier, DeviceError> {
        let (instance, local_id) = self.locate(target)?;
        if let Some(reason) = instance.unavailable(self.live) {
            return Err(DeviceError::Disabled(reason));
        }
        let driver = instance.driver.clone().ok_or_else(|| DeviceError::Disabled(unknown_device(target)))?;
        Ok(Carrier { instance, driver, local_id })
    }

    /// Runs one driver call in a task of its own, so that a panic in the plugin is
    /// that call's failure and nothing else's.
    async fn call<T, F>(instance: &Instance, work: F) -> Result<T, DeviceError>
    where
        T: Send + 'static,
        F: Future<Output = Result<T, DeviceError>> + Send + 'static,
    {
        let mut call = Call(tokio::spawn(work));
        match (&mut call.0).await {
            Ok(result) => result,
            Err(error) if error.is_panic() => {
                tracing::error!(instance = %instance.spec.id, plugin = %instance.spec.plugin, "a plugin call panicked");
                instance.record_panic();
                Err(DeviceError::Rejected(PLUGIN_FAILED.to_string()))
            }
            Err(_) => Err(DeviceError::Rejected(PLUGIN_FAILED.to_string())),
        }
    }

    pub fn availability(&self, target: &str) -> Availability {
        match self.locate(target) {
            Err(error) => Availability::Disabled(error.to_string()),
            Ok((instance, _)) => match instance.unavailable(self.live) {
                Some(reason) => Availability::Disabled(reason),
                None => Availability::Enabled,
            },
        }
    }

    /// Why this operation cannot be sent, if it cannot. This asks the plugin's own
    /// pure rules and never reaches a device, so it is the same in dry-run.
    pub fn refusal(&self, target: &str, op: &Op) -> Option<String> {
        match self.locate(target) {
            Err(error) => Some(error.to_string()),
            Ok((instance, local_id)) => instance.refusal(&local_id, op),
        }
    }

    /// Why a background poll must leave this device alone. Opens nothing.
    pub fn poll_refusal(&self, target: &str) -> Option<String> {
        if !self.live {
            return None;
        }
        let (instance, _) = self.locate(target).ok()?;
        let driver = instance.driver.clone()?;
        contained(|| driver.poll_refusal()).unwrap_or_else(Some)
    }

    /// Sends a command. In dry-run nothing is looked up or called.
    pub async fn command(&self, target: &str, op: &Op) -> Result<Dispatched, DeviceError> {
        if !self.live {
            return Ok(Dispatched::Simulated);
        }
        let Carrier { instance, driver, local_id } = self.driver_for(target)?;
        let op = op.clone();
        Self::call(&instance, async move { driver.command(&local_id, &op).await }).await.map(Dispatched::Sent)
    }

    /// Reads a device. In dry-run nothing is looked up or called and `None` is
    /// returned, because there is no real state to report. A state that differs
    /// from the last read of the device is published.
    pub async fn read_state(&self, target: &str) -> Result<Option<State>, DeviceError> {
        if !self.live {
            return Ok(None);
        }
        let Carrier { instance, driver, local_id } = self.driver_for(target)?;
        let state = Self::call(&instance, async move { driver.read_state(&local_id).await }).await?;
        self.remember_read(target, &state);
        Ok(Some(state))
    }

    /// Asks one instance what it can find. Nothing is adopted here. `None` in dry-run.
    pub async fn discover(&self, instance_id: &str) -> Result<Option<Vec<DeviceDecl>>, DeviceError> {
        if !self.live {
            return Ok(None);
        }
        let instance = self
            .instance(instance_id)
            .ok_or_else(|| DeviceError::Disabled(format!("There is no integration {instance_id} here.")))?;
        if let Some(reason) = instance.unavailable(true) {
            return Err(DeviceError::Disabled(reason));
        }
        let driver = instance.driver.clone().ok_or_else(DeviceError::unsupported)?;
        let window = self.timeouts.read.min(DISCOVERY_WINDOW);
        Self::call(&instance, async move { driver.discover(window).await }).await.map(Some)
    }

    /// How an instance is doing. In dry-run, what its readiness says.
    pub async fn health(&self, instance_id: &str) -> Health {
        let Some(instance) = self.instance(instance_id) else {
            return Health { status: HealthStatus::Unknown, detail: Some("There is no such integration.".to_string()) };
        };
        if let Some(reason) = instance.unavailable(self.live) {
            return Health { status: HealthStatus::Down, detail: Some(reason) };
        }
        let Some(driver) = instance.driver.clone() else {
            return Health::unknown();
        };
        Self::call(&instance, async move { Ok(driver.health().await) })
            .await
            .unwrap_or_else(|error| Health { status: HealthStatus::Down, detail: Some(error.to_string()) })
    }

    /// Asks every connected instance how it is, except those whose driver asks to
    /// be left alone for now, and keeps the answers for the plugin page. An instance
    /// that is not as it was last announced is announced again.
    pub async fn poll_health(&self) {
        for instance in self.instances() {
            let Some(driver) = instance.driver.clone() else {
                continue;
            };
            if contained(|| driver.poll_refusal()).unwrap_or(None).is_some() {
                continue;
            }
            let health = self.health(&instance.spec.id).await;
            instance.set_health(health);
        }
        self.announce_instances();
    }

    /// The machines every instance that hosts some reports, with the instance that
    /// reported them. `None` in dry-run, where nothing is asked.
    pub async fn list_machines(&self) -> Result<Option<Vec<(String, Vec<SeenMachine>)>>, DeviceError> {
        if !self.live {
            return Ok(None);
        }
        let mut found = Vec::new();
        let mut first_error = None;
        for instance in self.instances() {
            let Some(driver) = instance.driver.clone() else {
                continue;
            };
            if driver.machines().is_none() || instance.unavailable(true).is_some() {
                continue;
            }
            let listed = Self::call(&instance, async move {
                match driver.machines() {
                    Some(host) => host.list_machines().await,
                    None => Err(DeviceError::unsupported()),
                }
            })
            .await;
            match listed {
                Ok(machines) => found.push((instance.spec.id.clone(), machines)),
                Err(error) => {
                    tracing::warn!(instance = %instance.spec.id, %error, "could not list the machines");
                    first_error.get_or_insert(error);
                }
            }
        }
        match (found.is_empty(), first_error) {
            (true, Some(error)) => Err(error),
            _ => Ok(Some(found)),
        }
    }

    /// The stored history of a machine or host. `None` in dry-run.
    pub async fn history(&self, target: &str, range: HistoryRange) -> Result<Option<Vec<Value>>, DeviceError> {
        if !self.live {
            return Ok(None);
        }
        let Carrier { instance, driver, local_id } = self.driver_for(target)?;
        Self::call(&instance, async move {
            match driver.machines() {
                Some(host) => host.history(&local_id, range).await,
                None => Err(DeviceError::unsupported()),
            }
        })
        .await
        .map(Some)
    }

    /// Asks for a console on one VM: the instance that gave it, and the grant, whose
    /// target is that instance's local id. `None` in dry-run.
    pub async fn open_console(&self, target: &str) -> Result<Option<(String, ConsoleGrant)>, DeviceError> {
        if !self.live {
            return Ok(None);
        }
        let Carrier { instance, driver, local_id } = self.driver_for(target)?;
        let id = instance.spec.id.clone();
        let grant = Self::call(&instance, async move {
            match driver.machines() {
                Some(host) => host.open_console(&local_id).await,
                None => Err(DeviceError::unsupported()),
            }
        })
        .await?;
        Ok(Some((id, grant)))
    }

    /// Opens a granted console's socket, through the instance that granted it.
    /// `None` in dry-run.
    pub async fn connect_console(
        &self,
        instance_id: &str,
        grant: ConsoleGrant,
    ) -> Result<Option<ConsoleLink>, DeviceError> {
        if !self.live {
            return Ok(None);
        }
        let instance =
            self.instance(instance_id).ok_or_else(|| DeviceError::Disabled("The integration is gone.".to_string()))?;
        if let Some(reason) = instance.unavailable(true) {
            return Err(DeviceError::Disabled(reason));
        }
        let driver = instance.driver.clone().ok_or_else(DeviceError::unsupported)?;
        Self::call(&instance, async move {
            match driver.machines() {
                Some(host) => host.connect_console(&grant).await,
                None => Err(DeviceError::unsupported()),
            }
        })
        .await
        .map(Some)
    }

    /// Starts the agent pairing of the device that pairs agents. `None` in dry-run.
    pub async fn pair_agent(&self, target: &str, name: &str) -> Result<Option<Value>, DeviceError> {
        if !self.live {
            return Ok(None);
        }
        let Carrier { instance, driver, .. } = self.driver_for(target)?;
        let name = name.to_string();
        Self::call(&instance, async move {
            match driver.agents() {
                Some(agents) => agents.pair_agent(&name).await,
                None => Err(DeviceError::unsupported()),
            }
        })
        .await
        .map(Some)
    }

    /// What a setup flow of this instance is run with: the grant only when live, its
    /// plugin's timeouts, and which of its slots hold a value.
    pub fn setup_context(&self, instance: &Instance) -> SetupContext {
        SetupContext {
            instance_id: instance.spec.id.clone(),
            grant: self.grant,
            timeouts: self.timeouts_for(instance.plugin),
            secrets_present: self.slots(&instance.spec.id),
        }
    }

    /// The instance that carries the hub's first host: the one further hosts join.
    pub fn host_instance(&self) -> Option<Arc<Instance>> {
        let index = self.index();
        let host = index.of_class(swifty_plugin_api::DeviceClass::Host).next()?;
        self.instance(&host.instance)
    }

    /// Adds a further host to the instance that carries the first host. In dry-run
    /// no credential is read and nothing is added.
    pub fn add_host(&self, spec: &ExtraHost) -> Result<(), String> {
        if !self.live {
            return Ok(());
        }
        let instance = self.host_instance().ok_or_else(|| "There is no host here to add one beside.".to_string())?;
        if let Some(reason) = instance.unavailable(true) {
            return Err(reason);
        }
        let driver = instance.driver.clone().ok_or_else(|| "The host's integration is not connected.".to_string())?;
        let added = contained(|| match driver.machines() {
            Some(host) => host.add_host(spec),
            None => Err("This device cannot do that.".to_string()),
        });
        if added.is_err() {
            instance.record_panic();
        }
        added.and_then(|added| added)
    }

    /// Stops using a further host. In dry-run there is none to stop using.
    pub fn remove_host(&self, id: &str) {
        if !self.live {
            return;
        }
        let Some(driver) = self.host_instance().and_then(|instance| instance.driver.clone()) else {
            return;
        };
        if contained(|| {
            if let Some(host) = driver.machines() {
                host.remove_host(id);
            }
        })
        .is_err()
        {
            tracing::error!(host = %id, "the plugin failed while forgetting a host");
        }
    }
}

#[cfg(test)]
mod tests;
