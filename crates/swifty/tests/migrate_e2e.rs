//! `swifty migrate`, end to end, through the real binary.
//!
//! A complete glimdock-home install is built in a temporary directory: a schema 3
//! database with a passkey, an open session, rooms, favourites, a room scene, lights,
//! machines on two hosts, learned phrases, rows that name the old router and a signed
//! audit head; a version 1 configuration with every section; every credential file,
//! the audit key, the speech token, the assistant key and a further host's files. It is
//! migrated with a site file, the new hub is started on what was written, and the old
//! directories are compared byte for byte with what they were.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use axum::http::StatusCode;
use common::{production_registry, Harness};
use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;
use serde_json::{json, Value};
use swifty_hub::admin::site::SiteFile;
use swifty_hub::audit::{self, AuditKey, Checkpoint, VerifyOptions};
use swifty_hub::config::Config;
use swifty_hub::runtime::health;
use swifty_hub::secrets::{FileSecretStore, SecretStore as _};
use swifty_hub::store::Store;
use swifty_hub::{inventory, notes, scenes, store};
use swifty_plugin_api::Readiness;
use swifty_plugin_mock::MockDevices;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;

fn swifty(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_swifty")).args(args).stdin(Stdio::null()).output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[track_caller]
fn assert_success(output: &Output) {
    assert!(output.status.success(), "stdout:\n{}\nstderr:\n{}", text(&output.stdout), text(&output.stderr));
}

/// A path written the way the version 1 file has it.
fn toml_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// The old house's tables, as glimdock-home wrote them. `@OWNER@` is the owner's user id.
const HOUSE_V3: &str = r#"
INSERT INTO machines (target, host, vmid, name, status, first_seen, approved_at, approved_by) VALUES
    ('vm100', 'primary', 100, 'alpha', 'approved', 0, 0, 'seed'),
    ('vm101', 'primary', 101, 'bravo', 'approved', 0, 0, 'seed'),
    ('vm102', 'primary', 102, 'charlie', 'approved', 0, 0, 'seed'),
    ('vm103', 'primary', 103, 'delta', 'approved', 0, 0, 'seed'),
    ('vm150', 'primary', 150, 'new-vm', 'pending', 5, NULL, NULL),
    ('nas2-vm200', 'nas2', 200, 'backup-vm', 'approved', 6, 7, 'owner');
INSERT INTO proxmox_hosts (id, address, node, token_file, ca_file, created_at, created_by)
    VALUES ('nas2', '192.0.2.50', 'pve2', 'nas2.token', 'nas2.pem', 4, 'owner');
INSERT INTO lights (target, mac, address, name, added_at, added_by) VALUES
    ('wiz-a8bb50aaaaaa', 'a8bb50aaaaaa', '192.0.2.29:38899', 'Hall lamp', 10, 'owner'),
    ('hue-3', 'hue-3', 'hue-bridge', 'Desk strip', 11, 'owner');
INSERT INTO light_candidates (mac, address, seen_at) VALUES ('a8bb50bbbbbb', '192.0.2.28:38899', 12);
INSERT INTO rooms (id, name, name_key, position, created_at) VALUES
    ('r-living', 'Living room', 'living room', 0, 1),
    ('r-office', 'Office', 'office', 1, 2);
INSERT INTO room_devices (target, room_id, position) VALUES
    ('bulb1', 'r-living', 0), ('tv', 'r-living', 1), ('wiz-a8bb50aaaaaa', 'r-living', 2),
    ('vm101', 'r-office', 0), ('router', 'r-office', 1), ('plug-bf0123456789abcdef', 'r-office', 2);
INSERT INTO room_scenes (id, room_id, name, name_key, settings)
    VALUES ('rs-reading', 'r-living', 'Reading', 'reading', '[{"target":"bulb1","percent":60,"hue":30}]');
INSERT INTO favourites (user_id, target, position) VALUES
    ('@OWNER@', 'bulb1', 0), ('@OWNER@', 'evening', 1), ('@OWNER@', 'vm100', 2), ('@OWNER@', 'router', 3);
INSERT INTO pinned_rooms (user_id, room_id, position) VALUES ('@OWNER@', 'r-living', 0);
INSERT INTO device_names (target, name) VALUES
    ('vm101', 'Storage'), ('router', 'Gateway'), ('ac-0a1b2c3d-1111-2222-3333-444455556666', 'Lounge air');
INSERT INTO learned_phrases (id, phrase, action, target, params, label, learned_by, created_at, uses) VALUES
    ('p-quiet', 'make it quieter', 'sonos.volume', 'sonos', '{"level":10}', 'Volume 10', '@OWNER@', 1, 0),
    ('p-tunnel', 'restart the tunnel', 'router.wireguard_restart', 'router', '{}', 'Tunnel', '@OWNER@', 2, 0);
INSERT INTO memories (id, user_id, text, created_at, created_by)
    VALUES ('m-1', '@OWNER@', 'Likes the lights warm.', 3, '@OWNER@');
"#;

/// The old configuration, with every section the old service read.
const OLD_CONFIG: &str = r#"
live = false
listen = "@@LISTEN@@"
database = "@@DIR@@/old-data/home.db"
console_vm = 102

[webauthn]
rp_id = "home.example.org"
origin = "https://home.example.org"
rp_name = "Home"

[session]
absolute_secs = 28800
idle_secs = 1800

[timing]
poll_ms = 250
sync_wait_ms = 4000
duplicate_window_ms = 10000

[audit]
key_file = "@@DIR@@/old/audit.key"

[speech]
url = "http://127.0.0.1:8791"
token_file = "@@DIR@@/old/speech.token"

[assistant]
urls = ["http://127.0.0.1:8080/v1"]
model = "my-model"
key_file = "@@DIR@@/old/assistant.key"

[assistant.agent]
owner_url = "http://127.0.0.1:8801"
guest_url = "http://127.0.0.1:8802"
connection = "my-connection"
model = "my-model"

[devices]
connect_timeout_ms = 1600
read_timeout_ms = 2600

[devices.wiz]
port = 38899

[devices.wiz.bulbs]
bulb1 = "192.0.2.21"
bulb2 = "192.0.2.22"
bulb3 = "192.0.2.23"
bulb4 = "192.0.2.24"

[devices.hue]
url = "http://192.0.2.30"
key_file = "@@DIR@@/old-data/hue.key"

[devices.sonos]
arc_url = "http://192.0.2.31:1400"
sub_url = "http://192.0.2.32:1400"

[devices.webos]
url = "wss://192.0.2.33:3001"
cert_sha256 = "@@PIN@@"
client_key_file = "@@DIR@@/old/lgtv.key"
wol_mac = "AA:BB:CC:DD:EE:FF"
wol_broadcast = "192.0.2.255:9"

[devices.moonraker]
url = "http://192.0.2.34:7125"

[devices.plex]
url = "http://192.0.2.35:32400"
token_file = "@@DIR@@/old/plex.token"

[devices.camera]
host = "192.0.2.36"
port = 8554
credentials_file = "@@DIR@@/old/camera.login"

[devices.proxmox]
url = "https://192.0.2.10:8006"
node = "pve"
token_file = "@@DIR@@/old/proxmox.token"
ca_file = "@@DIR@@/old/pve-root-ca.pem"

[devices.collector]
url = "http://192.0.2.18:8765"
snapshot_path = "/api/v1/snapshot"
token_file = "@@DIR@@/old/collector.token"

[[devices.tuya.plugs]]
id = "bf0123456789abcdef"
address = "192.0.2.40"
key_file = "@@DIR@@/old/tuya-bf0123456789abcdef.key"
usb_dp = 7

[[devices.tuya.plugs]]
id = "bf0123456789abcde2"
address = "192.0.2.41"
name = "Desk lamp socket"
key_file = "@@DIR@@/old/tuya-bf0123456789abcde2.key"

[devices.petkit]
region = "DE"
credentials_file = "@@DIR@@/old/petkit.login"
timezone = "Europe/Berlin"
utc_offset_minutes = 60
commands = true
feeder_id = 123456
litter_id = 234567

[devices.toshiba]
credentials_file = "@@DIR@@/old/toshiba.login"

[[devices.toshiba.units]]
id = "0a1b2c3d-1111-2222-3333-444455556666"
unique_id = "AB-12:cd"
name = "Lounge"

[[devices.toshiba.units]]
id = "0a1b2c3d-1111-2222-3333-444455557777"
unique_id = "AB-12:ce"

[console]
targets = ["vm101", "vm103"]

[machines]
allowed_host_networks = ["192.0.2.0/24"]
secrets_dir = "@@DIR@@/old/hosts"

[lights]
wiz_broadcast = "192.0.2.255:38899"

[agents]
pairing_path = "/api/v1/agents/pair"
"#;

/// Every file of the old install that holds a credential or a key, with what it holds.
const SECRET_FILES: &[(&str, &str)] = &[
    ("old-data/hue.key", "hue-app-key-fixture"),
    ("old/lgtv.key", "webos-client-key-fixture"),
    ("old/plex.token", "plex-fixture-value"),
    ("old/camera.login", "viewer:camera-fixture"),
    ("old/proxmox.token", "swifty@pve!home=fixture-value"),
    ("old/pve-root-ca.pem", "-----BEGIN CERTIFICATE-----\nMIIfixture\n-----END CERTIFICATE-----"),
    ("old/collector.token", "collector-fixture-value"),
    ("old/tuya-bf0123456789abcdef.key", "MDEyMzQ1Njc4OWFiY2RlZg=="),
    ("old/tuya-bf0123456789abcde2.key", "ZmVkY2JhOTg3NjU0MzIxMA=="),
    ("old/petkit.login", "someone@example.org:0123456789abcdef0123456789abcdef"),
    ("old/toshiba.login", "eyJ1c2VybmFtZSI6InNvbWVvbmUifQ=="),
    ("old/audit.key", "old-audit-key-0123456789abcdefghijklmnopqrstuvwxyz"),
    ("old/speech.token", "old-speech-token-fixture-0123456789"),
    ("old/assistant.key", "old-assistant-key-fixture"),
    ("old/hosts/nas2.token", "swifty@pve!nas2=fixture-value"),
    ("old/hosts/nas2.pem", "-----BEGIN CERTIFICATE-----\nMIInas2\n-----END CERTIFICATE-----"),
];

/// The site file the owner keeps: a partial overlay, the pins, names, notes and scenes.
const SITE: &str = r#"
site_version = 1

[hub.assistant.agent]
session_prefix = "oldhub"
client_header = "x-old-client"
client_name = "Old Hub"

[[instance]]
id = "toshiba"
plugin = "toshiba"
[instance.config]
client_suffix = "6f6c64687562"

[[instance]]
id = "webos"
plugin = "webos"
[instance.config]
wol_mac = "02:00:00:00:00:01"

[[instance]]
id = "sonos"
plugin = "sonos"
[instance.config]

[[device]]
target = "bulb1"
name = "Desk lamp"

[[device]]
target = "vm101"
notes = ["The shared folders go offline while this is down."]

[[device]]
target = "tv"
name = "Living room TV"
notes = ["Turning it off rarely confirms."]

[[scene]]
id = "evening"
name = "Evening"
percent = 40
hue = 34
overrides = [{ target = "bulb3", percent = 0, hue = 0 }, { target = "bulb4", percent = 25, hue = 14 }]

[[scene]]
id = "movie"
extras = { tv_input = "HDMI 2", volume_cap = 25 }
"#;

/// Every device the migrated house has, by target: what the old house's sections, lights
/// and machines were called.
const TARGETS: &[&str] = &[
    "ac-0a1b2c3d-1111-2222-3333-444455556666",
    "ac-0a1b2c3d-1111-2222-3333-444455557777",
    "bulb1",
    "bulb2",
    "bulb3",
    "bulb4",
    "camera",
    "collector",
    "feeder",
    "host",
    "hue",
    "hue-3",
    "litter",
    "nas2-vm200",
    "plex",
    "plug-bf0123456789abcde2",
    "plug-bf0123456789abcdef",
    "plug-bf0123456789abcdef-usb",
    "printer",
    "sonos",
    "sub",
    "tv",
    "vm100",
    "vm101",
    "vm102",
    "vm103",
    "vm150",
    "wiz-a8bb50aaaaaa",
];

/// The rows of a table, with its column names.
type Rows = (Vec<String>, Vec<Vec<SqlValue>>);

/// Every row of a table, with its column names.
fn rows(conn: &Connection, table: &str) -> Rows {
    let mut statement = conn.prepare(&format!("SELECT * FROM {table}")).unwrap();
    let columns: Vec<String> = statement.column_names().map(str::to_string).collect();
    let count = columns.len();
    let rows = statement
        .query_map([], |row| (0..count).map(|i| row.get::<_, SqlValue>(i)).collect::<rusqlite::Result<Vec<_>>>())
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    (columns, rows)
}

fn insert_rows(conn: &Connection, table: &str, (columns, rows): &Rows) {
    let marks: Vec<String> = (1..=columns.len()).map(|n| format!("?{n}")).collect();
    let sql = format!("INSERT INTO {table} ({}) VALUES ({})", columns.join(", "), marks.join(", "));
    for row in rows {
        conn.execute(&sql, rusqlite::params_from_iter(row.iter())).unwrap();
    }
}

/// Every file under a directory, by its path from there, with its bytes (and mode).
fn snapshot(dir: &Path) -> BTreeMap<String, (Vec<u8>, Option<u32>)> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<String, (Vec<u8>, Option<u32>)>) {
        for entry in std::fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(root).unwrap().display().to_string().replace('\\', "/");
            let mode = swifty_hub::admin::files::mode_of(&path).unwrap();
            if path.is_dir() {
                out.insert(format!("{relative}/"), (Vec::new(), mode));
                walk(root, &path, out);
            } else {
                out.insert(relative, (std::fs::read(&path).unwrap(), mode));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// A glimdock-home install in a temporary directory, ready to migrate.
struct Old {
    dir: tempfile::TempDir,
    checkpoint: Checkpoint,
    listen: String,
    passkey: SoftPasskey,
    old_cookie: String,
}

impl Old {
    fn at(&self, relative: &str) -> PathBuf {
        self.dir.path().join(relative)
    }

    fn config(&self) -> String {
        self.at("old/config.toml").display().to_string()
    }

    fn new_config(&self) -> PathBuf {
        self.at("etc-new").join("swifty.toml")
    }

    fn new_data(&self) -> PathBuf {
        self.at("data-new")
    }

    fn site(&self) -> String {
        self.at("site.toml").display().to_string()
    }

    /// `swifty --config <new> migrate --from <old> --data-dir <new data>` and more.
    fn migrate(&self, more: &[&str]) -> Output {
        let (config, data, from) =
            (self.new_config().display().to_string(), self.new_data().display().to_string(), self.config());
        let mut args =
            vec!["--config", config.as_str(), "migrate", "--from", from.as_str(), "--data-dir", data.as_str()];
        args.extend_from_slice(more);
        swifty(&args)
    }

    /// The old directories, as they are now.
    fn old_files(&self) -> BTreeMap<String, (Vec<u8>, Option<u32>)> {
        let mut all = snapshot(&self.at("old"));
        for (path, value) in snapshot(&self.at("old-data")) {
            all.insert(format!("data:{path}"), value);
        }
        all
    }
}

fn free_address() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    address
}

/// Builds the old install. The passkey and the open session are made by a hub the way
/// the old one made them, and moved into the schema 3 database.
async fn old_install() -> Old {
    let scratch = Harness::on_database(|_| {});
    let (passkey, old_cookie) = scratch.sign_in().await;
    let people: Vec<(&str, Rows)> = ["users", "credentials", "sessions"]
        .into_iter()
        .map(|table| (table, scratch.app.store.with(|conn| Ok::<_, rusqlite::Error>(rows(conn, table))).unwrap()))
        .collect();
    let (owner_id, owner): (String, String) = scratch
        .app
        .store
        .with(|conn| {
            let handle = conn.query_row("SELECT value FROM meta WHERE key = 'owner_id'", [], |row| row.get(0))?;
            let user = conn.query_row("SELECT id FROM users WHERE role = 'owner'", [], |row| row.get(0))?;
            Ok::<_, rusqlite::Error>((handle, user))
        })
        .unwrap();
    drop(scratch);

    let dir = tempfile::tempdir().unwrap();
    for sub in ["old/hosts", "old-data"] {
        std::fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    for (file, value) in SECRET_FILES {
        let path = dir.path().join(file);
        // Padded the way a file written by hand often is.
        std::fs::write(&path, format!("  {value}\n\n")).unwrap();
        swifty_hub::admin::files::set_mode(&path, 0o600).unwrap();
    }
    let listen = free_address();
    let config = OLD_CONFIG
        .replace("@@DIR@@", &toml_path(dir.path()))
        .replace("@@LISTEN@@", &listen)
        .replace("@@PIN@@", &"ab".repeat(32));
    std::fs::write(dir.path().join("old/config.toml"), config).unwrap();
    std::fs::write(dir.path().join("site.toml"), SITE).unwrap();

    let key = AuditKey::load(&dir.path().join("old/audit.key")).unwrap();
    let database = dir.path().join("old-data/home.db");
    store::create_version(&database, 3).unwrap();
    let checkpoint = {
        let conn = Connection::open(&database).unwrap();
        conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get::<_, String>(0)).unwrap();
        conn.execute("INSERT INTO meta (key, value) VALUES ('owner_id', ?1)", [&owner_id]).unwrap();
        for (table, table_rows) in &people {
            insert_rows(&conn, table, table_rows);
        }
        conn.execute_batch(&HOUSE_V3.replace("@OWNER@", &owner)).unwrap();
        let events = [
            ("light.add", "wiz-a8bb50aaaaaa"),
            ("vm.start", "vm100"),
            ("sonos.volume", "sonos"),
            ("router.wireguard_restart", "router"),
        ];
        for (n, (action, target)) in events.into_iter().enumerate() {
            let event = audit::Event {
                ts: n as i64,
                action: action.to_string(),
                target: target.to_string(),
                params: json!({}),
                credential_ref: "c".to_string(),
                session_ref: "s".to_string(),
                user_ref: owner.clone(),
                device: "Phone".to_string(),
                channel: "panel".to_string(),
                step_up: "none".to_string(),
                result: "confirmed".to_string(),
                rtt_ms: None,
                run_id: None,
            };
            audit::append(&conn, &event, Some(&key)).unwrap();
        }
        let report = audit::verify_with(&conn, VerifyOptions { key: Some(&key), ..Default::default() }).unwrap();
        assert!(report.ok && report.keyed, "{report:?}");
        report.checkpoint.parse().unwrap()
    };
    let leftovers: Vec<_> =
        std::fs::read_dir(dir.path().join("old-data")).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(leftovers.len(), 2, "the old database closed cleanly: {leftovers:?}");
    Old { dir, checkpoint, listen, passkey, old_cookie }
}

#[tokio::test]
async fn a_complete_glimdock_home_install_moves_over_and_the_old_one_is_untouched() {
    let mut old = old_install().await;
    let before = old.old_files();
    let checkpoint = format!("{}:{}", old.checkpoint.seq, old.checkpoint.hash);
    let site = old.site();

    // --dry-run checks, verifies, rehearses everything and writes nothing.
    let plan = old.migrate(&["--site", &site, "--dry-run"]);
    assert_success(&plan);
    let shown = text(&plan.stdout);
    assert!(shown.contains(&checkpoint), "{shown}");
    assert!(shown.contains("Nothing was written"), "{shown}");
    for instance in [
        "wiz",
        "hue",
        "sonos",
        "webos",
        "moonraker",
        "plex",
        "camera",
        "proxmox",
        "glimdock",
        "tuya",
        "petkit",
        "toshiba",
    ] {
        assert!(shown.contains(&format!("   {instance:<10} {instance:<10} ready")), "{instance} is ready:\n{shown}");
    }
    assert!(shown.contains("toshiba: client_suffix = \"6f6c64687562\""), "{shown}");
    assert!(shown.contains("assistant.agent.session_prefix = \"oldhub\""), "{shown}");
    assert!(shown.contains("the router device, which no plugin carries: its rows"), "{shown}");
    assert!(!old.new_config().parent().unwrap().exists() && !old.new_data().exists(), "a dry-run creates nothing");
    assert_eq!(old.old_files(), before);
    for (_, value) in SECRET_FILES {
        for line in value.lines() {
            assert!(!shown.contains(line) && !text(&plan.stderr).contains(line), "a credential was printed");
        }
    }

    // The real run.
    let done = old.migrate(&["--site", &site]);
    assert_success(&done);
    let report = text(&done.stdout);
    assert!(report.contains("Done. Nothing of the old install was changed."), "{report}");
    assert!(report.contains(&format!("--expect-head {checkpoint}")), "{report}");
    assert_eq!(old.old_files(), before, "nothing under the old directories changed");
    for (_, value) in SECRET_FILES {
        for line in value.lines() {
            assert!(!report.contains(line), "a credential was printed");
        }
    }

    // The new swifty.toml: the same address and passkey origin, and the pins.
    let config = Config::load(&old.new_config()).unwrap();
    assert_eq!(config.listen, old.listen);
    assert_eq!(config.webauthn.origin, "https://home.example.org");
    assert_eq!(config.runs_on.as_deref(), Some("vm102"));
    let agent = config.assistant.as_ref().unwrap().agent.as_ref().unwrap();
    assert_eq!(
        (agent.session_prefix.as_str(), agent.client_header.as_str(), agent.client_name.as_str()),
        ("oldhub", "x-old-client", "Old Hub"),
        "the agent identity the site file pinned"
    );
    let etc = old.new_config().parent().unwrap().to_path_buf();
    for (old_file, new_file) in [
        ("old/audit.key", "audit.key"),
        ("old/speech.token", "speech.token"),
        ("old/assistant.key", "assistant.key"),
        ("old/hosts/nas2.token", "hosts/nas2.token"),
        ("old/hosts/nas2.pem", "hosts/nas2.pem"),
    ] {
        assert_eq!(std::fs::read(etc.join(new_file)).unwrap(), std::fs::read(old.at(old_file)).unwrap(), "{new_file}");
        #[cfg(unix)]
        assert_eq!(swifty_hub::admin::files::mode_of(&etc.join(new_file)).unwrap(), Some(0o600), "{new_file}");
    }
    assert_success(&swifty(&["config", "check", "--file", &old.new_config().display().to_string()]));

    // The audit chain verifies, with the key, and still holds the old checkpoint.
    let verify =
        swifty(&["--config", &old.new_config().display().to_string(), "audit", "verify", "--expect-head", &checkpoint]);
    assert_success(&verify);
    let verified: Value = serde_json::from_slice(&verify.stdout).unwrap();
    assert_eq!((verified["ok"].clone(), verified["keyed"].clone()), (json!(true), json!(true)), "{verified}");

    // The new hub starts on it.
    let registry = production_registry(&config);
    let harness = Harness::build(tempfile::tempdir().unwrap(), config.clone(), registry, MockDevices::new());

    // Every instance is ready: its config passes and its credentials are in their slots.
    let statuses = health::statuses(&harness.app.registry);
    let ids: Vec<&str> = statuses.iter().map(|status| status.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "camera",
            "glimdock",
            "hue",
            "moonraker",
            "petkit",
            "plex",
            "proxmox",
            "sonos",
            "toshiba",
            "tuya",
            "webos",
            "wiz"
        ]
    );
    for status in &statuses {
        assert_eq!(status.readiness, Readiness::Ready, "{}", status.id);
    }

    // The old session is over; the passkey signs in.
    let refused = harness.get("/api/v1/session", &old.old_cookie).await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED, "{}", refused.text);
    let cookie = harness.login(&mut old.passkey).await;

    // Every target, with the names people gave.
    let devices = harness.get("/api/v1/devices", &cookie).await;
    devices.assert_ok();
    let listed: BTreeMap<String, String> = devices.body["devices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|device| (device["target"].as_str().unwrap().to_string(), device["name"].as_str().unwrap().to_string()))
        .collect();
    let stored: BTreeSet<String> =
        harness.app.store.with(inventory::stored_devices).unwrap().into_iter().map(|device| device.target).collect();
    let expected: BTreeSet<String> = TARGETS.iter().map(|target| target.to_string()).collect();
    assert_eq!(stored, expected, "every device of the old house, by its old target");
    for target in TARGETS.iter().filter(|target| **target != "vm150") {
        assert!(listed.contains_key(*target), "{target} is listed: {listed:?}");
    }
    assert_eq!(listed["bulb1"], "Desk lamp", "a name from the site file");
    assert_eq!(listed["tv"], "Living room TV");
    assert_eq!(listed["vm101"], "Storage", "a name the old hub had");
    assert_eq!(listed["ac-0a1b2c3d-1111-2222-3333-444455556666"], "Lounge air");
    assert_eq!(listed["wiz-a8bb50aaaaaa"], "Hall lamp");

    // Rooms, their scenes, favourites and pinned rooms; the router is gone from each.
    let rooms = harness.get("/api/v1/rooms", &cookie).await;
    rooms.assert_ok();
    let members = |id: &str| -> Vec<String> {
        let room = rooms.body["rooms"].as_array().unwrap().iter().find(|room| room["id"] == json!(id)).unwrap();
        room["devices"].as_array().unwrap().iter().map(|d| d["target"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(members("r-living"), ["bulb1", "tv", "wiz-a8bb50aaaaaa"], "{}", rooms.text);
    assert_eq!(members("r-office"), ["vm101", "plug-bf0123456789abcdef"], "{}", rooms.text);
    assert!(rooms.text.contains("Reading"), "the room scene: {}", rooms.text);
    let me = harness.get("/api/v1/me", &cookie).await;
    assert_eq!(me.body["preferences"]["favourites"], json!(["bulb1", "evening", "vm100"]), "{}", me.text);
    assert_eq!(me.body["preferences"]["pinned_rooms"], json!(["r-living"]), "{}", me.text);

    // The database: scenes, notes, phrases, the router's rows, the sessions, the
    // instances' configs with the site file laid over them, and the audit row.
    harness
        .app
        .store
        .with(|conn| -> rusqlite::Result<()> {
            let evening = scenes::get(conn, "evening")?.unwrap();
            assert_eq!((evening.percent, evening.hue), (40, 34));
            let overrides: Vec<(&str, u8)> = evening.overrides.iter().map(|o| (o.target.as_str(), o.percent)).collect();
            assert_eq!(overrides, [("bulb3", 0), ("bulb4", 25)]);
            let movie = scenes::get(conn, "movie")?.unwrap();
            assert_eq!(movie.extras, json!({ "tv_input": "HDMI 2", "volume_cap": 25 }));
            assert_eq!((movie.name.as_str(), movie.percent), ("Movie", 10), "keys the site file left out are kept");
            assert_eq!(notes::get(conn, "vm101")?, ["The shared folders go offline while this is down."]);
            assert_eq!(notes::get(conn, "tv")?, ["Turning it off rarely confirms."]);
            let phrases: Vec<(String, String)> = conn
                .prepare("SELECT id, action FROM learned_phrases ORDER BY id")?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            assert_eq!(phrases, [("p-quiet".to_string(), "speaker.volume".to_string())]);
            for table in ["favourites", "room_devices", "device_names", "device_notes", "learned_phrases"] {
                let count: i64 =
                    conn.query_row(&format!("SELECT COUNT(*) FROM {table} WHERE target = 'router'"), [], |row| {
                        row.get(0)
                    })?;
                assert_eq!(count, 0, "{table} names the router");
            }
            let open: i64 =
                conn.query_row("SELECT COUNT(*) FROM sessions WHERE end_reason = 'upgrade'", [], |row| row.get(0))?;
            assert!(open >= 1, "the old sessions ended with reason upgrade");
            let memory: String = conn.query_row("SELECT text FROM memories WHERE id = 'm-1'", [], |row| row.get(0))?;
            assert_eq!(memory, "Likes the lights warm.");

            let specs = inventory::instance_specs(conn)?;
            let config = |id: &str| specs.iter().find(|spec| spec.id == id).unwrap().config.clone();
            assert_eq!(config("toshiba")["client_suffix"], json!("6f6c64687562"), "the Toshiba suffix is pinned");
            assert_eq!(config("toshiba")["units"].as_array().unwrap().len(), 2, "the old units are kept");
            assert_eq!(config("webos")["wol_mac"], json!("02:00:00:00:00:01"), "a key the site file sets");
            assert_eq!(config("webos")["cert_sha256"], json!("ab".repeat(32)), "a key it leaves out is kept");
            assert_eq!(config("sonos")["speakers"].as_array().unwrap().len(), 2);
            assert_eq!(config("petkit")["commands"], json!(true), "feeding and cleaning stay as they were");
            assert!(config("proxmox")["host_files_dir"].as_str().unwrap().ends_with("hosts"));

            let migrated: Vec<_> =
                audit::read(conn, 100, None)?.into_iter().filter(|row| row.action == "system.migrate").collect();
            assert_eq!(migrated.len(), 1, "one audit row for the migration");
            assert_eq!((migrated[0].channel.as_str(), migrated[0].session_ref.as_str()), ("cli", "local"));
            assert_eq!(migrated[0].seq, old.checkpoint.seq + 1, "right after the old head");
            assert_eq!(migrated[0].params["checkpoint"], json!(checkpoint));
            let router_rows: i64 =
                conn.query_row("SELECT COUNT(*) FROM audit_log WHERE target = 'router'", [], |row| row.get(0))?;
            assert_eq!(router_rows, 1, "audit rows are never removed");
            Ok(())
        })
        .unwrap();
    let key = AuditKey::load(&etc.join("audit.key")).unwrap();
    let options = VerifyOptions { key: Some(&key), expected: Some(&old.checkpoint), allow_empty: false };
    let report = harness.app.store.with(|conn| audit::verify_with(conn, options)).unwrap();
    assert!(report.ok && report.rows > old.checkpoint.seq, "{report:?}");
    drop(harness);

    // The site file goes to an empty install and gives the same house.
    let export = old.at("export.toml");
    let new_config = old.new_config().display().to_string();
    assert_success(&swifty(&["--config", &new_config, "config", "export", "--out", &export.display().to_string()]));
    let exported = SiteFile::load(&export).unwrap();
    assert!(exported.hub.is_none(), "an export carries no [hub]");
    let exported_text = std::fs::read_to_string(&export).unwrap();
    for (_, value) in SECRET_FILES {
        for line in value.lines() {
            assert!(!exported_text.contains(line), "a credential is in the site file");
        }
    }
    let fresh = tempfile::tempdir().unwrap();
    let fresh_config = fresh.path().join("etc").join("swifty.toml").display().to_string();
    let fresh_data = fresh.path().join("data").display().to_string();
    assert_success(&swifty(&[
        "--config",
        &fresh_config,
        "init",
        "--yes",
        "--data-dir",
        &fresh_data,
        "--origin",
        "https://home.example.org",
    ]));
    let imported = swifty(&["--config", &fresh_config, "config", "import", &export.display().to_string()]);
    assert_success(&imported);
    assert!(text(&imported.stdout).contains("integrations added:"), "{}", text(&imported.stdout));
    let again = swifty(&["--config", &fresh_config, "config", "export"]);
    assert_success(&again);
    let again = SiteFile::parse(&text(&again.stdout)).unwrap();
    assert_eq!(again.instances, exported.instances, "the same instances");
    assert_eq!(again.devices, exported.devices, "the same names and notes");
    assert_eq!(again.scenes, exported.scenes, "the same scenes");
    assert_eq!(old.old_files(), before, "still nothing changed under the old directories");
}

#[tokio::test]
async fn migrate_refuses_while_the_old_hub_still_answers() {
    let old = old_install().await;
    let before = old.old_files();
    let busy = std::net::TcpListener::bind(&old.listen).unwrap();
    let site = old.site();
    for more in [vec!["--site", site.as_str(), "--dry-run"], vec!["--site", site.as_str()]] {
        let refused = old.migrate(&more);
        assert!(!refused.status.success());
        let said = text(&refused.stderr);
        assert!(said.contains("still answers on") && said.contains("systemctl stop glimdock-home"), "{said}");
    }
    drop(busy);
    assert!(!old.new_config().parent().unwrap().exists() && !old.new_data().exists());
    assert_eq!(old.old_files(), before);
}

#[tokio::test]
async fn migrate_refuses_a_target_that_already_holds_a_database_and_changes_nothing() {
    let old = old_install().await;
    let before = old.old_files();
    std::fs::create_dir_all(old.new_data()).unwrap();
    std::fs::write(old.new_data().join("home.db"), b"another hub").unwrap();
    let refused = old.migrate(&["--site", &old.site()]);
    assert!(!refused.status.success());
    assert!(text(&refused.stderr).contains("already holds a database"), "{}", text(&refused.stderr));
    assert_eq!(std::fs::read(old.new_data().join("home.db")).unwrap(), b"another hub");
    assert!(!old.new_config().exists());

    std::fs::remove_file(old.new_data().join("home.db")).unwrap();
    std::fs::create_dir_all(old.new_config().parent().unwrap()).unwrap();
    std::fs::write(old.new_config(), b"# someone's").unwrap();
    let refused = old.migrate(&["--site", &old.site()]);
    assert!(text(&refused.stderr).contains("already exists"), "{}", text(&refused.stderr));
    assert_eq!(std::fs::read(old.new_config()).unwrap(), b"# someone's");
    assert_eq!(std::fs::read_dir(old.new_data()).unwrap().count(), 0, "nothing was written");
    assert_eq!(old.old_files(), before);
}

#[tokio::test]
async fn what_glimdock_home_compiled_in_must_be_pinned_by_the_site_file() {
    let old = old_install().await;
    let refused = old.migrate(&["--dry-run"]);
    assert!(!refused.status.success());
    assert!(text(&refused.stderr).contains("Toshiba cloud"), "{}", text(&refused.stderr));

    let without_agent = SITE.replace("session_prefix = \"oldhub\"\n", "");
    std::fs::write(old.at("partial.toml"), without_agent).unwrap();
    let refused = old.migrate(&["--site", &old.at("partial.toml").display().to_string(), "--dry-run"]);
    let said = text(&refused.stderr);
    assert!(!refused.status.success() && said.contains("session_prefix is not pinned"), "{said}");

    // A header name must be a header name: the value is refused, not written.
    let bad_header = SITE.replace("x-old-client", "Old-Client");
    std::fs::write(old.at("bad.toml"), bad_header).unwrap();
    let refused = old.migrate(&["--site", &old.at("bad.toml").display().to_string()]);
    let said = text(&refused.stderr);
    assert!(!refused.status.success() && said.contains("client_header"), "{said}");
    assert!(!old.new_config().parent().unwrap().exists() && !old.new_data().exists());
}

#[tokio::test]
async fn an_old_audit_log_that_does_not_verify_stops_everything() {
    let old = old_install().await;
    {
        let conn = Connection::open(old.at("old-data/home.db")).unwrap();
        conn.execute_batch(
            "DROP TRIGGER audit_log_no_update;
             UPDATE audit_log SET target = 'vm101' WHERE seq = 2;",
        )
        .unwrap();
    }
    let before = old.old_files();
    let refused = old.migrate(&["--site", &old.site()]);
    assert!(!refused.status.success());
    assert!(text(&refused.stderr).contains("does not verify"), "{}", text(&refused.stderr));
    assert!(!old.new_config().parent().unwrap().exists() && !old.new_data().exists());
    assert_eq!(old.old_files(), before);
}

#[tokio::test]
async fn a_database_the_old_hub_may_still_be_writing_is_refused() {
    let old = old_install().await;
    std::fs::write(old.at("old-data/home.db-wal"), b"frames the database does not have yet").unwrap();
    let before = old.old_files();
    let refused = old.migrate(&["--site", &old.site(), "--dry-run"]);
    assert!(!refused.status.success());
    assert!(text(&refused.stderr).contains("still running, or did not stop cleanly"), "{}", text(&refused.stderr));
    assert_eq!(old.old_files(), before);
}

#[tokio::test]
async fn the_migrated_database_opens_as_the_service_would_open_it() {
    // The copy is a database of its own: the store opens it at schema 4 without
    // another upgrade, and it holds the same people as the old one.
    let old = old_install().await;
    assert_success(&old.migrate(&["--site", &old.site()]));
    let store = Store::open_for_inspection(&old.new_data().join("home.db")).unwrap();
    let (version, people): (i64, i64) = store
        .with(|conn| {
            Ok::<_, rusqlite::Error>((
                conn.query_row("PRAGMA user_version", [], |row| row.get(0))?,
                conn.query_row("SELECT COUNT(*) FROM credentials", [], |row| row.get(0))?,
            ))
        })
        .unwrap();
    assert_eq!((version, people), (swifty_hub::store::SCHEMA_VERSION, 1));
    let secrets = FileSecretStore::new(old.new_data().join("secrets"));
    let slots = secrets.slots("tuya").unwrap();
    assert_eq!(slots.into_iter().collect::<Vec<_>>(), ["local_key/bf0123456789abcde2", "local_key/bf0123456789abcdef"]);
}
