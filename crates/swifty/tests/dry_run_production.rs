//! The dry-run guarantee, for the registry the real server starts with: every
//! first-party plugin, each instance pointed at a mock device that counts each
//! connection and datagram (the router stands in as a mock instance, Petkit waits
//! for its sign-in). The whole catalog then runs through its real flows. In dry-run
//! nothing may arrive. The same run with `live = true` proves the counters work.

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::devices::*;
use common::*;
use serde_json::{json, Map, Value};
use std::sync::Arc;
use swifty_hub::app::{App, StartupError};
use swifty_hub::catalog::{self, ActionDef, Executor, Targets, ACK_PARAM, CATALOG};
use swifty_hub::clock::ManualClock;
use swifty_hub::config::Config;
use swifty_hub::policy::{self, Flow};
use swifty_net::LiveGrant;
use swifty_plugin_api::ParamKind;
use swifty_plugin_mock::MockDevices;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;

async fn rig_harness(live: bool, speech: Option<&FakeSpeech>) -> (Rig, Harness, SoftPasskey, String) {
    let rig = Rig::start().await;
    let speech = speech.map(FakeSpeech::config);
    let harness = Harness::with_site(|site| {
        site.live = live;
        site.speech = speech;
        site.timing.confirm_timeout_ms = Some(400);
        site.timing.poll_ms = 5;
        rig.configure(site);
    });
    let mut auth = authenticator();
    harness.enroll(&mut auth, "iPhone").await;
    let cookie = harness.login(&mut auth).await;
    (rig, harness, auth, cookie)
}

fn valid_params(def: &ActionDef, target: &str) -> Value {
    let mut map = Map::new();
    for param in def.params.iter().filter(|p| p.required) {
        let value = match param.kind {
            ParamKind::Bool => json!(false),
            ParamKind::Int { min, .. } => json!(min),
            ParamKind::Enum(values) => json!(values[0]),
            ParamKind::Label { .. } => json!("A name"),
        };
        map.insert(param.name.to_string(), value);
    }
    if catalog::takes_console_offline(def, target, Some(RUNS_ON)) {
        map.insert(ACK_PARAM.to_string(), json!(true));
    }
    Value::Object(map)
}

/// Drives one action through whichever flow its tier requires.
async fn run_action(harness: &Harness, auth: &mut SoftPasskey, cookie: &str, def: &ActionDef, target: &str) -> Resp {
    let params = valid_params(def, target);
    match policy::flow(def.tier) {
        Flow::Direct => harness.execute(cookie, def.id, target, params).await,
        Flow::Confirm => {
            let prepared = harness.prepare(cookie, def.id, target, params).await;
            if !prepared.status.is_success() {
                return prepared;
            }
            harness.confirm_tap(cookie, &prepared).await
        }
        Flow::StepUp => {
            let prepared = harness.prepare(cookie, def.id, target, params).await;
            if !prepared.status.is_success() {
                return prepared;
            }
            let started = harness.step_up_start(cookie, &prepared).await;
            started.assert_ok();
            let assertion = assert_with(auth, &started.body["options"], origin_url());
            harness.confirm_assertion(cookie, &prepared, &assertion).await
        }
    }
}

struct Outcome {
    ran: Vec<(String, String, String)>,
    refused: BTreeSet<(String, String)>,
}

/// Every action on every allowed target that goes to a device or reads one.
async fn drive_the_catalog(harness: &Harness, auth: &mut SoftPasskey, cookie: &str) -> Outcome {
    let mut outcome = Outcome { ran: Vec::new(), refused: BTreeSet::new() };
    let index = harness.app.registry.index();
    for def in CATALOG.iter().filter(|d| matches!(d.executor, Executor::Device | Executor::Read)) {
        assert!(!matches!(def.targets, Targets::Credential | Targets::NewCredential | Targets::User), "{}", def.id);
        for target in def.admitted(&index) {
            let target = target.as_str();
            let response = run_action(harness, auth, cookie, def, target).await;
            if response.code() == "device_unavailable" {
                outcome.refused.insert((def.id.to_string(), target.to_string()));
            } else {
                assert!(response.status.is_success(), "{} {target}: {}", def.id, response.text);
                outcome.ran.push((def.id.to_string(), target.to_string(), response.phase().to_string()));
            }
            // Past the duplicate window, so each action is judged on its own.
            harness.advance(10_001);
        }
    }
    outcome
}

