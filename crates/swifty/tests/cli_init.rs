//! `swifty init`, `swifty config check`, `swifty doctor` and the rest of the
//! command line, through the real binary.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn swifty(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_swifty")).args(args).stdin(Stdio::null()).output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The paths of an install in a temporary directory.
struct Install {
    dir: tempfile::TempDir,
}

impl Install {
    fn new() -> Self {
        Self { dir: tempfile::tempdir().unwrap() }
    }

    fn config(&self) -> PathBuf {
        self.dir.path().join("etc").join("swifty.toml")
    }

    fn data(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    fn config_arg(&self) -> String {
        self.config().display().to_string()
    }

    /// `swifty init --yes` with these extra arguments.
    fn init(&self, extra: &[&str]) -> Output {
        let (config, data) = (self.config_arg(), self.data().display().to_string());
        let mut args = vec!["--config", config.as_str(), "init", "--yes", "--data-dir", data.as_str()];
        args.extend_from_slice(extra);
        swifty(&args)
    }

    fn check(&self) -> Output {
        swifty(&["config", "check", "--file", &self.config_arg()])
    }
}

#[test]
fn init_writes_a_file_that_config_check_accepts() {
    let install = Install::new();
    let output = install.init(&["--origin", "http://localhost:8790"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let shown = text(&output.stdout);
    assert!(shown.contains("Done."), "{shown}");
    assert!(shown.contains("sudo swifty enroll-token"), "{shown}");

    let check = install.check();
    assert!(check.status.success(), "{}", text(&check.stderr));
    assert!(text(&check.stdout).contains("is valid"));

    let written = std::fs::read_to_string(install.config()).unwrap();
    assert!(written.starts_with("# Swifty Home configuration, version 2."));
    assert!(written.contains("config_version = 2\n"));
    assert!(written.contains("live = false\n"));
    assert!(written.contains("origin  = \"http://localhost:8790\""));
    assert!(install.config().with_file_name("audit.key").is_file());
    assert!(install.data().join("home.db").is_file());
    assert!(install.data().join("secrets").is_dir());
}

#[test]
fn with_yes_and_no_origin_it_asks_nothing_and_takes_this_machine() {
    let install = Install::new();
    let output = install.init(&[]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let written = std::fs::read_to_string(install.config()).unwrap();
    assert!(written.contains("origin  = \"http://localhost:8790\""));
    assert!(written.contains("listen = \"127.0.0.1:8790\""));
}

#[test]
fn the_paths_written_are_absolute() {
    let install = Install::new();
    let output = Command::new(env!("CARGO_BIN_EXE_swifty"))
        .args(["--config", "etc/swifty.toml", "init", "--yes", "--data-dir", "data"])
        .current_dir(install.dir.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    let written: toml::Table = toml::from_str(&std::fs::read_to_string(install.config()).unwrap()).unwrap();
    let data_dir = Path::new(written["data_dir"].as_str().unwrap());
    assert!(data_dir.is_absolute() && data_dir.ends_with("data"), "{data_dir:?}");
    let key = Path::new(written["audit"]["key_file"].as_str().unwrap());
    assert!(key.is_absolute() && key.ends_with("audit.key"), "{key:?}");
}

#[test]
fn init_never_replaces_a_configuration() {
    let install = Install::new();
    assert!(install.init(&[]).status.success());
    let before = std::fs::read(install.config()).unwrap();
    let again = install.init(&["--origin", "https://home.example.org"]);
    assert!(!again.status.success());
    assert!(text(&again.stderr).contains("already exists"), "{}", text(&again.stderr));
    assert_eq!(std::fs::read(install.config()).unwrap(), before);
}

#[test]
fn an_unusable_origin_is_refused_with_the_reason_and_nothing_is_written() {
    for (origin, reason) in [
        ("http://home.example.org", "https"),
        ("https://192.0.2.10", "IP address"),
        ("https://home.example.org/app", "path"),
    ] {
        let install = Install::new();
        let output = install.init(&["--origin", origin]);
        assert!(!output.status.success(), "{origin}");
        let error = text(&output.stderr);
        assert!(error.starts_with("error: ") && error.contains(reason), "{origin}: {error}");
        assert!(!install.config().exists());
        assert!(!install.data().exists());
    }
}

#[test]
fn an_origin_without_a_scheme_gets_https() {
    let install = Install::new();
    let output = install.init(&["--origin", "home.example.org"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let written = std::fs::read_to_string(install.config()).unwrap();
    assert!(written.contains("origin  = \"https://home.example.org\""));
    assert!(written.contains("rp_id   = \"home.example.org\""));
}

#[test]
fn the_demo_flag_starts_with_the_demo_home() {
    let install = Install::new();
    let output = install.init(&["--demo"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(text(&output.stdout).contains("the demo home: 8 pretend devices"), "{}", text(&output.stdout));
    let empty = Install::new();
    let output = empty.init(&[]);
    assert!(text(&output.stdout).contains("empty: add devices with `sudo swifty plugin add <plugin>`"));
}

#[test]
fn config_check_names_a_glimdock_home_file_and_says_what_to_run() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("config.toml");
    std::fs::write(&legacy, "database = '/var/lib/glimdock-home/home.db'\nconsole_vm = 102\n[devices]\n").unwrap();
    let output = swifty(&["config", "check", "--file", &legacy.display().to_string()]);
    assert!(!output.status.success());
    let error = text(&output.stderr);
    assert!(error.contains("This is a glimdock-home configuration"), "{error}");
    assert!(error.contains(&format!("swifty migrate --from {}", legacy.display())), "{error}");
}

#[test]
fn config_check_refuses_an_unknown_key_without_repeating_its_value() {
    let install = Install::new();
    assert!(install.init(&[]).status.success());
    let mut written = std::fs::read_to_string(install.config()).unwrap();
    written.push_str("\n[secrets]\ntoken = \"PASTED-SECRET-VALUE\"\n");
    std::fs::write(install.config(), written).unwrap();
    let output = install.check();
    assert!(!output.status.success());
    let (error, shown) = (text(&output.stderr), text(&output.stdout));
    assert!(error.contains("unknown field") && error.contains("line"), "{error}");
    assert!(!error.contains("PASTED") && !shown.contains("PASTED"));
}

#[test]
fn config_check_without_a_file_says_how_to_make_one() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("swifty.toml");
    let output = swifty(&["--config", &missing.display().to_string(), "config", "check"]);
    assert!(!output.status.success());
    assert!(text(&output.stderr).contains("sudo swifty init"), "{}", text(&output.stderr));
}

#[test]
fn doctor_finds_a_fresh_install_healthy() {
    let install = Install::new();
    assert!(install.init(&[]).status.success());
    let output = swifty(&["--config", &install.config_arg(), "doctor"]);
    assert!(output.status.success(), "{}", text(&output.stdout));
    let report = text(&output.stdout);
    for check in ["config", "listen", "origin", "database", "audit", "secrets", "clock"] {
        assert!(report.lines().any(|line| line.contains(check)), "{check} in {report}");
    }

    let json = swifty(&["--config", &install.config_arg(), "doctor", "--json"]);
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let checks = report["checks"].as_array().unwrap();
    assert!(checks.iter().all(|check| check["status"] != "error"), "{report}");
}

#[test]
fn doctor_exits_with_1_and_a_fix_when_something_is_wrong() {
    let install = Install::new();
    assert!(install.init(&[]).status.success());
    std::fs::remove_file(install.data().join("home.db")).unwrap();
    let output = swifty(&["--config", &install.config_arg(), "doctor"]);
    assert_eq!(output.status.code(), Some(1));
    let report = text(&output.stdout);
    assert!(report.contains("ERROR") && report.contains("fix:"), "{report}");
}

#[test]
fn the_new_install_has_a_verified_audit_log() {
    let install = Install::new();
    assert!(install.init(&[]).status.success());
    let output = swifty(&["--config", &install.config_arg(), "audit", "verify"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], true);
    assert_eq!(report["keyed"], true);
    assert_eq!(report["rows"], 1);
}

#[test]
fn version_prints_the_version_on_its_first_line() {
    let output = swifty(&["version"]);
    assert!(output.status.success());
    let shown = text(&output.stdout);
    assert_eq!(shown.lines().next().unwrap(), format!("Swifty Home {}", env!("CARGO_PKG_VERSION")));
    assert!(text(&swifty(&["version", "--plugins"]).stdout).contains("plugins"));
}

#[test]
fn openapi_prints_the_document_of_the_api() {
    let output = swifty(&["openapi"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), swifty_hub::http::openapi::document_json());
}

#[test]
fn help_leads_a_newcomer_to_init() {
    let output = swifty(&["--help"]);
    assert!(output.status.success());
    let help = text(&output.stdout);
    assert!(help.contains("sudo swifty init"));
    for command in ["init", "serve", "doctor", "plugin", "config", "version"] {
        assert!(help.lines().any(|line| line.trim_start().starts_with(command)), "{command}");
    }
    // The old name still works, and is not shown.
    assert!(!help.contains("audit-verify"));
}

#[test]
fn the_cli_crate_runs_no_process_and_reads_no_environment() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut stack = vec![src];
    let mut seen = 0;
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            seen += 1;
            for needle in ["process::Command", "env::var", "std::env"] {
                assert!(!source.contains(needle), "{} names {needle}", path.display());
            }
        }
    }
    assert!(seen > 5);
}
