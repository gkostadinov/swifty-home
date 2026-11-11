//! The program behind `cargo xtask gen-docs`. It writes `docs/plugins/reference/`
//! from the real values: the samples of every contract type
//! (`swifty_plugin_api::Examples`), the hub's static table of standard actions
//! (`swifty_hub::catalog::CATALOG`), every first-party plugin's manifest, and the
//! doc comments of the contract's source files.
//!
//! This file is not part of xtask, which stays std-only. `gen-docs` writes a
//! scratch crate next to the build output that depends on the API crate, the
//! hub and every crate under `plugins/`, includes this file as a module, and
//! runs it with two arguments: the repository root and the directory to write
//! into. Whatever it prints on failure names the thing to fix.
//!
//! The text here that is not taken from the code is what code cannot say: how
//! the hub confirms each operation, how the app and the terminal draw each setup
//! step, and what the hub does with each error. Each such table is keyed by wire
//! name and checked against the samples, so a new variant stops the generator
//! until it has its line.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};
use swifty_hub::catalog::{self, ActionDef, Executor, Targets, CATALOG};
use swifty_hub::runtime::index::DeviceIndex;
use swifty_plugin_api::{
    CapKind, Capability, ConfigField, ConfigProblem, CustomAction, DeviceClass, DeviceError, Examples, Expect,
    FieldKind, FieldType, Health, Manifest, NetUse, Op, ParamDef, ParamKind, Plugin, PluginError, Reach, Readiness,
    SetupInput, SetupStep, State, Tier, API_VERSION,
};

/// A first-party plugin: the directory under `plugins/` and the plugin.
pub type Listed = (&'static str, &'static dyn Plugin);

/// Runs the generator. Returns the process exit code.
pub fn main(plugins: &[Listed]) -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [root, out] = args.as_slice() else {
        eprintln!("usage: swifty-gen-docs <repository root> <directory to write>");
        return 2;
    };
    match generate(Path::new(root), plugins).and_then(|pages| write(Path::new(out), &pages)) {
        Ok(()) => 0,
        Err(problem) => {
            eprintln!("gen-docs: {problem}");
            1
        }
    }
}

/// Every page, by its path under `reference/`.
fn generate(root: &Path, plugins: &[Listed]) -> Result<BTreeMap<String, String>, String> {
    let sources = Sources::load(root)?;
    let mut plugins = plugins.to_vec();
    plugins.sort_by_key(|(_, plugin)| plugin.manifest().id);
    let catalog = Catalog::read()?;
    let mut pages = BTreeMap::new();
    pages.insert("README.md".to_string(), index_page(&plugins));
    pages.insert("capabilities.md".to_string(), capabilities_page(&sources, &catalog)?);
    pages.insert("ops.md".to_string(), ops_page(&sources, &catalog)?);
    pages.insert("states.md".to_string(), states_page(&sources)?);
    pages.insert("manifest.md".to_string(), manifest_page(&sources, &plugins)?);
    pages.insert("setup-steps.md".to_string(), setup_page(&sources)?);
    pages.insert("errors.md".to_string(), errors_page(&sources)?);
    for (dir, plugin) in &plugins {
        pages.insert(format!("plugins/{}.md", plugin.manifest().id), plugin_page(dir, *plugin));
    }
    Ok(pages)
}

