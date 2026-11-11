//! `swifty plugin new`: a new plugin crate, written from the template in
//! `templates/plugin`, which is built into this binary.
//!
//! The template is plain files with a few markers:
//!
//! - `{{key}}` is replaced by a value. A line that holds only `{{key}}` takes a
//!   value of several lines, each indented like the marker.
//! - `{{#flag}}` and `{{/flag}}` keep what is between them only when the flag is
//!   set: `local` or `cloud`, and `workspace` or `standalone`. On lines of their
//!   own they enclose whole lines; inside a line they enclose part of it.
//!
//! `classes.toml` holds the code each device class adds.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The template's files: where each goes in the new crate, and its text.
const FILES: &[(&str, &str)] = &[
    ("Cargo.toml", include_str!("../../../templates/plugin/Cargo.toml.tmpl")),
    ("README.md", include_str!("../../../templates/plugin/README.md")),
    ("DEVELOPING.md", include_str!("../../../templates/plugin/DEVELOPING.md")),
    ("src/lib.rs", include_str!("../../../templates/plugin/src/lib.rs")),
    ("src/config.rs", include_str!("../../../templates/plugin/src/config.rs")),
    ("src/protocol.rs", include_str!("../../../templates/plugin/src/protocol.rs")),
    ("src/driver.rs", include_str!("../../../templates/plugin/src/driver.rs")),
    ("src/setup.rs", include_str!("../../../templates/plugin/src/setup.rs")),
    ("tests/common/mod.rs", include_str!("../../../templates/plugin/tests/common/mod.rs")),
    ("tests/mock_device.rs", include_str!("../../../templates/plugin/tests/mock_device.rs")),
    ("tests/conformance.rs", include_str!("../../../templates/plugin/tests/conformance.rs")),
];

/// Files only a crate of its own needs. Inside the workspace, its own CI and
/// format settings apply.
const STANDALONE_FILES: &[(&str, &str)] = &[
    (".github/workflows/ci.yml", include_str!("../../../templates/plugin/.github/workflows/ci.yml")),
    ("rustfmt.toml", include_str!("../../../templates/plugin/rustfmt.toml")),
];

const CLASSES: &str = include_str!("../../../templates/plugin/classes.toml");

/// The port the template's stand-in protocol uses.
const STAND_IN_PORT: &str = "38900";

/// The classes that need more than the template has.
const MACHINE_CLASSES: &[&str] = &["vm", "host", "router"];

/// What one device class adds to a plugin. See `templates/plugin/classes.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClassCode {
    variant: String,
    thing: String,
    does: String,
    capabilities: String,
    #[serde(default)]
    encode: String,
    decode: String,
    initial: String,
    #[serde(default)]
    ops: String,
    #[serde(default)]
    rules: String,
}

fn class_table() -> BTreeMap<String, ClassCode> {
    toml::from_str(CLASSES).expect("templates/plugin/classes.toml is valid; a unit test checks it")
}

/// Every class word `--class` takes.
pub fn known_classes() -> Vec<String> {
    class_table().into_keys().collect()
}

/// What `swifty plugin new` was asked for.
#[derive(Debug, Clone)]
pub struct NewPlugin {
    pub id: String,
    pub classes: Vec<String>,
    pub name: Option<String>,
    pub cloud: bool,
}

/// Whether the crate goes into a swifty-home checkout or stands on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// In `plugins/` of a checkout: workspace dependencies.
    Workspace,
    /// Anywhere else: dependencies by version.
    Standalone,
}

/// A plugin crate, ready to be written.
#[derive(Debug, Clone)]
pub struct Rendered {
    pub crate_name: String,
    /// Each file's path inside the crate, with `/` between parts, and its text.
    pub files: Vec<(String, String)>,
}

/// `[a-z][a-z0-9-]{1,31}`, with no `--` and no `-` at the end.
pub fn check_id(id: &str) -> Result<(), String> {
    let well_formed = id.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && (2..=32).contains(&id.len())
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.contains("--")
        && !id.ends_with('-');
    if well_formed {
        Ok(())
    } else {
        Err(format!(
            "`{id}` cannot be a plugin id. Use 2 to 32 lower-case letters, digits and single dashes, \
             starting with a letter, such as acme-blinds."
        ))
    }
}

/// A name people see. Plain text, so it can go into code and documents as it is.
pub fn check_name(name: &str) -> Result<(), String> {
    let plain = !name.trim().is_empty()
        && name.trim() == name
        && name.chars().count() <= 48
        && name.chars().all(|c| c.is_alphanumeric() || " -.&'()+,".contains(c));
    if plain {
        Ok(())
    } else {
        Err(format!("\"{name}\" cannot be the name. Use up to 48 letters, digits, spaces and - . & ' ( ) + ,"))
    }
}