fn expected_refusals() -> BTreeSet<(String, String)> {
    let mut expected: BTreeSet<(String, String)> =
        [("pets.feed", "feeder"), ("pets.clean", "litter"), ("router.wireguard_restart", "router")]
            .iter()
            .map(|(a, t)| (a.to_string(), t.to_string()))
            .collect();
    for vm in ["vm100", "vm101", "vm102", "vm103"] {
        expected.insert(("vm.force_stop".to_string(), vm.to_string()));
    }
    expected
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(150)).await;
}

#[tokio::test]
async fn in_dry_run_the_entire_catalog_reaches_no_device() {
    let (rig, harness, mut auth, cookie) = rig_harness(false, None).await;
    // Every first-party plugin of the build has an instance here.
    let loaded: BTreeSet<&str> =
        harness.app.registry.instances().iter().map(|instance| instance.plugin.manifest().id).collect();
    let built: BTreeSet<&str> = swifty::plugins::compiled_in().iter().map(|plugin| plugin.manifest().id).collect();
    assert_eq!(loaded, built);
    let outcome = drive_the_catalog(&harness, &mut auth, &cookie).await;
    settle().await;

    assert_eq!(rig.traffic(), 0, "dry-run reached a device");
    assert!(outcome.ran.len() > 40, "only {} actions ran", outcome.ran.len());
    for (action, target, phase) in &outcome.ran {
        assert_eq!(phase, "simulated", "{action} on {target}");
    }
    assert_eq!(outcome.refused, expected_refusals());
    assert!(harness.audit_has("simulated"));
    assert!(!harness.audit_has("confirmed"), "dry-run must never claim a device confirmed");
}

#[tokio::test]
async fn in_dry_run_every_way_of_reading_state_reaches_no_device() {
    let (rig, harness, _auth, cookie) = rig_harness(false, None).await;
    for _ in 0..3 {
        let all = harness.get("/api/v1/state", &cookie).await;
        all.assert_ok();
        assert_eq!(all.body["live"], json!(false));
        assert_eq!(all.body["simulated"], json!(true));
        harness.get("/api/v1/devices", &cookie).await.assert_ok();
    }
    let index = harness.app.registry.index();
    assert!(index.len() > 20, "{} devices", index.len());
    for device in index.active() {
        let one = harness.get(&format!("/api/v1/state/{}", device.target), &cookie).await;
        one.assert_ok();
        assert_ne!(one.body["available"], json!(true), "{} claimed real state", device.target);
    }
    settle().await;
    assert_eq!(rig.traffic(), 0, "dry-run reached a device");
}

#[tokio::test]
async fn in_dry_run_voice_reaches_the_speech_service_and_no_device() {
    let speech = FakeSpeech::start().await;
    let (rig, harness, _auth, cookie) = rig_harness(false, Some(&speech)).await;
    for text in [
        "whiz bulb one off",
        "volume 40",
        "louder",
        "clipper status",
        "lgtv hdmi two",
        "lights off",
        "what is the cpu temperature",
        "pause clipper",
        "stop bravo",
        "start charlie",
        "feed the pets",
        "emergency stop",
        "turn off the light",
        "mumble",
    ] {
        speech.say(text);
        post_voice(&harness, &cookie).await.assert_ok();
    }
    settle().await;
    assert_eq!(speech.calls(), 14, "the speech service is not a device and is still used");
    assert_eq!(rig.traffic(), 0, "dry-run reached a device");
    assert!(harness.audit_has("simulated"));
    assert!(!harness.audit_has("confirmed"));
}

