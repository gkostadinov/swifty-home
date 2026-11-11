//! `cargo xtask check-template`: proves the plugin template still builds and
//! passes its own tests against the current API.
//!
//! 1. Under the build directory it makes a small swifty-home checkout: the root
//!    manifest (with only these members), the lock file, the formatter's and the
//!    toolchain's settings, and copies of `swifty-net`, `swifty-plugin-api` and
//!    `swifty-plugin-testkit`, which is all a plugin needs.
//! 2. It runs `swifty plugin new` there, exactly as a plugin author does, twice:
//!    one crate offering every class the template knows, reached on the local
//!    network, and the same reached through a vendor's cloud (`--cloud`).
//! 3. On both crates: the source checks of `check-plugin`, `cargo fmt --check`,
//!    `cargo clippy --all-targets -- -D warnings`, and `cargo test`, which runs
//!    their unit tests, their mock-device tests and the conformance suite.
//!
//! The scratch checkout stays in place afterwards, for a look when something fails.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::check_plugin;
use crate::gen_docs::{cargo_in, slashed, target_dir, workspace_root};

/// One line for `cargo xtask help`.
pub const ABOUT: &str =
    "Writes the plugin template with `swifty plugin new` into a scratch checkout, for every class, \
                         local and cloud, then formats, lints and tests it";

/// The crates of the repository a plugin builds against.
const CRATES: &[&str] = &["crates/swifty-net", "crates/swifty-plugin-api", "crates/swifty-plugin-testkit"];

/// Files of the repository root the scratch checkout needs as they are.
const ROOT_FILES: &[&str] = &["Cargo.lock", "rustfmt.toml", "rust-toolchain.toml"];

/// The plugins it writes: id and whether it is the cloud variant.
const PROBES: &[(&str, bool)] = &[("template-local", false), ("template-cloud", true)];

/// The class words `swifty plugin new --class` takes: the sections of
/// `templates/plugin/classes.toml`.
pub fn template_classes(classes_toml: &str) -> Vec<String> {
    classes_toml
        .lines()
        .filter_map(|line| line.trim().strip_prefix('[')?.strip_suffix(']').map(str::to_string))
        .filter(|name| !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase()))
        .collect()
}