/// "acme-blinds" to "Acme Blinds".
pub fn default_name(id: &str) -> String {
    id.split('-').filter(|part| !part.is_empty()).map(capitalised).collect::<Vec<_>>().join(" ")
}

/// "acme-blinds" to "AcmeBlinds", for type names.
fn type_name(id: &str) -> String {
    id.split('-').map(capitalised).collect()
}

fn capitalised(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map(|first| first.to_ascii_uppercase().to_string() + chars.as_str()).unwrap_or_default()
}

/// The classes asked for, checked, in order and without repeats.
fn chosen_classes(words: &[String], table: &BTreeMap<String, ClassCode>) -> Result<Vec<String>, String> {
    let mut seen = BTreeSet::new();
    let mut chosen = Vec::new();
    for word in words.iter().map(|word| word.trim().to_ascii_lowercase()).filter(|word| !word.is_empty()) {
        if MACHINE_CLASSES.contains(&word.as_str()) {
            return Err(format!(
                "The template does not cover the class {word} yet: it needs the machine extension and an \
                 approval for every device. Start from plugins/proxmox instead."
            ));
        }
        if !table.contains_key(&word) {
            let known: Vec<&str> = table.keys().map(String::as_str).collect();
            return Err(format!("There is no device class `{word}`. Use one or more of: {}.", known.join(", ")));
        }
        if seen.insert(word.clone()) {
            chosen.push(word);
        }
    }
    if chosen.is_empty() {
        return Err("Name at least one device class, such as --class plug.".to_string());
    }
    Ok(chosen)
}

