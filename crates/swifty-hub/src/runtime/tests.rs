//! The registry on its own: routing by target, dry-run, readiness, connecting,
//! reloading, panics and timeouts.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use swifty_net::{LiveGrant, Timeouts};
use swifty_plugin_api::{
    CapKind, Capability, CommandAck, ConfigProblem, DeviceClass, DeviceDecl, DeviceError, Driver, Health, HealthStatus,
    InstanceView, LiveContext, Manifest, Op, Plugin, PluginError, Reach, Readiness, Secret as Credential, State,
    API_VERSION,
};
use swifty_plugin_mock::{MockDevices, MockPlugin};

use super::*;
use crate::secrets::{MemorySecretStore, SecretStore};

const TIMEOUTS: Timeouts = Timeouts { connect: Duration::from_millis(500), read: Duration::from_millis(700) };

fn spec(id: &str, plugin: &str, config: Value) -> InstanceSpec {
    InstanceSpec {
        id: id.to_string(),
        plugin: plugin.to_string(),
        config,
        config_version: 1,
        enabled: true,
        updated_at: 0,
    }
}

fn stored(target: &str, instance: &str, local_id: &str, class: DeviceClass) -> StoredDevice {
    StoredDevice {
        target: target.to_string(),
        instance: instance.to_string(),
        local_id: local_id.to_string(),
        class,
        name: target.to_string(),
        attrs: json!({}),
        origin: Origin::Config,
        pending: false,
    }
}

/// Two mock instances: `kitchen` with a lamp stored as `lamp` and `porch` with a
/// lamp of the same local id stored as `porch-lamp`.
fn mock_house(live: bool, world: &Arc<MockDevices>) -> Registry {
    let registry =
        Registry::new(live, vec![MockPlugin::bound(world)], Arc::new(MemorySecretStore::default()), TIMEOUTS);
    let lamp = json!({ "devices": [{ "id": "lamp", "class": "light" }] });
    registry.load(
        vec![spec("kitchen", "mock", lamp.clone()), spec("porch", "mock", lamp)],
        vec![
            stored("lamp", "kitchen", "lamp", DeviceClass::Light),
            stored("porch-lamp", "porch", "lamp", DeviceClass::Light),
        ],
    );
    registry
}

#[tokio::test]
async fn a_target_reaches_the_driver_of_its_instance_under_its_local_id() {
    let world = MockDevices::empty();
    let registry = mock_house(true, &world);
    assert!(registry.is_live());
    let index = registry.index();
    assert_eq!(index.len(), 2);
    assert_eq!(index.get("porch-lamp").unwrap().instance, "porch");
    assert!(index.has("lamp", CapKind::Brightness), "capabilities come from the plugin");
    assert_eq!(registry.availability("lamp"), Availability::Enabled);

    let sent = registry.command("porch-lamp", &Op::LightPower { on: true }).await;
    assert_eq!(sent, Ok(Dispatched::Sent(CommandAck)));
    assert_eq!(world.commands(), [("lamp".to_string(), Op::LightPower { on: true })]);
    assert!(matches!(registry.read_state("lamp").await, Ok(Some(State::Light { on: true, .. }))));
}

#[tokio::test]
async fn a_target_the_index_does_not_know_reaches_nothing() {
    let world = MockDevices::empty();
    let registry = mock_house(true, &world);
    let reason = "ghost is not a device here.".to_string();
    assert_eq!(registry.availability("ghost"), Availability::Disabled(reason.clone()));
    assert_eq!(registry.refusal("ghost", &Op::LightPower { on: true }), Some(reason.clone()));
    assert_eq!(registry.poll_refusal("ghost"), None);
    assert_eq!(registry.command("ghost", &Op::LightPower { on: true }).await, Err(DeviceError::Disabled(reason)));
    assert!(registry.read_state("ghost").await.is_err());
    assert_eq!(world.total_commands() + world.total_reads(), 0);
}

