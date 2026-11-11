//! `cargo xtask check-plugin <path or package>`: what a plugin must pass before
//! review.
//!
//! ```text
//! cargo xtask check-plugin plugins/wiz             a path to the crate
//! cargo xtask check-plugin swifty-plugin-wiz       a package of the workspace
//! cargo xtask check-plugin wiz                     the same, by plugin id
//! cargo xtask check-plugin wiz --static-only       only the source checks
//! cargo xtask check-plugin wiz --require-deny      fail when cargo-deny is missing
//! ```
//!
//! In order:
//!
//! 1. **Source**: `src/lib.rs` has `#![forbid(unsafe_code)]`; no file under
//!    `src/` or `tests/` names `std::net`, `tokio::net`, `reqwest`, `tungstenite`,
//!    `process::Command` or `env::var` outside a comment (the test kit has a mock
//!    for every kind of device, so tests need no socket of their own either);
//!    `[dependencies]` names no network client; `tests/conformance.rs` calls
//!    `conformance::run`.
//! 2. **Tests**: `cargo test` on the crate, which runs the conformance suite.
//! 3. **Clippy**: `cargo clippy --all-targets -- -D warnings` on the crate.
//! 4. **Licences and advisories**: `cargo deny check licenses advisories`. When
//!    cargo-deny is not installed this step is skipped with a warning, unless
//!    `--require-deny` is given (CI installs it).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// One line for `cargo xtask help`.
pub const ABOUT: &str = "Runs the conformance suite and the static checks on a plugin crate: \
                         check-plugin <path | package> [--static-only] [--require-deny]";

/// What no plugin source may name: every socket goes through `swifty_plugin_api::net`,
/// and a plugin runs no process and reads no environment.
pub const FORBIDDEN_IN_SOURCE: &[&str] =
    &["std::net", "tokio::net", "reqwest", "tungstenite", "process::Command", "env::var"];

/// Network clients a plugin may not depend on.
pub const FORBIDDEN_DEPENDENCIES: &[&str] =
    &["reqwest", "hyper", "hyper-util", "tungstenite", "tokio-tungstenite", "ureq", "surf", "isahc", "curl"];

/// What the arguments ask for.
#[derive(Debug, Default, PartialEq, Eq)]
struct Options {
    target: String,
    static_only: bool,
    require_deny: bool,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    for arg in args {
        match arg.as_str() {
            "--static-only" => options.static_only = true,
            "--require-deny" => options.require_deny = true,
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
            target if options.target.is_empty() => options.target = target.to_string(),
            extra => return Err(format!("one crate at a time; `{extra}` is one too many")),
        }
    }
    if options.target.is_empty() {
        return Err("name a plugin crate: a path, a package name or a plugin id".to_string());
    }
    Ok(options)
}

/// The workspace root: the directory above this crate.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
}

/// The `name` of a manifest's `[package]` section.
pub fn package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package {
            if let Some(value) = line.strip_prefix("name").map(str::trim_start).and_then(|rest| rest.strip_prefix('='))
            {
                return Some(value.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

/// The crate directory `target` names: a directory holding a `Cargo.toml`
/// (from here or from the workspace root), or a package under `plugins/` or
/// `crates/` called `target` or `swifty-plugin-<target>`.
fn resolve(target: &str, root: &Path) -> Result<PathBuf, String> {
    for base in [PathBuf::new(), root.to_path_buf()] {
        let dir = base.join(target);
        if dir.join("Cargo.toml").is_file() {
            return Ok(dir);
        }
    }
    let wanted = [target.to_string(), format!("swifty-plugin-{target}")];
    for parent in ["plugins", "crates"] {
        let Ok(entries) = fs::read_dir(root.join(parent)) else { continue };
        for entry in entries.flatten() {
            let manifest = entry.path().join("Cargo.toml");
            let Ok(text) = fs::read_to_string(&manifest) else { continue };
            if package_name(&text).is_some_and(|name| wanted.contains(&name)) {
                return Ok(entry.path());
            }
        }
    }
    Err(format!(
        "no crate at `{target}`, and no package called `{target}` or `swifty-plugin-{target}` in the workspace"
    ))
}

/// A line with its `//` comment taken off. A `//` inside a string ends the
/// line early, which can only hide a name, never invent one.
fn code_of(line: &str) -> &str {
    line.split("//").next().unwrap_or("")
}

/// Every forbidden name in one source file, as `file:line: name`.
pub fn source_problems(file: &str, text: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let code = code_of(line);
        for needle in FORBIDDEN_IN_SOURCE {
            if code.contains(needle) {
                problems.push(format!(
                    "{file}:{}: names `{needle}`; use swifty_plugin_api::net (in tests, the test kit's mocks), and run                      no process and read no environment",
                    index + 1
                ));
            }
        }
    }
    problems
}

/// Whether `lib.rs` forbids unsafe code for the whole crate.
pub fn forbids_unsafe(lib: &str) -> bool {
    lib.lines()
        .map(|line| line.split_whitespace().collect::<String>())
        .any(|line| line.starts_with("#![forbid(unsafe_code)]"))
}

/// Network clients named in the manifest's dependencies (not dev or build ones).
pub fn dependency_problems(manifest: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let mut in_dependencies = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            let header = line.trim_matches(['[', ']']);
            in_dependencies = header == "dependencies"
                || (header.starts_with("target.") && header.ends_with(".dependencies"))
                || header.starts_with("dependencies.");
            if let Some(name) = header.strip_prefix("dependencies.") {
                if FORBIDDEN_DEPENDENCIES.contains(&name) {
                    problems
                        .push(format!("Cargo.toml depends on `{name}`; reach devices through swifty_plugin_api::net"));
                }
            }
            continue;
        }
        if !in_dependencies {
            continue;
        }
        let name = line.split(['=', '.', ' ']).next().unwrap_or("").trim().trim_matches('"');
        let renamed = line.contains("package")
            && FORBIDDEN_DEPENDENCIES.iter().any(|client| line.contains(&format!("\"{client}\"")));
        if FORBIDDEN_DEPENDENCIES.contains(&name) || renamed {
            problems.push(format!("Cargo.toml depends on `{name}`; reach devices through swifty_plugin_api::net"));
        }
    }
    problems
}

