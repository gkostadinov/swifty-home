//! Stand-ins for the real devices, and what points the hub at them.
//!
//! The mock servers live in `swifty-plugin-testkit` and are re-exported here, so
//! a test reaches all of them through `common::devices::*`. What sets a hub up stays
//! here, because the test kit does not know the hub: the [`Site`] (the hub's
//! configuration, its plugin instances and their secrets), the [`Rig`] of every
//! device at once, which fills a site with an instance of every first-party plugin
//! pointed at the mocks, and the instance configs of single mocks.

use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde_json::{json, Value};
use swifty_hub::config::Config;
use swifty_hub::runtime::InstanceSpec;
pub use swifty_plugin_testkit::devices::*;
use tempfile::TempDir;

pub fn write_secret(dir: &Path, name: &str, value: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("{value}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    path
}

/// What a hub is set up with: `swifty.toml`, the plugin instances stored in its
/// database and what their secret slots hold. A test changes it before the hub
/// starts. It reads as the configuration, so `site.live = true` works.
pub struct Site {
    pub config: Config,
    pub instances: Vec<InstanceSpec>,
    /// Instance, slot and value.
    pub secrets: Vec<(String, String, String)>,
}

impl Deref for Site {
    type Target = Config;
    fn deref(&self) -> &Config {
        &self.config
    }
}

impl DerefMut for Site {
    fn deref_mut(&mut self) -> &mut Config {
        &mut self.config
    }
}

impl Site {
    pub fn new(config: Config) -> Self {
        Self { config, instances: Vec::new(), secrets: Vec::new() }
    }

    /// Adds an instance (or replaces the one with this id).
    pub fn add(&mut self, id: &str, plugin: &str, config: Value) {
        self.instances.retain(|instance| instance.id != id);
        let spec = InstanceSpec {
            id: id.to_string(),
            plugin: plugin.to_string(),
            config,
            config_version: 1,
            enabled: true,
            updated_at: 0,
        };
        self.instances.push(spec);
    }

    /// The config of an instance, to change.
    pub fn instance(&mut self, id: &str) -> &mut Value {
        let found = self.instances.iter_mut().find(|instance| instance.id == id);
        &mut found.unwrap_or_else(|| panic!("the site has no instance {id}")).config
    }

    pub fn has(&self, id: &str) -> bool {
        self.instances.iter().any(|instance| instance.id == id)
    }

    /// Takes an instance out, with its secrets.
    pub fn remove(&mut self, id: &str) {
        self.instances.retain(|instance| instance.id != id);
        self.secrets.retain(|(instance, _, _)| instance != id);
    }

    /// Fills a slot of an instance (or replaces what it holds).
    pub fn secret(&mut self, instance: &str, slot: &str, value: &str) {
        self.forget(instance, slot);
        self.secrets.push((instance.to_string(), slot.to_string(), value.to_string()));
    }

    /// Empties a slot of an instance.
    pub fn forget(&mut self, instance: &str, slot: &str) {
        self.secrets.retain(|(id, name, _)| !(id == instance && name == slot));
    }
}

/// What the router is, now that nothing here can change it: a mock device the
/// hub lists and cannot use.
pub const ROUTER_NOT_AVAILABLE: &str =
    "No router plugin is set up, so nothing here can change the router or WireGuard.";

/// The instance config of mock Tuya sockets.
pub fn tuya_config(plugs: &[&MockTuya], names: &[Option<&str>]) -> Value {
    let rows: Vec<Value> = plugs
        .iter()
        .enumerate()
        .map(|(n, plug)| {
            let mut row = json!({ "id": plug.id, "address": "127.0.0.1", "port": plug.port });
            if let Some(Some(name)) = names.get(n) {
                row["name"] = json!(name);
            }
            row
        })
        .collect();
    json!({ "plugs": rows })
}

/// The Toshiba sign-in a login slot holds: base64 of the JSON the cloud is sent.
pub fn toshiba_login() -> String {
    let pw = TOSHIBA_PASSWORD;
    BASE64.encode(json!({ "Username": TOSHIBA_EMAIL, "Password": pw }).to_string())
}

/// The instance config of the mock Toshiba cloud, with both units.
pub fn toshiba_config(cloud: &MockToshiba) -> Value {
    let units: Vec<Value> =
        AC_IDS.iter().zip(AC_UNIQUE_IDS).map(|(id, unique)| json!({ "id": id, "unique_id": unique })).collect();
    json!({
        "api_url": cloud.http.url,
        "hub_url": cloud.http.url,
        "timeout_ms": 500,
        // The test kit's cloud knows the device id this suffix makes.
        "client_suffix": "6f6c64687562",
        "units": units,
    })
}

pub struct Rig {
    pub bulbs: Vec<MockBulb>,
    /// Bulbs that only a discovery can find.
    pub unknown_bulbs: Vec<MockBulb>,
    pub broadcast: MockBroadcast,
    /// What the collector answers with.
    pub snapshot: Arc<Mutex<Value>>,
    pub sonos: MockSonos,
    pub sub: MockSonos,
    pub tv: MockTv,
    pub wol: MockWol,
    pub moonraker: MockMoonraker,
    pub proxmox: MockProxmox,
    pub hue: MockHttp,
    /// The lights on that bridge. It starts with none.
    pub hue_bridge: SharedBridge,
    pub plex: MockHttp,
    pub camera: MockRtsp,
    pub collector: MockHttp,
    pub plugs: Vec<MockTuya>,
    pub toshiba: MockToshiba,
    /// Where the Petkit instance would sign in. It never should: it has no login.
    pub petkit: MockHttp,
    pub dir: TempDir,
}

pub const NODE: &str = "pve";
pub const PAIRING_PATH: &str = "/api/agents/pair";
pub const DISCOVERED_MACS: [&str; 2] = ["a8bb50000001", "a8bb50000002"];
pub const PLUG_IDS: [&str; 2] = ["bf0123456789abcdef", "bf9876543210fedcba"];
/// The VMs that have a console, as version 1's default list had them.
pub const CONSOLE_VMS: [&str; 3] = ["vm101", "vm102", "vm103"];

impl Rig {
    pub async fn start() -> Self {
        let mut bulbs = Vec::new();
        for _ in 0..4 {
            bulbs.push(MockBulb::start().await);
        }
        let tv = MockTv::start().await;
        let wol = MockWol::start(tv.state.clone()).await;
        let (broadcast, unknown_bulbs) = MockBroadcast::start(&DISCOVERED_MACS).await;
        let snapshot = Arc::new(Mutex::new(json!({ "pkg_temp_c": 64, "pkg_watts": 118 })));
        let shared_snapshot = snapshot.clone();
        let bridge = SharedBridge::default();
        Self {
            bulbs,
            unknown_bulbs,
            broadcast,
            snapshot,
            sonos: MockSonos::start(true).await,
            sub: MockSonos::start(false).await,
            wol,
            tv,
            moonraker: MockMoonraker::start().await,
            proxmox: MockProxmox::start(NODE).await,
            hue: MockHttp::start(hue_bridge_handler(bridge.clone())).await,
            hue_bridge: bridge,
            plex: MockHttp::start(plex_handler).await,
            camera: MockRtsp::start().await,
            collector: MockHttp::start(move |seen| {
                if seen.method == "POST" && seen.path == PAIRING_PATH {
                    if seen.header("authorization") != format!("Bearer {COLLECTOR_TOKEN}") {
                        return (401, String::new());
                    }
                    return (200, json!({ "code": "PAIR-1234", "expires_in": 600 }).to_string());
                }
                let (status, _) = collector_handler(seen);
                if status != 200 {
                    return (status, String::new());
                }
                (200, shared_snapshot.lock().unwrap().to_string())
            })
            .await,
            plugs: vec![MockTuya::start(PLUG_IDS[0]).await, MockTuya::start(PLUG_IDS[1]).await],
            toshiba: MockToshiba::start().await,
            petkit: MockHttp::start(|_| (404, String::new())).await,
            dir: tempfile::tempdir().unwrap(),
        }
    }

    /// An instance of every first-party plugin, each pointed at its mock with its
    /// credential in its slot, under the instance ids `swifty migrate` gives them;
    /// a Petkit instance with no credential, whose feeder and litter box are listed
    /// and waiting for setup; and a mock instance standing in for the router.
    /// Discovery, agent pairing and further hosts are left off: see [`Rig::configure_all`].
    pub fn configure(&self, site: &mut Site) {
        site.config.network.connect_timeout_ms = 500;
        site.config.network.read_timeout_ms = 500;
        let bulbs: Vec<Value> = self
            .bulbs
            .iter()
            .enumerate()
            .map(|(i, bulb)| json!({ "id": format!("bulb{}", i + 1), "address": bulb.addr.to_string() }))
            .collect();
        site.add("wiz", "wiz", json!({ "bulbs": bulbs }));
        site.add("hue", "hue", json!({ "url": self.hue.url }));
        site.secret("hue", "app_key", HUE_KEY);
        site.add(
            "sonos",
            "sonos",
            json!({ "speakers": [
                { "id": "sonos", "url": self.sonos.http.url },
                { "id": "sub", "url": self.sub.http.url, "read_only": true },
            ] }),
        );
        site.add(
            "webos",
            "webos",
            json!({ "url": self.tv.url, "wol_mac": "aa:bb:cc:dd:ee:ff", "wol_broadcast": self.wol.addr.to_string() }),
        );
        site.secret("webos", "client_key", TV_KEY);
        let plugs: Vec<&MockTuya> = self.plugs.iter().collect();
        site.add("tuya", "tuya", tuya_config(&plugs, &[]));
        for plug in &self.plugs {
            site.secret("tuya", &format!("local_key/{}", plug.id), TUYA_KEY_BASE64);
        }
        site.add("toshiba", "toshiba", toshiba_config(&self.toshiba));
        site.secret("toshiba", "login", &toshiba_login());
        site.add("petkit", "petkit", json!({ "region": "DE", "passport_url": self.petkit.url }));
        site.add("moonraker", "moonraker", json!({ "url": self.moonraker.http.url }));
        site.secret("moonraker", "api_key", MOONRAKER_KEY);
        let console: Vec<Value> = CONSOLE_VMS.iter().map(|vm| json!({ "vm": vm })).collect();
        site.add(
            "proxmox",
            "proxmox",
            json!({ "url": self.proxmox.http.url, "node": NODE, "console_targets": console }),
        );
        site.secret("proxmox", "token", PROXMOX_TOKEN);
        site.add("plex", "plex", json!({ "url": self.plex.url }));
        site.secret("plex", "token", PLEX_TOKEN);
        site.add("camera", "camera", json!({ "host": "127.0.0.1", "port": self.camera.port }));
        site.secret("camera", "login", CAMERA_LOGIN);
        site.add("glimdock", "glimdock", json!({ "url": self.collector.url, "snapshot_path": SNAPSHOT_PATH }));
        site.secret("glimdock", "token", COLLECTOR_TOKEN);
        site.add(
            "router",
            "mock",
            json!({
                "devices": [{ "id": "router", "class": "router", "name": "Router", "target": "router" }],
                "disabled": ROUTER_NOT_AVAILABLE,
            }),
        );
    }

    /// As [`Rig::configure`], with discovery, agent pairing and further hosts on.
    pub fn configure_all(&self, site: &mut Site) {
        self.configure(site);
        site.instance("wiz")["broadcast"] = json!(self.broadcast.addr.to_string());
        site.instance("glimdock")["pairing_path"] = json!(PAIRING_PATH);
        site.config.network.allowed_host_networks.push("127.0.0.0/8".to_string());
        let files = self.dir.path().to_string_lossy().into_owned();
        site.instance("proxmox")["host_files_dir"] = json!(files);
    }

    /// Every connection or datagram that has reached any device.
    pub fn traffic(&self) -> usize {
        self.bulbs.iter().map(MockBulb::traffic).sum::<usize>()
            + self.unknown_bulbs.iter().map(MockBulb::traffic).sum::<usize>()
            + self.broadcast.traffic()
            + self.sonos.http.traffic()
            + self.sub.http.traffic()
            + self.tv.traffic()
            + self.wol.traffic()
            + self.moonraker.http.traffic()
            + self.proxmox.http.traffic()
            + self.hue.traffic()
            + self.plex.traffic()
            + self.camera.traffic()
            + self.collector.traffic()
            + self.plugs.iter().map(MockTuya::traffic).sum::<usize>()
            + self.toshiba.traffic()
            + self.petkit.traffic()
    }
}