#[tokio::test]
async fn dry_run_connects_nothing_and_still_refuses_what_live_would() {
    let world = MockDevices::empty();
    let registry = mock_house(false, &world);
    assert!(!registry.is_live());
    assert!(registry.instances().iter().all(|instance| instance.driver.is_none()), "no instance was connected");
    assert_eq!(registry.command("lamp", &Op::LightPower { on: true }).await, Ok(Dispatched::Simulated));
    assert_eq!(registry.command("ghost", &Op::Play).await, Ok(Dispatched::Simulated), "nothing is even looked up");
    assert_eq!(registry.read_state("lamp").await, Ok(None));
    assert_eq!(registry.discover("kitchen").await, Ok(None));
    assert_eq!(registry.list_machines().await, Ok(None));
    assert_eq!(registry.availability("lamp"), Availability::Enabled);
    assert!(registry.refusal("lamp", &Op::LightBrightness { percent: 5 }).is_some(), "the plugin's floor");
    assert!(registry.refusal("lamp", &Op::Play).is_some());
    assert_eq!(registry.refusal("lamp", &Op::LightBrightness { percent: 50 }), None);
    assert_eq!(world.total_commands() + world.total_reads(), 0);
}

#[tokio::test]
async fn an_instance_that_is_not_ready_says_why_and_reaches_nothing() {
    let world = MockDevices::empty();
    let registry =
        Registry::new(true, vec![MockPlugin::bound(&world)], Arc::new(MemorySecretStore::default()), TIMEOUTS);
    let waiting = json!({ "devices": [{ "id": "lamp", "class": "light" }], "require_code": true });
    let off = json!({ "devices": [{ "id": "fan", "class": "plug" }], "disabled": "Switched off for the winter." });
    let mut later = spec("later", "mock", json!({ "devices": [{ "id": "tv", "class": "tv" }] }));
    later.enabled = false;
    registry.load(
        vec![spec("waiting", "mock", waiting), spec("off", "mock", off), later],
        vec![
            stored("lamp", "waiting", "lamp", DeviceClass::Light),
            stored("fan", "off", "fan", DeviceClass::Plug),
            stored("tv", "later", "tv", DeviceClass::Tv),
        ],
    );
    let Availability::Disabled(reason) = registry.availability("lamp") else { panic!("lamp is usable") };
    assert!(reason.contains("access code"), "{reason}");
    assert_eq!(registry.availability("fan"), Availability::Disabled("Switched off for the winter.".to_string()));
    assert_eq!(registry.availability("tv"), Availability::Disabled(instance::SWITCHED_OFF.to_string()));
    assert!(matches!(registry.command("fan", &Op::PlugPower { on: true }).await, Err(DeviceError::Disabled(_))));
    assert!(registry.instances().iter().all(|instance| instance.driver.is_none()), "nothing unready is connected");
    assert_eq!(world.total_commands(), 0);

    // Once the slot holds a value, loading again connects it.
    registry.secret_store().write("waiting", "access_code", &Credential::new("1234")).unwrap();
    let devices = vec![stored("lamp", "waiting", "lamp", DeviceClass::Light)];
    let waiting = json!({ "devices": [{ "id": "lamp", "class": "light" }], "require_code": true });
    registry.load(vec![spec("waiting", "mock", waiting)], devices);
    assert_eq!(registry.availability("lamp"), Availability::Enabled);
    assert!(registry.command("lamp", &Op::LightPower { on: true }).await.is_ok());
}

#[test]
fn an_instance_of_a_plugin_this_build_lacks_is_left_out() {
    let world = MockDevices::empty();
    let registry =
        Registry::new(false, vec![MockPlugin::bound(&world)], Arc::new(MemorySecretStore::default()), TIMEOUTS);
    registry
        .load(vec![spec("blinds", "acme", json!({}))], vec![stored("blind", "blinds", "blind", DeviceClass::Other)]);
    assert!(registry.instance("blinds").is_none());
    assert!(registry.index().is_empty());
    assert_eq!(registry.declared(&spec("blinds", "acme", json!({})), &[]), None);
}

#[test]
fn what_an_instance_declares_is_asked_only_of_a_valid_config() {
    let world = MockDevices::empty();
    let registry =
        Registry::new(false, vec![MockPlugin::bound(&world)], Arc::new(MemorySecretStore::default()), TIMEOUTS);
    let good = spec("kitchen", "mock", json!({ "devices": [{ "id": "lamp", "class": "light", "target": "lamp" }] }));
    let declared = registry.declared(&good, &[]).unwrap();
    assert_eq!(declared.len(), 1);
    assert_eq!(declared[0].target_hint.as_deref(), Some("lamp"));
    assert_eq!(registry.declared(&spec("kitchen", "mock", json!({ "devices": "lamp" })), &[]), None);
}

// ----- a plugin that counts what it is asked, and can be made to panic

