//! Shared test harness. Requests go straight into the router, so no socket is
//! opened. Time comes from a manual clock, so expiry needs no sleeping. The
//! software authenticator signs real WebAuthn ceremonies.
#![allow(dead_code)]

pub mod devices;

use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::Router;
use devices::Site;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use swifty_hub::app::App;
use swifty_hub::clock::{Clock, ManualClock};
use swifty_hub::config::{Config, SpeechConfig};
use swifty_hub::http;
use swifty_hub::inventory::PRIMARY_HOST;
use swifty_hub::passkeys::Passkeys;
use swifty_hub::runtime::Registry;
use swifty_hub::secrets::{FileSecretStore, MemorySecretStore, SecretStore};
use swifty_hub::store::Store;
use swifty_hub::{notes, scenes};
use swifty_plugin_api::{Plugin, Secret as Credential};
use swifty_plugin_mock::{House, MockDevices, MockPlugin};
use tempfile::TempDir;
use tower::ServiceExt;
use url::Url;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;
use webauthn_authenticator_rs::{AuthenticatorBackend, WebauthnAuthenticator};
use webauthn_rs::prelude::{
    CreationChallengeResponse, PublicKeyCredential, RegisterPublicKeyCredential, RequestChallengeResponse,
};

pub const ORIGIN: &str = "https://home.example.org";
pub const EVIL_ORIGIN: &str = "https://evil.example";
pub const START_MS: i64 = 1_900_000_000_000;
pub const HOUR_MS: i64 = 3_600_000;
pub const MINUTE_MS: i64 = 60_000;

pub fn origin_url() -> Url {
    Url::parse(ORIGIN).unwrap()
}

pub fn evil_url() -> Url {
    Url::parse(EVIL_ORIGIN).unwrap()
}

/// A passkey that performs user verification.
pub fn authenticator() -> SoftPasskey {
    SoftPasskey::new(true)
}

pub struct Resp {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Value,
    pub text: String,
}

impl Resp {
    pub fn code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or("")
    }

    #[track_caller]
    pub fn assert_refused(&self, status: StatusCode, code: &str) {
        assert_eq!(self.status, status, "unexpected status, body: {}", self.text);
        assert_eq!(self.code(), code, "unexpected code, body: {}", self.text);
    }

    #[track_caller]
    pub fn assert_ok(&self) {
        assert!(self.status.is_success(), "expected success, got {}: {}", self.status, self.text);
    }

    pub fn phase(&self) -> &str {
        self.body["phase"].as_str().unwrap_or("")
    }

    /// `name=value` for sending back in a Cookie header.
    pub fn session_cookie(&self) -> String {
        let raw = self.headers[header::SET_COOKIE].to_str().unwrap();
        raw.split(';').next().unwrap().to_string()
    }
}

pub struct Harness {
    pub app: App,
    pub router: Router,
    pub clock: Arc<ManualClock>,
    pub devices: Arc<MockDevices>,
    pub dir: TempDir,
}

/// The text of a version 2 `swifty.toml` for a database at `database`, with more
/// lines after it.
pub fn config_text(database: &Path, more: &str) -> String {
    format!(
        "config_version = 2\ndatabase = '{}'\n{more}\n[webauthn]\norigin = \"{ORIGIN}\"\nrp_id = \"home.example.org\"\nrp_name = \"Swifty Home\"\n",
        database.display()
    )
}

pub fn test_config(dir: &TempDir) -> Config {
    let db = dir.path().join("home.db");
    let mut config = Config::parse(&config_text(&db, "")).unwrap();
    config.live = true;
    config.timing.poll_ms = 1;
    config.timing.sync_wait_ms = 3_000;
    config.timing.confirm_timeout_ms = Some(80);
    // The test house runs on one of its own VMs, as the owner's migrated file says.
    config.runs_on = Some(RUNS_ON.to_string());
    config
}