#[tokio::test]
async fn in_dry_run_no_audio_is_sent_to_a_speech_service_elsewhere() {
    let speech = FakeSpeech::start().await;
    let rig = Rig::start().await;
    let mut elsewhere = speech.config();
    // A documentation address that reaches nothing. The refusal comes first anyway.
    elsewhere.url = "https://192.0.2.1:9".to_string();
    let harness = Harness::with_site(|site| {
        site.live = false;
        site.speech = Some(elsewhere);
        rig.configure(site);
    });
    let mut auth = authenticator();
    harness.enroll(&mut auth, "iPhone").await;
    let cookie = harness.login(&mut auth).await;

    let refused = post_voice(&harness, &cookie).await;
    refused.assert_refused(axum::http::StatusCode::CONFLICT, "speech_not_local");
    assert_eq!(speech.calls(), 0);
    settle().await;
    assert_eq!(rig.traffic(), 0, "dry-run reached a device");
}

#[tokio::test]
async fn the_same_run_with_live_true_does_reach_every_device() {
    let (rig, harness, mut auth, cookie) = rig_harness(true, None).await;
    let outcome = drive_the_catalog(&harness, &mut auth, &cookie).await;
    harness.get("/api/v1/state", &cookie).await.assert_ok();
    settle().await;

    assert_eq!(outcome.refused, expected_refusals());
    assert!(outcome.ran.iter().any(|(_, _, phase)| phase == "confirmed"));
    assert!(outcome.ran.iter().all(|(_, _, phase)| phase != "simulated"));
    assert!(harness.audit_has("confirmed"));
    for (name, traffic) in [
        ("bulb1", rig.bulbs[0].traffic()),
        ("bulb4", rig.bulbs[3].traffic()),
        ("sonos", rig.sonos.http.traffic()),
        ("sub", rig.sub.http.traffic()),
        ("tv", rig.tv.traffic()),
        ("moonraker", rig.moonraker.http.traffic()),
        ("proxmox", rig.proxmox.http.traffic()),
        ("hue", rig.hue.traffic()),
        ("plex", rig.plex.traffic()),
        ("camera", rig.camera.traffic()),
        ("collector", rig.collector.traffic()),
        ("toshiba", rig.toshiba.traffic()),
    ] {
        assert!(traffic > 0, "the counter for {name} saw nothing, so a zero proves nothing");
    }
}

#[tokio::test]
async fn dry_run_predicts_what_live_would_refuse() {
    let (_rig, harness, mut auth, cookie) = rig_harness(false, None).await;
    let dry = drive_the_catalog(&harness, &mut auth, &cookie).await;
    let (_rig, harness, mut auth, cookie) = rig_harness(true, None).await;
    let live = drive_the_catalog(&harness, &mut auth, &cookie).await;
    assert_eq!(dry.refused, live.refused);
}

#[tokio::test]
async fn dry_run_says_simulated_in_the_clearest_way() {
    let (_rig, harness, _auth, cookie) = rig_harness(false, None).await;
    let done = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    done.assert_ok();
    assert_eq!(done.phase(), "simulated");
    assert_eq!(done.body["simulated"], json!(true));
    assert!(done.body["detail"].as_str().unwrap().contains("no device was contacted"));
    assert_eq!(done.body["steps"][0]["phase"], json!("simulated"));
}

#[tokio::test]
async fn dry_run_with_no_credentials_still_simulates_what_needs_none() {
    let rig = Rig::start().await;
    let harness = Harness::with_site(|site| {
        site.live = false;
        rig.configure(site);
        site.secrets.clear();
    });
    let mut auth = authenticator();
    harness.enroll(&mut auth, "iPhone").await;
    let cookie = harness.login(&mut auth).await;
    for (action, target, params) in
        [("light.power", "bulb1", json!({ "on": true })), ("speaker.volume", "sonos", json!({ "level": 10 }))]
    {
        assert_eq!(harness.execute(&cookie, action, target, params).await.phase(), "simulated");
    }
    // What waits for its credential is off in either mode, and dry-run says why as live would.
    let vm = harness.prepare(&cookie, "vm.start", "vm100", json!({})).await;
    vm.assert_refused(axum::http::StatusCode::CONFLICT, "device_unavailable");
    let tv = harness.execute(&cookie, "tv.input", "tv", json!({ "input": "HDMI 1" })).await;
    tv.assert_refused(axum::http::StatusCode::CONFLICT, "device_unavailable");
    settle().await;
    assert_eq!(rig.traffic(), 0);
}