/// The root manifest of the scratch checkout: the repository's, with its own
/// members, and without workspace dependencies whose path is not there.
pub fn scratch_manifest(root_manifest: &str, exists: &dyn Fn(&str) -> bool) -> String {
    let mut out = Vec::new();
    for line in root_manifest.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("members") {
            out.push("members = [\"crates/*\", \"plugins/*\"]".to_string());
            continue;
        }
        if trimmed.starts_with("exclude") {
            continue;
        }
        if let Some(path) = line.split("path = \"").nth(1).and_then(|rest| rest.split('"').next()) {
            if !exists(path) {
                continue;
            }
        }
        out.push(line.to_string());
    }
    out.join("\n") + "\n"
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|e| format!("cannot create {}: {e}", to.display()))?;
    let entries = fs::read_dir(from).map_err(|e| format!("cannot read {}: {e}", from.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if name == "target" {
            continue;
        }
        if path.is_dir() {
            copy_dir(&path, &to.join(&name))?;
        } else {
            fs::copy(&path, to.join(&name)).map_err(|e| format!("cannot copy {}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// Builds the scratch checkout. Returns its directory.
fn scratch_checkout(root: &Path, target: &Path) -> Result<PathBuf, String> {
    let scratch = target.join("xtask").join("check-template");
    if scratch.exists() {
        fs::remove_dir_all(&scratch).map_err(|e| format!("cannot clear {}: {e}", scratch.display()))?;
    }
    fs::create_dir_all(scratch.join("plugins")).map_err(|e| format!("cannot create {}: {e}", scratch.display()))?;
    for krate in CRATES {
        copy_dir(&root.join(krate), &scratch.join(krate))?;
    }
    for file in ROOT_FILES {
        if root.join(file).is_file() {
            fs::copy(root.join(file), scratch.join(file)).map_err(|e| format!("cannot copy {file}: {e}"))?;
        }
    }
    let manifest = fs::read_to_string(root.join("Cargo.toml")).map_err(|e| format!("cannot read Cargo.toml: {e}"))?;
    let exists = |path: &str| scratch.join(path).exists();
    fs::write(scratch.join("Cargo.toml"), scratch_manifest(&manifest, &exists))
        .map_err(|e| format!("cannot write the scratch Cargo.toml: {e}"))?;
    Ok(scratch)
}

/// Runs `swifty plugin new` from this workspace, writing into `plugins/` of the scratch checkout.
fn plugin_new(root: &Path, target: &Path, scratch: &Path, id: &str, classes: &str, cloud: bool) -> Result<(), String> {
    let manifest = slashed(&root.join("Cargo.toml"));
    let dir = slashed(&scratch.join("plugins"));
    let mut args = vec!["run", "--quiet", "--manifest-path", &manifest, "-p", "swifty", "--", "plugin", "new", id];
    args.extend(["--class", classes, "--dir", &dir]);
    if cloud {
        args.push("--cloud");
    }
    match cargo_in(scratch, &args, target)? {
        true => Ok(()),
        false => Err(format!("`swifty plugin new {id}` failed")),
    }
}

/// Runs the command.
pub fn run(args: &[String]) -> ExitCode {
    if !args.is_empty() {
        eprintln!("cargo xtask check-template: it takes no arguments\n{ABOUT}");
        return ExitCode::from(2);
    }
    match check() {
        Ok(()) => {
            println!("check-template: the template builds, is formatted and lint-free, and passes its tests");
            ExitCode::SUCCESS
        }
        Err(problem) => {
            eprintln!("check-template: {problem}");
            ExitCode::from(1)
        }
    }
}

fn check() -> Result<(), String> {
    let root = workspace_root();
    let target = target_dir(&root);
    let classes_toml = fs::read_to_string(root.join("templates/plugin/classes.toml"))
        .map_err(|e| format!("cannot read templates/plugin/classes.toml: {e}"))?;
    let classes = template_classes(&classes_toml);
    if classes.is_empty() {
        return Err("templates/plugin/classes.toml names no class".to_string());
    }
    let scratch = scratch_checkout(&root, &target)?;
    println!("check-template: a scratch checkout in {}", scratch.display());
    let classes = classes.join(",");
    for (id, cloud) in PROBES {
        plugin_new(&root, &target, &scratch, id, &classes, *cloud)?;
    }

    let mut problems = Vec::new();
    for (id, _) in PROBES {
        for problem in check_plugin::static_problems(&scratch.join("plugins").join(id)) {
            problems.push(format!("plugins/{id}: {problem}"));
        }
    }
    let manifest = slashed(&scratch.join("Cargo.toml"));
    let packages: Vec<String> = PROBES.iter().map(|(id, _)| format!("swifty-plugin-{id}")).collect();
    let mut selected = Vec::new();
    for package in &packages {
        selected.extend(["-p", package.as_str()]);
    }
    let fmt = [vec!["fmt", "--manifest-path", &manifest], selected.clone(), vec!["--check"]].concat();
    let clippy =
        [vec!["clippy", "--manifest-path", &manifest, "--all-targets"], selected.clone(), vec!["--", "-D", "warnings"]]
            .concat();
    let test = [vec!["test", "--manifest-path", &manifest], selected].concat();
    for (step, args) in [("formatting", fmt), ("clippy", clippy), ("tests", test)] {
        if !cargo_in(&scratch, &args, &target)? {
            problems.push(format!("{step} failed; the output above says where"));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_class_words_are_the_sections_of_the_table() {
        let table = "# comment\n[plug]\ndoes = \"x\"\n\n[light]\nencode = \"\"\"\n(DeviceClass::Light, op)\n\"\"\"\n";
        assert_eq!(template_classes(table), vec!["plug", "light"]);
        let real = fs::read_to_string(workspace_root().join("templates/plugin/classes.toml")).unwrap();
        let classes = template_classes(&real);
        assert!(classes.contains(&"plug".to_string()) && classes.len() >= 10, "{classes:?}");
    }

    #[test]
    fn the_scratch_manifest_keeps_only_what_is_there() {
        let manifest = "[workspace]\nresolver = \"2\"\nmembers = [\"crates/*\", \"plugins/*\", \"xtask\"]\n\
                        exclude = [\"services/x\"]\n\n[workspace.dependencies]\n\
                        swifty-hub = { path = \"crates/swifty-hub\", version = \"0.1.0\" }\n\
                        swifty-net = { path = \"crates/swifty-net\", version = \"0.1.0\" }\nserde = \"1\"\n";
        let exists = |path: &str| path == "crates/swifty-net";
        let scratch = scratch_manifest(manifest, &exists);
        assert!(scratch.contains("members = [\"crates/*\", \"plugins/*\"]"), "{scratch}");
        assert!(!scratch.contains("exclude"));
        assert!(!scratch.contains("swifty-hub"));
        assert!(scratch.contains("swifty-net = { path = \"crates/swifty-net\""));
        assert!(scratch.contains("serde = \"1\""));
    }
}
