//! `swifty plugin new` and the plugin template.
//!
//! These check what is written. That a written crate builds and passes its own
//! tests is checked by `cargo xtask check-template`, which builds one in a scratch
//! workspace for every class, local and cloud.

use std::path::Path;
use std::process::{Command, Output, Stdio};

use swifty::scaffold::{self, Layout, NewPlugin};

fn swifty_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_swifty")).args(args).current_dir(dir).stdin(Stdio::null()).output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn request(id: &str, classes: &[&str], cloud: bool) -> NewPlugin {
    NewPlugin { id: id.to_string(), classes: classes.iter().map(|c| c.to_string()).collect(), name: None, cloud }
}

/// What a plugin crate must never name. The same list the plugin packages are held to.
const FORBIDDEN: &[&str] = &["std::net", "tokio::net", "reqwest", "tungstenite", "process::Command", "env::var"];

#[test]
fn every_class_renders_for_every_reach_and_layout() {
    for class in scaffold::known_classes() {
        for cloud in [false, true] {
            for layout in [Layout::Workspace, Layout::Standalone] {
                let rendered = scaffold::render(&request("acme-blinds", &[&class], cloud), layout).unwrap();
                assert_eq!(rendered.crate_name, "swifty-plugin-acme-blinds");
                let file = |name: &str| {
                    rendered.files.iter().find(|(path, _)| path == name).map(|(_, text)| text.as_str()).unwrap()
                };
                for (path, text) in &rendered.files {
                    assert!(!text.contains("{{"), "{class} {cloud} {layout:?}: a marker is left in {path}");
                    if path.ends_with(".rs") && !path.starts_with("tests/") {
                        for needle in FORBIDDEN {
                            assert!(!text.contains(needle), "{path} names {needle}");
                        }
                    }
                }
                let manifest: toml::Table = toml::from_str(file("Cargo.toml")).unwrap();
                assert_eq!(manifest["package"]["name"].as_str(), Some("swifty-plugin-acme-blinds"));
                assert_eq!(manifest["lib"]["name"].as_str(), Some("swifty_plugin_acme_blinds"));
                let api = &manifest["dependencies"]["swifty-plugin-api"];
                match layout {
                    Layout::Workspace => assert_eq!(api["workspace"].as_bool(), Some(true)),
                    Layout::Standalone => assert_eq!(api.as_str(), Some("0.1")),
                }
                let standalone_only = rendered.files.iter().any(|(path, _)| path == ".github/workflows/ci.yml");
                assert_eq!(standalone_only, layout == Layout::Standalone);
                let lib = file("src/lib.rs");
                assert!(lib.contains("pub static PLUGIN: AcmeBlindsPlugin = AcmeBlindsPlugin;"));
                assert!(lib.contains("id: \"acme-blinds\""));
                assert!(lib.contains("name: \"Acme Blinds\""));
                assert_eq!(lib.contains("Reach::Cloud"), cloud);
                assert_eq!(lib.contains("Reach::Local"), !cloud);
                // Every check must run: a new plugin is held to all twelve from the start, with a
                // stand-in that can spoil its replies and report a hostile name.
                let conformance = file("tests/conformance.rs");
                assert!(conformance.contains("swifty_plugin_testkit::conformance::{run_every_check, Fixture"));
                assert!(conformance.contains("run_every_check(&PLUGIN, fixture).await;"));
                assert!(!conformance.contains("conformance::{run,"), "plain `run` lets a check go unrun silently");
                assert_eq!(conformance.contains("Rigged::new(stand.device, stand.switches.clone())"), !cloud);
                assert!(file("tests/common/mod.rs").contains("switches: &Switches"));
                assert!(!file("src/config.rs").contains("core::net"), "address types come from swifty_plugin_api::net");
            }
        }
    }
}

#[test]
fn several_classes_go_into_one_crate_in_the_order_given() {
    let rendered =
        scaffold::render(&request("acme-home", &["light", "plug", "light"], false), Layout::Workspace).unwrap();
    let protocol = &rendered.files.iter().find(|(path, _)| path == "src/protocol.rs").unwrap().1;
    let light = protocol.find("(\"light\", DeviceClass::Light").unwrap();
    let plug = protocol.find("(\"plug\", DeviceClass::Plug").unwrap();
    assert!(light < plug);
    assert_eq!(protocol.matches("DeviceClass::Light, \"Light\"").count(), 1, "a class given twice is there once");
}

