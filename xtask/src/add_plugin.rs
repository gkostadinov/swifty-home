//! `cargo xtask add-plugin <path | git url>`: compiles a plugin crate into this
//! build of the hub.
//!
//! ```text
//! cargo xtask add-plugin plugins/acme-blinds              a crate in this checkout
//! cargo xtask add-plugin ../acme-blinds                   a crate of your own, elsewhere
//! cargo xtask add-plugin https://example.org/acme-blinds.git [--crate NAME] [--id ID]
//! cargo xtask add-plugin ../acme-blinds --dry-run         say what would change, change nothing
//! ```
//!
//! It makes three edits, each in the shape the files already have:
//!
//! 1. `crates/swifty/Cargo.toml`: an optional dependency on the crate, a feature
//!    named after the plugin id that turns it on, and that feature in
//!    `all-plugins`, so a default build includes it.
//! 2. `crates/swifty/src/plugins.rs`: one entry in the list `compiled_in()`
//!    returns, behind the feature.
//! 3. For a crate outside `plugins/` (or from git), which depends on
//!    `swifty-plugin-api` by version, a `[patch.crates-io]` entry in the root
//!    `Cargo.toml` that points that name at `crates/swifty-plugin-api`, so it
//!    builds against this checkout's API.
//!
//! A plugin already in the build is left as it is. Nothing is built here: run
//! `cargo build -p swifty` afterwards, and `cargo run -p swifty -- version
//! --plugins` lists it.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use crate::gen_docs::{manifest_value, slashed, workspace_root};

/// One line for `cargo xtask help`.
pub const ABOUT: &str = "Adds a plugin crate to the build: a dependency, a feature and one line in plugins.rs: \
                         add-plugin <path | git url> [--crate NAME] [--id ID] [--dry-run]";

const SWIFTY_MANIFEST: &str = "crates/swifty/Cargo.toml";
const PLUGINS_RS: &str = "crates/swifty/src/plugins.rs";
const API_PACKAGE: &str = "swifty-plugin-api";

/// What the arguments ask for.
#[derive(Debug, Default, PartialEq, Eq)]
struct Options {
    source: String,
    package: Option<String>,
    id: Option<String>,
    dry_run: bool,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dry-run" => options.dry_run = true,
            "--crate" => options.package = Some(args.next().ok_or("--crate needs a crate name")?.clone()),
            "--id" => options.id = Some(args.next().ok_or("--id needs a plugin id")?.clone()),
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
            source if options.source.is_empty() => options.source = source.to_string(),
            extra => return Err(format!("one plugin at a time; `{extra}` is one too many")),
        }
    }
    if options.source.is_empty() {
        return Err("name the plugin crate: a path, or a git URL".to_string());
    }
    Ok(options)
}

/// Where the crate comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A directory in `plugins/` of this checkout, by its name there.
    Member(String),
    /// A directory anywhere else.
    Path(PathBuf),
    /// A git repository.
    Git(String),
}

/// The crate to add, as the edits need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Addition {
    pub source: Source,
    /// The package name, such as `swifty-plugin-acme-blinds`.
    pub package: String,
    /// The library name, such as `swifty_plugin_acme_blinds`.
    pub lib: String,
    /// The plugin id, which is also the feature's name.
    pub id: String,
    /// Whether it depends on `swifty-plugin-api` by version, and so needs the patch.
    pub needs_patch: bool,
}

/// Whether the text names a git repository rather than a directory.
pub fn is_git_url(text: &str) -> bool {
    ["https://", "http://", "ssh://", "git://", "git@"].iter().any(|start| text.starts_with(start))
        || text.ends_with(".git")
}

