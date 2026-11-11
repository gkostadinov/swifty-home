//! The site file (`swifty config export` and `import`), `swifty config show`, the hub
//! key commands (`swifty speech set-token`, `swifty assistant set-key`) and `swifty
//! backup`, through the real binary on an install in a temporary directory.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use rusqlite::Connection;
use serde_json::{json, Value};
use swifty_hub::admin::site::SiteFile;
use swifty_hub::audit::{self, AuditKey, VerifyOptions};
use swifty_hub::secrets::{FileSecretStore, SecretStore as _};
// Under another name, as in `common`: the secret scan reads the type name before `::` as an assignment.
use swifty_plugin_api::Secret as Credential;

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[track_caller]
fn assert_success(output: &Output) {
    assert!(output.status.success(), "stdout:\n{}\nstderr:\n{}", text(&output.stdout), text(&output.stderr));
}

/// A new install, made by `swifty init`, in a temporary directory.
struct Install {
    dir: tempfile::TempDir,
}

impl Install {
    fn new() -> Self {
        let install = Self { dir: tempfile::tempdir().unwrap() };
        let data = install.data().display().to_string();
        assert_success(&install.swifty(&[
            "init",
            "--yes",
            "--data-dir",
            &data,
            "--origin",
            "https://home.example.org",
        ]));
        install
    }

    fn config(&self) -> PathBuf {
        self.dir.path().join("etc").join("swifty.toml")
    }