fn write(out: &Path, pages: &BTreeMap<String, String>) -> Result<(), String> {
    for (path, text) in pages {
        let file = path.split('/').fold(out.to_path_buf(), |at, part| at.join(part));
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        std::fs::write(&file, text).map_err(|e| format!("cannot write {}: {e}", file.display()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Text that the code cannot say, keyed by wire name.
// ---------------------------------------------------------------------------

/// When the hub counts each operation as done, in words. The rule itself is
/// `Op::is_confirmed_by` in `crates/swifty-plugin-api/src/confirm.rs`.
const CONFIRMED_WHEN: &[(&str, &str)] = &[
    ("light_power", "`light.on` equals `on`."),
    (
        "light_brightness",
        "The light is on and `bri` equals `percent` (a request under 10 is also met by 10, the lowest many lights go). \
         `percent` 0 is also met by the light being off.",
    ),
    ("light_hue", "The light is on, `color` is true and `hue` equals `hue`."),
    (
        "light_set",
        "The light is on, `color` is true, `bri` matches as for `light_brightness` and `hue` equals `hue`. \
         `percent` 0 is also met by the light being off.",
    ),
    ("plug_power", "`plug.on` equals `on`."),
    ("aircon_power", "`aircon.on` equals `on`."),
    ("aircon_mode", "`aircon.mode` is `mode`."),
    ("aircon_temp", "`aircon.target` is `celsius`."),
    ("aircon_fan", "`aircon.fan` is `fan`."),
    ("volume", "`speaker.vol` equals `level`."),
    ("volume_cap", "`speaker.vol` is at most `max`."),
    ("mute", "`speaker.mute` equals `muted`."),
    ("night_mode", "`speaker.night` is present and equals `on`."),
    ("play", "`speaker.playing` is true."),
    ("pause", "`speaker.playing` is false."),
    ("tv_power", "`tv.on` equals `on`."),
    ("tv_input", "The TV is on and `tv.input` equals `input`."),
    ("tv_key", "Never: a key press leaves nothing to read back. The run ends `sent` once the TV accepts it."),
    ("pet_feed", "`feeder.portions` is greater than before the command."),
    ("litter_clean", "`litter.busy` is true, or `litter.cycles` is greater than before the command."),
    ("print_start", "`printer.state` is `\"printing\"`."),
    ("print_pause", "`printer.state` is `\"paused\"`."),
    ("print_resume", "`printer.state` is `\"printing\"`."),
    ("print_emergency_stop", "`printer.state` is `\"stopped\"`."),
    ("vm_start", "`vm.state` is `\"running\"`."),
    ("vm_shutdown", "`vm.state` is `\"stopped\"`."),
    ("vm_reboot", "`vm.state` is `\"running\"` and `vm.uptime_s` is lower than before the command."),
    ("vm_force_stop", "`vm.state` is `\"stopped\"`."),
    ("host_shutdown", "`host.up` is false."),
    ("host_reboot", "`host.up` is true and `host.uptime_s` is lower than before the command."),
    ("wireguard_restart", "`router.wireguard_up` is true and `router.uptime_s` is lower than before the command."),
    ("custom", "The action's `Expect`, from the manifest, holds on a `custom` state (see manifest.md)."),
];

/// The class of device each operation is for.
const OP_CLASS: &[(&str, &str)] = &[
    ("light_power", "light"),
    ("light_brightness", "light"),
    ("light_hue", "light"),
    ("light_set", "light"),
    ("plug_power", "plug"),
    ("aircon_power", "aircon"),
    ("aircon_mode", "aircon"),
    ("aircon_temp", "aircon"),
    ("aircon_fan", "aircon"),
    ("volume", "speaker"),
    ("volume_cap", "speaker"),
    ("mute", "speaker"),
    ("night_mode", "speaker"),
    ("play", "speaker"),
    ("pause", "speaker"),
    ("tv_power", "tv"),
    ("tv_input", "tv"),
    ("tv_key", "tv"),
    ("pet_feed", "feeder"),
    ("litter_clean", "litter"),
    ("print_start", "printer"),
    ("print_pause", "printer"),
    ("print_resume", "printer"),
    ("print_emergency_stop", "printer"),
    ("vm_start", "vm"),
    ("vm_shutdown", "vm"),
    ("vm_reboot", "vm"),
    ("vm_force_stop", "vm"),
    ("host_shutdown", "host"),
    ("host_reboot", "host"),
    ("wireguard_restart", "router"),
    ("custom", "the action's class"),
];

/// Standard actions that reach several devices: what each sends, as words and
/// as operations. Their plan needs the house, so it is not run here.
const GROUP_ACTIONS: &[(&str, &str, &[&str])] = &[
    ("lights.power", "`light_power` on every light with `power`", &["light_power"]),
    ("lights.brightness", "`light_brightness` on every light with `brightness`", &["light_brightness"]),
    (
        "scene.apply",
        "On every light, by what it can do: `light_set`, `light_brightness`, or `light_power`",
        &["light_set", "light_brightness", "light_power"],
    ),
    (
        "scene.movie",
        "As `scene.apply`, then `tv_power` and `tv_input` on the first TV, and `volume_cap` on the first speaker \
         with a volume",
        &["light_set", "light_brightness", "light_power", "tv_power", "tv_input", "volume_cap"],
    ),
    ("room.power", "`light_power` on every light in the room", &["light_power"]),
    ("room.brightness", "`light_brightness` on every light in the room", &["light_brightness"]),
    (
        "room.scene",
        "On every light in the room, as `scene.apply`; Movie also the TV and the speaker",
        &["light_set", "light_brightness", "light_power", "tv_power", "tv_input", "volume_cap"],
    ),
];

/// How the web wizard and `swifty plugin add` show each step, and what comes back.
const STEP_DRAWN: &[(&str, &str, &str)] = &[
    (
        "form",
        "The web wizard shows the title, the text and one input per field, in order, each with its label and help. \
         `swifty plugin add` asks for each field in turn and offers the prefill as the default; a `secret` field is \
         read without echo.",
        "`values`, with one entry per field key. `back` goes back.",
    ),
    (
        "wait",
        "The title and the text, with a countdown of `timeout_s`. Nobody types anything: the hub sends `poll` every \
         `poll_ms` until the flow returns another step, and gives up after `timeout_s`.",
        "`poll`, sent by the hub. `back` when the person gives up.",
    ),
    (
        "choose",
        "The title, the text and the options, with `detail` under each label: tick boxes when `multiple`, otherwise \
         one choice. At least `min` must be picked. The terminal numbers the options and reads the numbers.",
        "`chosen`, with the `id` of each option picked. `back` goes back.",
    ),
    (
        "external",
        "The title and the text, with `link` as a button that opens in a new tab (the terminal prints it), then the \
         fields as in `form`.",
        "`values`, as for `form`.",
    ),
    (
        "done",
        "Only `summary`. The hub validates `config` with the plugin, stores it, the secrets and the devices to adopt, \
         in one transaction, and audits the slot names, never a value. The config and the secrets never reach the \
         app or the terminal.",
        "Nothing. The flow is over.",
    ),
    (
        "failed",
        "`reason`, with a button to try the previous step again when `retry` is true.",
        "When `retry` is true, the input of the previous step again. Otherwise nothing.",
    ),
];

/// How each field type is drawn, and the `Scalar` its value comes back as.
const FIELD_DRAWN: &[(&str, &str, &str)] = &[
    ("text", "A one-line text box of at most `max_len` characters.", "text"),
    ("host", "A text box for an IP address or a host name.", "text"),
    ("url", "A text box for a URL.", "text"),
    ("host_port", "A text box for `host:port`.", "text"),
    ("port", "A number box from 1 to 65535.", "int (the terminal may send text; accept both)"),
    ("int", "A number box from `min` to `max`.", "int (the terminal may send text; accept both)"),
    ("bool", "A tick box. The terminal asks yes or no.", "bool"),
    ("choice", "A list to pick one of `options` from.", "text, one of the options"),
    ("mac", "A text box for a MAC address such as `aa:bb:cc:dd:ee:ff`.", "text"),
    ("secret", "A masked box, never prefilled. The value is sent once, and never echoed, logged or audited.", "text"),
    ("multiline", "A box for a longer paste of at most `max_len` characters, such as a CA certificate.", "text"),
];

/// What the hub does with each device error.
const ERROR_HANDLED: &[(&str, &str)] = &[
    (
        "Timeout",
        "Silence: the device may not have heard. An idempotent action is sent once more within its timeout; any other \
         action never is. The run ends `no_response`. While the hub reads back for confirmation, it keeps reading.",
    ),
    ("Unreachable", "Silence, as `Timeout`."),
    (
        "Rejected",
        "The device answered and said no, or answered with nonsense. The run ends `failed` with this text. During \
         read-back it ends the run `failed` too, unless the operation takes the device away (a host shutdown).",
    ),
    (
        "Disabled",
        "The driver will not do this: the device is not one of this instance's, or cannot do it. The run ends `failed`. \
         Anything the driver refuses this way should also be refused by `Plugin::refusal`, so that dry-run says the \
         same (conformance check 5).",
    ),
];

/// What the hub does with each readiness.
const READINESS_HANDLED: &[(&str, &str)] = &[
    ("ready", "The instance is loaded: in dry-run as a stand-in, when live through `Plugin::connect`."),
    (
        "needs_setup",
        "Its devices are listed, disabled, with this reason, and `connect` is not called. The app offers the setup.",
    ),
    ("disabled", "The same listing, with this reason. Nothing is called until the config changes."),
];

/// What the conformance suite and the hub hold each manifest field to.
const MANIFEST_RULES: &[(&str, &str)] = &[
    ("id", "`[a-z][a-z0-9-]{1,31}`. Unique among plugins. Custom action ids start with it."),
    ("name", "Not empty."),
    ("version", "`env!(\"CARGO_PKG_VERSION\")`, so it is always the crate's version."),
    ("api", "`API_VERSION`. A plugin built for another version is not loaded."),
    ("summary", "Not empty. One sentence."),
    (
        "reach",
        "`Local`, or `Cloud` with `official: false` when the vendor does not document the API for third parties.",
    ),
    ("network", "Every port, broadcast and cloud host it opens a socket to. Shown before setup."),
    ("classes", "At least one. Every device, custom action and noun uses one of these."),
    ("config_version", "Start at 1. Bump it when the stored config changes shape, and implement `Plugin::migrate`."),
    (
        "config",
        "Keys `[a-z][a-z0-9_]*`, each once. A field is required or has a default, not both. A choice's default is one \
         of its options. The typed config must accept every key and refuse an unknown one.",
    ),
    ("secrets", "Slots `name` or `name/{key}` in `[a-z0-9_]`, each once, and never also a config key."),
    ("actions", "Ids `<plugin id>.<verb>`. Never `read` or `safe_direction`. Class one of `classes`."),
    ("vocabulary", "Lower-case words. Never a word the voice grammar acts on, and never a word mapped to itself."),
    ("nouns", "Lower-case words, not grammar words, each naming a class of `classes`."),
    ("multi_instance", "True when a second instance makes sense (a second bridge, a second account)."),
];

// ---------------------------------------------------------------------------
// Doc comments, read from the contract's source.
// ---------------------------------------------------------------------------

/// A field or variant, with its doc comment.
#[derive(Debug, Clone, Default)]
struct Member {
    name: String,
    ty: String,
    doc: String,
    fields: Vec<Member>,
}

/// A struct or an enum, with its doc comment and its members.
#[derive(Debug, Clone, Default)]
struct Item {
    doc: String,
    members: Vec<Member>,
}

/// The source of the contract.
struct Sources {
    files: Vec<(String, String)>,
}

impl Sources {
    fn load(root: &Path) -> Result<Self, String> {
        let mut files = Vec::new();
        let api = root.join("crates/swifty-plugin-api/src");
        let entries = std::fs::read_dir(&api).map_err(|e| format!("cannot read {}: {e}", api.display()))?;
        let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        paths.push(root.join("crates/swifty-net/src/error.rs"));
        paths.sort();
        for path in paths.into_iter().filter(|path| path.extension().is_some_and(|ext| ext == "rs")) {
            let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            files.push((path.display().to_string(), text.replace("\r\n", "\n")));
        }
        Ok(Self { files })
    }

    /// The struct or enum called `name`.
    fn item(&self, name: &str) -> Result<Item, String> {
        let heads = [
            format!("pub enum {name} {{"),
            format!("pub struct {name} {{"),
            format!("pub struct {name}<'a> {{"),
            format!("pub struct {name}("),
            format!("pub struct {name};"),
        ];
        for (_, text) in &self.files {
            let lines: Vec<&str> = text.lines().collect();
            let Some(start) = lines.iter().position(|line| heads.iter().any(|head| line.starts_with(head.as_str())))
            else {
                continue;
            };
            let mut doc = Vec::new();
            for line in lines[..start].iter().rev() {
                if let Some(text) = line.strip_prefix("///") {
                    doc.push(text.trim().to_string());
                } else if !line.starts_with("#[") {
                    break;
                }
            }
            doc.reverse();
            let members = if lines[start].ends_with('{') { members_of(&lines[start + 1..]) } else { Vec::new() };
            return Ok(Item { doc: joined(&doc), members });
        }
        Err(format!("the contract's source has no `pub struct {name}` or `pub enum {name}`"))
    }
}

/// Doc lines as one paragraph per blank line.
fn joined(lines: &[String]) -> String {
    let mut out = String::new();
    for line in lines {
        if line.is_empty() {
            out.push_str("\n\n");
        } else {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push(' ');
            }
            out.push_str(line);
        }
    }
    without_links(out.trim())
}

/// Rustdoc's intra-doc links (`` [`Op`] ``, `` [`X`](crate::X) ``) as plain code.
fn without_links(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("[`") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let Some(end) = after.find("`]") else {
            out.push_str(&rest[at..]);
            return out;
        };
        out.push_str(&after[..end + 1]);
        rest = &after[end + 2..];
        if rest.starts_with('(') {
            rest = rest.find(')').map_or("", |close| &rest[close + 1..]);
        }
    }
    out.push_str(rest);
    out
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The fields or variants of a block that starts after its opening line.
fn members_of(lines: &[&str]) -> Vec<Member> {
    let mut members: Vec<Member> = Vec::new();
    let mut doc: Vec<String> = Vec::new();
    let mut open = false;
    for line in lines {
        let indent = indent_of(line);
        let text = line.trim();
        if indent == 0 && text.starts_with('}') {
            break;
        }
        if text.is_empty() {
            continue;
        }
        if let Some(rest) = text.strip_prefix("///") {
            doc.push(rest.trim().to_string());
            continue;
        }
        if text.starts_with("#[") || text.starts_with("//") {
            continue;
        }
        if indent == 4 && (text == "}," || text == "}" || text.starts_with("})")) {
            open = false;
            continue;
        }
        if indent >= 8 && open {
            if let Some(field) = field_of(text, &doc) {
                if let Some(last) = members.last_mut() {
                    last.fields.push(field);
                }
            }
            doc.clear();
            continue;
        }
        if indent == 4 {
            if let Some(field) = text.strip_prefix("pub ").and_then(|rest| field_of(rest, &doc)) {
                members.push(field);
            } else {
                let name: String = text.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                let ty = text.split_once('(').map(|(_, rest)| rest.trim_end_matches([',', ')']).to_string());
                let mut member = Member { name, ty: ty.unwrap_or_default(), doc: joined(&doc), fields: Vec::new() };
                if let Some((_, inline)) = text.split_once('{') {
                    if text.ends_with('{') {
                        open = true;
                    } else {
                        // `Variant { a: u8, b: u16 },` on one line.
                        for part in inline.trim_end_matches([',', '}', ' ']).split(',') {
                            if let Some(field) = field_of(part.trim(), &[]) {
                                member.fields.push(field);
                            }
                        }
                    }
                }
                members.push(member);
            }
            doc.clear();
        }
    }
    members
}

/// `name: Type,` with its doc lines.
fn field_of(text: &str, doc: &[String]) -> Option<Member> {
    let (name, ty) = text.split_once(':')?;
    let name = name.trim();
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    Some(Member {
        name: name.to_string(),
        ty: ty.trim().trim_end_matches(',').to_string(),
        doc: joined(doc),
        fields: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// The catalog.
// ---------------------------------------------------------------------------

/// One standard action as the reference shows it.
struct Action {
    def: &'static ActionDef,
    /// The operations it sends, by wire name. Empty for a read or a hub action.
    ops: Vec<String>,
    /// What it sends, when that is more than one operation for one device.
    group: Option<&'static str>,
}

struct Catalog {
    actions: Vec<Action>,
}

impl Catalog {
    fn read() -> Result<Self, String> {
        let index = DeviceIndex::default();
        let context = catalog::Context { index: &index, lights: &[], scene: None };
        let mut actions = Vec::new();
        for def in CATALOG {
            let mut action = Action { def, ops: Vec::new(), group: None };
            if def.executor == Executor::Device {
                if let Some((_, words, ops)) = GROUP_ACTIONS.iter().find(|(id, _, _)| *id == def.id) {
                    action.group = Some(*words);
                    action.ops = ops.iter().map(|op| op.to_string()).collect();
                } else {
                    let params = sample_params(def.params);
                    let steps = catalog::plan(def, "sample", &params, &context).map_err(|invalid| {
                        format!(
                            "{} sends nothing for one device ({}). If it reaches several devices, add it to \
                             GROUP_ACTIONS in xtask/src/gen_docs/generator.rs",
                            def.id, invalid.0
                        )
                    })?;
                    for step in steps {
                        action.ops.push(tag(&step.op, "op"));
                    }
                }
            }
            actions.push(action);
        }
        for (id, _, _) in GROUP_ACTIONS {
            if !CATALOG.iter().any(|def| def.id == *id) {
                return Err(format!("GROUP_ACTIONS names {id}, which the catalog no longer has"));
            }
        }
        Ok(Self { actions })
    }

    fn device_actions(&self) -> impl Iterator<Item = &Action> {
        self.actions.iter().filter(|action| matches!(action.def.executor, Executor::Device | Executor::Read))
    }

    fn hub_actions(&self) -> impl Iterator<Item = &Action> {
        self.actions.iter().filter(|action| !matches!(action.def.executor, Executor::Device | Executor::Read))
    }

    /// The actions that send this operation.
    fn sending(&self, op: &str) -> Vec<&'static str> {
        self.actions
            .iter()
            .filter(|action| action.ops.iter().any(|sent| sent == op))
            .map(|action| action.def.id)
            .collect()
    }
}

/// Parameters that pass the schema, to see which operation an action plans.
fn sample_params(params: &[ParamDef]) -> Value {
    let mut out = serde_json::Map::new();
    for param in params {
        let value = match param.kind {
            ParamKind::Bool => json!(true),
            ParamKind::Int { min, max } => json!(if min <= 1 && max >= 1 { 1 } else { min }),
            ParamKind::Enum(options) => json!(options.first().copied().unwrap_or_default()),
            ParamKind::Label { .. } => json!("Sample"),
        };
        out.insert(param.name.to_string(), value);
    }
    Value::Object(out)
}

// ---------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------

fn compact(value: &impl Serialize) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

fn pretty(value: &impl Serialize) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".to_string())
}

/// A variant's wire name, read from the `key` of its JSON.
fn tag(value: &impl Serialize, key: &str) -> String {
    let json = serde_json::to_value(value).unwrap_or(Value::Null);
    match &json {
        Value::String(name) => name.clone(),
        Value::Object(map) => match map.get(key) {
            Some(Value::String(name)) => name.clone(),
            _ => map.keys().next().cloned().unwrap_or_default(),
        },
        _ => String::new(),
    }
}

/// The wire name of a class.
fn class_name(class: DeviceClass) -> String {
    tag(&class, "")
}

/// Text in a table cell.
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace("\n\n", "<br>").replace('\n', " ")
}

/// Inline code.
fn code(text: &str) -> String {
    if text.contains('`') {
        format!("`` {text} ``")
    } else {
        format!("`{text}`")
    }
}

/// `CamelCase` as `snake_case`, the way serde's `rename_all` spells it.
fn snake(name: &str) -> String {
    let mut out = String::new();
    for (index, c) in name.chars().enumerate() {
        if c.is_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn lookup<'a>(table: &'a [(&str, &str)], key: &str, what: &str) -> Result<&'a str, String> {
    table.iter().find(|(name, _)| *name == key).map(|(_, text)| *text).ok_or_else(|| {
        format!("`{key}` has no line in {what} in xtask/src/gen_docs/generator.rs; add one that says it in words")
    })
}

fn tier_text(tier: Tier) -> String {
    format!("{} {}", tier.code(), tier.name())
}

fn param_text(kind: &ParamKind) -> String {
    match kind {
        ParamKind::Bool => "`true` or `false`".to_string(),
        ParamKind::Int { min, max } => format!("whole number, {min} to {max}"),
        ParamKind::Enum(options) => {
            format!("one of {}", options.iter().map(|option| code(option)).collect::<Vec<_>>().join(", "))
        }
        ParamKind::Label { max_len } => format!("plain text, at most {max_len} characters"),
    }
}

fn params_text(params: &[ParamDef]) -> String {
    if params.is_empty() {
        return "none".to_string();
    }
    params
        .iter()
        .map(|param| {
            let optional = if param.required { "" } else { ", optional" };
            format!("{}: {}{optional}", code(param.name), param_text(&param.kind))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn targets_text(targets: Targets) -> String {
    match targets {
        Targets::Fixed(words) => {
            format!("the word {}", words.iter().map(|word| code(word)).collect::<Vec<_>>().join(" or "))
        }
        Targets::Class(class) | Targets::Capable(class, _) => code(&class_name(class)),
        Targets::Readable => "any device".to_string(),
        Targets::Pending => "a device waiting for approval".to_string(),
        Targets::Room => "a room".to_string(),
        Targets::User => "a family member".to_string(),
        Targets::Credential => "a passkey".to_string(),
        Targets::NewCredential => "the word `new`".to_string(),
        Targets::FurtherHost => "a further host".to_string(),
        Targets::Instance => "an integration (a plugin instance), by its id".to_string(),
        Targets::NewInstance => "the word `new`".to_string(),
        other => code(other.kind_name()),
    }
}

/// A table of a struct's fields: name, type, meaning.
fn fields_table(out: &mut String, fields: &[Member]) {
    if fields.is_empty() {
        return;
    }
    out.push_str("| Field | Type | Meaning |\n| --- | --- | --- |\n");
    for field in fields {
        let _ = writeln!(out, "| {} | {} | {} |", code(&field.name), code(&field.ty), cell(&field.doc));
    }
    out.push('\n');
}

const GENERATED: &str =
    "<!-- Generated by `cargo xtask gen-docs`. Do not edit: change the code and run it again. -->\n\n";

// ---------------------------------------------------------------------------
// The pages.
// ---------------------------------------------------------------------------

fn index_page(plugins: &[Listed]) -> String {
    let mut out = String::from(GENERATED);
    out.push_str("# Plugin API reference\n\n");
    let _ = writeln!(
        out,
        "Plugin API version **{API_VERSION}**. Everything here is written by `cargo xtask gen-docs` from the code \
         itself: the samples of every contract type in `crates/swifty-plugin-api`, its doc comments, the hub's table \
         of standard actions in `crates/swifty-hub/src/catalog/actions.rs`, and every manifest under `plugins/`. CI \
         runs `cargo xtask gen-docs --check`, which fails when these pages and the code disagree. To change a page, \
         change the code and run `cargo xtask gen-docs`.\n"
    );
    out.push_str(
        "The rustdoc of `swifty-plugin-api` (`cargo doc -p swifty-plugin-api --open`) is the API reference for the \
         Rust side. These pages add the wire form of every value, the catalog, and what the hub does with what a \
         plugin returns.\n\n",
    );
    out.push_str("| Page | What it holds |\n| --- | --- |\n");
    for (page, what) in [
        (
            "capabilities.md",
            "Device classes, capabilities, every standard action with its operation, tier and how it is confirmed, \
             and the floors for custom actions",
        ),
        ("ops.md", "Every `Op` variant: its fields, its JSON, which actions send it, and when it counts as done"),
        ("states.md", "Every `State` variant: its fields, its JSON, and the class that reports it"),
        ("manifest.md", "Every manifest field, `Reach`, `NetUse`, `FieldKind`, `SecretSlot`, `CustomAction`, `Expect`"),
        (
            "setup-steps.md",
            "Every setup step, field type and input, with its JSON and how the app and the terminal draw it",
        ),
        (
            "errors.md",
            "`DeviceError`, `PluginError`, `ConfigProblem`, `Readiness`, `Health`, and what the hub does with each",
        ),
    ] {
        let _ = writeln!(out, "| [{page}]({page}) | {what} |");
    }
    out.push_str(
        "\n## First-party plugins\n\n| Plugin | Id | Classes | Reach | Page |\n| --- | --- | --- | --- | --- |\n",
    );
    for (_, plugin) in plugins {
        let manifest = plugin.manifest();
        let classes: Vec<String> = manifest.classes.iter().map(|class| code(&class_name(*class))).collect();
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | [plugins/{id}.md](plugins/{id}.md) |",
            cell(manifest.name),
            code(manifest.id),
            classes.join(", "),
            reach_text(&manifest.reach),
            id = manifest.id,
        );
    }
    out
}

fn reach_text(reach: &Reach) -> String {
    match reach {
        Reach::Local => "local network".to_string(),
        Reach::Cloud { vendor, official: true } => format!("{vendor}'s cloud"),
        Reach::Cloud { vendor, official: false } => format!("{vendor}'s cloud, not documented for third parties"),
    }
}

/// The `State` kind of each class.
fn state_of_class(class: DeviceClass) -> Result<String, String> {
    let name = class_name(class);
    let kinds: BTreeSet<String> = State::examples().iter().map(|state| tag(state, "kind")).collect();
    if kinds.contains(&name) {
        Ok(name)
    } else if class == DeviceClass::Other {
        Ok("custom".to_string())
    } else {
        Err(format!("no `State` variant for the class {name}; add a sample or map it in state_of_class"))
    }
}

fn capabilities_page(sources: &Sources, catalog: &Catalog) -> Result<String, String> {
    let mut out = String::from(GENERATED);
    out.push_str("# Classes, capabilities and actions\n\n");
    out.push_str(
        "A device has one **class** and a list of **capabilities**. The class picks its card in the app, who may see \
         it by default, the nouns voice understands, and which standard actions can name it. The capabilities narrow \
         those actions to what this device can do, and their lists (inputs, modes, portions) narrow the values.\n\n\
         The hub owns the actions. A plugin never sees an action, a tier or a person: it receives one `Op` for one \
         device, already checked against the action's parameter ranges, and first answers `Plugin::refusal` for it. \
         Map your device onto these classes, capabilities and operations. Only when nothing fits, declare a custom \
         action (last section).\n\n",
    );

    // Classes.
    let classes = sources.item("DeviceClass")?;
    out.push_str("## Device classes\n\n");
    out.push_str("| Class | Meaning | State it reports | Seen by default by | Approval | Standard actions |\n");
    out.push_str("| --- | --- | --- | --- | --- | --- |\n");
    for class in DeviceClass::examples() {
        let name = class_name(class);
        let doc = classes.members.iter().find(|member| snake(&member.name) == name).map_or("", |m| m.doc.as_str());
        let audience = tag(&class.default_audience(), "");
        let approval = if class.needs_approval() { "pending until a passkey approves it" } else { "no" };
        let actions: Vec<String> = catalog
            .device_actions()
            .filter(|action| action.def.targets.class() == Some(class))
            .map(|action| code(action.def.id))
            .collect();
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} |",
            code(&name),
            cell(doc),
            code(&state_of_class(class)?),
            audience,
            approval,
            if actions.is_empty() { "none; reads, and custom actions".to_string() } else { actions.join(", ") }
        );
    }
    out.push('\n');

    // Capabilities.
    let caps = sources.item("Capability")?;
    out.push_str("## Capabilities\n\n");
    out.push_str(
        "A device lists what it can do in `DeviceDecl::capabilities`. An action that names a capability admits only a \
         device that has it, so a read-only speaker without `volume` is never sent `speaker.volume`. The lists inside \
         a capability are the values this device accepts; `refusal` must turn down any other value with a sentence.\n\n",
    );
    out.push_str("| Capability | JSON | Meaning | Actions that need it |\n| --- | --- | --- | --- |\n");
    for capability in Capability::examples() {
        let name = tag(&capability, "cap");
        let doc = caps.members.iter().find(|member| snake(&member.name) == name).map_or("", |m| m.doc.as_str());
        let kind = capability.kind();
        let needing: Vec<String> = catalog
            .actions
            .iter()
            .filter(|action| action.def.targets.requires() == Some(kind) && kind != CapKind::Custom)
            .map(|action| code(action.def.id))
            .collect();
        let needing = if kind == CapKind::Custom {
            "the custom action it names".to_string()
        } else if needing.is_empty() {
            "none: it tells the app and the hub what the device offers".to_string()
        } else {
            needing.join(", ")
        };
        let _ = writeln!(out, "| {} | {} | {} | {} |", code(&name), code(&compact(&capability)), cell(doc), needing);
    }
    out.push('\n');

    // Standard actions.
    out.push_str("## Standard actions\n\n");
    out.push_str(
        "Every action a person, voice or Swifty can run on a device. **Tier**: 0 read, 1 comfort (runs at once), \
         2 consequential (prepare, then confirm), 3 critical (prepare, then a passkey), S safe direction (an \
         emergency stop: runs at once, also by voice). **Idempotent**: only an idempotent action is sent a second \
         time after silence; any other is sent exactly once. **Timeout**: how long the hub reads the state back \
         before the run ends `no_response`.\n\n",
    );
    out.push_str(
        "| Class | Capability | Action (old id) | Sends | Tier | Idempotent | Timeout | Confirmed when |\n\
         | --- | --- | --- | --- | --- | --- | --- | --- |\n",
    );
    for action in catalog.device_actions() {
        let def = action.def;
        let class = match def.targets {
            Targets::Class(_) | Targets::Capable(..) => targets_text(def.targets),
            Targets::Fixed(_) => targets_text(def.targets),
            other => targets_text(other),
        };
        let capability = def.targets.requires().map_or_else(|| "none".to_string(), |cap| code(&tag(&cap, "")));
        let id = if def.aliases.is_empty() {
            code(def.id)
        } else {
            format!("{} ({})", code(def.id), def.aliases.iter().map(|alias| code(alias)).collect::<Vec<_>>().join(", "))
        };
        let sends = match (def.executor, action.group) {
            (Executor::Read, _) => "nothing: reads the state".to_string(),
            (_, Some(words)) => words.to_string(),
            _ => action.ops.iter().map(|op| code(op)).collect::<Vec<_>>().join(", "),
        };
        let confirmed = match (def.executor, action.group, action.ops.first()) {
            (Executor::Read, _, _) => "nothing to confirm".to_string(),
            (_, Some(_), _) => "each operation by its own rule (ops.md)".to_string(),
            (_, None, Some(op)) => lookup(CONFIRMED_WHEN, op, "CONFIRMED_WHEN")?.to_string(),
            _ => String::new(),
        };
        let _ = writeln!(
            out,
            "| {class} | {capability} | {id} | {} | {} | {} | {} ms | {} |",
            sends,
            tier_text(def.tier),
            if def.idempotent { "yes" } else { "no" },
            def.confirm_timeout_ms,
            cell(&confirmed)
        );
    }
    out.push('\n');

    out.push_str("### Parameters\n\n");
    out.push_str(
        "The catalog checks every value against these before a plugin sees it, so `refusal` only has to turn down \
         what this particular device does not accept.\n\n",
    );
    out.push_str("| Action | Parameters |\n| --- | --- |\n");
    for action in catalog.device_actions().filter(|action| !action.def.params.is_empty()) {
        let _ = writeln!(out, "| {} | {} |", code(action.def.id), params_text(action.def.params));
    }
    out.push('\n');

    out.push_str("### Value lists\n\n");
    out.push_str(
        "The words the catalog accepts. A device's capability lists the ones it has, and is never wider than these.\n\n",
    );
    for (name, list) in [
        ("TV inputs (`tv.input`)", catalog::TV_INPUTS),
        ("TV keys (`tv.key`)", catalog::TV_KEYS),
        ("Air conditioner modes (`ac.mode`)", catalog::AC_MODES),
        ("Air conditioner fan speeds (`ac.fan`)", catalog::AC_FANS),
        ("Readings `state.read` can highlight", catalog::METRICS),
    ] {
        let _ = writeln!(out, "- {name}: {}", list.iter().map(|word| code(word)).collect::<Vec<_>>().join(", "));
    }
    out.push('\n');

    // Hub actions.
    out.push_str("## Actions only the hub runs\n\n");
    out.push_str(
        "These act on the hub itself, not on a device through a plugin. A plugin never receives them and must not \
         offer anything like them. Machine approval and consoles reach a plugin only through the `MachineHost` and \
         `AgentPairing` extensions of `Driver`.\n\n",
    );
    out.push_str("| Action | Tier | Aimed at | What it does |\n| --- | --- | --- | --- |\n");
    for action in catalog.hub_actions() {
        let def = action.def;
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            code(def.id),
            tier_text(def.tier),
            targets_text(def.targets),
            cell(def.description)
        );
    }
    out.push('\n');

    // Floors.
    let (shortest, longest) = catalog::CONFIRM_TIMEOUT_RANGE_MS;
    out.push_str("## Custom actions and their floors\n\n");
    out.push_str(
        "When nothing above fits, a plugin declares a `CustomAction` in its manifest (manifest.md). The hub adds it \
         to the catalog as `<plugin id>.<verb>` and sends it as `Op::Custom { action, args }`. It reaches only a \
         device of the same plugin whose capabilities include `Capability::Custom { action }` with that id. Custom \
         actions are for the owner only, and voice and Swifty never propose them.\n\n\
         The tier in the manifest is a request. The hub applies floors when it loads the manifest, and never lowers \
         a tier:\n\n\
         - at least comfort;\n\
         - at least consequential when `idempotent` is false;\n\
         - at least critical on class `vm`, `host` or `router`;\n\
         - never read, never safe direction (the conformance suite refuses both).\n\n",
    );
    let _ = writeln!(
        out,
        "`confirm_timeout_ms` is kept between {shortest} and {longest} ms. The table is what the hub's `floor` \
         function gives:\n"
    );
    out.push_str(
        "| Class | Idempotent | Asks comfort | Asks consequential | Asks critical |\n| --- | --- | --- | --- | --- |\n",
    );
    for class in [DeviceClass::Other, DeviceClass::Vm] {
        for idempotent in [true, false] {
            let given = |tier| {
                let action = CustomAction {
                    id: "sample.verb",
                    label: "Sample",
                    description: "Sample.",
                    consequences: &[],
                    class,
                    tier,
                    params: &[],
                    idempotent,
                    confirm_timeout_ms: 5_000,
                    expect: Expect::Unreadable,
                };
                tier_text(catalog::floor(&action))
            };
            let class_words =
                if class == DeviceClass::Vm { "`vm`, `host`, `router`".to_string() } else { "any other".to_string() };
            let _ = writeln!(
                out,
                "| {class_words} | {} | {} | {} | {} |",
                if idempotent { "yes" } else { "no" },
                given(Tier::Comfort),
                given(Tier::Consequential),
                given(Tier::Critical)
            );
        }
    }
    Ok(out)
}

fn ops_page(sources: &Sources, catalog: &Catalog) -> Result<String, String> {
    let item = sources.item("Op")?;
    let mut out = String::from(GENERATED);
    out.push_str("# Operations (`Op`)\n\n");
    let _ = writeln!(out, "{}\n", item.doc);
    out.push_str(
        "`Driver::command(device, op)` receives one of these for one device, by its local id. Send it once and return \
         `Ok(CommandAck)` as soon as the device has accepted it. The hub then reads the state back and decides whether \
         it worked, by the rule under **Confirmed when**. Before any of that, `Plugin::refusal` has said whether this \
         device takes this operation at all, in dry-run as well as live.\n\n\
         `Op` is `#[non_exhaustive]`: a `match` in a plugin ends with a `_` arm that refuses.\n\n\
         The JSON is the wire form: an object with the variant in `op`.\n\n",
    );
    out.push_str("| Op | Class | Sent by | Read first | JSON |\n| --- | --- | --- | --- | --- |\n");
    let mut seen = BTreeSet::new();
    for op in Op::examples() {
        let name = tag(&op, "op");
        if !seen.insert(name.clone()) {
            continue;
        }
        let senders = catalog.sending(&name);
        let sent_by = if name == "custom" {
            "a custom action".to_string()
        } else if senders.is_empty() {
            "no standard action yet".to_string()
        } else {
            senders.iter().map(|id| code(id)).collect::<Vec<_>>().join(", ")
        };
        let _ = writeln!(
            out,
            "| [{}](#{name}) | {} | {} | {} | {} |",
            code(&name),
            lookup(OP_CLASS, &name, "OP_CLASS")?,
            sent_by,
            if op.needs_baseline() { "yes" } else { "no" },
            code(&compact(&op))
        );
    }
    out.push_str(
        "\n**Read first**: the hub reads the device's state before it sends the operation, because confirming it \
         needs the value from before (a counter that must grow, an uptime that must drop). If that first read fails, \
         nothing is sent.\n\n",
    );
    for op in Op::examples() {
        let name = tag(&op, "op");
        let Some(member) = item.members.iter().find(|member| snake(&member.name) == name) else {
            return Err(format!("no doc comment found for Op::{name}"));
        };
        let _ = writeln!(out, "## {name}\n");
        let _ = writeln!(out, "`Op::{}`. {}\n", member.name, member.doc);
        fields_table(&mut out, &member.fields);
        let _ = writeln!(out, "```json\n{}\n```\n", compact(&op));
        let _ = writeln!(out, "- Class: {}", lookup(OP_CLASS, &name, "OP_CLASS")?);
        let _ = writeln!(out, "- Confirmed when: {}", lookup(CONFIRMED_WHEN, &name, "CONFIRMED_WHEN")?);
        let mut notes = Vec::new();
        if op.needs_baseline() {
            notes.push("the hub reads the state first (`needs_baseline`)");
        }
        if op.cannot_be_read_back() {
            notes.push("nothing can show it worked (`cannot_be_read_back`)");
        }
        if op.takes_the_device_away() {
            notes.push("the device is expected to stop answering; an error while it goes is not a refusal (`takes_the_device_away`)");
        }
        if !notes.is_empty() {
            let _ = writeln!(out, "- Note: {}", notes.join("; "));
        }
        out.push('\n');
    }
    Ok(out)
}

fn states_page(sources: &Sources) -> Result<String, String> {
    let item = sources.item("State")?;
    let mut out = String::from(GENERATED);
    out.push_str("# States (`State`)\n\n");
    let _ = writeln!(out, "{}\n", item.doc);
    out.push_str(
        "`Driver::read_state(device)` returns what the device says **now**, in the variant for its class. A value the \
         device does not report is `None` (or the variant's \"unknown\" form), never a guess and never the last value \
         you sent: the hub confirms commands from this, so a state made up from what was sent would confirm commands \
         that did nothing. Text the device supplies goes through `net::device_text` first.\n\n\
         `State` is `#[non_exhaustive]`. The JSON is what API v1 returns under `state`: an object with the variant \
         in `kind`.\n\n",
    );
    let mut by_kind: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for class in DeviceClass::examples() {
        by_kind.entry(state_of_class(class)?).or_default().push(code(&class_name(class)));
    }
    out.push_str("| State | Reported by class | JSON |\n| --- | --- | --- |\n");
    for state in State::examples() {
        let kind = tag(&state, "kind");
        let classes = by_kind.get(&kind).map_or_else(|| "none".to_string(), |list| list.join(", "));
        let _ = writeln!(out, "| [{}](#{kind}) | {classes} | {} |", code(&kind), code(&compact(&state)));
    }
    out.push('\n');
    for state in State::examples() {
        let kind = tag(&state, "kind");
        let Some(member) = item.members.iter().find(|member| snake(&member.name) == kind) else {
            return Err(format!("no doc comment found for State::{kind}"));
        };
        let _ = writeln!(out, "## {kind}\n");
        let _ = writeln!(out, "`State::{}`. {}\n", member.name, member.doc);
        fields_table(&mut out, &member.fields);
        let _ = writeln!(out, "```json\n{}\n```\n", pretty(&state));
        let confirms: Vec<String> = Op::examples()
            .iter()
            .map(|op| tag(op, "op"))
            .filter(|op| {
                let class = OP_CLASS.iter().find(|(name, _)| name == op).map(|(_, class)| *class);
                class.is_some_and(|class| state_of_class_name(class) == kind) || (op == "custom" && kind == "custom")
            })
            .map(|op| code(&op))
            .collect();
        if !confirms.is_empty() {
            let _ = writeln!(out, "Confirms: {}\n", confirms.join(", "));
        }
    }
    Ok(out)
}

fn state_of_class_name(class: &str) -> String {
    DeviceClass::examples()
        .into_iter()
        .find(|known| class_name(*known) == class)
        .and_then(|known| state_of_class(known).ok())
        .unwrap_or_default()
}

fn manifest_page(sources: &Sources, plugins: &[Listed]) -> Result<String, String> {
    let manifest = sources.item("Manifest")?;
    let mut out = String::from(GENERATED);
    out.push_str("# The manifest\n\n");
    let _ = writeln!(out, "{}\n", manifest.doc);
    out.push_str(
        "Write it as `pub static MANIFEST: Manifest = Manifest { .. };` in `src/lib.rs`, and return `&MANIFEST` from \
         `Plugin::manifest`. Conformance check 1 holds it to the rules below. The JSON is what \
         `GET /api/v1/plugins/{plugin}` and `swifty plugin info --json` show.\n\n",
    );
    out.push_str("## `Manifest`\n\n| Field | Type | Meaning | Rule |\n| --- | --- | --- | --- |\n");
    for field in &manifest.members {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            code(&field.name),
            code(&field.ty),
            cell(&field.doc),
            cell(lookup(MANIFEST_RULES, &field.name, "MANIFEST_RULES")?)
        );
    }
    out.push('\n');

    let enum_section = |out: &mut String, name: &str, samples: Vec<String>, wire: &str| -> Result<(), String> {
        let item = sources.item(name)?;
        let _ = writeln!(out, "## `{name}`\n\n{}\n", item.doc);
        out.push_str("| Variant | Meaning | JSON |\n| --- | --- | --- |\n");
        let mut below = Vec::new();
        for (member, sample) in item.members.iter().zip(samples) {
            let shown = if sample.len() > 100 {
                below.push((member.name.clone(), sample));
                "below".to_string()
            } else {
                code(&sample)
            };
            let _ = writeln!(out, "| {} | {} | {} |", code(&member.name), cell(&member.doc), shown);
        }
        let _ = writeln!(out, "\n{wire}\n");
        for (name, sample) in below {
            let value: Value = serde_json::from_str(&sample).unwrap_or(Value::Null);
            let _ = writeln!(out, "`{name}`:\n\n```json\n{}\n```\n", pretty(&value));
        }
        Ok(())
    };
    enum_section(&mut out, "Reach", Reach::examples().iter().map(compact).collect(), "The variant is in `reach`.")?;
    enum_section(&mut out, "NetUse", NetUse::examples().iter().map(compact).collect(), "The variant is in `use`.")?;

    let config = sources.item("ConfigField")?;
    let _ = writeln!(out, "## `ConfigField`\n\n{}\n", config.doc);
    fields_table(&mut out, &config.members);
    out.push_str(
        "The fields describe the config for forms, `swifty plugin info` and these pages. The typed struct in your \
         `src/config.rs` is what really parses it (`serde` with `deny_unknown_fields`, plus your checks), and \
         conformance check 2 proves the two agree: the fixture config validates, a missing required key and an \
         unknown key are refused with the key named, and every declared key parses with a value of its kind. A \
         `default` is text, read the way the field's kind reads.\n\n",
    );
    enum_section(
        &mut out,
        "FieldKind",
        FieldKind::examples().iter().map(compact).collect(),
        "The variant is in `type`.",
    )?;

    let slot = sources.item("SecretSlot")?;
    let _ = writeln!(out, "## `SecretSlot`\n\n{}\n", slot.doc);
    fields_table(&mut out, &slot.members);
    out.push_str(
        "A slot is `name`, or `name/{key}` for one secret per config row: `local_key/{id}` declares one key per row, \
         stored as `local_key/<that row's id>`. The setup flow returns values in \
         `SetupResult::secrets`; the hub stores them and hands them to `connect` in `LiveContext::secrets`. The pure \
         calls only see which slots hold a value (`InstanceView::secrets_present`). A value never appears in the \
         config.\n\n",
    );

    let action = sources.item("CustomAction")?;
    let _ = writeln!(out, "## `CustomAction`\n\n{}\n", action.doc);
    fields_table(&mut out, &action.members);
    out.push_str(
        "The floors the hub applies to `tier` are in [capabilities.md](capabilities.md#custom-actions-and-their-floors). \
         For the device to be a target, its `DeviceDecl::capabilities` must include `Capability::Custom { action }` \
         with this id. The driver receives `Op::Custom { action, args }`, whose `args` passed `params`.\n\n",
    );
    enum_section(&mut out, "Expect", Expect::examples().iter().map(compact).collect(), "The variant is in `expect`.")?;
    out.push_str(
        "Only a `State::Custom` can meet an `Expect`, so a device with a custom action reports `custom` state with \
         the field the rule looks at. `Unreadable` is never met: the run ends `sent`, like a key press. \
         `CounterIncreased` makes the hub read the state before sending.\n\n",
    );
    let tiers: Vec<String> = Tier::examples().iter().map(compact).collect();
    let tier = sources.item("Tier")?;
    let _ = writeln!(out, "## `Tier`\n\n{}\n", tier.doc);
    out.push_str("| Variant | Code | Meaning | JSON |\n| --- | --- | --- | --- |\n");
    for ((member, sample), value) in tier.members.iter().zip(&tiers).zip(Tier::examples()) {
        let _ =
            writeln!(out, "| {} | {} | {} | {} |", code(&member.name), value.code(), cell(&member.doc), code(sample));
    }
    out.push('\n');
    let param = sources.item("ParamDef")?;
    let _ = writeln!(out, "## `ParamDef`\n\n{}\n", param.doc);
    fields_table(&mut out, &param.members);
    enum_section(
        &mut out,
        "ParamKind",
        ParamKind::examples().iter().map(compact).collect(),
        "The variant is the object's one key, or the whole value for `bool`.",
    )?;
    out.push_str(
        "A parameter's value reaches the driver in `Op::Custom::args` as a `Scalar`: `bool` as `Bool`, `int` as \
         `Int`, `enum` and `label` as `Text`.\n\n",
    );

    // A whole manifest.
    if let Some((dir, plugin)) = plugins
        .iter()
        .filter(|(_, plugin)| !plugin.manifest().actions.is_empty())
        .min_by_key(|(_, plugin)| compact(plugin.manifest()).len())
    {
        let action = &plugin.manifest().actions[0];
        let _ =
            writeln!(out, "## A custom action, as `plugins/{dir}` declares it\n\n```json\n{}\n```\n", pretty(action));
    }
    if let Some((dir, plugin)) = plugins.iter().min_by_key(|(_, plugin)| compact(plugin.manifest()).len()) {
        let _ = writeln!(
            out,
            "## A whole manifest: `plugins/{dir}`\n\nThe shortest first-party manifest, as JSON.\n\n```json\n{}\n```",
            pretty(plugin.manifest())
        );
    }
    Ok(out)
}

fn setup_page(sources: &Sources) -> Result<String, String> {
    let step = sources.item("SetupStep")?;
    let mut out = String::from(GENERATED);
    out.push_str("# Setup steps\n\n");
    out.push_str(
        "A setup flow is a small state machine inside the plugin (`impl SetupFlow`). It never draws anything: it \
         returns **steps as data**, and the web wizard, `swifty plugin add` and any later app draw the same steps and \
         send back the same inputs. The hub keeps the flow in memory, one per person setting up, and applies the \
         result.\n\n\
         ```text\n\
         hub                                   your SetupFlow\n\
         begin(ctx, existing config or None) -> first step\n\
         advance(ctx, input)                 -> next step      (repeated)\n\
         ...                                 -> done(result) or failed\n\
         hub: validate result.config, store config, secrets and devices, audit slot names\n\
         ```\n\n\
         `SetupContext::grant` is `None` in dry-run: a step that needs the network then returns `failed` with a \
         sentence saying live mode is needed, and a flow made only of forms still works. With a grant, end by reading \
         each chosen device once and fail with a helpful reason if that read fails.\n\n",
    );
    let context = sources.item("SetupContext")?;
    let _ = writeln!(out, "## `SetupContext`\n\n{}\n", context.doc);
    fields_table(&mut out, &context.members);

    let _ = writeln!(out, "## `SetupStep`\n\n{}\n", step.doc);
    out.push_str("The variant is in `step`.\n\n");
    for sample in SetupStep::examples() {
        let name = tag(&sample, "step");
        let Some(member) = step.members.iter().find(|member| snake(&member.name) == name) else {
            return Err(format!("no doc comment found for SetupStep::{name}"));
        };
        let (_, drawn, answer) = STEP_DRAWN
            .iter()
            .find(|(step, _, _)| *step == name)
            .ok_or_else(|| format!("`{name}` has no line in STEP_DRAWN in xtask/src/gen_docs/generator.rs"))?;
        let _ = writeln!(out, "### {name}\n\n`SetupStep::{}`. {}\n", member.name, member.doc);
        fields_table(&mut out, &member.fields);
        let _ = writeln!(out, "**Drawn:** {drawn}\n\n**Answered with:** {answer}\n");
        let _ = writeln!(out, "```json\n{}\n```\n", pretty(&sample));
    }

    let field = sources.item("Field")?;
    let _ = writeln!(out, "## `Field`\n\n{}\n", field.doc);
    fields_table(&mut out, &field.members);
    let kinds = sources.item("FieldType")?;
    let _ = writeln!(out, "## `FieldType`\n\n{}\n", kinds.doc);
    out.push_str("| Type | Meaning | Drawn as | Comes back as | JSON |\n| --- | --- | --- | --- | --- |\n");
    for sample in FieldType::examples() {
        let name = tag(&sample, "type");
        let doc = kinds.members.iter().find(|member| snake(&member.name) == name).map_or("", |m| m.doc.as_str());
        let (_, drawn, back) = FIELD_DRAWN
            .iter()
            .find(|(kind, _, _)| *kind == name)
            .ok_or_else(|| format!("`{name}` has no line in FIELD_DRAWN in xtask/src/gen_docs/generator.rs"))?;
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} |",
            code(&name),
            cell(doc),
            cell(drawn),
            back,
            code(&compact(&sample))
        );
    }
    out.push_str(
        "\nRead a value from `SetupInput::Values` by its key, and accept what a person may type: trim text, and \
         take a number either as `Int` or as `Text` holding digits. A required field can still arrive empty; answer \
         with the form again or `failed` with `retry: true`, never a panic.\n\n",
    );
    let choice = sources.item("Choice")?;
    let _ = writeln!(out, "## `Choice`\n\n{}\n", choice.doc);
    fields_table(&mut out, &choice.members);

    let inputs = sources.item("SetupInput")?;
    let _ = writeln!(out, "## `SetupInput`\n\n{}\n", inputs.doc);
    out.push_str("| Input | Meaning | JSON |\n| --- | --- | --- |\n");
    for sample in SetupInput::examples() {
        let name = tag(&sample, "input");
        let doc = inputs.members.iter().find(|member| snake(&member.name) == name).map_or("", |m| m.doc.as_str());
        let _ = writeln!(out, "| {} | {} | {} |", code(&name), cell(doc), code(&compact(&sample)));
    }
    out.push('\n');
    let result = sources.item("SetupResult")?;
    let _ = writeln!(out, "## `SetupResult`\n\n{}\n", result.doc);
    fields_table(&mut out, &result.members);
    out.push_str(
        "`secrets` is never serialised: the `done` step's JSON above has no secret in it, and its `Debug` prints \
         `<redacted>` for each value. A slot you return must be declared in the manifest (`name`, or `name/<key>` for \
         a `name/{key}` slot). Leave a slot out to keep the value the hub already holds: a flow never sees a stored \
         value, only `SetupContext::secrets_present`.\n",
    );
    Ok(out)
}

