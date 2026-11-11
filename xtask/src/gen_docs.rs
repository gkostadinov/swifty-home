//! `cargo xtask gen-docs [--check]`: the plugin reference that cannot drift
//!.
//!
//! 1. **The reference.** It writes a scratch crate under the build directory
//!    that depends on `swifty-plugin-api`, the hub (for its catalog) and every
//!    crate under `plugins/`, and runs it. That program is
//!    `xtask/src/gen_docs/generator.rs`; it writes every page of
//!    `docs/plugins/reference/` from the real values. This crate stays std-only,
//!    so `cargo xtask` itself builds in seconds.
//! 2. **Quoted code.** The hand-written pages in `docs/plugins/` quote the
//!    template and the mock plugin instead of copying them. A quote is
//!
//!    ```text
//!    <!-- quote templates/plugin/src/driver.rs "async fn command" "Ok(CommandAck)" -->
//!    (a fenced block, rewritten from the file)
//!    <!-- /quote -->
//!    ```
//!
//!    the lines from the first one that contains the first text to the first one
//!    after it that contains the second, with their common indentation taken off.
//!    A text that starts with `=` must be the whole line instead (`"=    }"` is the
//!    closing brace of a method).
//!
//! Without `--check` it rewrites what differs and removes reference pages that
//! nothing generates any more. With `--check` it changes nothing and fails when
//! anything differs, which is what CI runs.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// One line for `cargo xtask help`.
pub const ABOUT: &str = "Writes docs/plugins/reference/** from the API's samples, the catalog and every manifest, \
                         and refreshes the code quoted in docs/plugins/*.md: gen-docs [--check]";

/// Where the reference lives, from the workspace root.
const REFERENCE: &str = "docs/plugins/reference";
/// Where the hand-written pages live.
const PAGES: &str = "docs/plugins";
/// The generator's source, from the workspace root.
const GENERATOR: &str = "xtask/src/gen_docs/generator.rs";

/// The workspace root: the directory above this crate.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
}

/// The build directory cargo uses for this workspace.
pub fn target_dir(root: &Path) -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) if Path::new(&dir).is_absolute() => PathBuf::from(dir),
        Some(dir) => std::env::current_dir().unwrap_or_else(|_| root.to_path_buf()).join(dir),
        None => root.join("target"),
    }
}

/// A path as cargo and rustc read it on every platform: forward slashes.
pub fn slashed(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    match text.strip_prefix("//?/") {
        Some(rest) => rest.to_string(),
        None => text,
    }
}

/// Runs cargo with these arguments in `dir`, output going straight through.
pub fn cargo_in(dir: &Path, args: &[&str], target: &Path) -> Result<bool, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    println!("> cargo {}", args.join(" "));
    Command::new(cargo)
        .args(args)
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", target)
        .status()
        .map(|status| status.success())
        .map_err(|error| format!("could not start cargo: {error}"))
}

/// A first-party plugin crate: its directory under `plugins/`, its package and
/// its library name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCrate {
    pub dir: String,
    pub package: String,
    pub lib: String,
}

/// The `name` of a `[section]` of a manifest.
pub fn manifest_value(manifest: &str, section: &str, key: &str) -> Option<String> {
    let mut inside = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == format!("[{section}]");
            continue;
        }
        if inside {
            if let Some((name, value)) = line.split_once('=') {
                if name.trim() == key {
                    return Some(value.trim().trim_matches('"').to_string());
                }
            }
        }
    }
    None
}

/// Every crate under `plugins/` that exports `pub static PLUGIN`, by directory.
pub fn plugin_crates(root: &Path) -> Vec<PluginCrate> {
    let Ok(entries) = fs::read_dir(root.join("plugins")) else { return Vec::new() };
    let mut found: Vec<PluginCrate> = entries
        .flatten()
        .filter_map(|entry| {
            let dir = entry.path();
            let manifest = fs::read_to_string(dir.join("Cargo.toml")).ok()?;
            let lib = fs::read_to_string(dir.join("src/lib.rs")).ok()?;
            if !lib.contains("pub static PLUGIN") {
                return None;
            }
            let package = manifest_value(&manifest, "package", "name")?;
            let lib = manifest_value(&manifest, "lib", "name").unwrap_or_else(|| package.replace('-', "_"));
            Some(PluginCrate { dir: entry.file_name().to_string_lossy().into_owned(), package, lib })
        })
        .collect();
    found.sort_by(|a, b| a.dir.cmp(&b.dir));
    found
}