/// `[a-z][a-z0-9-]{1,31}`.
fn is_plugin_id(id: &str) -> bool {
    id.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && (2..=32).contains(&id.len())
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The id a crate name stands for: `swifty-plugin-acme` is `acme`.
pub fn id_of_package(package: &str) -> String {
    package.strip_prefix("swifty-plugin-").unwrap_or(package).to_string()
}

/// The package a git URL most likely holds: its last part, as `swifty-plugin-<id>`.
pub fn package_of_url(url: &str) -> String {
    let last = url.trim_end_matches('/').rsplit(['/', ':']).next().unwrap_or(url);
    let name = last.strip_suffix(".git").unwrap_or(last);
    if name.starts_with("swifty-plugin-") {
        name.to_string()
    } else {
        format!("swifty-plugin-{name}")
    }
}

/// The manifest id written in a plugin's `src/lib.rs`: the first `id: "..."` after `Manifest {`.
pub fn manifest_id(lib: &str) -> Option<String> {
    let from = lib.find("Manifest {")?;
    let rest = &lib[from..];
    let at = rest.find("id: \"")? + "id: \"".len();
    rest[at..].split('"').next().map(str::to_string)
}

/// Whether a manifest depends on the plugin API by version, not by path or workspace.
pub fn api_by_version(manifest: &str) -> bool {
    let mut in_dependencies = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_dependencies = line == "[dependencies]";
            continue;
        }
        if in_dependencies && line.starts_with(API_PACKAGE) {
            return !line.contains("path") && !line.contains("workspace") && !line.contains("git");
        }
    }
    false
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// Works out what to add, from the arguments.
fn resolve(options: &Options, root: &Path) -> Result<Addition, String> {
    if is_git_url(&options.source) {
        let package = options.package.clone().unwrap_or_else(|| package_of_url(&options.source));
        let id = options.id.clone().unwrap_or_else(|| id_of_package(&package));
        return Ok(Addition {
            source: Source::Git(options.source.clone()),
            lib: package.replace('-', "_"),
            package,
            id,
            needs_patch: true,
        });
    }
    let given = PathBuf::from(&options.source);
    let dir = [std::env::current_dir().unwrap_or_default().join(&given), root.join(&given)]
        .into_iter()
        .find(|dir| dir.join("Cargo.toml").is_file())
        .ok_or_else(|| format!("no Cargo.toml in {}", options.source))?;
    // Canonical, without the long-path prefix Windows adds, so it compares with the root.
    let dir = fs::canonicalize(&dir).map_or(dir, |canonical| PathBuf::from(slashed(&canonical)));
    let manifest = read(&dir.join("Cargo.toml"))?;
    let package = manifest_value(&manifest, "package", "name")
        .ok_or_else(|| format!("{} has no [package] name", dir.join("Cargo.toml").display()))?;
    let lib_rs = read(&dir.join("src/lib.rs"))?;
    if !lib_rs.contains("pub static PLUGIN") {
        return Err(format!(
            "{} does not export `pub static PLUGIN`, so the hub cannot list it; is it a plugin crate?",
            dir.join("src/lib.rs").display()
        ));
    }
    let lib = manifest_value(&manifest, "lib", "name").unwrap_or_else(|| package.replace('-', "_"));
    let id = options.id.clone().or_else(|| manifest_id(&lib_rs)).unwrap_or_else(|| id_of_package(&package));
    let plugins = fs::canonicalize(root.join("plugins"))
        .map_or_else(|_| root.join("plugins"), |dir| PathBuf::from(slashed(&dir)));
    let source = match dir.strip_prefix(&plugins) {
        Ok(inside) if inside.components().count() == 1 => Source::Member(slashed(inside)),
        _ => Source::Path(dir),
    };
    let needs_patch = !matches!(source, Source::Member(_)) && api_by_version(&manifest);
    Ok(Addition { source, package, lib, id, needs_patch })
}

/// `to` as a path from `from`, when both are absolute on the same drive.
pub fn relative(from: &Path, to: &Path) -> Option<String> {
    let from: Vec<Component> = from.components().collect();
    let to: Vec<Component> = to.components().collect();
    if from.first() != to.first() {
        return None;
    }
    let shared = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".to_string(); from.len() - shared];
    parts.extend(to[shared..].iter().map(|part| part.as_os_str().to_string_lossy().into_owned()));
    Some(parts.join("/"))
}

/// The dependency line for `crates/swifty/Cargo.toml`.
pub fn dependency_line(addition: &Addition, swifty_dir: &Path, root: &Path) -> String {
    let package = &addition.package;
    match &addition.source {
        Source::Member(dir) => {
            let at = relative(swifty_dir, &root.join("plugins").join(dir)).unwrap_or(format!("../../plugins/{dir}"));
            format!("{package} = {{ path = \"{at}\", optional = true }}")
        }
        Source::Path(dir) => {
            let at = relative(swifty_dir, dir).unwrap_or_else(|| slashed(dir));
            format!("{package} = {{ path = \"{at}\", optional = true }}")
        }
        Source::Git(url) => format!("{package} = {{ git = \"{url}\", optional = true }}"),
    }
}