/// The VM the test hub runs on.
pub const RUNS_ON: &str = "vm102";

/// The machines every test house has, approved, on the primary host. A new
/// database has none of its own.
pub const TEST_VMS: [(u32, &str); 4] = [(100, "alpha"), (101, "bravo"), (102, "charlie"), (103, "delta")];

/// The legacy house the tests run against, from the mock plugin: one mock instance
/// for each old kind of device, the same targets, and [`TEST_VMS`] as its VMs.
pub fn legacy_house() -> House {
    swifty_plugin_mock::legacy_house_with_vms(&TEST_VMS)
}

/// The compiled-in plugins with the mock bound to `world` in front of the mock of
/// the plugin list, so the house's instances drive what a test holds and every
/// other plugin's words and actions are known, as in the shipped build.
pub fn plugins_with(world: &Arc<MockDevices>) -> Vec<&'static dyn Plugin> {
    let mut plugins: Vec<&'static dyn Plugin> = vec![MockPlugin::bound(world)];
    plugins.extend(swifty::plugins::compiled_in().into_iter().filter(|plugin| plugin.manifest().id != "mock"));
    plugins
}

/// A registry whose mock instances all drive `world`.
pub fn mock_registry(live: bool, world: &Arc<MockDevices>) -> Registry {
    let config = Config::parse(&config_text(Path::new("x.db"), "")).unwrap();
    Registry::new(live, plugins_with(world), Arc::new(MemorySecretStore::default()), (&config.network).into())
}

/// The registry `swifty serve` builds: every compiled-in plugin, and the secret
/// store in the data directory.
pub fn production_registry(config: &Config) -> Registry {
    let secrets = Arc::new(FileSecretStore::new(config.secrets_path()));
    Registry::new(config.live, swifty::plugins::compiled_in(), secrets, (&config.network).into())
}

/// Stores the instances and devices of a house.
pub fn store_house(conn: &rusqlite::Connection, house: &House) -> rusqlite::Result<()> {
    for instance in &house.instances {
        conn.execute(
            "INSERT INTO plugin_instances (id, plugin, config, config_version, enabled, created_at, created_by, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, 'test', 0)",
            rusqlite::params![
                instance.id,
                instance.plugin,
                instance.config.to_string(),
                instance.config_version,
                instance.enabled
            ],
        )?;
    }
    for device in &house.devices {
        let class = serde_json::to_value(device.class).unwrap();
        let origin = serde_json::to_value(device.origin).unwrap();
        let status = serde_json::to_value(device.status).unwrap();
        let adopted = device.origin == swifty_plugin_mock::Origin::Adopted;
        conn.execute(
            "INSERT INTO devices (target, instance_id, local_id, class, name, attrs, origin, status,
                                  first_seen, adopted_at, adopted_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9, ?10)",
            rusqlite::params![
                device.target,
                device.instance,
                device.local_id,
                class.as_str(),
                device.name,
                device.attrs.to_string(),
                origin.as_str(),
                status.as_str(),
                adopted.then_some(0),
                adopted.then_some("seed"),
            ],
        )?;
    }
    Ok(())
}

/// Stores [`TEST_VMS`] as approved machines of `instance`, as a scan and the owner would.
pub fn store_test_vms(conn: &rusqlite::Connection, instance: &str) -> rusqlite::Result<()> {
    for (vmid, name) in TEST_VMS {
        conn.execute(
            "INSERT INTO devices (target, instance_id, local_id, class, name, attrs, origin, status,
                                  first_seen, adopted_at, adopted_by)
             VALUES (?1, ?2, ?1, 'vm', ?3, ?4, 'adopted', 'active', 0, 0, 'seed')",
            rusqlite::params![
                format!("vm{vmid}"),
                instance,
                name,
                json!({ "host": PRIMARY_HOST, "vmid": vmid }).to_string()
            ],
        )?;
    }
    Ok(())
}