static COUNTING: Manifest = Manifest {
    id: "counting",
    name: "Counting",
    version: "0",
    api: API_VERSION,
    summary: "Counts.",
    reach: Reach::Local,
    network: &[],
    classes: &[DeviceClass::Plug],
    config_version: 1,
    config: &[],
    secrets: &[],
    actions: &[],
    vocabulary: &[],
    nouns: &[],
    multi_instance: true,
};

static CLOUDY: Manifest =
    Manifest { id: "cloudy", reach: Reach::Cloud { vendor: "Somebody", official: false }, ..COUNTING };

#[derive(Default)]
struct Counts {
    connects: AtomicUsize,
    commands: AtomicUsize,
    health_checks: AtomicUsize,
    timeouts: Mutex<Vec<Timeouts>>,
    secrets: Mutex<Vec<Option<String>>>,
}

struct Counting {
    manifest: &'static Manifest,
    counts: Arc<Counts>,
}

impl Plugin for Counting {
    fn manifest(&self) -> &'static Manifest {
        self.manifest
    }
    fn validate(&self, _config: &Value) -> Result<(), ConfigProblem> {
        Ok(())
    }
    fn readiness(&self, _instance: &InstanceView<'_>) -> Readiness {
        Readiness::Ready
    }
    fn devices(&self, _instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
        vec![DeviceDecl {
            local_id: "socket".to_string(),
            target_hint: None,
            class: DeviceClass::Plug,
            name: "Socket".to_string(),
            capabilities: vec![Capability::Power],
            attrs: json!({}),
        }]
    }
    fn refusal(&self, _instance: &InstanceView<'_>, _device: &str, _op: &Op) -> Option<String> {
        None
    }
    fn connect(&self, ctx: LiveContext) -> Result<Arc<dyn Driver>, PluginError> {
        self.counts.connects.fetch_add(1, Ordering::SeqCst);
        self.counts.timeouts.lock().unwrap().push(ctx.timeouts);
        self.counts.secrets.lock().unwrap().push(ctx.secrets.get("key").map(|secret| secret.expose().to_string()));
        if ctx.config["refuse"] == json!(true) {
            return Err(PluginError("The bridge address is not known.".to_string()));
        }
        Ok(Arc::new(CountingDriver {
            counts: self.counts.clone(),
            panics: ctx.config["panics"] == json!(true),
            rests: ctx.config["rests"] == json!(true),
        }))
    }
}

struct CountingDriver {
    counts: Arc<Counts>,
    panics: bool,
    /// Asks background polls to leave it alone, as a cloud that rate-limits does.
    rests: bool,
}

#[async_trait]
impl Driver for CountingDriver {
    async fn command(&self, _device: &str, _op: &Op) -> Result<CommandAck, DeviceError> {
        self.counts.commands.fetch_add(1, Ordering::SeqCst);
        if self.panics {
            panic!("a plugin bug");
        }
        Ok(CommandAck)
    }
    async fn read_state(&self, _device: &str) -> Result<State, DeviceError> {
        Ok(State::Plug { on: true })
    }

    fn poll_refusal(&self) -> Option<String> {
        self.rests.then(|| "Resting until the cloud allows it again.".to_string())
    }

    async fn health(&self) -> Health {
        self.counts.health_checks.fetch_add(1, Ordering::SeqCst);
        Health { status: HealthStatus::Ok, detail: None }
    }
}

#[tokio::test]
async fn the_health_poll_leaves_a_resting_instance_alone() {
    let (plugin, counts) = counting(&COUNTING);
    let registry = Registry::new(true, vec![plugin], Arc::new(MemorySecretStore::default()), TIMEOUTS);
    registry.load(
        vec![spec("awake", "counting", json!({})), spec("resting", "counting", json!({ "rests": true }))],
        vec![socket("awake-socket", "awake"), socket("resting-socket", "resting")],
    );
    registry.poll_health().await;
    assert_eq!(counts.health_checks.load(Ordering::SeqCst), 1, "only the awake one was asked");
    assert_eq!(registry.instance("awake").unwrap().last_health().status, HealthStatus::Ok);
    assert_eq!(registry.instance("resting").unwrap().last_health().status, HealthStatus::Unknown);
    assert_eq!(registry.poll_refusal("resting-socket").as_deref(), Some("Resting until the cloud allows it again."));
    let statuses = health::statuses(&registry);
    assert_eq!(statuses.len(), 2);
    assert!(statuses.iter().all(|status| status.readiness == Readiness::Ready && status.unavailable.is_none()));
    // A person asking is not a poll.
    assert_eq!(registry.health("resting").await.status, HealthStatus::Ok);
}

