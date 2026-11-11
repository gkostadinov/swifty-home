//! The command line's plugin commands (`swifty plugin list|info|add|setup|remove|
//! secret set`, `swifty device list`, `swifty go-live|go-dry`, `swifty init --demo`):
//! through the real binary beside a hub that runs on the same database, and the
//! terminal renderer of every kind of setup step through a scripted terminal.

mod common;

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use common::*;
use serde_json::{json, Value};
use swifty::cli::plugin::{opener, remove_with, run_setup_with, Start};
use swifty::cli::terminal::Script;
use swifty::cli::wizard::Answers;
use swifty_hub::config::Config;
use swifty_hub::secrets::{FileSecretStore, SecretStore};
use swifty_hub::{background, inventory};
use swifty_net::LiveGrant;
use swifty_plugin_api::{
    Capability, Choice, ConfigProblem, DeviceClass, DeviceDecl, Driver, Field, FieldType, InstanceView, LiveContext,
    Manifest, Op, Plugin, PluginError, Reach, Readiness, Scalar, SecretSlot, SetupContext, SetupFlow, SetupInput,
    SetupResult, SetupStep, API_VERSION,
};
use swifty_plugin_mock::MockDevices;

/// How soon a running hub must show what the command line changed.
const HUB_NOTICES_WITHIN: Duration = Duration::from_secs(3);

// ----- the binary

/// Runs `swifty --config <config> <args>` with `input` on standard input.
fn swifty(config: &Path, args: &[&str], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_swifty"))
        .arg("--config")
        .arg(config)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The command may finish before it reads everything; that is its business.
    let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
    child.wait_with_output().unwrap()
}

/// As [`swifty`], off the async test's own threads.
async fn swifty_async(config: &Path, args: &[&str], input: &str) -> Output {
    let (config, input) = (config.to_path_buf(), input.to_string());
    let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    tokio::task::spawn_blocking(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        swifty(&config, &args, &input)
    })
    .await
    .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[track_caller]
fn succeeded(output: &Output) -> String {
    assert!(output.status.success(), "stdout: {}\nstderr: {}", text(&output.stdout), text(&output.stderr));
    text(&output.stdout)
}

/// A hub's configuration file on disk, with its database beside it.
fn config_file(dir: &Path, more: &str) -> PathBuf {
    let path = dir.join("swifty.toml");
    std::fs::write(&path, config_text(&dir.join("home.db"), more)).unwrap();
    path
}

/// A hub as `swifty serve` runs it, on the configuration at `path`: every plugin of
/// the build, the secret store on disk, and the watch for changes another process
/// makes. The test drives it through its router.
fn running_hub(dir: tempfile::TempDir, path: &Path) -> Harness {
    let config = Config::load(path).unwrap();
    let registry = production_registry(&config);
    let harness = Harness::build(dir, config, registry, MockDevices::new());
    background::watch_instances(harness.app.clone());
    harness
}