/// The scratch crate's manifest.
fn generator_manifest(root: &Path, plugins: &[PluginCrate]) -> String {
    let at = |path: &str| slashed(&root.join(path));
    let mut text = String::from(
        "# Written by `cargo xtask gen-docs`. Do not edit.\n\
         [package]\nname = \"swifty-gen-docs\"\nversion = \"0.0.0\"\nedition = \"2021\"\npublish = false\n\n\
         [[bin]]\nname = \"swifty-gen-docs\"\npath = \"main.rs\"\n\n[dependencies]\n\
         serde = \"1\"\nserde_json = \"1\"\n",
    );
    text.push_str(&format!("swifty-plugin-api = {{ path = '{}' }}\n", at("crates/swifty-plugin-api")));
    text.push_str(&format!("swifty-hub = {{ path = '{}' }}\n", at("crates/swifty-hub")));
    for plugin in plugins {
        text.push_str(&format!("{} = {{ path = '{}' }}\n", plugin.package, at(&format!("plugins/{}", plugin.dir))));
    }
    text.push_str("\n# Its own workspace, so the repository's does not claim it.\n[workspace]\n");
    text
}

/// The scratch crate's `main.rs`: the generator, and the list of plugins.
fn generator_main(root: &Path, plugins: &[PluginCrate]) -> String {
    let mut text = format!(
        "//! Written by `cargo xtask gen-docs`. Do not edit.\n\n#![deny(warnings)]\n\n\
         #[path = \"{}\"]\nmod generator;\n\nuse swifty_plugin_api::Plugin;\n\n\
         fn main() {{\n    let plugins: Vec<generator::Listed> = vec![\n",
        slashed(&root.join(GENERATOR))
    );
    for plugin in plugins {
        text.push_str(&format!("        (\"{}\", &{}::PLUGIN as &'static dyn Plugin),\n", plugin.dir, plugin.lib));
    }
    text.push_str("    ];\n    std::process::exit(generator::main(&plugins));\n}\n");
    text
}