// ----- the structural guarantees

#[test]
fn a_live_grant_only_comes_from_a_live_configuration() {
    let database = Path::new("x.db");
    let dry = Config::parse(&config_text(database, "")).unwrap();
    assert!(LiveGrant::when_live(dry.live).is_none());
    let off = Config::parse(&config_text(database, "live = false")).unwrap();
    assert!(LiveGrant::when_live(off.live).is_none());
    let live = Config::parse(&config_text(database, "live = true")).unwrap();
    assert!(LiveGrant::when_live(live.live).is_some());
}

#[test]
fn a_live_registry_cannot_start_with_a_dry_run_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("home.db");
    let config = Config::parse(&config_text(&database, "")).unwrap();
    let live_registry = mock_registry(true, &MockDevices::new());
    let clock = Arc::new(ManualClock::new(START_MS));
    let result = App::new(config, clock, live_registry);
    assert!(matches!(result, Err(StartupError::LiveMismatch)));
}

#[test]
fn a_dry_run_registry_is_not_live() {
    let config = Config::parse(&config_text(Path::new("x.db"), "")).unwrap();
    assert!(!production_registry(&config).is_live());
}

fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            source_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Crates in the workspace that only tests use. They are not shipped.
const TEST_ONLY_CRATES: &[&str] = &["swifty-plugin-testkit"];

/// The source of everything that ships: `src/` of every crate under `crates/`
/// and `plugins/` at the workspace root. Tests are not part of it.
fn shipped_source_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for group in ["crates", "plugins"] {
        let Ok(crates) = std::fs::read_dir(root.join(group)) else { continue };
        for entry in crates {
            let path = entry.unwrap().path();
            let test_only = path.file_name().is_some_and(|name| TEST_ONLY_CRATES.iter().any(|t| name == *t));
            if !test_only && path.join("src").is_dir() {
                source_files(&path.join("src"), &mut files);
            }
        }
    }
    for needed in ["swifty-hub", "swifty"] {
        let src = root.join("crates").join(needed).join("src");
        assert!(files.iter().any(|path| path.starts_with(&src)), "nothing found in {}", src.display());
    }
    files
}

/// The grant-guarded socket helpers, from the workspace root.
const NET: &str = "crates/swifty-net/src/lib.rs";

/// Anything that could open a socket. Each is allowed in these files only, named
/// from the workspace root.
const SOCKET_APIS: &[(&str, &[&str])] = &[
    ("UdpSocket", &[NET]),
    ("TcpStream", &[NET]),
    ("TcpListener", &["crates/swifty/src/main.rs"]),
    ("tokio::net", &[NET, "crates/swifty/src/main.rs"]),
    ("UnixStream", &[]),
    ("UnixDatagram", &[]),
    ("socket2", &[]),
    ("lookup_host", &[NET]),
    ("to_socket_addrs", &[]),
    ("client_async", &[NET]),
    ("connect_async", &[]),
    // The speech client talks to the speech service on this machine only.
    ("reqwest::Client", &[NET, "crates/swifty-hub/src/voice.rs"]),
    ("ClientBuilder", &[]),
    ("reqwest::get", &[]),
    ("tungstenite", &[NET]),
];

#[test]
fn only_the_grant_guarded_module_can_open_a_socket() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let files = shipped_source_files();
    assert!(files.len() > 20);
    for path in files {
        let relative = path.strip_prefix(&root).unwrap();
        let name: Vec<&str> = relative.components().map(|part| part.as_os_str().to_str().unwrap()).collect();
        let name = name.join("/");
        let text = std::fs::read_to_string(&path).unwrap();
        for (api, allowed) in SOCKET_APIS {
            if text.contains(api) {
                assert!(allowed.contains(&name.as_str()), "{name} uses {api}");
            }
        }
    }
}

/// Where a grant may be made: the registry, which makes the hub's only
/// one, and the CLI's setup command, which runs a plugin's setup in the terminal.
/// Tests make their own anywhere.
const GRANT_MAKERS: &[&str] = &["crates/swifty-hub/src/runtime/mod.rs", "crates/swifty/src/cli/plugin.rs"];