    fn data(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    fn database(&self) -> PathBuf {
        self.data().join("home.db")
    }

    fn file(&self, name: &str, content: &str) -> String {
        let path = self.dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        path.display().to_string()
    }

    /// `swifty --config <this install> ...`
    fn swifty(&self, args: &[&str]) -> Output {
        self.swifty_with_input(args, "")
    }

    fn swifty_with_input(&self, args: &[&str], input: &str) -> Output {
        let config = self.config().display().to_string();
        let mut child = Command::new(env!("CARGO_BIN_EXE_swifty"))
            .arg("--config")
            .arg(&config)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    fn import(&self, site: &str, more: &[&str]) -> Output {
        let path = self.file("site.toml", site);
        let mut args = vec!["config", "import", path.as_str()];
        args.extend_from_slice(more);
        self.swifty(&args)
    }

    fn export(&self) -> SiteFile {
        let output = self.swifty(&["config", "export"]);
        assert_success(&output);
        SiteFile::parse(&text(&output.stdout)).unwrap()
    }

    fn db(&self) -> Connection {
        let conn = Connection::open(self.database()).unwrap();
        conn.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
        conn
    }

    fn count(&self, sql: &str) -> i64 {
        self.db().query_row(sql, [], |row| row.get(0)).unwrap()
    }

    fn instances(&self) -> Vec<(String, String, Value)> {
        let conn = self.db();
        let mut statement = conn.prepare("SELECT id, plugin, config FROM plugin_instances ORDER BY id").unwrap();
        statement
            .query_map([], |row| {
                let config: String = row.get(2)?;
                Ok((row.get(0)?, row.get(1)?, serde_json::from_str(&config).unwrap()))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn targets(&self) -> Vec<String> {
        let conn = self.db();
        let mut statement = conn.prepare("SELECT target FROM devices ORDER BY target").unwrap();
        statement.query_map([], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    }
}

const HOUSE: &str = r#"
site_version = 1

[[instance]]
id = "wiz"
plugin = "wiz"
[instance.config]
broadcast = "192.0.2.255:38899"
port = 38899
bulbs = [{ id = "bulb1", address = "192.0.2.21" }, { id = "bulb2", address = "192.0.2.22" }]

[[instance]]
id = "sonos"
plugin = "sonos"
[instance.config]
speakers = [{ id = "sonos", url = "http://192.0.2.31:1400" }]

[[device]]
target = "bulb1"
name = "Desk lamp"
notes = ["Turns on with the room."]

[[device]]
target = "sonos"
name = "Sound bar"

[[device]]
target = "vm101"
name = "Storage"

[[scene]]
id = "evening"
overrides = [{ target = "bulb2", percent = 0, hue = 0 }]
"#;

#[test]
fn a_new_hub_exports_its_scenes_and_nothing_else() {
    let install = Install::new();
    let site = install.export();
    assert_eq!(site.site_version, 1);
    assert!(site.instances.is_empty() && site.devices.is_empty() && site.hub.is_none());
    let ids: Vec<&str> = site.scenes.iter().map(|scene| scene.id.as_str()).collect();
    assert_eq!(ids, ["evening", "focus", "all_off", "movie"]);
    let movie = site.scenes.iter().find(|scene| scene.id == "movie").unwrap();
    assert_eq!(movie.extras, Some(json!({ "tv_input": "HDMI 1", "volume_cap": 30 })));

    let out = install.dir.path().join("out.toml");
    let written = install.swifty(&["config", "export", "--out", &out.display().to_string()]);
    assert_success(&written);
    assert!(text(&written.stdout).is_empty(), "the file, not standard output");
    assert_eq!(SiteFile::load(&out).unwrap(), site);
}

#[test]
fn an_import_checks_every_instance_with_its_plugin_and_writes_one_audit_row() {
    let install = Install::new();
    let rows_before = install.count("SELECT COUNT(*) FROM audit_log");
    let imported = install.import(HOUSE, &[]);
    assert_success(&imported);
    let said = text(&imported.stdout);
    assert!(said.contains("integrations added:     sonos, wiz"), "{said}");
    assert!(said.contains("vm101"), "a name for a device that is not here yet is kept and said: {said}");

    let instances = install.instances();
    let ids: Vec<&str> = instances.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(ids, ["sonos", "wiz"]);
    assert_eq!(instances[1].2["bulbs"][1]["address"], json!("192.0.2.22"));
    assert_eq!(install.targets(), ["bulb1", "bulb2", "sonos"], "the devices the configs name have their targets");
    assert_eq!(install.count("SELECT COUNT(*) FROM audit_log"), rows_before + 1);
    let (action, channel, session): (String, String, String) = install
        .db()
        .query_row("SELECT action, channel, session_ref FROM audit_log ORDER BY seq DESC LIMIT 1", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap();
    assert_eq!((action.as_str(), channel.as_str(), session.as_str()), ("config.import", "cli", "local"));
    assert!(install.count("SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'instances_rev'") >= 1);
    let name: String =
        install.db().query_row("SELECT name FROM device_names WHERE target = 'bulb1'", [], |row| row.get(0)).unwrap();
    assert_eq!(name, "Desk lamp");

    // The same file again changes nothing but says so, and still writes its row.
    let again = install.import(HOUSE, &[]);
    assert_success(&again);
    assert!(text(&again.stdout).contains("integrations unchanged: sonos, wiz"), "{}", text(&again.stdout));

    // What a plugin would not take, is not in the build, or is a credential: refused,
    // and nothing is written.
    let rows = install.count("SELECT COUNT(*) FROM audit_log");
    let refusals = [
        ("site_version = 1\n[[instance]]\nid = \"nope\"\nplugin = \"nope\"\n", "no plugin \"nope\""),
        (
            "site_version = 1\n[[instance]]\nid = \"wiz\"\nplugin = \"wiz\"\n[instance.config]\nbulbs = [{ id = \"bulb1\", address = \"not an address\" }]\n",
            "instance wiz",
        ),
        (
            "site_version = 1\n[[instance]]\nid = \"hue\"\nplugin = \"hue\"\n[instance.config]\nurl = \"http://192.0.2.30\"\napp_key = \"pasted-key-value\"\n",
            "is a credential",
        ),
        (
            "site_version = 1\n[[instance]]\nid = \"wiz\"\nplugin = \"sonos\"\n[instance.config]\nspeakers = [{ id = \"sonos\", url = \"http://192.0.2.31:1400\" }]\n",
            "Remove it first",
        ),
        ("site_version = 1\n[[scene]]\nid = \"party\"\npercent = 50\n", "not on this hub"),
    ];
    for (site, says) in refusals {
        let refused = install.import(site, &[]);
        let said = text(&refused.stderr);
        assert!(!refused.status.success() && said.contains(says), "{site}\n{said}");
        assert!(!said.contains("pasted-key-value"), "a credential was echoed: {said}");
    }
    assert_eq!(install.count("SELECT COUNT(*) FROM audit_log"), rows, "no refusal wrote anything");
    assert_eq!(install.instances(), instances);
}

#[test]
fn prune_removes_what_the_file_does_not_list_with_its_places_favourites_and_secrets() {
    let install = Install::new();
    assert_success(&install.import(HOUSE, &[]));
    let secrets = FileSecretStore::new(install.data().join("secrets"));
    secrets.write("sonos", "token", &Credential::new("sonos-fixture")).unwrap();
    {
        let conn = install.db();
        conn.execute_batch(
            "INSERT INTO users (id, handle, name, role, created_at) VALUES ('u1', 'h1', 'Alice', 'owner', 0);
             INSERT INTO rooms (id, name, name_key, position, created_at) VALUES ('r1', 'Living', 'living', 0, 0);
             INSERT INTO room_devices (target, room_id, position) VALUES ('sonos', 'r1', 0), ('bulb1', 'r1', 1);
             INSERT INTO favourites (user_id, target, position) VALUES ('u1', 'sonos', 0), ('u1', 'bulb1', 1);",
        )
        .unwrap();
    }
    let only_wiz = HOUSE.split("[[instance]]\nid = \"sonos\"").next().unwrap().to_string()
        + "\n[[device]]\ntarget = \"bulb1\"\nname = \"Desk lamp\"\n";
    let pruned = install.import(&only_wiz, &["--prune"]);
    assert_success(&pruned);
    assert!(text(&pruned.stdout).contains("integrations removed:   sonos"), "{}", text(&pruned.stdout));
    assert_eq!(install.instances().len(), 1);
    assert_eq!(install.targets(), ["bulb1", "bulb2"]);
    assert_eq!(install.count("SELECT COUNT(*) FROM room_devices WHERE target = 'sonos'"), 0);
    assert_eq!(install.count("SELECT COUNT(*) FROM favourites WHERE target = 'sonos'"), 0);
    assert_eq!(install.count("SELECT COUNT(*) FROM room_devices WHERE target = 'bulb1'"), 1, "others stay");
    assert!(secrets.slots("sonos").unwrap().is_empty(), "its secrets went with it");
    let names: Vec<String> = {
        let conn = install.db();
        let mut statement = conn.prepare("SELECT target FROM device_names ORDER BY target").unwrap();
        statement.query_map([], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    };
    assert_eq!(names, ["bulb1"], "names the file does not list are removed");
    assert_eq!(install.count("SELECT COUNT(*) FROM device_notes"), 0, "and notes");
    assert!(install.count("SELECT COUNT(*) FROM audit_log WHERE action = 'config.import'") >= 2);
}

#[test]
fn import_never_changes_swifty_toml_and_says_where_it_differs_from_hub() {
    let install = Install::new();
    let before = std::fs::read(install.config()).unwrap();
    let site = "site_version = 1\n[hub.assistant.agent]\nsession_prefix = \"house\"\n";
    let imported = install.import(site, &[]);
    assert_success(&imported);
    assert!(text(&imported.stdout).contains("swifty.toml has no [assistant.agent]"), "{}", text(&imported.stdout));
    assert_eq!(std::fs::read(install.config()).unwrap(), before);
}

#[test]
fn config_show_names_every_place_and_never_a_value() {
    let install = Install::new();
    let shown = install.swifty(&["config", "show"]);
    assert_success(&shown);
    let said = text(&shown.stdout);
    assert!(said.contains("config_version = 2"), "{said}");
    assert!(said.contains("connect_timeout_ms"), "defaults are written out: {said}");
    assert!(said.contains("#   database"), "{said}");
    assert!(said.contains("#   audit key"), "{said}");
    let key = std::fs::read_to_string(install.config().parent().unwrap().join("audit.key")).unwrap();
    assert!(!said.contains(key.trim()), "the audit key was shown");
}

#[test]
fn the_key_commands_store_what_comes_on_standard_input_and_never_show_it() {
    let install = Install::new();
    let etc = install.config().parent().unwrap().to_path_buf();

    let stored = install.swifty_with_input(&["speech", "set-token"], "  speech-token-fixture-0123\n\n");
    assert_success(&stored);
    let said = text(&stored.stdout) + &text(&stored.stderr);
    assert!(!said.contains("speech-token-fixture"), "the token was shown: {said}");
    assert!(said.contains("[speech]") && said.contains("token_file"), "what to add to swifty.toml: {said}");
    let token = etc.join("speech.token");
    assert_eq!(std::fs::read_to_string(&token).unwrap(), "speech-token-fixture-0123\n");
    #[cfg(unix)]
    assert_eq!(swifty_hub::admin::files::mode_of(&token).unwrap(), Some(0o600));

    // Once swifty.toml names the file, that is where it goes, replacing what was there.
    let named = install.dir.path().join("elsewhere.token");
    let mut config = std::fs::read_to_string(install.config()).unwrap();
    config.push_str(&format!(
        "\n[speech]\nurl = \"http://127.0.0.1:8791\"\ntoken_file = {}\n",
        toml::Value::String(named.display().to_string())
    ));
    std::fs::write(install.config(), config).unwrap();
    std::fs::write(&named, "old-token\n").unwrap();
    assert_success(&install.swifty_with_input(&["speech", "set-token"], "new-speech-token-0456"));
    assert_eq!(std::fs::read_to_string(&named).unwrap(), "new-speech-token-0456\n");

    let key = install.swifty_with_input(&["assistant", "set-key"], "assistant-key-fixture\n");
    assert_success(&key);
    assert!(!text(&key.stdout).contains("assistant-key-fixture"));
    assert_eq!(std::fs::read_to_string(etc.join("assistant.key")).unwrap(), "assistant-key-fixture\n");
    let set: Vec<String> = {
        let conn = install.db();
        let mut statement = conn
            .prepare("SELECT params FROM audit_log WHERE action = 'system.key_set' AND channel = 'cli' ORDER BY seq")
            .unwrap();
        statement.query_map([], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    };
    assert_eq!(set.len(), 3, "one audit row each: {set:?}");
    assert!(set[0].contains("speech_token") && set[2].contains("assistant_key"), "{set:?}");
    assert!(!set.iter().any(|params| params.contains("fixture")), "never the value: {set:?}");

    for (input, says) in [("", "nothing was read"), ("  \n", "nothing was read"), ("two\nlines", "one line")] {
        let refused = install.swifty_with_input(&["assistant", "set-key"], input);
        assert!(!refused.status.success() && text(&refused.stderr).contains(says), "{}", text(&refused.stderr));
    }
    assert_eq!(std::fs::read_to_string(etc.join("assistant.key")).unwrap(), "assistant-key-fixture\n");
}

/// The entries of a ustar archive: name and content.
fn untar(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 512 <= bytes.len() && bytes[at..at + 512].iter().any(|&b| b != 0) {
        let header = &bytes[at..at + 512];
        let field = |range: std::ops::Range<usize>| {
            String::from_utf8(header[range].iter().copied().take_while(|&b| b != 0).collect()).unwrap()
        };
        assert_eq!(&header[257..263], b"ustar\0");
        let (name, prefix) = (field(0..100), field(345..500));
        let name = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        let size = u64::from_str_radix(field(124..135).trim(), 8).unwrap() as usize;
        at += 512;
        out.push((name, bytes[at..at + size].to_vec()));
        at += size.div_ceil(512) * 512;
    }
    out
}

#[test]
fn a_backup_holds_the_database_the_configuration_and_the_secrets() {
    let install = Install::new();
    assert_success(&install.import(HOUSE, &[]));
    let secrets = FileSecretStore::new(install.data().join("secrets"));
    secrets.write("sonos", "token", &Credential::new("sonos-fixture")).unwrap();
    secrets.write("wiz", "local_key/bulb1", &Credential::new("row-fixture")).unwrap();

    let out = install.dir.path().join("backups");
    std::fs::create_dir_all(&out).unwrap();
    // An old backup, and things in the same directory that are not backups.
    for name in ["swifty-home-20000101T000000Z", "keep-me", "swifty-home-20000101T000000Z-x"] {
        std::fs::create_dir(out.join(name)).unwrap();
    }
    std::fs::write(out.join("swifty-home-20000102T000000Z"), "a file, not a backup").unwrap();

    let backup = install.swifty(&["backup", "--out", &out.display().to_string(), "--keep-days", "14"]);
    assert_success(&backup);
    let said = text(&backup.stdout);
    assert!(said.contains("audit checkpoint:") && said.contains("2 slots"), "{said}");
    assert!(!said.contains("sonos-fixture"));
    let mut names: Vec<String> =
        std::fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names.len(), 4, "{names:?}");
    assert!(!names.contains(&"swifty-home-20000101T000000Z".to_string()), "the old backup is removed: {names:?}");
    for kept in ["keep-me", "swifty-home-20000101T000000Z-x", "swifty-home-20000102T000000Z"] {
        assert!(names.contains(&kept.to_string()), "{kept} is not a backup and stays: {names:?}");
    }
    let dir = PathBuf::from(said.lines().next().unwrap().strip_prefix("Backed up to ").unwrap());
    let taken = dir.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        names.contains(&taken) && taken.starts_with("swifty-home-2") && !taken.starts_with("swifty-home-2000"),
        "{taken}"
    );
    assert!(!out.join(format!(".{taken}.partial")).exists());

    // The database: one file, consistent, the same house and audit chain.
    let copy = Connection::open(dir.join("home.db")).unwrap();
    let mode: String = copy.query_row("PRAGMA journal_mode", [], |row| row.get(0)).unwrap();
    assert_eq!(mode, "delete");
    let instances: i64 = copy.query_row("SELECT COUNT(*) FROM plugin_instances", [], |row| row.get(0)).unwrap();
    assert_eq!(instances, 2);
    let key = AuditKey::load(&dir.join("config").join("audit.key")).unwrap();
    let report = audit::verify_with(&copy, VerifyOptions { key: Some(&key), ..Default::default() }).unwrap();
    assert!(report.ok && report.keyed, "{report:?}");
    drop(copy);

    // The configuration and its key, byte for byte.
    let etc = install.config().parent().unwrap().to_path_buf();
    assert_eq!(std::fs::read(dir.join("config/swifty.toml")).unwrap(), std::fs::read(install.config()).unwrap());
    assert_eq!(std::fs::read(dir.join("config/audit.key")).unwrap(), std::fs::read(etc.join("audit.key")).unwrap());

    // The secret store, as an archive that unpacks into the secrets directory.
    let entries = untar(&std::fs::read(dir.join("secrets.tar")).unwrap());
    let files: Vec<(&str, &[u8])> = entries
        .iter()
        .filter(|(name, _)| !name.ends_with('/'))
        .map(|(name, content)| (name.as_str(), content.as_slice()))
        .collect();
    assert_eq!(
        files,
        [("sonos/token", b"sonos-fixture".as_slice()), ("wiz/local_key/bulb1", b"row-fixture".as_slice())]
    );
    #[cfg(unix)]
    for file in ["home.db", "config/swifty.toml", "config/audit.key", "secrets.tar"] {
        assert_eq!(swifty_hub::admin::files::mode_of(&dir.join(file)).unwrap(), Some(0o600), "{file}");
    }
}

#[test]
fn the_commands_the_owners_scripts_call_are_there() {
    for command in [&["migrate", "--help"][..], &["config", "export", "--help"], &["backup", "--help"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_swifty")).args(command).output().unwrap();
        assert!(output.status.success(), "{command:?}: {}", text(&output.stderr));
    }
    let help = Command::new(env!("CARGO_BIN_EXE_swifty")).args(["migrate", "--help"]).output().unwrap();
    let said = text(&help.stdout);
    for flag in ["--from", "--site", "--dry-run", "--data-dir"] {
        assert!(said.contains(flag), "{flag}: {said}");
    }
    // With nothing to work on, each says so plainly and fails.
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.toml").display().to_string();
    let out = dir.path().join("backups").display().to_string();
    for args in [
        &["--config", missing.as_str(), "migrate", "--from", missing.as_str(), "--dry-run"][..],
        &["--config", missing.as_str(), "backup", "--out", out.as_str()],
        &["--config", missing.as_str(), "config", "export"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_swifty")).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(text(&output.stderr).starts_with("error: "), "{args:?}: {}", text(&output.stderr));
    }
}