#[test]
fn bad_requests_are_refused_with_what_to_do() {
    let refused = |id: &str, classes: &[&str], name: Option<&str>| {
        let mut asked = request(id, classes, false);
        asked.name = name.map(str::to_string);
        scaffold::render(&asked, Layout::Standalone).unwrap_err()
    };
    for id in ["Acme", "a", "acme_blinds", "acme--blinds", "acme-", "9acme", "acme blinds"] {
        assert!(refused(id, &["plug"], None).contains("acme-blinds"), "{id}");
    }
    assert!(refused("acme", &["spaceship"], None).contains("plug"));
    assert!(refused("acme", &[], None).contains("--class"));
    for machine in ["vm", "host", "router"] {
        assert!(refused("acme", &[machine], None).contains("plugins/proxmox"), "{machine}");
    }
    for name in ["", " padded", "quote\"d", "back\\slash", "brace{s}"] {
        assert!(refused("acme", &["plug"], Some(name)).contains("name"), "{name}");
    }
}

#[test]
fn plugin_new_writes_a_crate_of_its_own_outside_a_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let output = swifty_in(dir.path(), &["plugin", "new", "acme-blinds", "--class", "plug", "--name", "Acme Blinds"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let shown = text(&output.stdout);
    assert!(shown.contains("Created swifty-plugin-acme-blinds"), "{shown}");
    assert!(shown.contains("cargo test"), "{shown}");
    let crate_dir = dir.path().join("acme-blinds");
    for file in [
        "Cargo.toml",
        "README.md",
        "DEVELOPING.md",
        "src/lib.rs",
        "src/config.rs",
        "src/protocol.rs",
        "src/driver.rs",
        "src/setup.rs",
        "tests/mock_device.rs",
        "tests/conformance.rs",
        ".github/workflows/ci.yml",
        "rustfmt.toml",
    ] {
        assert!(crate_dir.join(file).is_file(), "{file}");
    }

    // A second run never writes over the first.
    let again = swifty_in(dir.path(), &["plugin", "new", "acme-blinds", "--class", "light"]);
    assert!(!again.status.success());
    assert!(text(&again.stderr).contains("already exists"));
    assert!(std::fs::read_to_string(crate_dir.join("src/protocol.rs")).unwrap().contains("DeviceClass::Plug"));
}

#[test]
fn nothing_is_written_for_a_bad_request() {
    let dir = tempfile::tempdir().unwrap();
    let output = swifty_in(dir.path(), &["plugin", "new", "acme-blinds", "--class", "spaceship"]);
    assert!(!output.status.success());
    assert!(text(&output.stderr).contains("There is no device class"));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

/// A directory that looks like a swifty-home checkout to `plugin new`.
fn fake_checkout() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("Cargo.toml"), "[workspace]\nmembers = [\"crates/*\", \"plugins/*\"]\n").unwrap();
    std::fs::create_dir_all(root.path().join("crates/swifty-plugin-api")).unwrap();
    std::fs::write(root.path().join("crates/swifty-plugin-api/Cargo.toml"), "[package]\n").unwrap();
    std::fs::create_dir_all(root.path().join("plugins")).unwrap();
    std::fs::create_dir_all(root.path().join("docs/notes")).unwrap();
    root
}

#[test]
fn inside_a_checkout_the_crate_goes_in_plugins_and_uses_the_workspace() {
    let root = fake_checkout();
    let output = swifty_in(&root.path().join("docs/notes"), &["plugin", "new", "acme-blinds", "--class", "plug"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(text(&output.stdout).contains("cargo test -p swifty-plugin-acme-blinds"));
    let crate_dir = root.path().join("plugins/acme-blinds");
    let manifest = std::fs::read_to_string(crate_dir.join("Cargo.toml")).unwrap();
    assert!(manifest.contains("swifty-plugin-api.workspace = true"), "{manifest}");
    assert!(!crate_dir.join(".github").exists(), "the workspace's CI applies");
    assert!(!crate_dir.join("rustfmt.toml").exists(), "the workspace's format applies");

    // Elsewhere inside the checkout, it says where a plugin goes.
    let crates = root.path().join("crates").display().to_string();
    let elsewhere = swifty_in(root.path(), &["plugin", "new", "acme-other", "--class", "plug", "--dir", &crates]);
    assert!(!elsewhere.status.success());
    assert!(text(&elsewhere.stderr).contains("plugins"), "{}", text(&elsewhere.stderr));
    assert!(!root.path().join("crates/acme-other").exists());
}

#[test]
fn a_crate_of_its_own_is_formatted_like_the_workspace() {
    let ours = include_str!("../../../templates/plugin/rustfmt.toml");
    let workspace = include_str!("../../../rustfmt.toml");
    assert_eq!(ours.trim(), workspace.trim());
}