fn errors_page(sources: &Sources) -> Result<String, String> {
    let mut out = String::from(GENERATED);
    out.push_str("# Errors, readiness and health\n\n");
    out.push_str(
        "Error text a plugin writes is fixed English plus at most a number, and may name what the config holds (a \
         device's local id, a config key). It never holds a secret, a URL, or what the device sent: map the \
         device's codes to sentences of your own. The hub shows it to people and writes it to the audit log.\n\n",
    );
    let device = sources.item("DeviceError")?;
    let _ = writeln!(out, "## `DeviceError`\n\n{}\n", device.doc);
    out.push_str(
        "Returned by `Driver::command`, `Driver::read_state` and the other driver calls, and by every socket helper in \
         `swifty_plugin_api::net`, so `?` passes a helper's error straight on.\n\n",
    );
    out.push_str("| Variant | Shown as | Silence | What the hub does | JSON |\n| --- | --- | --- | --- | --- |\n");
    for (member, sample) in device.members.iter().zip(DeviceError::examples()) {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} |",
            code(&member.name),
            code(&sample.to_string()),
            if sample.is_silence() { "yes" } else { "no" },
            cell(lookup(ERROR_HANDLED, &member.name, "ERROR_HANDLED")?),
            code(&compact(&sample))
        );
    }
    let _ = writeln!(
        out,
        "\n`DeviceError::unsupported()` is {}: the answer for something this device cannot do at all.\n",
        code(&compact(&DeviceError::unsupported()))
    );
    let plugin = sources.item("PluginError")?;
    let _ = writeln!(out, "## `PluginError`\n\n{}\n", plugin.doc);
    for sample in PluginError::examples() {
        let _ = writeln!(out, "Returned by `Plugin::connect`. Example: {}\n", code(&sample.to_string()));
    }
    out.push_str(
        "`Secrets::require(slot)` returns one when a slot is empty, naming the slot and nothing else. The hub \
         disables that one instance with the text and keeps the others running.\n\n",
    );
    let problem = sources.item("ConfigProblem")?;
    let _ = writeln!(out, "## `ConfigProblem`\n\n{}\n", problem.doc);
    fields_table(&mut out, &problem.members);
    for sample in ConfigProblem::examples() {
        let _ = writeln!(out, "Shown as {}. JSON: {}\n", code(&sample.to_string()), code(&compact(&sample)));
    }
    out.push_str(
        "Returned by `Plugin::validate` and `Plugin::migrate`. Name the field (a dotted path for a row, \
         `devices[0].address`), and say what is wrong in a sentence. Conformance check 2 looks for the key in the \
         field or the message.\n\n",
    );
    let readiness = sources.item("Readiness")?;
    let _ = writeln!(out, "## `Readiness`\n\n{}\n", readiness.doc);
    out.push_str("| Variant | Meaning | What the hub does | JSON |\n| --- | --- | --- | --- |\n");
    for (member, sample) in readiness.members.iter().zip(Readiness::examples()) {
        let name = tag(&sample, "status");
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            code(&member.name),
            cell(&member.doc),
            cell(lookup(READINESS_HANDLED, &name, "READINESS_HANDLED")?),
            code(&compact(&sample))
        );
    }
    out.push_str(
        "\nReturned by the pure `Plugin::readiness`, from the config and the slots that hold a value. Conformance \
         checks 4 to 12 need `Ready` for the fixture.\n\n",
    );
    let health = sources.item("Health")?;
    let status = sources.item("HealthStatus")?;
    let _ = writeln!(out, "## `Health`\n\n{}\n", health.doc);
    fields_table(&mut out, &health.members);
    out.push_str("| Status | Meaning | Example JSON |\n| --- | --- | --- |\n");
    for (member, sample) in status.members.iter().zip(Health::examples()) {
        let _ =
            writeln!(out, "| {} | {} | {} |", code(&snake(&member.name)), cell(&member.doc), code(&compact(&sample)));
    }
    out.push_str(
        "\nReturned by `Driver::health`, which the hub calls now and then while live. Its default is `unknown`. \
         Keep it to one cheap read; never change anything.\n",
    );
    Ok(out)
}