/// Every `.rs` file under `dir`, in a stable order.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Step 1 for the crate at `dir`.
pub fn static_problems(dir: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    let read = |path: &Path| fs::read_to_string(path).unwrap_or_default();
    match fs::read_to_string(dir.join("src/lib.rs")) {
        Ok(lib) if forbids_unsafe(&lib) => {}
        Ok(_) => problems.push("src/lib.rs lacks #![forbid(unsafe_code)]".to_string()),
        Err(_) => problems.push("src/lib.rs is missing: a plugin is a library crate".to_string()),
    }
    let mut files = Vec::new();
    rust_files(&dir.join("src"), &mut files);
    rust_files(&dir.join("tests"), &mut files);
    for file in &files {
        let shown = file.strip_prefix(dir).unwrap_or(file).display().to_string().replace('\\', "/");
        problems.extend(source_problems(&shown, &read(file)));
    }
    problems.extend(dependency_problems(&read(&dir.join("Cargo.toml"))));
    let conformance = read(&dir.join("tests/conformance.rs"));
    if !conformance.contains("conformance::run") && !conformance.contains("conformance::{") {
        problems.push(
            "tests/conformance.rs must run swifty_plugin_testkit::conformance::run(&PLUGIN, fixture())".to_string(),
        );
    }
    problems
}

/// Runs cargo with these arguments, its output going straight through.
fn cargo(args: &[&str]) -> Result<bool, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    println!("> cargo {}", args.join(" "));
    Command::new(cargo)
        .args(args)
        .status()
        .map(|status| status.success())
        .map_err(|error| format!("could not start cargo: {error}"))
}

/// Whether `cargo deny` is installed.
fn deny_installed() -> bool {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    Command::new(cargo).args(["deny", "--version"]).output().is_ok_and(|output| output.status.success())
}