/// Stores the legacy house's scenes, with the bulbs' own levels, and its notes on
/// devices, as the owner's site file would.
pub fn seed_scenes_and_notes(path: &Path) {
    let house = legacy_house();
    let store = Store::open(path).unwrap();
    store
        .tx(|tx| -> rusqlite::Result<()> {
            for scene in &house.scenes {
                let overrides = scene
                    .overrides
                    .iter()
                    .map(|level| scenes::SceneOverride {
                        target: level.target.clone(),
                        percent: level.percent,
                        hue: level.hue,
                    })
                    .collect();
                let stored = scenes::Scene {
                    id: scene.id.clone(),
                    name: scene.name.clone(),
                    position: i64::from(scene.position),
                    percent: scene.percent,
                    hue: scene.hue,
                    overrides,
                    extras: scene.extras.clone(),
                };
                scenes::save(tx, &stored)?;
            }
            for note in &house.notes {
                notes::set(tx, &note.target, &note.notes)?;
            }
            Ok(())
        })
        .unwrap();
}

/// Stores this house, and the legacy house's scenes and notes.
pub fn seed_house(path: &Path, house: &House) {
    Store::open(path).unwrap().tx(|tx| store_house(tx, house)).unwrap();
    seed_scenes_and_notes(path);
}

/// Stores a site: its instances, its test machines (under its `proxmox` instance,
/// or a placeholder one as an upgraded database has before setup), its scenes and
/// notes, and what its secret slots hold.
pub fn seed_site(site: &Site) {
    let path = site.config.database_path();
    Store::open(&path)
        .unwrap()
        .tx(|tx| -> rusqlite::Result<()> {
            for instance in &site.instances {
                tx.execute(
                    "INSERT INTO plugin_instances (id, plugin, config, config_version, enabled, created_at, created_by, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, 0, 'test', 0)",
                    rusqlite::params![
                        instance.id,
                        instance.plugin,
                        instance.config.to_string(),
                        instance.config_version,
                        instance.enabled
                    ],
                )?;
            }
            if !site.has("proxmox") {
                tx.execute(
                    "INSERT INTO plugin_instances (id, plugin, config, config_version, enabled, created_at, created_by, updated_at)
                     VALUES ('proxmox', 'proxmox', '{}', 1, 1, 0, 'schema-v4', 0)",
                    [],
                )?;
            }
            store_test_vms(tx, "proxmox")
        })
        .unwrap();
    seed_scenes_and_notes(&path);
    let store = FileSecretStore::new(site.config.secrets_path());
    for (instance, slot, value) in &site.secrets {
        store.write(instance, slot, &Credential::new(value.as_str())).unwrap();
    }
}

/// Stores a light that was added after a discovery, as a database that already had
/// it would hold it: under its own target, with its MAC and address, for the
/// instance named after what used to carry it (`hue` for one on the bridge).
pub fn store_light(
    conn: &rusqlite::Connection,
    target: &str,
    mac: &str,
    address: &str,
    name: &str,
) -> rusqlite::Result<()> {
    let instance = if address == "hue-bridge" { "hue" } else { "wiz" };
    conn.execute(
        "INSERT OR IGNORE INTO plugin_instances
             (id, plugin, config, config_version, enabled, created_at, created_by, updated_at)
         VALUES (?1, ?1, '{}', 1, 1, 0, 'test', 0)",
        [instance],
    )?;
    conn.execute(
        "INSERT INTO devices (target, instance_id, local_id, class, name, attrs, origin, status,
                              first_seen, adopted_at, adopted_by)
         VALUES (?1, ?2, ?1, 'light', ?3, ?4, 'adopted', 'active', 1, 1, 'x')",
        rusqlite::params![target, instance, name, json!({ "mac": mac, "address": address }).to_string()],
    )?;
    Ok(())
}

impl Harness {
    /// Live registry, with the mock plugin behind every instance of the legacy house.
    pub fn new() -> Self {
        Self::with(|_| {})
    }