/// Waits until `check` holds, for at most [`HUB_NOTICES_WITHIN`]. Returns how long it took.
async fn within_seconds(what: &str, mut check: impl FnMut() -> bool) -> Duration {
    let started = Instant::now();
    loop {
        if check() {
            return started.elapsed();
        }
        assert!(started.elapsed() < HUB_NOTICES_WITHIN, "the running hub did not notice in time: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The targets the hub lists to its owner, with their instance.
async fn listed(harness: &Harness, cookie: &str) -> Vec<(String, String)> {
    let response = harness.get("/api/v1/devices", cookie).await;
    response.assert_ok();
    response.body["devices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|device| {
            (device["target"].as_str().unwrap().to_string(), device["instance"].as_str().unwrap().to_string())
        })
        .collect()
}

/// Every audit row of an action: (result, channel, session, device, params).
fn audit_rows(harness: &Harness, action: &str) -> Vec<(String, String, String, String, String)> {
    harness
        .app
        .store
        .with(|conn| {
            let mut statement = conn.prepare(
                "SELECT result, channel, session_ref, device, params FROM audit_log WHERE action = ?1 ORDER BY seq",
            )?;
            let rows = statement
                .query_map([action], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap()
}

/// Everything the database holds, as text, to look for a value that must not be there.
fn everything_stored(harness: &Harness) -> String {
    ["audit_log", "plugin_instances", "devices", "meta"].iter().map(|table| harness.dump(table)).collect()
}

// ----- the acceptance: a scripted session beside a running hub

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scripted_terminal_session_adds_the_mock_and_the_running_hub_lists_it_within_seconds() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_file(dir.path(), "");
    let harness = running_hub(dir, &config);
    let (_owner, cookie) = harness.sign_in().await;
    assert!(listed(&harness, &cookie).await.iter().all(|(_, instance)| instance != "mock"));

    // Enter keeps the ticked demo devices; then an access code, typed in.
    let added = swifty_async(&config, &["plugin", "add", "mock"], "\nopen-sesame-4711\n").await;
    let shown = succeeded(&added);
    assert!(shown.contains("Add Mock"), "{shown}");
    assert!(shown.contains("The new integration is called mock."), "{shown}");
    assert!(shown.contains("[x] Floor lamp"), "{shown}");
    assert!(shown.contains("Access code (hidden, optional): "), "{shown}");
    assert!(shown.contains("Done. 8 demo devices added."), "{shown}");
    assert!(!shown.contains("open-sesame-4711"), "a secret is never shown back: {shown}");

    let started = Instant::now();
    let devices = loop {
        let devices: Vec<(String, String)> =
            listed(&harness, &cookie).await.into_iter().filter(|(_, instance)| instance == "mock").collect();
        if devices.len() == 8 {
            break devices;
        }
        assert!(started.elapsed() < HUB_NOTICES_WITHIN, "the running hub lists {devices:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(started.elapsed() < HUB_NOTICES_WITHIN);
    assert!(devices.iter().any(|(target, _)| target == "mock-lamp"), "{devices:?}");

    // The setup is audited as the command line's: channel cli, session local, the
    // Unix user as the device; the slot is named, its value is nowhere.
    let setup = audit_rows(&harness, "plugin.setup");
    let results: Vec<&str> = setup.iter().map(|row| row.0.as_str()).collect();
    assert_eq!(results, ["opened", "done"]);
    for (_, channel, session, device, _) in &setup {
        assert_eq!((channel.as_str(), session.as_str()), ("cli", "local"));
        assert!(!device.is_empty() && device != "-");
    }
    assert!(setup[1].4.contains("access_code") && setup[1].4.contains("\"devices\":8"), "{}", setup[1].4);
    assert!(!everything_stored(&harness).contains("open-sesame-4711"));
    let slots = harness.app.registry.secret_store().slots("mock").unwrap();
    assert_eq!(slots, BTreeSet::from(["access_code".to_string()]));

    // `plugin list` and `device list` read the same files.
    let list = succeeded(&swifty(&config, &["plugin", "list"], ""));
    assert!(
        list.lines()
            .any(|line| line.contains("mock") && line.contains("Mock") && line.contains("8") && line.contains("ready")),
        "{list}"
    );
    let devices = succeeded(&swifty(&config, &["device", "list"], ""));
    assert!(
        devices.lines().any(|line| line.contains("mock-lamp") && line.contains("Floor lamp") && line.contains("light")),
        "{devices}"
    );

    // Removing it asks first; no answer removes nothing.
    let kept = swifty_async(&config, &["plugin", "remove", "mock"], "").await;
    assert!(!kept.status.success());
    assert!(text(&kept.stdout).contains("Its 8 devices go"), "{}", text(&kept.stdout));
    assert!(text(&kept.stderr).starts_with("error: nothing was removed"), "{}", text(&kept.stderr));
    let declined = swifty_async(&config, &["plugin", "remove", "mock"], "n\n").await;
    assert!(!declined.status.success());
    assert!(text(&declined.stdout).contains("Nothing was removed."));
    assert_eq!(listed(&harness, &cookie).await.iter().filter(|(_, instance)| instance == "mock").count(), 8);

    // Then it goes, with its secret, and the running hub lets go of it.
    let removed = succeeded(&swifty_async(&config, &["plugin", "remove", "mock"], "y\n").await);
    assert!(removed.contains("Removed mock: 8 devices"), "{removed}");
    within_seconds("the removed devices", || {
        harness.app.registry.index().all().all(|device| device.instance != "mock")
    })
    .await;
    assert!(harness.app.registry.secret_store().slots("mock").unwrap().is_empty());
    let remove = audit_rows(&harness, "plugin.remove");
    assert_eq!(remove.len(), 1);
    assert_eq!((remove[0].0.as_str(), remove[0].1.as_str()), ("done", "cli"));
}

#[test]
fn plugin_info_hue_json_is_the_manifest() {
    let dir = tempfile::tempdir().unwrap();
    // No hub here: a plugin is described all the same.
    let output = swifty(&dir.path().join("missing.toml"), &["plugin", "info", "hue", "--json"], "");
    let shown: Value = serde_json::from_str(&succeeded(&output)).unwrap();
    let hue = swifty::plugins::compiled_in().into_iter().find(|plugin| plugin.manifest().id == "hue").unwrap();
    let manifest = serde_json::to_value(hue.manifest()).unwrap();
    for (key, value) in manifest.as_object().unwrap() {
        if key == "actions" {
            // Each action as declared, with the tier it really has beside it.
            let actions = shown["actions"].as_array().unwrap();
            assert_eq!(actions.len(), value.as_array().unwrap().len());
            for (declared, shown) in value.as_array().unwrap().iter().zip(actions) {
                for (field, field_value) in declared.as_object().unwrap() {
                    assert_eq!(&shown[field], field_value, "actions.{field}");
                }
                assert!(shown.get("tier_effective").is_some());
            }
        } else {
            assert_eq!(&shown[key], value, "{key}");
        }
    }
    assert_eq!(shown["setup"], true);
    assert_eq!(shown["can_add"], true);
    assert_eq!(shown["instances"], json!([]));
    assert_eq!(shown["reach_text"], json!(swifty_hub::plugin_actions::reach_text(hue)));

    // And as text, the same facts for a person.
    let text_output = succeeded(&swifty(&dir.path().join("missing.toml"), &["plugin", "info", "hue"], ""));
    assert!(text_output.starts_with(&format!("{} (hue), version {}", hue.manifest().name, hue.manifest().version)));
    for slot in hue.manifest().secrets {
        assert!(text_output.contains(slot.slot), "{text_output}");
    }
    let unknown = swifty(&dir.path().join("missing.toml"), &["plugin", "info", "nothing-like-it"], "");
    assert!(!unknown.status.success());
    assert!(text(&unknown.stderr).contains("swifty plugin list --available"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_json_of_the_command_line_is_what_the_api_answers() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_file(dir.path(), "");
    seed_house(&dir.path().join("home.db"), &legacy_house());
    let harness = running_hub(dir, &config);
    let (_owner, cookie) = harness.sign_in().await;

    let api = harness.get("/api/v1/plugins", &cookie).await;
    api.assert_ok();
    let cli: Value = serde_json::from_str(&succeeded(&swifty(&config, &["plugin", "list", "--json"], ""))).unwrap();
    assert_eq!(cli, api.body);

    for plugin in ["hue", "mock", "toshiba"] {
        let api = harness.get(&format!("/api/v1/plugins/{plugin}"), &cookie).await;
        api.assert_ok();
        let cli: Value =
            serde_json::from_str(&succeeded(&swifty(&config, &["plugin", "info", plugin, "--json"], ""))).unwrap();
        assert_eq!(cli, api.body, "{plugin}");
    }

    // The device list is the API's, each marked whether it waits for approval.
    let api = harness.get("/api/v1/devices", &cookie).await;
    let cli: Value = serde_json::from_str(&succeeded(&swifty(&config, &["device", "list", "--json"], ""))).unwrap();
    let mut marked = api.body.clone();
    for device in marked["devices"].as_array_mut().unwrap() {
        device["pending"] = json!(false);
    }
    assert_eq!(cli, marked);
}

// ----- secrets set from the command line reach the running hub

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_secret_set_beside_a_live_hub_connects_its_instance_anew() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_file(dir.path(), "live = true");
    let harness = running_hub(dir, &config);
    harness.add_instance("gadgets", "mock", json!({ "devices": [{ "id": "lamp", "class": "light" }] }));
    let driver =
        || harness.app.registry.instance("gadgets").unwrap().driver.clone().expect("a live instance is connected");
    let first = driver();
    // Loading the instances again with nothing changed keeps the driver.
    harness.app.sync_directory().unwrap();
    assert!(Arc::ptr_eq(&first, &driver()));

    let stored = succeeded(
        &swifty_async(&config, &["plugin", "secret", "set", "gadgets", "access_code"], "first-value-1\n").await,
    );
    assert!(stored.contains("Stored access_code for gadgets."), "{stored}");
    within_seconds("the first secret", || !Arc::ptr_eq(&first, &driver())).await;
    let second = driver();

    // The same slot again, with another value: nothing the hub could see changed but
    // the instance's own revision, and that is enough.
    succeeded(
        &swifty_async(&config, &["plugin", "secret", "set", "gadgets", "access_code"], "second-value-2\r\n").await,
    );
    within_seconds("the new value of the same slot", || !Arc::ptr_eq(&second, &driver())).await;

    let grant = LiveGrant::when_live(true).unwrap();
    let store = FileSecretStore::new(harness.app.config.secrets_path());
    let value = store.read(grant, "gadgets", "access_code").unwrap().unwrap();
    assert_eq!(value.expose(), "second-value-2", "the value is stored without its line ending");

    let rows = audit_rows(&harness, "plugin.secret");
    assert_eq!(rows.len(), 2);
    for (result, channel, session, _, params) in &rows {
        assert_eq!((result.as_str(), channel.as_str(), session.as_str()), ("done", "cli", "local"));
        assert!(params.contains("\"slot\":\"access_code\""), "{params}");
    }
    let everything = everything_stored(&harness);
    assert!(!everything.contains("first-value-1") && !everything.contains("second-value-2"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_setup_run_again_from_the_command_line_connects_its_instance_anew() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_file(dir.path(), "live = true");
    let harness = running_hub(dir, &config);
    succeeded(&swifty_async(&config, &["plugin", "add", "mock", "--yes"], "").await);
    within_seconds("the new instance", || harness.app.registry.instance("mock").is_some_and(|i| i.driver.is_some()))
        .await;
    let before = harness.app.registry.instance("mock").unwrap().driver.clone().unwrap();

    // The same devices and a new access code: the config is the same, the slot set
    // is the same; only the code changed.
    succeeded(&swifty_async(&config, &["plugin", "setup", "mock"], "\nfirst-code-1\n").await);
    within_seconds("the first code", || {
        !Arc::ptr_eq(&before, harness.app.registry.instance("mock").unwrap().driver.as_ref().unwrap())
    })
    .await;
    let between = harness.app.registry.instance("mock").unwrap().driver.clone().unwrap();
    let again = succeeded(&swifty_async(&config, &["plugin", "setup", "mock"], "\nsecond-code-2\n").await);
    assert!(again.contains("Set up mock again"), "{again}");
    // It knows a code is stored: the plugin offers to keep it.
    assert!(again.contains("Leave it empty to keep"), "{again}");
    within_seconds("the second code", || {
        !Arc::ptr_eq(&between, harness.app.registry.instance("mock").unwrap().driver.as_ref().unwrap())
    })
    .await;
}

#[test]
fn a_secret_is_refused_for_what_is_not_there() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_file(dir.path(), "");
    let harness = Harness::build(
        tempfile::tempdir().unwrap(),
        Config::load(&config).unwrap(),
        production_registry(&Config::load(&config).unwrap()),
        MockDevices::new(),
    );
    harness.add_instance("gadgets", "mock", json!({ "devices": [] }));
    for (args, input, says) in [
        (vec!["plugin", "secret", "set", "nothere", "access_code"], "x\n", "there is no integration nothere"),
        (vec!["plugin", "secret", "set", "gadgets", "app_key"], "x\n", "has no secret slot app_key"),
        (vec!["plugin", "secret", "set", "gadgets", "access_code"], "", "no value was given"),
        (vec!["plugin", "secret", "set", "gadgets", "access_code"], "\n", "no value was given"),
    ] {
        let output = swifty(&config, &args, input);
        assert!(!output.status.success(), "{args:?}");
        assert!(text(&output.stderr).contains(says), "{args:?}: {}", text(&output.stderr));
    }
    assert!(harness.app.registry.secret_store().slots("gadgets").unwrap().is_empty());
    assert!(audit_rows(&harness, "plugin.secret").is_empty());
}

#[test]
fn a_setup_with_nobody_to_answer_stores_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_file(dir.path(), "");
    let harness = Harness::build(
        tempfile::tempdir().unwrap(),
        Config::load(&config).unwrap(),
        production_registry(&Config::load(&config).unwrap()),
        MockDevices::new(),
    );
    // What `sudo swifty plugin add mock < /dev/null` does: a clean refusal.
    let output = swifty(&config, &["plugin", "add", "mock"], "");
    assert!(!output.status.success());
    let error = text(&output.stderr);
    assert!(error.starts_with("error: standard input ended"), "{error}");
    assert!(error.contains("Nothing was stored."), "{error}");
    // A secret is never taken from the command line.
    let output = swifty(&config, &["plugin", "add", "mock", "--set", "access_code=visible-to-all"], "\n");
    assert!(text(&output.stderr).contains("--set cannot give access_code: it is a secret"), "{}", text(&output.stderr));
    // A plugin this build lacks, and an id that is taken.
    let output = swifty(&config, &["plugin", "add", "nothing-like-it"], "");
    assert!(text(&output.stderr).contains("no plugin nothing-like-it"));
    succeeded(&swifty(&config, &["plugin", "add", "mock", "--yes"], ""));
    let output = swifty(&config, &["plugin", "add", "mock", "--instance", "mock"], "");
    assert!(text(&output.stderr).contains("swifty plugin setup mock"), "{}", text(&output.stderr));

    let rows = audit_rows(&harness, "plugin.setup");
    let results: Vec<&str> = rows.iter().map(|row| row.0.as_str()).collect();
    assert_eq!(results, ["opened", "abandoned", "opened", "abandoned", "opened", "done"]);
    assert!(!everything_stored(&harness).contains("visible-to-all"));
}

#[test]
fn the_command_lines_app_leaves_the_running_hubs_runs_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = config_file(dir.path(), "");
    let config = Config::load(&path).unwrap();
    let hub =
        Harness::build(tempfile::tempdir().unwrap(), config.clone(), production_registry(&config), MockDevices::new());
    // A command the hub sent and is still waiting to see confirmed.
    hub.app
        .store
        .with(|conn| {
            conn.execute(
                "INSERT INTO action_runs (id, action, target, params, dedupe_key, session_ref, credential_id, device,
                                          channel, step_up, phase, started_at)
                 VALUES ('run-1', 'light.power', 'bulb1', '{}', 'k', 's', 'c', 'd', 'panel', 'none', 'commanded', 1)",
                [],
            )
        })
        .unwrap();
    let phase = || -> String {
        hub.app
            .store
            .with(|conn| conn.query_row("SELECT phase FROM action_runs WHERE id = 'run-1'", [], |row| row.get(0)))
            .unwrap()
    };
    let clock: Arc<dyn swifty_hub::clock::Clock> = Arc::new(swifty_hub::clock::SystemClock);
    let beside = swifty_hub::app::App::for_command_line(config.clone(), clock.clone(), production_registry(&config));
    drop(beside.unwrap());
    assert_eq!(phase(), "commanded", "the command line closes none of the hub's runs");
    // What the hub does when it starts again, and only then.
    drop(swifty_hub::app::App::new(config.clone(), clock, production_registry(&config)).unwrap());
    assert_eq!(phase(), "no_response");
}

// ----- the demo home, the first run and the mode

#[test]
fn init_with_the_demo_home_lists_its_devices_at_once_and_prints_the_link() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("etc").join("swifty.toml");
    let data = dir.path().join("data");
    let output = swifty(
        &config,
        &["init", "--yes", "--demo", "--origin", "http://localhost:8790", "--data-dir", data.to_str().unwrap()],
        "",
    );
    let shown = succeeded(&output);
    assert!(shown.contains("the demo home: 8 pretend devices"), "{shown}");
    assert!(shown.contains("added   the integration mock with 8 devices"), "{shown}");
    let token = shown
        .lines()
        .map(str::trim)
        .find(|line| line.len() == 43 && line.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
        .expect("the token on a line of its own");
    assert!(shown.contains(&format!("http://localhost:8790/#enroll={token}")), "{shown}");

    let devices = succeeded(&swifty(&config, &["device", "list"], ""));
    for name in
        ["Floor lamp", "Ceiling light", "Kettle plug", "Speaker", "TV", "Air conditioner", "Cat feeder", "Gadget"]
    {
        assert!(devices.contains(name), "{name}: {devices}");
    }
    let plugins = succeeded(&swifty(&config, &["plugin", "list"], ""));
    assert!(plugins.lines().any(|line| line.trim_start().starts_with("mock") && line.ends_with("ready")), "{plugins}");
    // The first run's row and the demo home's setup row, signed with the new key.
    let audit: Value = serde_json::from_str(&succeeded(&swifty(&config, &["audit", "verify"], ""))).unwrap();
    assert_eq!(
        (audit["ok"].clone(), audit["rows"].clone(), audit["keyed"].clone()),
        (json!(true), json!(2), json!(true))
    );
}

#[test]
fn go_live_and_go_dry_change_one_line_and_say_to_restart() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("etc").join("swifty.toml");
    let data = dir.path().join("data");
    succeeded(&swifty(&config, &["init", "--yes", "--data-dir", data.to_str().unwrap()], ""));
    let written = std::fs::read_to_string(&config).unwrap();

    let live = succeeded(&swifty(&config, &["go-live"], ""));
    assert!(live.contains("Swifty Home is set to live mode"), "{live}");
    assert!(live.contains("sudo systemctl restart swifty-home"), "{live}");
    let now = std::fs::read_to_string(&config).unwrap();
    assert_eq!(now, written.replace("\nlive = false\n", "\nlive = true\n"));
    succeeded(&swifty(&config, &["config", "check", "--file", config.to_str().unwrap()], ""));
    let again = succeeded(&swifty(&config, &["go-live"], ""));
    assert!(again.contains("already. Nothing changed."), "{again}");

    let dry = succeeded(&swifty(&config, &["go-dry"], ""));
    assert!(dry.contains("dry-run"), "{dry}");
    assert_eq!(std::fs::read_to_string(&config).unwrap(), written, "back to the file init wrote, byte for byte");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&config).unwrap().permissions().mode() & 0o777, 0o640);
    }
    // Both changes are in the audit log, as the command line's.
    let store = swifty_hub::store::Store::open_for_inspection(&data.join("home.db")).unwrap();
    let modes: Vec<(String, String)> = store
        .with(|conn| {
            let mut statement =
                conn.prepare("SELECT params, channel FROM audit_log WHERE action = 'system.mode' ORDER BY seq")?;
            let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap();
    assert_eq!(modes.len(), 2);
    assert!(modes[0].0.contains("true") && modes[1].0.contains("false"));
    assert!(modes.iter().all(|(_, channel)| channel == "cli"));
    let missing = swifty(&dir.path().join("none.toml"), &["go-live"], "");
    assert!(text(&missing.stderr).contains("there is no configuration"), "{}", text(&missing.stderr));
}

// ----- every kind of step, in a scripted terminal

/// How often the pretend dial was polled, per instance.
static POLLS: Mutex<Vec<(String, u32)>> = Mutex::new(Vec::new());

const DIAL_KEY: &str = "key";

static DIALS_MANIFEST: Manifest = Manifest {
    id: "dials",
    name: "Dials",
    version: "0.1.0",
    api: API_VERSION,
    summary: "Pretend dials that only exist in this test.",
    reach: Reach::Local,
    network: &[],
    classes: &[DeviceClass::Other],
    config_version: 1,
    config: &[],
    secrets: &[SecretSlot { slot: DIAL_KEY, label: "Key", help: "Any text.", required: true }],
    actions: &[],
    vocabulary: &[],
    nouns: &[],
    multi_instance: true,
};

struct Dials;

static DIALS: Dials = Dials;

fn dial(id: &str) -> DeviceDecl {
    DeviceDecl {
        local_id: id.to_string(),
        target_hint: None,
        class: DeviceClass::Other,
        name: format!("Dial {id}"),
        capabilities: vec![Capability::Power],
        attrs: json!({}),
    }
}

impl Plugin for Dials {
    fn manifest(&self) -> &'static Manifest {
        &DIALS_MANIFEST
    }
    fn validate(&self, config: &Value) -> Result<(), ConfigProblem> {
        match config.get("name").and_then(Value::as_str) {
            Some(name) if !name.is_empty() => Ok(()),
            _ => Err(ConfigProblem { field: "name".into(), message: "must be given".into() }),
        }
    }
    fn readiness(&self, instance: &InstanceView<'_>) -> Readiness {
        if instance.secrets_present.contains(DIAL_KEY) {
            Readiness::Ready
        } else {
            Readiness::NeedsSetup("No key yet.".into())
        }
    }
    fn devices(&self, instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
        instance
            .adopted
            .iter()
            .map(|record| DeviceDecl { name: record.name.clone(), ..dial(&record.local_id) })
            .collect()
    }
    fn refusal(&self, _instance: &InstanceView<'_>, _device: &str, _op: &Op) -> Option<String> {
        None
    }
    fn connect(&self, _ctx: LiveContext) -> Result<Arc<dyn Driver>, PluginError> {
        Err(PluginError("The dials are pretend.".into()))
    }
    fn setup(&self) -> Option<Box<dyn SetupFlow>> {
        Some(Box::<DialSetup>::default())
    }
}

#[derive(Default, PartialEq)]
enum Stage {
    #[default]
    External,
    Form,
    Wait,
    Choose,
}

#[derive(Default)]
struct DialSetup {
    stage: Stage,
    config: Value,
    typed: String,
}

impl DialSetup {
    fn external() -> SetupStep {
        SetupStep::External {
            title: "Make a key".into(),
            text: "Make a key for the dials on the maker's page, then come back.".into(),
            link: Some("https://example.org/keys".into()),
            fields: Vec::new(),
        }
    }

    fn form() -> SetupStep {
        let field = |key: &str, kind: FieldType, required: bool, value: Option<Scalar>| Field {
            key: key.into(),
            label: key.to_uppercase(),
            help: format!("Help for {key}."),
            kind,
            required,
            value,
        };
        SetupStep::Form {
            title: "The dials".into(),
            text: "Where they are, and the key.".into(),
            fields: vec![
                field("name", FieldType::Text { max_len: 16 }, true, None),
                field("host", FieldType::Host, true, Some(Scalar::Text("192.0.2.40".into()))),
                field("port", FieldType::Port, false, Some(Scalar::Int(8080))),
                field("level", FieldType::Int { min: 1, max: 5 }, true, None),
                field("region", FieldType::Choice { options: vec!["eu".into(), "us".into()] }, false, None),
                field("night", FieldType::Bool, false, None),
                field("mac", FieldType::Mac, false, None),
                field("note", FieldType::Multiline { max_len: 200 }, false, None),
                field("key", FieldType::Secret, true, None),
            ],
        }
    }

    fn wait() -> SetupStep {
        SetupStep::Wait { title: "Turn a dial".into(), text: "Turn any dial once.".into(), poll_ms: 250, timeout_s: 30 }
    }

    fn choose() -> SetupStep {
        SetupStep::Choose {
            title: "Which dials?".into(),
            text: "Tick the dials to add.".into(),
            options: ["a", "b", "c"]
                .iter()
                .map(|id| Choice {
                    id: id.to_string(),
                    label: format!("Dial {id}"),
                    detail: format!("192.0.2.4{}", id.len()),
                    selected: *id == "a",
                })
                .collect(),
            multiple: true,
            min: 1,
        }
    }
}

#[async_trait]
impl SetupFlow for DialSetup {
    async fn begin(&mut self, _ctx: &SetupContext, _existing: Option<&Value>) -> SetupStep {
        Self::external()
    }

    async fn advance(&mut self, ctx: &SetupContext, input: SetupInput) -> SetupStep {
        match (&self.stage, input) {
            (Stage::External, SetupInput::Values { .. }) => {
                self.stage = Stage::Form;
                Self::form()
            }
            (Stage::Form, SetupInput::Values { values }) => {
                if values.get("name") == Some(&Scalar::Text("taken".into())) {
                    return SetupStep::Failed { reason: "That name is taken.".into(), retry: true };
                }
                self.typed = match values.get("key") {
                    Some(Scalar::Text(key)) => key.clone(),
                    _ => String::new(),
                };
                let mut config = serde_json::Map::new();
                for (key, value) in values.iter().filter(|(key, _)| *key != "key") {
                    config.insert(key.clone(), serde_json::to_value(value).unwrap());
                }
                self.config = Value::Object(config);
                self.stage = Stage::Wait;
                Self::wait()
            }
            (Stage::Wait, SetupInput::Poll) => {
                let mut polls = POLLS.lock().unwrap();
                let count = match polls.iter_mut().find(|(id, _)| *id == ctx.instance_id) {
                    Some((_, count)) => {
                        *count += 1;
                        *count
                    }
                    None => {
                        polls.push((ctx.instance_id.clone(), 1));
                        1
                    }
                };
                if count >= 2 {
                    self.stage = Stage::Choose;
                    Self::choose()
                } else {
                    Self::wait()
                }
            }
            (Stage::Choose, SetupInput::Chosen { ids }) => SetupStep::Done(SetupResult {
                config: self.config.clone(),
                secrets: vec![(DIAL_KEY.to_string(), self.typed.clone().into())],
                adopt: ids.iter().map(|id| dial(id)).collect(),
                summary: format!("Dials ready: {} added.", ids.len()),
            }),
            _ => SetupStep::Failed { reason: "Not now.".into(), retry: false },
        }
    }
}

fn with_dials() -> Harness {
    Harness::with_plugins(vec![&DIALS], |_| {})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_kind_of_step_is_drawn_and_answered_in_the_terminal() {
    let harness = with_dials();
    let cli = opener("tester");
    let mut script = Script::interactive(&[
        "", // the key was made elsewhere: Enter
        // The form, the first time: a name the plugin refuses.
        "taken",
        "",
        "",
        "3",
        "EU",
        "y",
        "",
        "line one",
        "line two",
        "",
        "dial-key-123456",
        "y", // try again
        // The form again: a level out of range is asked for again.
        "Kitchen",
        "",
        "",
        "9",
        "4",
        "",
        "",
        "",
        "",
        "dial-key-123456",
        // The wait needs nothing typed. Then the dials.
        "1 3",
    ]);
    let start = Start::Add { plugin: "dials".to_string(), instance: None };
    let open = Mutex::new(None);
    let summary = run_setup_with(&harness.app, &cli, &mut script, &start, Answers::default(), &open).await.unwrap();
    assert_eq!(summary, "Dials ready: 2 added.");
    assert_eq!(script.left(), 0);
    assert!(open.lock().unwrap().is_none());

    let shown = script.text();
    for expected in [
        "  The new integration is called dials.",
        "Make a key",
        "  Open: https://example.org/keys",
        "  Help for name.",
        "  NAME: taken",
        "  HOST [192.0.2.40]: ",
        "  PORT (1-65535) [8080]: ",
        "  LEVEL (1-5): 3",
        "  REGION (eu, us, optional): EU",
        "  NIGHT? [y/N]: y",
        "  MAC (optional): ",
        "  NOTE (optional): paste it, then an empty line.",
        "  | line one",
        "  KEY (hidden): ",
        "That name is taken.",
        "  Try again? [Y/n]: y",
        "  LEVEL must be a whole number from 1 to 5.",
        "Turn a dial",
        "  Waiting: 0:30 left. Ctrl+C stops; nothing is stored.",
        "Which dials?",
        "     1. [x] Dial a  192.0.2.41",
        "     2. [ ] Dial b  192.0.2.41",
        "  Numbers to pick, such as 1 3, or all (Enter keeps the ticked ones): 1 3",
        "Done. Dials ready: 2 added.",
    ] {
        assert!(script.transcript.iter().any(|line| line == expected), "missing {expected:?} in:\n{shown}");
    }
    assert!(!shown.contains("dial-key-123456"), "the key is never shown:\n{shown}");
    assert_eq!(script.hidden_reads, 2);

    // Stored as the hub stores any finished setup.
    let config: String = harness
        .app
        .store
        .with(|conn| conn.query_row("SELECT config FROM plugin_instances WHERE id = 'dials'", [], |row| row.get(0)))
        .unwrap();
    let config: Value = serde_json::from_str(&config).unwrap();
    // A choice left empty is left out, as the app's form leaves it out.
    assert_eq!(
        config,
        json!({ "name": "Kitchen", "host": "192.0.2.40", "port": 8080, "level": 4, "night": false, "mac": "", "note": "" })
    );
    assert_eq!(harness.app.registry.secret_store().slots("dials").unwrap(), BTreeSet::from([DIAL_KEY.to_string()]));
    let index = harness.app.registry.index();
    let mut targets: Vec<&str> =
        index.all().filter(|device| device.instance == "dials").map(|device| device.target.as_str()).collect();
    targets.sort_unstable();
    assert_eq!(targets, ["dials-a", "dials-c"]);
    let rows = audit_rows(&harness, "plugin.setup");
    assert!(rows.iter().all(|row| row.1 == "cli" && row.3 == "tester"), "{rows:?}");
    assert!(!everything_stored(&harness).contains("dial-key-123456"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn given_ahead_and_taken_as_offered_a_setup_asks_only_for_the_rest() {
    let harness = with_dials();
    let cli = opener("tester");
    // Nobody at the keyboard: one line, for the one field with nothing to offer.
    let mut script = Script::new(&["dial-key-7890"]);
    let answers = Answers::new(&["name=Hall".to_string(), "level=2".to_string()], true).unwrap();
    let start = Start::Add { plugin: "dials".to_string(), instance: Some("hall".to_string()) };
    let summary = run_setup_with(&harness.app, &cli, &mut script, &start, answers, &Mutex::new(None)).await.unwrap();
    assert_eq!(summary, "Dials ready: 1 added.");
    let shown = script.text();
    assert!(shown.contains("  NAME: Hall") && shown.contains("  HOST: 192.0.2.40"), "{shown}");
    assert!(shown.contains("  Picked: Dial a"), "{shown}");
    assert!(!shown.contains("dial-key-7890"));
    assert_eq!(script.left(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_setup_that_runs_out_of_answers_is_abandoned_and_leaves_nothing() {
    let harness = with_dials();
    let cli = opener("tester");
    // The name, then Enter for the address and the port, then nothing more.
    let mut script = Script::new(&["", "Porch", "", ""]);
    let start = Start::Add { plugin: "dials".to_string(), instance: Some("porch".to_string()) };
    let error = run_setup_with(&harness.app, &cli, &mut script, &start, Answers::default(), &Mutex::new(None))
        .await
        .unwrap_err();
    assert!(error.message.contains("standard input ended before the setup asked for LEVEL"), "{}", error.message);
    assert!(error.hint.unwrap().contains("--set level=VALUE"));
    let exists = harness.app.store.with(|conn| inventory::instance_exists(conn, "porch")).unwrap();
    assert!(!exists);
    let results: Vec<String> = audit_rows(&harness, "plugin.setup").into_iter().map(|row| row.0).collect();
    assert_eq!(results, ["opened", "abandoned"]);
    assert_eq!(harness.app.setup.open_count(&harness.app), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_from_the_terminal_lists_what_goes_and_keeps_the_audit_log() {
    let harness = with_dials();
    let cli = opener("tester");
    let mut script = Script::new(&["", "Shed", "", "", "1", "", "", "", "", "k-123456", "all"]);
    let start = Start::Add { plugin: "dials".to_string(), instance: Some("shed".to_string()) };
    run_setup_with(&harness.app, &cli, &mut script, &start, Answers::default(), &Mutex::new(None)).await.unwrap();

    let mut answer_no = Script::new(&["no"]);
    assert!(remove_with(&harness.app, &cli, &mut answer_no, "shed", false).await.unwrap().is_none());
    assert!(answer_no.text().contains("Its 3 devices go: Dial a, Dial b and Dial c."), "{}", answer_no.text());
    assert!(answer_no.text().contains("Its stored secrets go: key."));

    let mut quiet = Script::new(&[]);
    let removed = remove_with(&harness.app, &cli, &mut quiet, "shed", true).await.unwrap().unwrap();
    assert_eq!(removed.devices.len(), 3);
    assert!(harness.app.registry.index().all().all(|device| device.instance != "shed"));
    let unknown = remove_with(&harness.app, &cli, &mut Script::new(&[]), "shed", true).await.unwrap_err();
    assert!(unknown.message.contains("there is no integration shed"));
    // The instance the hub itself runs on cannot go, as in the app.
    let refused = remove_with(&harness.app, &cli, &mut Script::new(&[]), "proxmox", true).await.unwrap_err();
    assert!(refused.message.contains("cannot be removed"), "{}", refused.message);
    assert_eq!(audit_rows(&harness, "plugin.setup").len(), 2, "the setup's rows stay");
}