/// Writes `text` to `path` unless it is already there, so cargo sees no change.
fn write_if_changed(path: &Path, text: &str) -> Result<(), String> {
    if fs::read_to_string(path).is_ok_and(|now| now == text) {
        return Ok(());
    }
    fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Builds and runs the generator. The pages land in the returned directory.
fn run_generator(root: &Path) -> Result<PathBuf, String> {
    let target = target_dir(root);
    let scratch = target.join("xtask").join("gen-docs");
    fs::create_dir_all(&scratch).map_err(|e| format!("cannot create {}: {e}", scratch.display()))?;
    let plugins = plugin_crates(root);
    write_if_changed(&scratch.join("Cargo.toml"), &generator_manifest(root, &plugins))?;
    write_if_changed(&scratch.join("main.rs"), &generator_main(root, &plugins))?;
    // The repository's lock, so the generator links exactly what the workspace does.
    let lock = fs::read_to_string(root.join("Cargo.lock")).map_err(|e| format!("cannot read Cargo.lock: {e}"))?;
    write_if_changed(&scratch.join("Cargo.lock"), &lock)?;
    let out = scratch.join("out");
    if out.exists() {
        fs::remove_dir_all(&out).map_err(|e| format!("cannot clear {}: {e}", out.display()))?;
    }
    let manifest = slashed(&scratch.join("Cargo.toml"));
    let (root_arg, out_arg) = (slashed(root), slashed(&out));
    let args = ["run", "--quiet", "--manifest-path", &manifest, "--", &root_arg, &out_arg];
    match cargo_in(root, &args, &target)? {
        true => Ok(out),
        false => Err("the generator failed; what it printed above says why".to_string()),
    }
}

/// Every file under `dir`, by its path from `dir` with `/` between parts.
fn files_under(dir: &Path) -> BTreeMap<String, String> {
    fn walk(dir: &Path, prefix: &str, out: &mut BTreeMap<String, String>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            let shown = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            if path.is_dir() {
                walk(&path, &shown, out);
            } else if let Ok(text) = fs::read_to_string(&path) {
                out.insert(shown, text.replace("\r\n", "\n"));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, "", &mut out);
    out
}

/// What differs between what was generated and what is committed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Drift {
    /// Generated and not committed.
    pub missing: Vec<String>,
    /// Committed with other text.
    pub changed: Vec<String>,
    /// Committed and no longer generated.
    pub stale: Vec<String>,
}

impl Drift {
    pub fn is_empty(&self) -> bool {
        self.missing.is_empty() && self.changed.is_empty() && self.stale.is_empty()
    }
}

pub fn drift(generated: &BTreeMap<String, String>, committed: &BTreeMap<String, String>) -> Drift {
    let mut drift = Drift::default();
    for (path, text) in generated {
        match committed.get(path) {
            None => drift.missing.push(path.clone()),
            Some(now) if now != text => drift.changed.push(path.clone()),
            Some(_) => {}
        }
    }
    drift.stale = committed.keys().filter(|path| !generated.contains_key(*path)).cloned().collect();
    drift
}

// ---------------------------------------------------------------------------
// Quotes.
// ---------------------------------------------------------------------------

/// A quote marker: the file, the text its first line contains, and the text
/// its last line contains.
#[derive(Debug, PartialEq, Eq)]
pub struct Quote {
    pub path: String,
    pub from: String,
    pub to: String,
}

/// Reads `<!-- quote PATH "FROM" "TO" -->`.
pub fn quote_marker(line: &str) -> Option<Result<Quote, String>> {
    let inner = line.trim().strip_prefix("<!-- quote ")?.strip_suffix("-->")?.trim();
    let (path, rest) = inner.split_once(' ')?;
    let parts: Vec<&str> = rest.split('"').collect();
    // ["", FROM, " ", TO, ""]
    if parts.len() != 5 || !parts[0].trim().is_empty() || !parts[2].trim().is_empty() || !parts[4].trim().is_empty() {
        return Some(Err(format!("a quote marker reads <!-- quote PATH \"FROM\" \"TO\" -->, not: {}", line.trim())));
    }
    Some(Ok(Quote { path: path.to_string(), from: parts[1].to_string(), to: parts[3].to_string() }))
}

/// The lines a quote stands for, without their common indentation.
pub fn quoted(source: &str, quote: &Quote) -> Result<String, String> {
    let lines: Vec<&str> = source.lines().collect();
    let fits = |line: &str, text: &str| match text.strip_prefix('=') {
        Some(whole) => line.trim_end() == whole,
        None => line.contains(text),
    };
    let start = lines
        .iter()
        .position(|line| fits(line, &quote.from))
        .ok_or_else(|| format!("{} has no line with \"{}\"", quote.path, quote.from))?;
    let end = lines[start..]
        .iter()
        .position(|line| fits(line, &quote.to))
        .map(|offset| start + offset)
        .ok_or_else(|| format!("{} has no line with \"{}\" after \"{}\"", quote.path, quote.to, quote.from))?;
    let chosen = &lines[start..=end];
    let margin =
        chosen.iter().filter(|line| !line.trim().is_empty()).map(|line| line.len() - line.trim_start().len()).min();
    let margin = margin.unwrap_or(0);
    Ok(chosen.iter().map(|line| line.get(margin..).unwrap_or("").trim_end()).collect::<Vec<_>>().join("\n"))
}

fn language_of(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("rs") => "rust",
        Some("toml" | "tmpl") => "toml",
        Some("md") => "markdown",
        _ => "text",
    }
}

/// `page` with every quote filled from the files under `root`.
pub fn refresh_quotes(page: &str, read: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    let mut out = Vec::new();
    let mut lines = page.lines();
    while let Some(line) = lines.next() {
        out.push(line.to_string());
        let Some(marker) = quote_marker(line) else { continue };
        let quote = marker?;
        let source = read(&quote.path).ok_or_else(|| format!("cannot read {}", quote.path))?;
        let body = quoted(&source.replace("\r\n", "\n"), &quote)?;
        let fence = if body.contains("```") { "````" } else { "```" };
        out.push(format!("{fence}{}", language_of(&quote.path)));
        out.push(body);
        out.push(fence.to_string());
        let mut closed = false;
        for skipped in lines.by_ref() {
            if skipped.trim() == "<!-- /quote -->" {
                out.push(skipped.to_string());
                closed = true;
                break;
            }
        }
        if !closed {
            return Err(format!("the quote of {} has no <!-- /quote --> after it", quote.path));
        }
    }
    let mut text = out.join("\n");
    if page.ends_with('\n') {
        text.push('\n');
    }
    Ok(text)
}