    pub fn with(tweak: impl FnOnce(&mut Config)) -> Self {
        Self::with_plugins(Vec::new(), tweak)
    }

    /// The mock house, with plugins of a test's own in the build as well.
    pub fn with_plugins(extra: Vec<&'static dyn Plugin>, tweak: impl FnOnce(&mut Config)) -> Self {
        Self::with_house(extra, |_| {}, tweak)
    }

    /// The legacy house as `change` leaves it, with plugins of a test's own as well.
    pub fn with_house(
        extra: Vec<&'static dyn Plugin>,
        change: impl FnOnce(&mut House),
        tweak: impl FnOnce(&mut Config),
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(&dir);
        tweak(&mut config);
        let mut house = legacy_house();
        change(&mut house);
        seed_house(&config.database_path(), &house);
        let devices = MockDevices::new();
        let mut plugins = plugins_with(&devices);
        plugins.extend(extra);
        let registry =
            Registry::new(config.live, plugins, Arc::new(MemorySecretStore::default()), (&config.network).into());
        Self::build(dir, config, registry, devices)
    }

    /// Stores an instance of a plugin with this config, and loads it with its devices.
    pub fn add_instance(&self, id: &str, plugin: &str, config: Value) {
        self.app
            .store
            .with(|conn| {
                conn.execute(
                    "INSERT INTO plugin_instances (id, plugin, config, config_version, enabled, created_at, created_by, updated_at)
                     VALUES (?1, ?2, ?3, 1, 1, 0, 'test', 0)",
                    rusqlite::params![id, plugin, config.to_string()],
                )
            })
            .unwrap();
        self.app.sync_directory().unwrap();
    }

    /// Changes the config of a stored instance, and loads it again.
    pub fn set_instance_config(&self, id: &str, config: Value) {
        self.app
            .store
            .with(|conn| {
                conn.execute(
                    "UPDATE plugin_instances SET config = ?2 WHERE id = ?1",
                    rusqlite::params![id, config.to_string()],
                )
            })
            .unwrap();
        self.app.sync_directory().unwrap();
    }

    /// The mock plugin, on a database that `prepare` makes first. Nothing is seeded:
    /// this is a house as that database has it. The instances an upgrade left with
    /// no config yet are given to the mock, as `swifty migrate` gives them to their
    /// plugins, so their devices can be reached without a network.
    pub fn on_database(prepare: impl FnOnce(&Path)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(&dir);
        prepare(&config.database_path());
        Store::open(&config.database_path())
            .unwrap()
            .with(|conn| {
                conn.execute(
                    "UPDATE plugin_instances SET plugin = 'mock', config = '{\"devices\":[]}' WHERE config = '{}'",
                    [],
                )
            })
            .unwrap();
        let devices = MockDevices::new();
        let registry = mock_registry(config.live, &devices);
        Self::build(dir, config, registry, devices)
    }