/// Runs the command.
pub fn run(args: &[String]) -> ExitCode {
    let options = match parse(args) {
        Ok(options) => options,
        Err(problem) => {
            eprintln!("cargo xtask check-plugin: {problem}\n{ABOUT}");
            return ExitCode::from(2);
        }
    };
    let root = workspace_root();
    let dir = match resolve(&options.target, &root) {
        Ok(dir) => dir,
        Err(problem) => {
            eprintln!("cargo xtask check-plugin: {problem}");
            return ExitCode::from(2);
        }
    };
    let manifest_path = dir.join("Cargo.toml");
    let name = package_name(&fs::read_to_string(&manifest_path).unwrap_or_default()).unwrap_or_default();
    let manifest = manifest_path.display().to_string();
    println!("check-plugin: {name} at {}", dir.display());

    let mut failed = Vec::new();
    let problems = static_problems(&dir);
    if problems.is_empty() {
        println!("source: ok");
    } else {
        for problem in &problems {
            println!("source: {problem}");
        }
        failed.push("source");
    }

    if !options.static_only {
        let steps: [(&str, Vec<&str>); 2] = [
            ("tests and the conformance suite", vec!["test", "--manifest-path", &manifest]),
            ("clippy", vec!["clippy", "--manifest-path", &manifest, "--all-targets", "--", "-D", "warnings"]),
        ];
        for (step, args) in steps {
            match cargo(&args) {
                Ok(true) => println!("{step}: ok"),
                Ok(false) => failed.push(step),
                Err(problem) => {
                    eprintln!("{step}: {problem}");
                    failed.push(step);
                }
            }
        }
        if deny_installed() {
            match cargo(&["deny", "--manifest-path", &manifest, "check", "licenses", "advisories"]) {
                Ok(true) => println!("cargo deny: ok"),
                _ => failed.push("cargo deny"),
            }
        } else if options.require_deny {
            eprintln!("cargo deny: not installed (cargo install cargo-deny)");
            failed.push("cargo deny");
        } else {
            println!("cargo deny: not installed, skipped; CI runs it (cargo install cargo-deny)");
        }
    }

    if failed.is_empty() {
        println!("check-plugin: {name} passes");
        ExitCode::SUCCESS
    } else {
        eprintln!("check-plugin: {name} fails: {}", failed.join(", "));
        ExitCode::from(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn the_arguments_are_read() {
        assert_eq!(
            parse(&args(&["wiz", "--static-only"])),
            Ok(Options { target: "wiz".into(), static_only: true, require_deny: false })
        );
        assert!(parse(&args(&[])).is_err());
        assert!(parse(&args(&["a", "b"])).is_err());
        assert!(parse(&args(&["a", "--fast"])).is_err());
    }

    #[test]
    fn forbidden_names_are_found_outside_comments() {
        let source = "use swifty_plugin_api::net;\n// not std::net here\nlet s = std::net::UdpSocket::bind(x);\n\
                      let k = std::env::var(\"KEY\");\nuse tokio_tungstenite as ws; // tungstenite\n";
        let problems = source_problems("src/lib.rs", source);
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems[0].starts_with("src/lib.rs:3: names `std::net`"));
        assert!(problems[1].starts_with("src/lib.rs:4: names `env::var`"));
        assert!(problems[2].starts_with("src/lib.rs:5: names `tungstenite`"));
        assert!(source_problems("src/x.rs", "/// Never std::net.\nfn f() {}\n").is_empty());
    }

    #[test]
    fn unsafe_must_be_forbidden_crate_wide() {
        assert!(forbids_unsafe("//! Doc\n#![forbid(unsafe_code)]\n"));
        assert!(forbids_unsafe("#![ forbid( unsafe_code ) ]"));
        assert!(!forbids_unsafe("#![deny(unsafe_code)]\n"));
        assert!(!forbids_unsafe("// #![forbid(unsafe_code)]\n"));
    }

    #[test]
    fn network_clients_are_found_in_dependencies_only() {
        let manifest = "[package]\nname = \"swifty-plugin-x\"\n\n[dependencies]\nserde.workspace = true\n\
                        reqwest = { version = \"0.12\" }\nweb = { package = \"ureq\", version = \"2\" }\n\n\
                        [dev-dependencies]\nhyper = \"1\"\n\n[dependencies.tungstenite]\nversion = \"0.30\"\n";
        let problems = dependency_problems(manifest);
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(
            problems[0].contains("`reqwest`") && problems[1].contains("`web`") && problems[2].contains("`tungstenite`")
        );
        assert_eq!(package_name(manifest).as_deref(), Some("swifty-plugin-x"));
    }

    #[test]
    fn a_crate_is_found_by_path_package_or_id() {
        let root = std::env::temp_dir().join(format!("xtask-check-plugin-{}", std::process::id()));
        let crate_dir = root.join("plugins").join("acme");
        fs::create_dir_all(crate_dir.join("src")).unwrap();
        fs::create_dir_all(crate_dir.join("tests")).unwrap();
        fs::write(crate_dir.join("Cargo.toml"), "[package]\nname = \"swifty-plugin-acme\"\n[dependencies]\n").unwrap();
        fs::write(crate_dir.join("src/lib.rs"), "#![forbid(unsafe_code)]\npub fn f() {}\n").unwrap();
        fs::write(crate_dir.join("src/driver.rs"), "use tokio::net::UdpSocket;\n").unwrap();

        assert_eq!(resolve("plugins/acme", &root), Ok(crate_dir.clone()));
        assert_eq!(resolve("swifty-plugin-acme", &root), Ok(crate_dir.clone()));
        assert_eq!(resolve("acme", &root), Ok(crate_dir.clone()));
        assert!(resolve("nothing", &root).is_err());

        let problems = static_problems(&crate_dir);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].starts_with("src/driver.rs:1: names `tokio::net`"));
        assert!(problems[1].contains("tests/conformance.rs"));
        fs::write(crate_dir.join("src/driver.rs"), "use swifty_plugin_api::net;\n").unwrap();
        fs::write(crate_dir.join("tests/conformance.rs"), "conformance::run(&PLUGIN, fixture()).await;\n").unwrap();
        assert_eq!(static_problems(&crate_dir), Vec::<String>::new());
        // A test names no socket either.
        fs::write(
            crate_dir.join("tests/device.rs"),
            "let socket = tokio::net::UdpSocket::bind(x);
",
        )
        .unwrap();
        let problems = static_problems(&crate_dir);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].starts_with("tests/device.rs:1: names `tokio::net`"), "{problems:?}");
        let _ = fs::remove_dir_all(&root);
    }
}