fn counting(manifest: &'static Manifest) -> (&'static Counting, Arc<Counts>) {
    let counts = Arc::new(Counts::default());
    let plugin: &'static Counting = Box::leak(Box::new(Counting { manifest, counts: counts.clone() }));
    (plugin, counts)
}

fn socket(target: &str, instance: &str) -> StoredDevice {
    stored(target, instance, "socket", DeviceClass::Plug)
}

#[tokio::test]
async fn loading_again_keeps_a_driver_unless_its_instance_changed() {
    let (plugin, counts) = counting(&COUNTING);
    let registry = Registry::new(true, vec![plugin], Arc::new(MemorySecretStore::default()), TIMEOUTS);
    let devices = || vec![socket("desk", "office")];
    registry.load(vec![spec("office", "counting", json!({}))], devices());
    registry.load(vec![spec("office", "counting", json!({}))], devices());
    assert_eq!(counts.connects.load(Ordering::SeqCst), 1, "nothing changed, so nothing was built again");
    registry.load(vec![spec("office", "counting", json!({ "note": "moved" }))], devices());
    assert_eq!(counts.connects.load(Ordering::SeqCst), 2, "a changed config is connected again");
    registry.secret_store().write("office", "key", &Credential::new("new-key-value")).unwrap();
    registry.load(vec![spec("office", "counting", json!({ "note": "moved" }))], devices());
    assert_eq!(counts.connects.load(Ordering::SeqCst), 3, "a newly filled slot too");
    assert_eq!(counts.secrets.lock().unwrap().last().cloned().flatten().as_deref(), Some("new-key-value"));
    registry.reload_instance("office", vec![spec("office", "counting", json!({ "note": "moved" }))], devices());
    assert_eq!(counts.connects.load(Ordering::SeqCst), 4, "and one asked to be built again");
    registry.load(Vec::new(), Vec::new());
    assert!(registry.index().is_empty() && registry.instances().is_empty(), "a removed instance is gone");
}

#[tokio::test]
async fn an_instance_whose_row_moved_is_connected_anew_with_its_new_secret() {
    let (plugin, counts) = counting(&COUNTING);
    let registry = Registry::new(true, vec![plugin], Arc::new(MemorySecretStore::default()), TIMEOUTS);
    let devices = || vec![socket("desk", "office")];
    let at = |updated_at| InstanceSpec { updated_at, ..spec("office", "counting", json!({})) };
    registry.secret_store().write("office", "key", &Credential::new("first-key-value")).unwrap();
    registry.load(vec![at(1)], devices());
    assert_eq!(counts.connects.load(Ordering::SeqCst), 1);
    // Another process (the command line) replaces the value in the same slot: the
    // config and the filled slots look as they were.
    registry.secret_store().write("office", "key", &Credential::new("second-key-value")).unwrap();
    registry.load(vec![at(1)], devices());
    assert_eq!(counts.connects.load(Ordering::SeqCst), 1, "nothing the registry can see changed");
    // It moved the instance's row as well, and that is what the registry goes by.
    registry.load(vec![at(2)], devices());
    assert_eq!(counts.connects.load(Ordering::SeqCst), 2, "a row that moved is connected anew");
    assert_eq!(counts.secrets.lock().unwrap().last().cloned().flatten().as_deref(), Some("second-key-value"));
}