/// The hand-written pages, refreshed: path from the root, text now, text refreshed.
fn quoted_pages(root: &Path) -> Result<Vec<(String, String, String)>, String> {
    let mut pages = Vec::new();
    let dir = root.join(PAGES);
    let Ok(entries) = fs::read_dir(&dir) else { return Ok(pages) };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    let read = |path: &str| fs::read_to_string(root.join(path)).ok();
    for path in paths.into_iter().filter(|path| path.extension().is_some_and(|ext| ext == "md")) {
        let shown = format!("{PAGES}/{}", path.file_name().unwrap_or_default().to_string_lossy());
        let now = fs::read_to_string(&path).map_err(|e| format!("cannot read {shown}: {e}"))?.replace("\r\n", "\n");
        let refreshed = refresh_quotes(&now, &read).map_err(|problem| format!("{shown}: {problem}"))?;
        pages.push((shown, now, refreshed));
    }
    Ok(pages)
}

/// Runs the command.
pub fn run(args: &[String]) -> ExitCode {
    let check = match args {
        [] => false,
        [flag] if flag == "--check" => true,
        _ => {
            eprintln!("cargo xtask gen-docs: unknown arguments {args:?}\n{ABOUT}");
            return ExitCode::from(2);
        }
    };
    let root = workspace_root();
    let generated = match run_generator(&root) {
        Ok(out) => files_under(&out),
        Err(problem) => {
            eprintln!("cargo xtask gen-docs: {problem}");
            return ExitCode::from(1);
        }
    };
    let reference = root.join(REFERENCE);
    let found = drift(&generated, &files_under(&reference));
    let pages = match quoted_pages(&root) {
        Ok(pages) => pages,
        Err(problem) => {
            eprintln!("cargo xtask gen-docs: {problem}");
            return ExitCode::from(1);
        }
    };
    let stale_quotes: Vec<&(String, String, String)> = pages.iter().filter(|(_, now, new)| now != new).collect();

    if check {
        if found.is_empty() && stale_quotes.is_empty() {
            println!("gen-docs: {REFERENCE} and the quotes in {PAGES} match the code");
            return ExitCode::SUCCESS;
        }
        for (list, what) in
            [(&found.missing, "missing"), (&found.changed, "out of date"), (&found.stale, "no longer generated")]
        {
            for path in list {
                eprintln!("gen-docs: {REFERENCE}/{path} is {what}");
            }
        }
        for (path, _, _) in &stale_quotes {
            eprintln!("gen-docs: {path} quotes code that has changed");
        }
        eprintln!("gen-docs: run `cargo xtask gen-docs` and commit the result");
        return ExitCode::from(1);
    }

    let mut written = 0;
    let write = |path: &Path, text: &str| -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
    };
    let mut result = Ok(());
    for path in found.missing.iter().chain(&found.changed) {
        let file = path.split('/').fold(reference.clone(), |at, part| at.join(part));
        result = result.and_then(|()| write(&file, &generated[path]));
        written += 1;
    }
    for path in &found.stale {
        let file = path.split('/').fold(reference.clone(), |at, part| at.join(part));
        result = result.and_then(|()| fs::remove_file(&file).map_err(|e| format!("cannot remove {path}: {e}")));
    }
    for (path, _, refreshed) in &stale_quotes {
        result = result.and_then(|()| write(&root.join(path), refreshed));
        written += 1;
    }
    if let Err(problem) = result {
        eprintln!("cargo xtask gen-docs: {problem}");
        return ExitCode::from(1);
    }
    println!(
        "gen-docs: {written} file(s) written, {} removed; {} reference pages, {} pages with quotes checked",
        found.stale.len(),
        generated.len(),
        pages.len()
    );
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quote_is_the_lines_between_its_two_texts() {
        let source = "fn a() {}\n\nimpl X {\n    fn one() {\n        1\n    }\n}\n";
        let quote = Quote { path: "x.rs".into(), from: "fn one".into(), to: "    }".into() };
        assert_eq!(quoted(source, &quote).unwrap(), "fn one() {\n    1\n}");
        let missing = Quote { path: "x.rs".into(), from: "fn two".into(), to: "}".into() };
        assert!(quoted(source, &missing).unwrap_err().contains("fn two"));
        // `=` asks for the whole line: the method's own closing brace, not the first `}` inside it.
        let source = "    fn one() {\n        if x {\n        }\n    }\n";
        let whole = Quote { path: "x.rs".into(), from: "fn one".into(), to: "=    }".into() };
        assert_eq!(quoted(source, &whole).unwrap(), "fn one() {\n    if x {\n    }\n}");
    }

    #[test]
    fn quote_markers_are_read_strictly() {
        assert_eq!(
            quote_marker("<!-- quote src/lib.rs \"pub static\" \"};\" -->"),
            Some(Ok(Quote { path: "src/lib.rs".into(), from: "pub static".into(), to: "};".into() }))
        );
        assert!(quote_marker("<!-- quote src/lib.rs pub -->").unwrap().is_err());
        assert_eq!(quote_marker("<!-- /quote -->"), None);
        assert_eq!(quote_marker("plain text"), None);
    }

    #[test]
    fn quotes_are_refreshed_in_place() {
        let page = "Intro\n<!-- quote a.rs \"fn one\" \"}\" -->\n```rust\nold\n```\n<!-- /quote -->\nOutro\n";
        let read = |path: &str| (path == "a.rs").then(|| "fn one() {\n    2\n}\n".to_string());
        let fresh = refresh_quotes(page, &read).unwrap();
        assert_eq!(
            fresh,
            "Intro\n<!-- quote a.rs \"fn one\" \"}\" -->\n```rust\nfn one() {\n    2\n}\n```\n<!-- /quote -->\nOutro\n"
        );
        assert_eq!(refresh_quotes(&fresh, &read).unwrap(), fresh, "a fresh page stays as it is");
        assert!(refresh_quotes("<!-- quote a.rs \"fn one\" \"}\" -->\n", &read).is_err(), "unclosed");
        assert!(refresh_quotes("<!-- quote b.rs \"x\" \"y\" -->\n<!-- /quote -->\n", &read).is_err(), "no file");
    }

    #[test]
    fn drift_names_what_is_missing_changed_and_stale() {
        let generated: BTreeMap<String, String> =
            [("a.md".to_string(), "A".to_string()), ("b.md".to_string(), "B".to_string())].into();
        let committed: BTreeMap<String, String> =
            [("b.md".to_string(), "old".to_string()), ("c.md".to_string(), "C".to_string())].into();
        let found = drift(&generated, &committed);
        assert_eq!(found.missing, vec!["a.md"]);
        assert_eq!(found.changed, vec!["b.md"]);
        assert_eq!(found.stale, vec!["c.md"]);
        assert!(drift(&generated, &generated).is_empty());
    }

    #[test]
    fn manifests_are_read_by_section() {
        let manifest = "[package]\nname = \"swifty-plugin-acme\"\n\n[lib]\nname = \"acme\"\npath = \"src/lib.rs\"\n";
        assert_eq!(manifest_value(manifest, "package", "name").as_deref(), Some("swifty-plugin-acme"));
        assert_eq!(manifest_value(manifest, "lib", "name").as_deref(), Some("acme"));
        assert_eq!(manifest_value(manifest, "lib", "crate-type"), None);
    }

    #[test]
    fn the_generator_crate_names_every_plugin() {
        let root = Path::new("/repo");
        let plugins = vec![PluginCrate {
            dir: "wiz".into(),
            package: "swifty-plugin-wiz".into(),
            lib: "swifty_plugin_wiz".into(),
        }];
        let manifest = generator_manifest(root, &plugins);
        assert!(manifest.contains("swifty-plugin-wiz = { path = '/repo/plugins/wiz' }"), "{manifest}");
        assert!(manifest.contains("swifty-hub = { path = '/repo/crates/swifty-hub' }"));
        assert!(manifest.trim_end().ends_with("[workspace]"));
        let main = generator_main(root, &plugins);
        assert!(main.contains("#[path = \"/repo/xtask/src/gen_docs/generator.rs\"]"), "{main}");
        assert!(main.contains("(\"wiz\", &swifty_plugin_wiz::PLUGIN as &'static dyn Plugin),"));
        assert_eq!(slashed(Path::new(r"C:\a\b")), "C:/a/b");
    }

    #[test]
    fn every_first_party_plugin_is_found() {
        let found = plugin_crates(&workspace_root());
        assert!(found.iter().any(|plugin| plugin.dir == "mock" && plugin.lib == "swifty_plugin_mock"), "{found:?}");
        assert!(found.iter().all(|plugin| plugin.package.starts_with("swifty-plugin-")));
    }
}