/// What of each source file ships, by its name from the workspace root: every
/// top-level `#[cfg(test)]` module is left out, whether it is written in the file or
/// in a file of its own. Anything else marked for tests stays in view.
fn shipped_code() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut test_files = Vec::new();
    let mut out = Vec::new();
    for path in shipped_source_files() {
        let text = std::fs::read_to_string(&path).unwrap();
        let mut kept = String::new();
        let mut lines = text.lines();
        while let Some(line) = lines.next() {
            let item = (line == "#[cfg(test)]").then(|| lines.next()).flatten();
            let module = item.map(|item| item.trim_start_matches("pub(crate) ").trim_start_matches("pub "));
            match module.and_then(|item| item.strip_prefix("mod ")) {
                Some(inline) if inline.ends_with('{') => {
                    // rustfmt closes a top-level module with `}` alone on its line.
                    lines.by_ref().find(|inner| *inner == "}");
                }
                Some(declared) => {
                    let name = declared.trim_end_matches(';');
                    let owner = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
                    let dir = match owner {
                        "mod.rs" | "lib.rs" | "main.rs" => path.parent().unwrap().to_path_buf(),
                        _ => path.with_extension(""),
                    };
                    test_files.push(dir.join(format!("{name}.rs")));
                }
                None => {
                    for kept_line in std::iter::once(line).chain(item) {
                        kept.push_str(kept_line);
                        kept.push('\n');
                    }
                }
            }
        }
        out.push((path, kept));
    }
    out.into_iter()
        .filter(|(path, _)| !test_files.contains(path))
        .map(|(path, code)| {
            let name: Vec<&str> =
                path.strip_prefix(&root).unwrap().components().map(|part| part.as_os_str().to_str().unwrap()).collect();
            (name.join("/"), code)
        })
        .collect()
}

#[test]
fn only_the_registry_and_the_setup_command_make_a_grant() {
    let code = shipped_code();
    assert!(code.iter().any(|(name, _)| name == "crates/swifty-hub/src/runtime/mod.rs"));
    assert!(
        !code.iter().any(|(name, _)| name == "crates/swifty-hub/src/runtime/tests.rs"),
        "a test module in a file of its own is not shipped code"
    );
    for (name, code) in &code {
        let made = code.matches("::when_live(").count();
        if made > 0 {
            assert!(GRANT_MAKERS.contains(&name.as_str()), "{name} makes a LiveGrant");
        }
        if name == GRANT_MAKERS[0] {
            assert_eq!(made, 1, "the registry makes the hub's one grant in one place");
        }
    }
}

#[test]
fn test_modules_are_told_from_shipped_code() {
    let code = shipped_code();
    let webos = &code.iter().find(|(name, _)| name == "plugins/webos/src/driver.rs").unwrap().1;
    let real = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/webos/src/driver.rs"))
        .unwrap();
    assert!(real.contains("::when_live("), "its tests make a grant");
    assert!(!webos.contains("::when_live("), "and they are left out");
    assert!(webos.contains("impl Driver for"), "the driver itself is kept");
}

#[test]
fn every_socket_helper_demands_a_grant() {
    let text = include_str!("../../swifty-net/src/lib.rs");
    for function in [
        "pub fn new(",
        "pub fn with_roots(",
        "pub async fn udp_exchange(",
        "pub async fn udp_broadcast(",
        "pub async fn udp_discover(",
        "pub async fn tcp_exchange(",
        "pub async fn tcp_open(",
        "pub async fn ws_connect(",
        "pub async fn console_connect(",
    ] {
        let start = text.find(function).unwrap_or_else(|| panic!("{function} is gone"));
        let signature = &text[start..start + text[start..].find(')').unwrap()];
        assert!(signature.contains("grant: LiveGrant"), "{function} has no grant");
    }
    // The only private socket opener is reached from the grant-holding helpers.
    assert_eq!(text.matches("tcp_connect(").count(), 5, "one definition and four callers");
    assert_eq!(text.matches("udp_socket(").count(), 4, "one definition and three callers");
}