fn kind_text(kind: &FieldKind) -> String {
    match kind {
        FieldKind::Text { max_len } => format!("text, at most {max_len}"),
        FieldKind::Host => "IP address or host name".to_string(),
        FieldKind::Url { schemes } => format!("URL ({})", schemes.join(", ")),
        FieldKind::HostPort => "host:port".to_string(),
        FieldKind::Port => "port".to_string(),
        FieldKind::Int { min, max } => format!("whole number, {min} to {max}"),
        FieldKind::Bool => "true or false".to_string(),
        FieldKind::Choice { options } => {
            format!("one of {}", options.iter().map(|option| code(option)).collect::<Vec<_>>().join(", "))
        }
        FieldKind::Mac => "MAC address".to_string(),
        FieldKind::Rows { .. } => "a list of rows".to_string(),
    }
}

fn config_rows(out: &mut String, fields: &[ConfigField], prefix: &str) {
    for field in fields {
        let key = format!("{prefix}{}", field.key);
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} |",
            code(&key),
            cell(field.label),
            kind_text(&field.kind),
            if field.required { "yes" } else { "no" },
            field.default.map_or_else(String::new, code),
            cell(field.help)
        );
        if let FieldKind::Rows { fields } = field.kind {
            config_rows(out, fields, &format!("{key}[]."));
        }
    }
}

