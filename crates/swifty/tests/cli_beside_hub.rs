//! The commands that write the database or a key beside a running hub: `config
//! import`, `speech set-token` and `assistant set-key` (and what they share with
//! `plugin secret set`, which `cli_plugin.rs` covers). Each runs as the real binary
//! while a hub of the same build serves the same database, and none may disturb it:
//!
//! - no run the hub is waiting on is closed (that is what the hub's own start-up
//!   does, and the command line must never do it: only `swifty serve` builds an `App`
//!   with `App::new`);
//! - an instance whose configuration a command changed is connected anew within
//!   seconds, one it did not change keeps its driver, and a hub key (read when the
//!   hub starts, not an instance's secret) reconnects nothing;
//! - the audit log is still one chain that verifies, with the command's row in it.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use serde_json::{json, Value};
use swifty_hub::background;
use swifty_hub::config::Config;
use swifty_plugin_mock::MockDevices;

/// How soon a running hub must show what the command line changed.
const HUB_NOTICES_WITHIN: Duration = Duration::from_secs(3);

/// Long enough for the hub's once-a-second look at the database to have happened.
const A_LOOK_AT_THE_DATABASE: Duration = Duration::from_millis(2_200);

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

/// A live hub's configuration file on disk, with its database beside it.
fn config_file(dir: &Path) -> PathBuf {
    let path = dir.join("swifty.toml");
    std::fs::write(&path, config_text(&dir.join("home.db"), "live = true")).unwrap();
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

/// Waits until `check` holds, for at most [`HUB_NOTICES_WITHIN`].
async fn within_seconds(what: &str, mut check: impl FnMut() -> bool) {
    let started = Instant::now();
    while !check() {
        assert!(started.elapsed() < HUB_NOTICES_WITHIN, "the running hub did not notice in time: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A command the hub sent and is still waiting to see confirmed.
fn wait_on_a_device(harness: &Harness) {
    harness
        .app
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
}

fn run_phase(harness: &Harness) -> String {
    harness
        .app
        .store
        .with(|conn| conn.query_row("SELECT phase FROM action_runs WHERE id = 'run-1'", [], |row| row.get(0)))
        .unwrap()
}

/// The revision of the stored instances, which a hub in another process watches.
fn instances_rev(harness: &Harness) -> i64 {
    harness.app.store.with(swifty_hub::inventory::instances_rev).unwrap()
}

/// What `plugin_instances.updated_at` says for an instance.
fn updated_at(harness: &Harness, id: &str) -> i64 {
    harness
        .app
        .store
        .with(|conn| conn.query_row("SELECT updated_at FROM plugin_instances WHERE id = ?1", [id], |row| row.get(0)))
        .unwrap()
}

/// Every audit row of an action: (result, channel, session, params).
fn audit_rows(harness: &Harness, action: &str) -> Vec<(String, String, String, String)> {
    harness
        .app
        .store
        .with(|conn| {
            let mut statement = conn
                .prepare("SELECT result, channel, session_ref, params FROM audit_log WHERE action = ?1 ORDER BY seq")?;
            let rows = statement.query_map([action], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap()
}

/// The targets of one instance the hub lists to its owner.
async fn devices_of(harness: &Harness, cookie: &str, instance: &str) -> Vec<String> {
    let response = harness.get("/api/v1/devices", cookie).await;
    response.assert_ok();
    response.body["devices"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|device| device["instance"] == json!(instance))
        .map(|device| device["target"].as_str().unwrap().to_string())
        .collect()
}

/// `swifty audit verify` as an operator runs it: the report must say the chain is whole.
fn the_chain_verifies(config: &Path) -> Value {
    let report: Value = serde_json::from_str(&succeeded(&swifty(config, &["audit", "verify"], ""))).unwrap();
    assert_eq!(report["ok"], json!(true), "{report}");
    report
}

fn site_with(lamps: &[&str]) -> String {
    let devices: Vec<String> = lamps.iter().map(|id| format!("{{ id = \"{id}\", class = \"light\" }}")).collect();
    format!(
        "site_version = 1\n\n[[instance]]\nid = \"gadgets\"\nplugin = \"mock\"\n[instance.config]\ndevices = [{}]\n",
        devices.join(", ")
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_config_import_beside_a_live_hub_connects_what_it_changed_and_closes_no_run() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_file(dir.path());
    let site = dir.path().join("site.toml");
    let harness = running_hub(dir, &config);
    harness.add_instance("gadgets", "mock", json!({ "devices": [{ "id": "lamp", "class": "light" }] }));
    harness.add_instance("others", "mock", json!({ "devices": [{ "id": "fan", "class": "light" }] }));
    let (_owner, cookie) = harness.sign_in().await;
    wait_on_a_device(&harness);
    let driver_of =
        |id: &str| harness.app.registry.instance(id).unwrap().driver.clone().expect("a live instance is connected");
    let (gadgets, others) = (driver_of("gadgets"), driver_of("others"));
    assert_eq!(devices_of(&harness, &cookie, "gadgets").await.len(), 1);
    let stamp = updated_at(&harness, "gadgets");

    // The site file gives gadgets another lamp. `others` is not in it, and is not pruned.
    std::fs::write(&site, site_with(&["lamp", "lamp2"])).unwrap();
    let said = succeeded(&swifty_async(&config, &["config", "import", site.to_str().unwrap()], "").await);
    assert!(said.contains("gadgets"), "{said}");
    within_seconds("the changed instance is connected anew", || !Arc::ptr_eq(&gadgets, &driver_of("gadgets"))).await;
    let started = Instant::now();
    while devices_of(&harness, &cookie, "gadgets").await.len() < 2 {
        assert!(started.elapsed() < HUB_NOTICES_WITHIN, "the running hub never listed the new lamp");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(updated_at(&harness, "gadgets") > stamp, "the instance's own revision moved forward");
    assert!(Arc::ptr_eq(&others, &driver_of("others")), "an instance the import did not change keeps its driver");

    // Importing the same file again changes nothing: no reconnect, and no spurious revision.
    let settled = driver_of("gadgets");
    let stamp = updated_at(&harness, "gadgets");
    succeeded(&swifty_async(&config, &["config", "import", site.to_str().unwrap()], "").await);
    tokio::time::sleep(A_LOOK_AT_THE_DATABASE).await;
    assert!(Arc::ptr_eq(&settled, &driver_of("gadgets")), "nothing changed, so nothing was connected anew");
    assert_eq!(updated_at(&harness, "gadgets"), stamp);

    assert_eq!(run_phase(&harness), "commanded", "the command line closed none of the hub's runs");
    let rows = audit_rows(&harness, "config.import");
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .all(|(result, channel, session, _)| (result.as_str(), channel.as_str(), session.as_str())
            == ("ok", "cli", "local")));
    the_chain_verifies(&config);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_hub_keys_are_set_beside_a_live_hub_without_touching_its_instances_or_runs() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_file(dir.path());
    let harness = running_hub(dir, &config);
    harness.add_instance("gadgets", "mock", json!({ "devices": [{ "id": "lamp", "class": "light" }] }));
    wait_on_a_device(&harness);
    let driver = || harness.app.registry.instance("gadgets").unwrap().driver.clone().expect("connected");
    let before = driver();
    let stamp = updated_at(&harness, "gadgets");
    let revision = instances_rev(&harness);

    let set = succeeded(&swifty_async(&config, &["speech", "set-token"], "speech-value-0123456789\n").await);
    assert!(set.contains("speech"), "{set}");
    let set = succeeded(&swifty_async(&config, &["assistant", "set-key"], "assistant-value-0123456789\r\n").await);
    assert!(set.contains("assistant") || set.contains("API key"), "{set}");
    tokio::time::sleep(A_LOOK_AT_THE_DATABASE).await;

    // Both are read when the hub starts: they are not an instance's secret, so nothing reconnects.
    assert!(Arc::ptr_eq(&before, &driver()), "a hub key reconnects no instance");
    assert_eq!(updated_at(&harness, "gadgets"), stamp);
    assert_eq!(instances_rev(&harness), revision, "and moves no revision of the instances");
    assert_eq!(run_phase(&harness), "commanded", "the command line closed none of the hub's runs");

    let rows = audit_rows(&harness, "system.key_set");
    assert_eq!(rows.len(), 2);
    let named: Vec<Value> = rows.iter().map(|(_, _, _, params)| serde_json::from_str(params).unwrap()).collect();
    assert_eq!(named, [json!({ "key": "speech_token" }), json!({ "key": "assistant_key" })]);
    assert!(rows
        .iter()
        .all(|(result, channel, session, _)| (result.as_str(), channel.as_str(), session.as_str())
            == ("ok", "cli", "local")));
    let stored = ["audit_log", "plugin_instances", "meta"].iter().map(|table| harness.dump(table)).collect::<String>();
    assert!(!stored.contains("speech-value-0123456789") && !stored.contains("assistant-value-0123456789"));
    the_chain_verifies(&config);
}

/// Only `swifty serve` builds the hub's `App` with `App::new`, which closes the runs that were waiting
/// for a device ("the service restarted"). Every command, and everything under `admin/` and `migrate/`
/// that the commands call, goes through the store or `App::for_command_line`.
#[test]
fn only_the_service_builds_the_hubs_app_the_way_that_closes_its_runs() {
    fn rust_files(dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_files(&path, found);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                found.push(path);
            }
        }
    }
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for source in ["swifty/src", "swifty-hub/src"] {
        rust_files(&crates.join(source), &mut files);
    }
    assert!(files.len() > 50, "the scan found the sources ({} files)", files.len());
    let needle = concat!("App::", "new(");
    let mut callers: Vec<String> = files
        .iter()
        .filter(|path| std::fs::read_to_string(path).unwrap().contains(needle))
        .map(|path| path.strip_prefix(&crates).unwrap().to_string_lossy().replace('\\', "/"))
        .collect();
    callers.sort();
    assert_eq!(callers, ["swifty/src/main.rs"], "something besides `swifty serve` builds the hub's App with {needle}");
}