    /// A site and a registry of a test's own: the instances and secrets the site
    /// names, the test machines, scenes and notes.
    pub fn with_registry(tweak: impl FnOnce(&mut Site), registry: impl FnOnce(&Config) -> Registry) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut site = Site::new(test_config(&dir));
        tweak(&mut site);
        seed_site(&site);
        let registry = registry(&site.config);
        Self::build(dir, site.config, registry, MockDevices::new())
    }

    /// A site with the registry `swifty serve` builds: every compiled-in plugin.
    pub fn with_site(tweak: impl FnOnce(&mut Site)) -> Self {
        Self::with_registry(tweak, production_registry)
    }

    /// Enrolls one passkey, signs in and returns the passkey and the cookie.
    pub async fn sign_in(&self) -> (SoftPasskey, String) {
        let mut auth = authenticator();
        self.enroll(&mut auth, "iPhone").await;
        let cookie = self.login(&mut auth).await;
        (auth, cookie)
    }

    pub fn build(dir: TempDir, config: Config, registry: Registry, devices: Arc<MockDevices>) -> Self {
        let clock = Arc::new(ManualClock::new(START_MS));
        let app = App::new(config, clock.clone(), registry).unwrap();
        let router = http::router(app.clone());
        Self { app, router, clock, devices, dir }
    }

    pub fn advance(&self, ms: i64) {
        self.clock.advance_ms(ms);
    }

    pub fn now(&self) -> i64 {
        self.clock.now_ms()
    }

    pub async fn send(
        &self,
        method: Method,
        path: &str,
        cookie: Option<&str>,
        origin: Option<&str>,
        body: Option<Value>,
    ) -> Resp {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        let body = match body {
            Some(value) => {
                request = request.header(header::CONTENT_TYPE, "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        self.dispatch(request.body(body).unwrap()).await
    }

    pub async fn dispatch(&self, request: Request<Body>) -> Resp {
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).to_string();
        let body = serde_json::from_str(&text).unwrap_or(Value::Null);
        Resp { status, headers, body, text }
    }

    pub async fn post(&self, path: &str, cookie: &str, body: Value) -> Resp {
        self.send(Method::POST, path, Some(cookie), Some(ORIGIN), Some(body)).await
    }

    pub async fn post_public(&self, path: &str, body: Value) -> Resp {
        self.send(Method::POST, path, None, Some(ORIGIN), Some(body)).await
    }

    pub async fn get(&self, path: &str, cookie: &str) -> Resp {
        self.send(Method::GET, path, Some(cookie), None, None).await
    }

    // ----- passkeys

    pub fn enroll_token(&self) -> String {
        Passkeys::issue_enroll_token(&self.app.store, self.now()).unwrap().0
    }

    pub async fn enroll_with(&self, auth: &mut SoftPasskey, token: &str, name: &str) -> Resp {
        let started = self.post_public("/api/v1/auth/enroll/start", json!({ "token": token, "name": name })).await;
        started.assert_ok();
        let credential = register(auth, &started.body["options"], origin_url());
        self.post_public(
            "/api/v1/auth/enroll/finish",
            json!({ "ceremony_id": started.body["ceremony_id"], "credential": credential }),
        )
        .await
    }

    pub async fn enroll(&self, auth: &mut SoftPasskey, name: &str) -> String {
        let token = self.enroll_token();
        let done = self.enroll_with(auth, &token, name).await;
        done.assert_ok();
        done.body["credential"]["id"].as_str().unwrap().to_string()
    }

    pub async fn login_start(&self) -> Resp {
        let started = self.post_public("/api/v1/auth/login/start", json!({})).await;
        started.assert_ok();
        started
    }

    pub async fn login_submit(&self, ceremony: &Value, assertion: &PublicKeyCredential, cookie: Option<&str>) -> Resp {
        self.send(
            Method::POST,
            "/api/v1/auth/login/finish",
            cookie,
            Some(ORIGIN),
            Some(json!({ "ceremony_id": ceremony, "assertion": assertion })),
        )
        .await
    }

    /// Signs in and returns the cookie.
    pub async fn login(&self, auth: &mut SoftPasskey) -> String {
        let started = self.login_start().await;
        let assertion = assert_with(auth, &started.body["options"], origin_url());
        let done = self.login_submit(&started.body["ceremony_id"], &assertion, None).await;
        done.assert_ok();
        done.session_cookie()
    }

    /// Enrolls one passkey, signs in, and returns everything a test needs.
    pub async fn signed_in() -> (Self, SoftPasskey, String) {
        Self::signed_in_with(|_| {}).await
    }

    pub async fn signed_in_with(tweak: impl FnOnce(&mut Config)) -> (Self, SoftPasskey, String) {
        let harness = Self::with(tweak);
        let mut auth = authenticator();
        harness.enroll(&mut auth, "iPhone").await;
        let cookie = harness.login(&mut auth).await;
        (harness, auth, cookie)
    }

    // ----- actions

    pub async fn execute(&self, cookie: &str, action: &str, target: &str, params: Value) -> Resp {
        self.post("/api/v1/actions/execute", cookie, json!({ "action": action, "target": target, "params": params }))
            .await
    }

    pub async fn prepare(&self, cookie: &str, action: &str, target: &str, params: Value) -> Resp {
        self.post("/api/v1/actions/prepare", cookie, json!({ "action": action, "target": target, "params": params }))
            .await
    }

    /// Confirms with the single-use token from `prepared`.
    pub async fn confirm_tap(&self, cookie: &str, prepared: &Resp) -> Resp {
        let body = &prepared.body;
        self.post(
            "/api/v1/actions/confirm",
            cookie,
            json!({
                "prepared_id": body["prepared_id"],
                "action": body["action"],
                "target": body["target"],
                "params": body["params"],
                "confirmation": body["confirmation"],
            }),
        )
        .await
    }

    pub async fn step_up_start(&self, cookie: &str, prepared: &Resp) -> Resp {
        let body = &prepared.body;
        self.post(
            "/api/v1/auth/step-up/start",
            cookie,
            json!({
                "prepared_id": body["prepared_id"],
                "action": body["action"],
                "target": body["target"],
                "params": body["params"],
            }),
        )
        .await
    }

    pub async fn confirm_assertion(&self, cookie: &str, prepared: &Resp, assertion: &PublicKeyCredential) -> Resp {
        let body = &prepared.body;
        self.post(
            "/api/v1/actions/confirm",
            cookie,
            json!({
                "prepared_id": body["prepared_id"],
                "action": body["action"],
                "target": body["target"],
                "params": body["params"],
                "assertion": assertion,
            }),
        )
        .await
    }

    /// Prepare, step-up and confirm a critical action.
    pub async fn critical(
        &self,
        cookie: &str,
        auth: &mut SoftPasskey,
        action: &str,
        target: &str,
        params: Value,
    ) -> Resp {
        let prepared = self.prepare(cookie, action, target, params).await;
        prepared.assert_ok();
        let started = self.step_up_start(cookie, &prepared).await;
        started.assert_ok();
        let assertion = assert_with(auth, &started.body["options"], origin_url());
        self.confirm_assertion(cookie, &prepared, &assertion).await
    }

    /// Adds a second passkey through the full critical flow.
    pub async fn register_passkey(
        &self,
        cookie: &str,
        approver: &mut SoftPasskey,
        new_key: &mut SoftPasskey,
        name: &str,
        recovery: bool,
    ) -> String {
        let confirmed = self
            .critical(cookie, approver, "passkey.register", "new", json!({ "name": name, "recovery": recovery }))
            .await;
        confirmed.assert_ok();
        let registration = &confirmed.body["registration"];
        let credential = register(new_key, &registration["options"], origin_url());
        let done = self
            .post(
                "/api/v1/passkeys/register/finish",
                cookie,
                json!({ "ceremony_id": registration["ceremony_id"], "credential": credential }),
            )
            .await;
        done.assert_ok();
        done.body["credential"]["id"].as_str().unwrap().to_string()
    }

    // ----- people

    /// The owner creates an invitation, through the full critical flow.
    pub async fn invite(&self, cookie: &str, owner: &mut SoftPasskey, name: &str) -> String {
        let done = self.critical(cookie, owner, "user.invite", "new", json!({ "name": name })).await;
        done.assert_ok();
        assert_eq!(done.phase(), "confirmed", "{}", done.text);
        done.body["invitation"]["token"].as_str().unwrap().to_string()
    }

    pub async fn join_start(&self, token: &str, passkey_name: &str) -> Resp {
        self.post_public("/api/v1/auth/invite/start", json!({ "token": token, "name": passkey_name })).await
    }

    pub async fn join_finish(&self, auth: &mut SoftPasskey, started: &Resp) -> Resp {
        let credential = register(auth, &started.body["options"], origin_url());
        self.post_public(
            "/api/v1/auth/invite/finish",
            json!({ "ceremony_id": started.body["ceremony_id"], "credential": credential }),
        )
        .await
    }

    /// Invites someone, has them join, signs them in, and returns their passkey,
    /// cookie and user id.
    pub async fn add_family(
        &self,
        owner_cookie: &str,
        owner: &mut SoftPasskey,
        name: &str,
    ) -> (SoftPasskey, String, String) {
        let token = self.invite(owner_cookie, owner, name).await;
        let started = self.join_start(&token, "Their phone").await;
        started.assert_ok();
        let mut auth = authenticator();
        let joined = self.join_finish(&mut auth, &started).await;
        joined.assert_ok();
        let cookie = self.login(&mut auth).await;
        let user = joined.body["user"]["user_id"].as_str().unwrap().to_string();
        (auth, cookie, user)
    }

    // ----- a real server on loopback, for what needs a real socket

    pub async fn serve(&self) -> Served {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = self.router.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Served { addr, task }
    }

    // ----- database inspection

    pub fn audit_results(&self) -> Vec<String> {
        self.app
            .store
            .with(|conn| {
                let mut statement = conn.prepare("SELECT result FROM audit_log ORDER BY seq")?;
                let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
                rows.collect::<rusqlite::Result<Vec<String>>>()
            })
            .unwrap()
    }

    pub fn audit_has(&self, result: &str) -> bool {
        self.audit_results().iter().any(|r| r == result)
    }

    pub fn audit_count(&self, result: &str) -> usize {
        self.audit_results().iter().filter(|r| r.as_str() == result).count()
    }

    /// Every text and hex value in the database, for checking what was stored.
    pub fn dump(&self, table: &str) -> String {
        self.app
            .store
            .with(|conn| -> rusqlite::Result<String> {
                let mut statement = conn.prepare(&format!("SELECT * FROM {table}"))?;
                let columns = statement.column_count();
                let mut rows = statement.query([])?;
                let mut out = String::new();
                while let Some(row) = rows.next()? {
                    for i in 0..columns {
                        match row.get_ref(i)? {
                            rusqlite::types::ValueRef::Text(t) => {
                                out.push_str(&String::from_utf8_lossy(t));
                            }
                            rusqlite::types::ValueRef::Integer(n) => out.push_str(&n.to_string()),
                            rusqlite::types::ValueRef::Blob(b) => out.push_str(&hex::encode(b)),
                            _ => {}
                        }
                        out.push('|');
                    }
                    out.push('\n');
                }
                Ok(out)
            })
            .unwrap()
    }
}

/// The service on a loopback port. Stops when dropped.
pub struct Served {
    pub addr: std::net::SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Served {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Every device mock, the whole service with every first-party plugin, and a
/// signed-in owner.
pub async fn rig_harness(live: bool, tweak: impl FnOnce(&mut Site)) -> (devices::Rig, Harness, SoftPasskey, String) {
    let rig = devices::Rig::start().await;
    let harness = Harness::with_site(|site| {
        site.live = live;
        site.timing.confirm_timeout_ms = Some(1_200);
        site.timing.poll_ms = 5;
        rig.configure_all(site);
        tweak(site);
    });
    let mut auth = authenticator();
    harness.enroll(&mut auth, "iPhone").await;
    let cookie = harness.login(&mut auth).await;
    (rig, harness, auth, cookie)
}

pub fn register(auth: &mut SoftPasskey, options: &Value, origin: Url) -> RegisterPublicKeyCredential {
    let options: CreationChallengeResponse =
        serde_json::from_value(json!({ "publicKey": options["publicKey"] })).unwrap();
    auth.do_registration(origin, options).unwrap()
}

pub fn request_options(options: &Value) -> RequestChallengeResponse {
    serde_json::from_value(options.clone()).unwrap()
}

/// Signs a challenge as a browser at `origin` would.
pub fn assert_with(auth: &mut SoftPasskey, options: &Value, origin: Url) -> PublicKeyCredential {
    auth.do_authentication(origin, request_options(options)).unwrap()
}

/// Signs a challenge claiming an origin other than the one it should be, which
/// is what a phishing page would produce.
pub fn assert_from_wrong_origin(auth: &mut SoftPasskey, options: &Value) -> PublicKeyCredential {
    let options = request_options(options).public_key;
    AuthenticatorBackend::perform_auth(auth, evil_url(), options, 60_000).unwrap()
}

/// Signs a challenge with the user-verification flag left clear.
pub fn assert_without_uv(auth: &mut SoftPasskey, options: &Value) -> PublicKeyCredential {
    use webauthn_rs_proto::UserVerificationPolicy;
    let mut options = request_options(options).public_key;
    options.user_verification = UserVerificationPolicy::Discouraged_DO_NOT_USE;
    AuthenticatorBackend::perform_auth(auth, origin_url(), options, 60_000).unwrap()
}

// ----- a stand-in for the speech service, bound to loopback only

#[derive(Default)]
pub struct SpeechLog {
    pub authorization: Option<String>,
    pub content_type: Option<String>,
    pub body_len: usize,
    pub calls: usize,
}

pub struct FakeSpeech {
    pub url: String,
    pub text: Arc<Mutex<String>>,
    pub log: Arc<Mutex<SpeechLog>>,
    pub dir: TempDir,
    pub task: tokio::task::JoinHandle<()>,
}

pub const SPEECH_TOKEN: &str = "speech-test-token-0123456789";

impl FakeSpeech {
    pub async fn start() -> Self {
        use axum::routing::post;
        let text = Arc::new(Mutex::new(String::new()));
        let log = Arc::new(Mutex::new(SpeechLog::default()));
        let (text_handle, log_handle) = (text.clone(), log.clone());
        let route = post(move |headers: HeaderMap, body: axum::body::Bytes| {
            let text = text_handle.clone();
            let log = log_handle.clone();
            async move {
                {
                    let mut log = log.lock().unwrap();
                    log.calls += 1;
                    log.body_len = body.len();
                    log.authorization = headers.get(header::AUTHORIZATION).map(|v| v.to_str().unwrap().to_string());
                    log.content_type = headers.get(header::CONTENT_TYPE).map(|v| v.to_str().unwrap().to_string());
                }
                let spoken = text.lock().unwrap().clone();
                axum::Json(json!({ "text": spoken, "audio_ms": 900, "decode_ms": 30 }))
            }
        });
        let router = Router::new().route("/v1/transcribe", route);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("speech.token"), format!("{SPEECH_TOKEN}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path().join("speech.token"), std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        Self { url, text, log, dir, task }
    }

    pub fn config(&self) -> SpeechConfig {
        SpeechConfig {
            url: self.url.clone(),
            token_file: self.dir.path().join("speech.token"),
            connect_timeout_secs: 2,
            timeout_secs: 5,
        }
    }

    pub fn say(&self, text: &str) {
        *self.text.lock().unwrap() = text.to_string();
    }

    pub fn calls(&self) -> usize {
        self.log.lock().unwrap().calls
    }
}

impl Drop for FakeSpeech {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub const FAKE_WAV: &[u8] = b"RIFF$\0\0\0WAVEfmt fake-audio-bytes-for-tests";

pub async fn post_voice(harness: &Harness, cookie: &str) -> Resp {
    post_audio(harness, cookie, "/api/v1/voice").await
}

pub async fn post_audio(harness: &Harness, cookie: &str, path: &str) -> Resp {
    let request = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(header::COOKIE, cookie)
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "audio/wav")
        .body(Body::from(FAKE_WAV.to_vec()))
        .unwrap();
    harness.dispatch(request).await
}