#[tokio::test]
async fn a_failed_connect_disables_only_that_instance() {
    let (plugin, counts) = counting(&COUNTING);
    let registry = Registry::new(true, vec![plugin], Arc::new(MemorySecretStore::default()), TIMEOUTS);
    registry.load(
        vec![spec("bad", "counting", json!({ "refuse": true })), spec("good", "counting", json!({}))],
        vec![socket("bad-socket", "bad"), socket("good-socket", "good")],
    );
    assert_eq!(
        registry.availability("bad-socket"),
        Availability::Disabled("The bridge address is not known.".to_string())
    );
    assert!(registry.command("bad-socket", &Op::PlugPower { on: true }).await.is_err());
    assert_eq!(registry.availability("good-socket"), Availability::Enabled);
    assert!(registry.command("good-socket", &Op::PlugPower { on: true }).await.is_ok());
    assert_eq!(counts.commands.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_panic_in_a_plugin_is_that_calls_failure_and_three_stop_the_instance() {
    let (plugin, counts) = counting(&COUNTING);
    let registry = Registry::new(true, vec![plugin], Arc::new(MemorySecretStore::default()), TIMEOUTS);
    registry.load(
        vec![spec("buggy", "counting", json!({ "panics": true })), spec("fine", "counting", json!({}))],
        vec![socket("buggy-socket", "buggy"), socket("fine-socket", "fine")],
    );
    for _ in 0..instance::PANICS_ALLOWED {
        assert_eq!(registry.availability("buggy-socket"), Availability::Enabled);
        let failed = registry.command("buggy-socket", &Op::PlugPower { on: true }).await;
        assert_eq!(failed, Err(DeviceError::Rejected(PLUGIN_FAILED.to_string())));
    }
    assert_eq!(registry.availability("buggy-socket"), Availability::Disabled(PLUGIN_FAILED.to_string()));
    let before = counts.commands.load(Ordering::SeqCst);
    assert!(matches!(
        registry.command("buggy-socket", &Op::PlugPower { on: true }).await,
        Err(DeviceError::Disabled(_))
    ));
    assert_eq!(counts.commands.load(Ordering::SeqCst), before, "a stopped instance is not asked again");
    // Loading the same instance again does not forget it.
    registry.load(
        vec![spec("buggy", "counting", json!({ "panics": true })), spec("fine", "counting", json!({}))],
        vec![socket("buggy-socket", "buggy"), socket("fine-socket", "fine")],
    );
    assert_eq!(registry.availability("buggy-socket"), Availability::Disabled(PLUGIN_FAILED.to_string()));
    assert!(registry.command("fine-socket", &Op::PlugPower { on: true }).await.is_ok(), "the others carry on");
}

#[tokio::test]
async fn a_cloud_instance_waits_at_least_five_and_ten_seconds() {
    let (local, local_counts) = counting(&COUNTING);
    let (cloud, cloud_counts) = counting(&CLOUDY);
    let registry = Registry::new(true, vec![local, cloud], Arc::new(MemorySecretStore::default()), TIMEOUTS);
    registry.load(
        vec![spec("near", "counting", json!({})), spec("far", "cloudy", json!({}))],
        vec![socket("near-socket", "near"), socket("far-socket", "far")],
    );
    let near = local_counts.timeouts.lock().unwrap()[0];
    let far = cloud_counts.timeouts.lock().unwrap()[0];
    assert_eq!((near.connect, near.read), (TIMEOUTS.connect, TIMEOUTS.read), "a device on the network: the hub's own");
    assert_eq!((far.connect, far.read), (CLOUD_CONNECT, CLOUD_READ));
    let slow = Timeouts { connect: Duration::from_secs(8), read: Duration::from_secs(20) };
    let registry = Registry::new(false, vec![cloud], Arc::new(MemorySecretStore::default()), slow);
    let longer = registry.timeouts_for(cloud);
    assert_eq!((longer.connect, longer.read), (slow.connect, slow.read), "longer settings are kept");
}

/// A secret store that counts its reads.
#[derive(Default)]
struct Watched {
    inner: MemorySecretStore,
    reads: AtomicUsize,
}

impl SecretStore for Watched {
    fn slots(&self, instance: &str) -> std::io::Result<BTreeSet<String>> {
        self.inner.slots(instance)
    }
    fn read(&self, grant: LiveGrant, instance: &str, slot: &str) -> std::io::Result<Option<Credential>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.read(grant, instance, slot)
    }
    fn write(&self, instance: &str, slot: &str, value: &Credential) -> std::io::Result<()> {
        self.inner.write(instance, slot, value)
    }
    fn remove(&self, instance: &str, slot: &str) -> std::io::Result<()> {
        self.inner.remove(instance, slot)
    }
    fn remove_instance(&self, instance: &str) -> std::io::Result<()> {
        self.inner.remove_instance(instance)
    }
}

#[test]
fn only_a_live_registry_reads_a_secret() {
    for live in [false, true] {
        let (plugin, counts) = counting(&COUNTING);
        let store = Arc::new(Watched::default());
        store.write("office", "key", &Credential::new("office-key-value")).unwrap();
        let registry = Registry::new(live, vec![plugin], store.clone(), TIMEOUTS);
        registry.load(vec![spec("office", "counting", json!({}))], vec![socket("desk", "office")]);
        assert_eq!(registry.instance("office").unwrap().secrets_present, BTreeSet::from(["key".to_string()]));
        let expected = usize::from(live);
        assert_eq!(store.reads.load(Ordering::SeqCst), expected, "live: {live}");
        assert_eq!(counts.connects.load(Ordering::SeqCst), expected, "live: {live}");
    }
}

// ----- what the registry publishes

/// Everything the bus holds, oldest first.
fn published(bus: &EventBus) -> Vec<Event> {
    bus.since(0).unwrap().iter().map(|published| published.event.clone()).collect()
}

fn lamps() -> (Vec<InstanceSpec>, Vec<StoredDevice>) {
    let lamp = json!({ "devices": [{ "id": "lamp", "class": "light" }] });
    (
        vec![spec("kitchen", "mock", lamp.clone()), spec("porch", "mock", lamp)],
        vec![
            stored("lamp", "kitchen", "lamp", DeviceClass::Light),
            stored("porch-lamp", "porch", "lamp", DeviceClass::Light),
        ],
    )
}

#[tokio::test]
async fn a_reading_is_published_only_when_it_differs_from_the_last() {
    let world = MockDevices::empty();
    let registry = mock_house(true, &world);
    // Before the bus: the first load and the first reading only set what is compared with.
    let first = registry.read_state("lamp").await.unwrap().unwrap();
    assert!(matches!(first, State::Light { on: false, .. }));
    let bus = Arc::new(EventBus::new());
    registry.publish_to(bus.clone());
    assert!(published(&bus).is_empty());

    registry.read_state("lamp").await.unwrap();
    assert!(published(&bus).is_empty(), "the same reading again is no news");
    registry.command("lamp", &Op::LightPower { on: true }).await.unwrap();
    let now = registry.read_state("lamp").await.unwrap().unwrap();
    assert_eq!(published(&bus), [Event::State { target: "lamp".to_string(), state: now }]);

    // A failed read is not a reading, and another device has its own last reading.
    world.set_disabled("kitchen", "Unplugged for the test.");
    assert!(registry.read_state("lamp").await.is_err());
    registry.read_state("porch-lamp").await.unwrap();
    assert_eq!(published(&bus).len(), 1);
}

#[tokio::test]
async fn dry_run_publishes_no_reading() {
    let world = MockDevices::empty();
    let registry = mock_house(false, &world);
    let bus = Arc::new(EventBus::new());
    registry.publish_to(bus.clone());
    for _ in 0..2 {
        assert_eq!(registry.read_state("lamp").await, Ok(None));
    }
    assert!(published(&bus).is_empty());
}

#[test]
fn a_load_publishes_the_index_and_each_instance_that_changed() {
    let world = MockDevices::empty();
    let registry = mock_house(true, &world);
    let bus = Arc::new(EventBus::new());
    registry.publish_to(bus.clone());

    let (specs, devices) = lamps();
    registry.load(specs.clone(), devices.clone());
    assert_eq!(published(&bus), [Event::Devices], "loaded as it was: only the index is news");

    world.set_disabled("porch", "Unplugged for the test.");
    registry.load(specs.clone(), devices.clone());
    let disabled = Readiness::Disabled("Unplugged for the test.".to_string());
    assert_eq!(
        published(&bus)[1..],
        [
            Event::Plugin { instance: "porch".to_string(), readiness: disabled, health: Health::unknown() },
            Event::Devices
        ]
    );

    // An instance that goes is forgotten, so when it is back it is announced again.
    registry.load(specs[..1].to_vec(), devices[..1].to_vec());
    registry.load(specs, devices);
    let last: Vec<&str> = published(&bus)[3..].iter().map(Event::kind).collect();
    assert_eq!(last, ["devices", "plugin", "devices"]);
}

#[tokio::test]
async fn a_health_poll_announces_an_instance_whose_health_changed() {
    let world = MockDevices::empty();
    let registry = mock_house(true, &world);
    let bus = Arc::new(EventBus::new());
    registry.publish_to(bus.clone());

    registry.poll_health().await;
    let ok = Health { status: HealthStatus::Ok, detail: None };
    let announced: Vec<(String, Health)> = published(&bus)
        .into_iter()
        .map(|event| match event {
            Event::Plugin { instance, health, .. } => (instance, health),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(announced, [("kitchen".to_string(), ok.clone()), ("porch".to_string(), ok)]);

    registry.poll_health().await;
    assert_eq!(published(&bus).len(), 2, "the same health again is no news");
}

#[test]
fn the_registry_holds_the_only_grant_and_only_when_live() {
    let world = MockDevices::empty();
    assert!(mock_house(true, &world).grant().is_some());
    assert!(mock_house(false, &world).grant().is_none());
}