/// Joins lines, each ending in a newline but the last.
fn lines(parts: impl IntoIterator<Item = String>) -> String {
    parts
        .into_iter()
        .map(|part| part.trim_end().to_string())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Renders the plugin crate. Nothing is written.
pub fn render(request: &NewPlugin, layout: Layout) -> Result<Rendered, String> {
    check_id(&request.id)?;
    let name = request.name.clone().unwrap_or_else(|| default_name(&request.id));
    check_name(&name)?;
    let table = class_table();
    let words = chosen_classes(&request.classes, &table)?;
    let classes: Vec<(&String, &ClassCode)> = words.iter().map(|word| (word, &table[word])).collect();
    let crate_name = format!("swifty-plugin-{}", request.id);
    let first = classes[0].1;

    let encode_arms = lines(classes.iter().map(|(_, code)| code.encode.clone()));
    let encode_body = if encode_arms.is_empty() {
        "let _ = (class, op);\nErr(CANNOT)".to_string()
    } else {
        format!("match (class, op) {{\n{}\n    _ => Err(CANNOT),\n}}", indent(&encode_arms, "    "))
    };
    let device = |word: &str| format!("{word}1");
    let mut values: BTreeMap<&str, String> = BTreeMap::new();
    values.insert("id", request.id.clone());
    values.insert("crate", crate_name.clone());
    values.insert("lib", crate_name.replace('-', "_"));
    values.insert("Type", type_name(&request.id));
    values.insert("name", name.clone());
    values.insert("port", STAND_IN_PORT.to_string());
    values.insert("first_class", words[0].clone());
    values.insert("first_thing", first.thing.clone());
    values.insert("first_variant", first.variant.clone());
    values.insert("classes", lines(classes.iter().map(|(_, code)| format!("DeviceClass::{},", code.variant))));
    values.insert("class_options", array_items(words.iter().map(|word| format!("\"{word}\"")).collect(), 16));
    values.insert(
        "class_table",
        lines(
            classes
                .iter()
                .map(|(word, code)| format!("(\"{word}\", DeviceClass::{}, \"{}\"),", code.variant, code.thing)),
        ),
    );
    values.insert(
        "class_rows",
        lines(classes.iter().map(|(word, code)| format!("| {} (`{word}`) | {} |", code.thing, code.does))),
    );
    values.insert(
        "capability_arms",
        lines(
            classes.iter().map(|(_, code)| format!("DeviceClass::{} => {},", code.variant, code.capabilities.trim())),
        ),
    );
    values.insert("encode_body", encode_body);
    values.insert("decode_arms", lines(classes.iter().map(|(_, code)| code.decode.clone())));
    values.insert("device_table", lines(words.iter().map(|word| format!("(\"{}\", \"{word}\"),", device(word)))));
    values.insert(
        "initial_states",
        lines(classes.iter().map(|(word, code)| format!("(\"{}\", json!({})),", device(word), code.initial.trim()))),
    );
    values.insert(
        "sample_ops",
        lines(classes.iter().flat_map(|(word, code)| {
            let id = device(word);
            code.ops
                .lines()
                .map(str::trim)
                .filter(|op| !op.is_empty())
                .map(move |op| format!("(\"{id}\".to_string(), {op}),"))
                .collect::<Vec<_>>()
        })),
    );
    values.insert("mock_rules", lines(classes.iter().map(|(_, code)| code.rules.clone())));
    values.insert(
        "test_command",
        match layout {
            Layout::Workspace => format!("`cargo test -p {crate_name}`"),
            Layout::Standalone => "`cargo test`".to_string(),
        },
    );
    let mut flags = BTreeMap::new();
    flags.insert("cloud", request.cloud);
    flags.insert("local", !request.cloud);
    flags.insert("workspace", layout == Layout::Workspace);
    flags.insert("standalone", layout == Layout::Standalone);

    let mut files = Vec::new();
    let extra = if layout == Layout::Standalone { STANDALONE_FILES } else { &[] };
    for (path, template) in FILES.iter().chain(extra) {
        let text = fill(template, &values, &flags).map_err(|problem| format!("templates/plugin/{path}: {problem}"))?;
        files.push((path.to_string(), text));
    }
    Ok(Rendered { crate_name, files })
}

/// The items of an array literal, laid out the way rustfmt lays them out with this
/// workspace's settings: one per line, or, when every item is short, as many to a
/// line as fit. `margin` is the indentation of the items.
fn array_items(items: Vec<String>, margin: usize) -> String {
    const MAX_WIDTH: usize = 120;
    const SHORT_ITEM: usize = 10;
    if items.iter().any(|item| item.chars().count() > SHORT_ITEM) {
        return items.iter().map(|item| format!("{item},")).collect::<Vec<_>>().join("\n");
    }
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    for item in items {
        let piece = format!("{item},");
        if !line.is_empty() && margin + line.chars().count() + 1 + piece.chars().count() > MAX_WIDTH {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&piece);
    }
    out.push(line);
    out.join("\n")
}

fn indent(text: &str, by: &str) -> String {
    text.lines()
        .map(|line| if line.is_empty() { String::new() } else { format!("{by}{line}") })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Applies the markers of the module documentation to one template.
pub fn fill(template: &str, values: &BTreeMap<&str, String>, flags: &BTreeMap<&str, bool>) -> Result<String, String> {
    let mut out = String::new();
    let mut open: Vec<(&str, bool)> = Vec::new();
    for line in template.lines() {
        let trimmed = line.trim();
        if let Some(flag) = marker(trimmed, "{{#") {
            let set = *flags.get(flag).ok_or_else(|| format!("unknown flag {flag}"))?;
            open.push((flag, set));
            continue;
        }
        if let Some(flag) = marker(trimmed, "{{/") {
            match open.pop() {
                Some((name, _)) if name == flag => continue,
                _ => return Err(format!("{{{{/{flag}}}}} closes nothing")),
            }
        }
        if !open.iter().all(|(_, set)| *set) {
            continue;
        }
        let line = inline_flags(line, flags)?;
        let trimmed = line.trim();
        if let Some(key) = marker(trimmed, "{{") {
            let value = values.get(key).ok_or_else(|| format!("unknown value {key}"))?;
            let margin = &line[..line.len() - line.trim_start().len()];
            for value_line in value.lines() {
                if value_line.is_empty() {
                    out.push('\n');
                } else {
                    out.push_str(margin);
                    out.push_str(value_line);
                    out.push('\n');
                }
            }
            continue;
        }
        let mut filled = line.clone();
        for (key, value) in values {
            let marker = format!("{{{{{key}}}}}");
            if filled.contains(&marker) {
                if value.contains('\n') {
                    return Err(format!("{key} has several lines and must stand on a line of its own"));
                }
                filled = filled.replace(&marker, value);
            }
        }
        if filled.contains("{{") {
            return Err(format!("a marker is left in: {}", filled.trim()));
        }
        out.push_str(&filled);
        out.push('\n');
    }
    if let Some((flag, _)) = open.last() {
        return Err(format!("{{{{#{flag}}}}} is never closed"));
    }
    Ok(out)
}

/// The word in a line that is only `<start>word}}`.
fn marker<'a>(line: &'a str, start: &str) -> Option<&'a str> {
    let word = line.strip_prefix(start)?.strip_suffix("}}")?;
    (!word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then_some(word)
}

/// `{{#flag}}text{{/flag}}` inside one line.
fn inline_flags(line: &str, flags: &BTreeMap<&str, bool>) -> Result<String, String> {
    let mut rest = line;
    let mut out = String::new();
    while let Some(start) = rest.find("{{#") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 3..];
        let end = after.find("}}").ok_or("an unclosed marker")?;
        let flag = &after[..end];
        let set = *flags.get(flag).ok_or_else(|| format!("unknown flag {flag}"))?;
        let body = &after[end + 2..];
        let close = format!("{{{{/{flag}}}}}");
        let stop = body.find(&close).ok_or_else(|| format!("{{{{#{flag}}}}} is never closed on its line"))?;
        if set {
            out.push_str(&body[..stop]);
        }
        rest = &body[stop + close.len()..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Where the new crate goes, and as which kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub dir: PathBuf,
    pub layout: Layout,
}

/// The swifty-home checkout `path` is in, if it is in one.
pub fn checkout_of(path: &Path) -> Option<PathBuf> {
    let start = std::fs::canonicalize(path).ok()?;
    start.ancestors().find(|dir| is_checkout(dir)).map(plain)
}

/// A path as people write it: Windows' `\\?\` prefix for long paths left out.
fn plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

fn is_checkout(dir: &Path) -> bool {
    dir.join("crates").join("swifty-plugin-api").join("Cargo.toml").is_file()
        && std::fs::read_to_string(dir.join("Cargo.toml")).is_ok_and(|text| text.contains("[workspace]"))
}

/// Decides where the crate goes. `dir` is the directory to create it in; without
/// one it is `plugins/` of the checkout `here` is in, or `here` itself.
pub fn place(id: &str, dir: Option<&Path>, here: &Path) -> Result<Placement, String> {
    let parent = match dir {
        Some(dir) => dir.to_path_buf(),
        None => match checkout_of(here) {
            Some(root) => root.join("plugins"),
            None => here.to_path_buf(),
        },
    };
    let target = parent.join(id);
    if target.exists() {
        return Err(format!("{} already exists. Pick another id, or move it away first.", target.display()));
    }
    std::fs::create_dir_all(&parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    let layout = match checkout_of(&parent) {
        None => Layout::Standalone,
        Some(root) => {
            let plugins = std::fs::canonicalize(root.join("plugins")).ok();
            if plugins.is_some() && plugins == std::fs::canonicalize(&parent).ok() {
                Layout::Workspace
            } else {
                return Err(format!(
                    "{} is inside the swifty-home checkout at {}, where a plugin goes in plugins/. \
                     Leave --dir out, or use --dir {}.",
                    parent.display(),
                    root.display(),
                    root.join("plugins").display()
                ));
            }
        }
    };
    Ok(Placement { dir: target, layout })
}

/// Writes the crate. A crate that could not be written whole is removed again.
pub fn write(rendered: &Rendered, dir: &Path) -> Result<(), String> {
    std::fs::create_dir(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let result = rendered.files.iter().try_for_each(|(path, text)| {
        let file = path.split('/').fold(dir.to_path_buf(), |at, part| at.join(part));
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        std::fs::write(&file, text).map_err(|e| format!("cannot write {}: {e}", file.display()))
    });
    if result.is_err() {
        let _ = std::fs::remove_dir_all(dir);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_class_table_parses_and_names_no_machine_class() {
        let table = class_table();
        assert!(table.len() >= 10);
        for machine in MACHINE_CLASSES {
            assert!(!table.contains_key(*machine));
        }
    }

    #[test]
    fn markers_are_filled_and_flags_keep_or_drop_text() {
        let values: BTreeMap<&str, String> = [("a", "A".to_string()), ("many", "one\ntwo".to_string())].into();
        let flags: BTreeMap<&str, bool> = [("yes", true), ("no", false)].into();
        let filled = fill("x {{a}}\n{{#yes}}\nkept\n{{/yes}}\n{{#no}}\ngone\n{{/no}}\n  {{many}}\nP{{#no}}Q{{/no}}{{#yes}}R{{/yes}}\n", &values, &flags);
        assert_eq!(filled.unwrap(), "x A\nkept\n  one\n  two\nPR\n");
        assert!(fill("{{missing}}\n", &values, &flags).is_err());
        assert!(fill("{{#yes}}\n", &values, &flags).is_err());
        assert!(fill("{{/yes}}\n", &values, &flags).is_err());
        assert!(fill("{{#maybe}}\n{{/maybe}}\n", &values, &flags).is_err());
        assert!(fill("x {{many}} y\n", &values, &flags).is_err());
    }
}