fn net_text(net: &NetUse) -> String {
    match net {
        NetUse::Http { port } => format!("HTTP to the device, port {port}"),
        NetUse::Https { port } => format!("HTTPS to the device, port {port}"),
        NetUse::Ws { port } => format!("A websocket to the device, port {port}"),
        NetUse::Tcp { port } => format!("TCP to the device, port {port}"),
        NetUse::Udp { port } => format!("UDP to the device, port {port}"),
        NetUse::UdpBroadcast { port } => format!("UDP broadcasts on the local network, port {port}"),
        NetUse::WakeOnLan => "Wake-on-LAN packets".to_string(),
        NetUse::CloudHost { host } => format!("HTTPS to {}", code(host)),
    }
}

fn plugin_page(dir: &str, plugin: &'static dyn Plugin) -> String {
    let manifest: &Manifest = plugin.manifest();
    let mut out = String::from(GENERATED);
    let _ = writeln!(out, "# {} (`{}`)\n", manifest.name, manifest.id);
    let _ = writeln!(out, "{}\n", manifest.summary);
    let _ = writeln!(
        out,
        "Generated from the manifest of `plugins/{dir}`. The plugin's own manuals: \
         [README](../../../../plugins/{dir}/README.md) (what it supports, setup) and \
         [DEVELOPING](../../../../plugins/{dir}/DEVELOPING.md) (protocol notes, sources).\n"
    );
    let classes: Vec<String> = manifest.classes.iter().map(|class| code(&class_name(*class))).collect();
    out.push_str("| | |\n| --- | --- |\n");
    let _ = writeln!(out, "| Version | {} |", manifest.version);
    let _ = writeln!(out, "| Plugin API | {} |", manifest.api);
    let _ = writeln!(out, "| Reaches | {} |", reach_text(&manifest.reach));
    let _ = writeln!(out, "| Classes | {} |", classes.join(", "));
    let _ = writeln!(out, "| Config version | {} |", manifest.config_version);
    let _ = writeln!(out, "| More than one instance | {} |", if manifest.multi_instance { "yes" } else { "no" });
    let _ = writeln!(out, "| Guided setup | {} |", if plugin.setup().is_some() { "yes" } else { "no" });
    out.push('\n');
    if let Reach::Cloud { official: false, vendor } = manifest.reach {
        let _ = writeln!(
            out,
            "**{vendor} does not document this API for third parties.** It can change or stop working at any time, \
             and using it may be against the vendor's terms.\n"
        );
    }

    out.push_str("## What it reaches\n\n");
    if manifest.network.is_empty() {
        out.push_str("Nothing: it opens no socket.\n\n");
    } else {
        for net in manifest.network {
            let _ = writeln!(out, "- {}", net_text(net));
        }
        out.push_str("\nNothing at all while the hub is in dry-run.\n\n");
    }

    out.push_str("## Settings\n\n");
    if manifest.config.is_empty() {
        out.push_str("None.\n\n");
    } else {
        out.push_str("| Key | Label | Kind | Required | Default | Help |\n| --- | --- | --- | --- | --- | --- |\n");
        config_rows(&mut out, manifest.config, "");
        out.push('\n');
    }

    out.push_str("## Secrets\n\n");
    if manifest.secrets.is_empty() {
        out.push_str("None.\n\n");
    } else {
        out.push_str("| Slot | Label | Required | Help |\n| --- | --- | --- | --- |\n");
        for slot in manifest.secrets {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} |",
                code(slot.slot),
                cell(slot.label),
                if slot.required { "yes" } else { "no" },
                cell(slot.help)
            );
        }
        out.push('\n');
    }

    out.push_str("## Custom actions\n\n");
    if manifest.actions.is_empty() {
        out.push_str("None: it offers the standard actions of its classes.\n\n");
    } else {
        out.push_str(
            "| Action | Label | Class | Tier asked | Tier applied | Idempotent | Timeout | Parameters | Confirmed by |\n\
             | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n",
        );
        for action in manifest.actions {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} ms | {} | {} |",
                code(action.id),
                cell(action.label),
                code(&class_name(action.class)),
                tier_text(action.tier),
                tier_text(catalog::floor(action)),
                if action.idempotent { "yes" } else { "no" },
                action.confirm_timeout_ms,
                params_text(action.params),
                code(&compact(&action.expect))
            );
        }
        out.push('\n');
        for action in manifest.actions {
            let _ = writeln!(out, "`{}`: {}", action.id, action.description);
            for consequence in action.consequences {
                let _ = writeln!(out, "- {consequence}");
            }
            out.push('\n');
        }
    }

    out.push_str("## Words for voice\n\n");
    if manifest.vocabulary.is_empty() && manifest.nouns.is_empty() {
        out.push_str("None.\n");
    } else {
        for (heard, meant) in manifest.vocabulary {
            let _ = writeln!(out, "- Heard {}, meant {}", code(heard), code(meant));
        }
        for (noun, class) in manifest.nouns {
            let _ = writeln!(out, "- {} names the class {}", code(noun), code(&class_name(*class)));
        }
    }
    out
}