/// The lines of the TOML section `[name]`: where its header is and where it ends.
fn section(lines: &[String], name: &str) -> Option<(usize, usize)> {
    let header = format!("[{name}]");
    let start = lines.iter().position(|line| line.trim() == header)?;
    let end = lines[start + 1..]
        .iter()
        .position(|line| line.trim_start().starts_with('['))
        .map_or(lines.len(), |offset| start + 1 + offset);
    Some((start, end))
}

/// The last line of a section that holds something, or its header.
fn last_content(lines: &[String], (start, end): (usize, usize)) -> usize {
    (start..end).rev().find(|at| !lines[*at].trim().is_empty() && !lines[*at].trim().starts_with('#')).unwrap_or(start)
}

/// `crates/swifty/Cargo.toml` with the dependency and the feature added.
pub fn edit_swifty_manifest(text: &str, addition: &Addition, dependency: &str) -> Result<String, String> {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let package = &addition.package;
    let id = &addition.id;

    // The dependency, after the other plugins, or at the end of [dependencies].
    let deps = section(&lines, "dependencies").ok_or("crates/swifty/Cargo.toml has no [dependencies]")?;
    if !lines[deps.0..deps.1].iter().any(|line| line.trim_start().starts_with(&format!("{package} "))) {
        let after = (deps.0..deps.1)
            .rev()
            .find(|at| lines[*at].trim_start().starts_with("swifty-plugin-"))
            .unwrap_or_else(|| last_content(&lines, deps));
        lines.insert(after + 1, dependency.to_string());
    }

    // The feature, and the feature in all-plugins (or default).
    let feature = format!("{id} = [\"dep:{package}\"]");
    let features = match section(&lines, "features") {
        Some(found) => found,
        None => {
            return Err("crates/swifty/Cargo.toml has no [features] table to add the plugin's feature to".to_string())
        }
    };
    let has_feature = lines[features.0..features.1].iter().any(|line| {
        line.split_once('=').is_some_and(|(name, _)| name.trim() == id.as_str() || name.trim() == format!("\"{id}\""))
    });
    if !has_feature {
        let after = last_content(&lines, features);
        lines.insert(after + 1, feature);
    }
    let features = section(&lines, "features").ok_or("[features] went missing")?;
    let group = ["all-plugins", "default"]
        .into_iter()
        .find_map(|name| {
            (features.0..features.1).find(|at| lines[*at].split_once('=').is_some_and(|(key, _)| key.trim() == name))
        })
        .ok_or("[features] has neither all-plugins nor default to add the plugin to")?;
    add_to_array(&mut lines, group, id);
    let mut out = lines.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// Adds `"word"` to the TOML array that starts on line `at`, on one line or several.
fn add_to_array(lines: &mut Vec<String>, at: usize, word: &str) {
    let quoted = format!("\"{word}\"");
    let end = (at..lines.len()).find(|index| lines[*index].contains(']')).unwrap_or(at);
    if lines[at..=end].iter().any(|line| line.contains(&quoted)) {
        return;
    }
    if end == at {
        let line = &lines[at];
        let close = line.rfind(']').unwrap_or(line.len());
        let inside = line[..close].rsplit('[').next().unwrap_or("").trim();
        let joiner = if inside.is_empty() {
            ""
        } else if inside.ends_with(',') {
            " "
        } else {
            ", "
        };
        lines[at] = format!("{}{joiner}{quoted}{}", line[..close].trim_end(), &line[close..]);
    } else {
        let indent: String = lines[end - 1].chars().take_while(|c| c.is_whitespace()).collect();
        let previous = lines[end - 1].trim_end().to_string();
        if !previous.ends_with(',') && !previous.ends_with('[') {
            lines[end - 1] = format!("{previous},");
        }
        lines.insert(end, format!("{indent}{quoted},"));
    }
}

/// `plugins.rs` with the plugin added to the list `compiled_in()` returns.
pub fn edit_plugins_rs(text: &str, addition: &Addition) -> Result<String, String> {
    let entry = format!("&{}::PLUGIN", addition.lib);
    if text.contains(&entry) {
        return Ok(text.to_string());
    }
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let function = lines
        .iter()
        .position(|line| line.contains("fn compiled_in"))
        .ok_or("crates/swifty/src/plugins.rs has no `fn compiled_in` to add the plugin to")?;
    let open = (function..lines.len()).find(|at| lines[*at].contains("vec![")).ok_or("compiled_in has no vec![")?;
    let close = (open + 1..lines.len())
        .find(|at| lines[*at].trim_start().starts_with(']'))
        .ok_or("the vec! in compiled_in does not close on a line of its own")?;
    let cfg = format!("#[cfg(feature = \"{}\")]", addition.id);
    // Copy the shape of the entry before: the attribute on its own line, or on the entry's line.
    let previous = (open + 1..close).rev().find(|at| lines[*at].contains("::PLUGIN"));
    let (indent, same_line) = match previous {
        Some(at) => {
            let line = &lines[at];
            (line.chars().take_while(|c| c.is_whitespace()).collect::<String>(), line.trim_start().starts_with("#["))
        }
        None => (format!("{}    ", lines[close].chars().take_while(|c| c.is_whitespace()).collect::<String>()), false),
    };
    if same_line {
        lines.insert(close, format!("{indent}{cfg} {entry},"));
    } else {
        lines.insert(close, format!("{indent}{entry},"));
        lines.insert(close, format!("{indent}{cfg}"));
    }
    let mut out = lines.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// The root `Cargo.toml` with `swifty-plugin-api` patched to this checkout.
pub fn edit_root_manifest(text: &str) -> String {
    let patch = format!("{API_PACKAGE} = {{ path = \"crates/{API_PACKAGE}\" }}");
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    match section(&lines, "patch.crates-io") {
        Some(found) if lines[found.0..found.1].iter().any(|line| line.trim_start().starts_with(API_PACKAGE)) => {
            return text.to_string();
        }
        Some(found) => {
            let after = last_content(&lines, found);
            lines.insert(after + 1, patch);
        }
        None => {
            if lines.last().is_some_and(|line| !line.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push("# Plugin crates outside plugins/ depend on the API by version: build them against".to_string());
            lines.push("# this checkout's. Written by `cargo xtask add-plugin`.".to_string());
            lines.push("[patch.crates-io]".to_string());
            lines.push(patch);
        }
    }
    lines.join("\n") + "\n"
}

/// Runs the command.
pub fn run(args: &[String]) -> ExitCode {
    let options = match parse(args) {
        Ok(options) => options,
        Err(problem) => {
            eprintln!("cargo xtask add-plugin: {problem}\n{ABOUT}");
            return ExitCode::from(2);
        }
    };
    match add(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(problem) => {
            eprintln!("cargo xtask add-plugin: {problem}");
            ExitCode::from(1)
        }
    }
}

fn add(options: &Options) -> Result<(), String> {
    let root = workspace_root();
    let addition = resolve(options, &root)?;
    if !is_plugin_id(&addition.id) {
        return Err(format!("`{}` is not a plugin id; give one with --id", addition.id));
    }
    let swifty_dir = root.join("crates/swifty");
    let mut changes: Vec<(PathBuf, String, String)> = Vec::new();

    let path = root.join(SWIFTY_MANIFEST);
    let before = read(&path)?;
    let dependency = dependency_line(&addition, &swifty_dir, &root);
    let after = edit_swifty_manifest(&before, &addition, &dependency)?;
    changes.push((path, before, after));

    let path = root.join(PLUGINS_RS);
    let before = read(&path)?;
    let after = edit_plugins_rs(&before, &addition)?;
    changes.push((path, before, after));

    if addition.needs_patch {
        let path = root.join("Cargo.toml");
        let before = read(&path)?;
        let after = edit_root_manifest(&before);
        changes.push((path, before, after));
    }

    let changed: Vec<&(PathBuf, String, String)> =
        changes.iter().filter(|(_, before, after)| before != after).collect();
    if changed.is_empty() {
        println!("add-plugin: {} ({}) is already in the build; nothing changed", addition.id, addition.package);
        return Ok(());
    }
    for (path, before, after) in &changed {
        let shown = path.strip_prefix(&root).map_or_else(|_| slashed(path), slashed);
        println!("{} {shown}:", if options.dry_run { "would change" } else { "changes" });
        for line in after.lines().filter(|line| !before.lines().any(|old| old == *line)) {
            println!("  + {}", line.trim_end());
        }
    }
    if options.dry_run {
        println!("add-plugin: --dry-run, so nothing was written");
        return Ok(());
    }
    for (path, _, after) in &changed {
        fs::write(path, after).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    println!(
        "add-plugin: {} is in the build behind the feature `{}`. Next: `cargo build -p swifty`, then \
         `cargo run -p swifty -- version --plugins` lists it.",
        addition.package, addition.id
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of the list, as `docs/plugins/contract.md` gives it.
    const PLUGINS: &str = "//! Every plugin in this build.\n\nuse swifty_plugin_api::Plugin;\n\n\
                           /// Every plugin in this build.\npub fn compiled_in() -> Vec<&'static dyn Plugin> {\n    vec![\n\
                           \x20       #[cfg(feature = \"wiz\")]\n        &swifty_plugin_wiz::PLUGIN,\n\
                           \x20       #[cfg(feature = \"mock\")]\n        &swifty_plugin_mock::PLUGIN,\n    ]\n}\n";

    const SWIFTY: &str = "[package]\nname = \"swifty\"\n\n[dependencies]\nclap.workspace = true\n\
                          swifty-plugin-wiz = { workspace = true, optional = true }\n\
                          swifty-plugin-mock = { workspace = true, optional = true }\ntoml.workspace = true\n\n\
                          [features]\ndefault = [\"all-plugins\"]\nall-plugins = [\"wiz\", \"mock\"]\n\
                          wiz = [\"dep:swifty-plugin-wiz\"]\nmock = [\"dep:swifty-plugin-mock\"]\n\n\
                          [dev-dependencies]\ntempfile.workspace = true\n";

    fn acme(source: Source) -> Addition {
        Addition {
            source,
            package: "swifty-plugin-acme-blinds".into(),
            lib: "swifty_plugin_acme_blinds".into(),
            id: "acme-blinds".into(),
            needs_patch: false,
        }
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn the_arguments_are_read() {
        let options = parse(&args(&["../acme", "--id", "acme", "--dry-run"])).unwrap();
        assert_eq!(
            options,
            Options { source: "../acme".into(), package: None, id: Some("acme".into()), dry_run: true }
        );
        assert!(parse(&args(&[])).is_err());
        assert!(parse(&args(&["a", "b"])).is_err());
        assert!(parse(&args(&["a", "--crate"])).is_err());
        assert!(parse(&args(&["a", "--fast"])).is_err());
    }

    #[test]
    fn a_git_url_names_its_crate() {
        assert!(is_git_url("https://example.org/acme-blinds.git") && is_git_url("git@example.org:me/acme.git"));
        assert!(!is_git_url("../acme-blinds") && !is_git_url("plugins/acme"));
        assert_eq!(package_of_url("https://example.org/me/acme-blinds.git"), "swifty-plugin-acme-blinds");
        assert_eq!(package_of_url("https://example.org/me/swifty-plugin-acme/"), "swifty-plugin-acme");
        assert_eq!(package_of_url("git@example.org:me/acme.git"), "swifty-plugin-acme");
        assert_eq!(id_of_package("swifty-plugin-acme-blinds"), "acme-blinds");
    }

    #[test]
    fn the_manifest_id_and_the_api_dependency_are_read() {
        let lib = "pub static MANIFEST: Manifest = Manifest {\n    id: \"acme-blinds\",\n    name: \"Acme\",\n";
        assert_eq!(manifest_id(lib).as_deref(), Some("acme-blinds"));
        assert_eq!(manifest_id("fn f() {}"), None);
        assert!(api_by_version("[dependencies]\nswifty-plugin-api = \"0.1\"\n"));
        assert!(!api_by_version("[dependencies]\nswifty-plugin-api.workspace = true\n"));
        assert!(!api_by_version("[dependencies]\nswifty-plugin-api = { path = \"../api\" }\n"));
        assert!(!api_by_version("[dev-dependencies]\nswifty-plugin-api = \"0.1\"\n"));
    }

    #[test]
    fn a_member_is_a_dependency_a_feature_and_a_line_in_the_list() {
        let root = Path::new("/repo");
        let addition = acme(Source::Member("acme-blinds".into()));
        let dependency = dependency_line(&addition, &root.join("crates/swifty"), root);
        assert_eq!(dependency, "swifty-plugin-acme-blinds = { path = \"../../plugins/acme-blinds\", optional = true }");

        let manifest = edit_swifty_manifest(SWIFTY, &addition, &dependency).unwrap();
        assert!(manifest.contains(
            "swifty-plugin-mock = { workspace = true, optional = true }\n\
             swifty-plugin-acme-blinds = { path = \"../../plugins/acme-blinds\", optional = true }\ntoml"
        ));
        assert!(manifest.contains("all-plugins = [\"wiz\", \"mock\", \"acme-blinds\"]"), "{manifest}");
        assert!(manifest
            .contains("mock = [\"dep:swifty-plugin-mock\"]\nacme-blinds = [\"dep:swifty-plugin-acme-blinds\"]\n"));
        assert!(manifest.ends_with("[dev-dependencies]\ntempfile.workspace = true\n"));
        assert_eq!(
            edit_swifty_manifest(&manifest, &addition, &dependency).unwrap(),
            manifest,
            "a second run changes nothing"
        );

        let list = edit_plugins_rs(PLUGINS, &addition).unwrap();
        assert!(list.contains(
            "        &swifty_plugin_mock::PLUGIN,\n        #[cfg(feature = \"acme-blinds\")]\n        \
             &swifty_plugin_acme_blinds::PLUGIN,\n    ]"
        ));
        assert_eq!(edit_plugins_rs(&list, &addition).unwrap(), list, "a second run changes nothing");
    }

    #[test]
    fn the_list_keeps_the_shape_it_has() {
        let one_line = PLUGINS
            .replace("#[cfg(feature = \"wiz\")]\n        ", "#[cfg(feature = \"wiz\")] ")
            .replace("#[cfg(feature = \"mock\")]\n        ", "#[cfg(feature = \"mock\")] ");
        let list = edit_plugins_rs(&one_line, &acme(Source::Member("acme-blinds".into()))).unwrap();
        assert!(list.contains("        #[cfg(feature = \"acme-blinds\")] &swifty_plugin_acme_blinds::PLUGIN,\n    ]"));
        let empty = "pub fn compiled_in() -> Vec<&'static dyn Plugin> {\n    vec![\n    ]\n}\n";
        let list = edit_plugins_rs(empty, &acme(Source::Member("acme-blinds".into()))).unwrap();
        assert!(list.contains(
            "    vec![\n        #[cfg(feature = \"acme-blinds\")]\n        &swifty_plugin_acme_blinds::PLUGIN,\n    ]"
        ));
        assert!(edit_plugins_rs("//! Empty until the plugin contract exists.\n", &acme(Source::Git("x".into())))
            .unwrap_err()
            .contains("compiled_in"));
    }

    #[test]
    fn arrays_over_several_lines_grow_by_a_line() {
        let manifest =
            SWIFTY.replace("all-plugins = [\"wiz\", \"mock\"]", "all-plugins = [\n    \"wiz\",\n    \"mock\",\n]");
        let addition = acme(Source::Git("https://example.org/acme-blinds.git".into()));
        let dependency = dependency_line(&addition, Path::new("/repo/crates/swifty"), Path::new("/repo"));
        assert_eq!(
            dependency,
            "swifty-plugin-acme-blinds = { git = \"https://example.org/acme-blinds.git\", optional = true }"
        );
        let edited = edit_swifty_manifest(&manifest, &addition, &dependency).unwrap();
        assert!(edited.contains("all-plugins = [\n    \"wiz\",\n    \"mock\",\n    \"acme-blinds\",\n]"), "{edited}");
    }

    #[test]
    fn a_crate_outside_the_checkout_gets_the_api_patched_once() {
        let root = "[workspace]\nmembers = [\"crates/*\"]\n\n[profile.release]\nlto = \"thin\"\n";
        let patched = edit_root_manifest(root);
        assert!(patched.ends_with("[patch.crates-io]\nswifty-plugin-api = { path = \"crates/swifty-plugin-api\" }\n"));
        assert_eq!(edit_root_manifest(&patched), patched);
        let addition = acme(Source::Path(PathBuf::from("/work/acme-blinds")));
        let dependency =
            dependency_line(&addition, Path::new("/work/swifty-home/crates/swifty"), Path::new("/work/swifty-home"));
        assert_eq!(dependency, "swifty-plugin-acme-blinds = { path = \"../../../acme-blinds\", optional = true }");
    }

    #[test]
    fn before_the_list_exists_it_says_so() {
        let no_features = "[package]\nname = \"swifty\"\n\n[dependencies]\nclap.workspace = true\n";
        let addition = acme(Source::Member("acme-blinds".into()));
        let problem = edit_swifty_manifest(no_features, &addition, "x = 1").unwrap_err();
        assert!(problem.contains("[features]"), "{problem}");
    }
}
