//! The site file: what describes one house, in a
//! form a private repository can keep. Plugin instances and their config, the names
//! and notes people gave devices, and the house scenes. It never holds a secret:
//! credentials live in the secret store only.
//!
//! ```toml
//! site_version = 1
//!
//! [[instance]]
//! id = "wiz"
//! plugin = "wiz"
//! [instance.config]
//! broadcast = "192.0.2.255:38899"
//! bulbs = [{ id = "bulb1", address = "192.0.2.21" }]
//!
//! [[device]]
//! target = "vm101"
//! name = "Storage"
//! notes = ["The shared folders go offline while this is down."]
//!
//! [[scene]]
//! id = "evening"
//! percent = 40
//! hue = 34
//! overrides = [{ target = "bulb3", percent = 0, hue = 0 }]
//! ```
//!
//! - `swifty config export` writes the whole house in this form ([`export`]).
//! - `swifty config import` applies one ([`import`]): every instance's config is
//!   checked by its plugin first, an unknown plugin is refused, and the change is one
//!   transaction with one audit row.
//! - `swifty migrate --site` reads it as an overlay on what it converts from the old
//!   configuration: a key under `[instance.config]` replaces that key, and a key left
//!   out keeps the converted value ([`overlay`]).
//!
//! An optional `[hub]` table names what `swifty.toml` must say for this site:
//! `[hub.assistant.agent]` with `session_prefix`, `client_header` and `client_name`,
//! the identity an agent already knows the hub by. `swifty migrate` writes them into
//! the new `swifty.toml`; `swifty config import` never changes `swifty.toml` and
//! only says where it differs. An export does not write `[hub]`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rusqlite::{params, Connection};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use swifty_plugin_api::Plugin;

use crate::audit::{self, AuditKey};
use crate::catalog::validate_label;
use crate::config::Config;
use crate::runtime::{contained, Origin, Registry};
use crate::scenes::{self, Scene, SceneOverride};
use crate::secrets::is_instance_id;
use crate::store::Store;
use crate::{inventory, notes};

/// The only version of the site file there is.
pub const SITE_VERSION: u32 = 1;
/// The longest name a person can give a device.
const MAX_DEVICE_NAME: usize = 64;
/// The audit action of an import.
pub const IMPORT_ACTION: &str = "config.import";

/// Why a site file cannot be used. The text names the key or the line, never a
/// value from the file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SiteError(pub String);

impl From<rusqlite::Error> for SiteError {
    fn from(error: rusqlite::Error) -> Self {
        Self(format!("the database: {error}"))
    }
}

fn refuse<T>(message: impl Into<String>) -> Result<T, SiteError> {
    Err(SiteError(message.into()))
}

/// A site file, as read.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteFile {
    pub site_version: u32,
    #[serde(default)]
    pub hub: Option<HubPins>,
    #[serde(default, rename = "instance")]
    pub instances: Vec<SiteInstance>,
    #[serde(default, rename = "device")]
    pub devices: Vec<SiteDevice>,
    #[serde(default, rename = "scene")]
    pub scenes: Vec<SiteScene>,
}

/// `[hub]`: what `swifty.toml` must say for this site.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HubPins {
    #[serde(default)]
    pub assistant: Option<AssistantPins>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantPins {
    #[serde(default)]
    pub agent: Option<AgentPins>,
}

/// `[hub.assistant.agent]`: the identity an agent knows the hub by.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPins {
    pub session_prefix: Option<String>,
    pub client_header: Option<String>,
    pub client_name: Option<String>,
}

impl AgentPins {
    /// Each pinned key, by its path in `swifty.toml`, with its value.
    pub fn keys(&self) -> Vec<(&'static str, &str)> {
        [
            ("assistant.agent.session_prefix", &self.session_prefix),
            ("assistant.agent.client_header", &self.client_header),
            ("assistant.agent.client_name", &self.client_name),
        ]
        .into_iter()
        .filter_map(|(key, value)| value.as_deref().map(|value| (key, value)))
        .collect()
    }
}

impl SiteFile {
    /// The agent identity `[hub]` pins, if it pins any.
    pub fn agent_pins(&self) -> Option<&AgentPins> {
        self.hub.as_ref()?.assistant.as_ref()?.agent.as_ref()
    }

    pub fn instance(&self, id: &str) -> Option<&SiteInstance> {
        self.instances.iter().find(|instance| instance.id == id)
    }
}

/// `[[instance]]`: a plugin instance and its config. Never a credential.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteInstance {
    pub id: String,
    pub plugin: String,
    /// Left out means switched on.
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default = "empty_object")]
    pub config: Value,
}

fn empty_object() -> Value {
    Value::Object(Map::new())
}

/// `[[device]]`: the name and notes people gave a device, by its target.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteDevice {
    pub target: String,
    #[serde(default)]
    pub name: Option<String>,
    /// Shown in the confirm sheet of every action on the device. An empty list
    /// removes them.
    #[serde(default)]
    pub notes: Option<Vec<String>>,
}

/// `[[scene]]`: a house scene, or the keys of one that differ.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteScene {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub position: Option<i64>,
    #[serde(default)]
    pub percent: Option<u8>,
    #[serde(default)]
    pub hue: Option<u16>,
    #[serde(default)]
    pub overrides: Option<Vec<SceneOverride>>,
    #[serde(default)]
    pub extras: Option<Value>,
}

/// A TOML error with its line, and without a value from the file: someone may have
/// pasted a credential where it does not belong.
fn parse_error(error: &toml::de::Error, text: &str) -> SiteError {
    let message = error.message();
    let echoes_value = message.starts_with("invalid type")
        || message.starts_with("invalid value")
        || message.starts_with("unknown variant")
        || message.contains('"')
        || message.contains('\'');
    let detail = if echoes_value { "this value has the wrong type" } else { message };
    SiteError(match error.span() {
        Some(span) => {
            let start = span.start.min(text.len());
            let line = text.as_bytes()[..start].iter().filter(|b| **b == b'\n').count() + 1;
            format!("site file, line {line}: {detail}")
        }
        None => format!("site file: {detail}"),
    })
}

/// `[a-z0-9][a-z0-9_-]{0,63}`: the shape of every target.
fn is_target(text: &str) -> bool {
    let bytes = text.as_bytes();
    (1..=64).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

impl SiteFile {
    /// Reads and checks a site file's text. Nothing about plugins is checked here:
    /// that needs the build's plugins ([`check_instance`]).
    pub fn parse(text: &str) -> Result<Self, SiteError> {
        let site: SiteFile = toml::from_str(text).map_err(|error| parse_error(&error, text))?;
        site.check()?;
        Ok(site)
    }

    pub fn load(path: &Path) -> Result<Self, SiteError> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| SiteError(format!("cannot read the site file {}: {error}", path.display())))?;
        Self::parse(&text)
    }

    /// Everything that can be checked without a database or a plugin.
    pub fn check(&self) -> Result<(), SiteError> {
        if self.site_version != SITE_VERSION {
            return refuse(format!("site_version must be {SITE_VERSION}"));
        }
        if let Some(agent) = self.agent_pins() {
            if agent.keys().is_empty() {
                return refuse("[hub.assistant.agent] pins nothing: give session_prefix, client_header or client_name");
            }
        }
        let mut ids = BTreeSet::new();
        for instance in &self.instances {
            if !is_instance_id(&instance.id) {
                return refuse(format!(
                    "instance {:?}: an instance id is 1 to 16 lower-case letters and digits, starting with a letter",
                    instance.id
                ));
            }
            if !ids.insert(instance.id.as_str()) {
                return refuse(format!("instance {} appears twice", instance.id));
            }
            if instance.plugin.trim().is_empty() {
                return refuse(format!("instance {} names no plugin", instance.id));
            }
            if !instance.config.is_object() {
                return refuse(format!("instance {}: config must be a table", instance.id));
            }
        }
        let mut targets = BTreeSet::new();
        for device in &self.devices {
            if !is_target(&device.target) {
                return refuse(format!("device {:?} is not a device target", device.target));
            }
            if !targets.insert(device.target.as_str()) {
                return refuse(format!("device {} appears twice", device.target));
            }
            if let Some(name) = &device.name {
                validate_label("name", name, MAX_DEVICE_NAME)
                    .map_err(|problem| SiteError(format!("device {}: {}", device.target, problem.0)))?;
            }
            if let Some(problem) = device.notes.as_deref().and_then(notes::problem) {
                return refuse(format!("device {}: {problem}", device.target));
            }
        }
        let mut scene_ids = BTreeSet::new();
        for scene in &self.scenes {
            if !scene_ids.insert(scene.id.as_str()) {
                return refuse(format!("scene {} appears twice", scene.id));
            }
            if scene.extras.as_ref().is_some_and(|extras| !extras.is_object()) {
                return refuse(format!("scene {}: extras must be a table", scene.id));
            }
            // Each key that is given must be one a stored scene could have.
            let probe = Scene {
                id: scene.id.clone(),
                name: scene.name.clone().unwrap_or_else(|| "probe".to_string()),
                position: scene.position.unwrap_or(0),
                percent: scene.percent.unwrap_or(0),
                hue: scene.hue.unwrap_or(0),
                overrides: scene.overrides.clone().unwrap_or_default(),
                extras: scene.extras.clone().unwrap_or_else(empty_object),
            };
            if let Some(problem) = probe.problem() {
                return refuse(format!("scene {}: {problem}", scene.id));
            }
        }
        Ok(())
    }
}

/// Checks one instance against this build: its plugin is compiled in, its config
/// passes the plugin's own checks, and no key of it is one of the plugin's secret
/// slots, because a credential never goes into a config or a site file.
pub fn check_instance(
    plugins: &[&'static dyn Plugin],
    id: &str,
    plugin: &str,
    config: &Value,
) -> Result<(), SiteError> {
    let Some(found) = plugins.iter().copied().find(|candidate| candidate.manifest().id == plugin) else {
        return refuse(format!("instance {id}: this build has no plugin {plugin:?}"));
    };
    let slots: BTreeSet<&str> =
        found.manifest().secrets.iter().map(|slot| slot.slot.split('/').next().unwrap_or(slot.slot)).collect();
    if let Some(object) = config.as_object() {
        if let Some(key) = object.keys().find(|key| slots.contains(key.as_str())) {
            return refuse(format!(
                "instance {id}: {key} is a credential, and credentials never go in a config or a site file. \
                 Set it with: swifty plugin secret set {id} {key}"
            ));
        }
    }
    match contained(|| found.validate(config)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(problem)) => refuse(format!("instance {id}: {}: {}", problem.field, problem.message)),
        Err(_) => refuse(format!("instance {id}: the {plugin} plugin failed while checking its config")),
    }
}

/// `top` laid over `base`: a key of `top` replaces that key of `base`, a table in
/// both is laid over key by key, and a key `top` leaves out keeps `base`'s value.
pub fn overlay(base: &Value, top: &Value) -> Value {
    match (base, top) {
        (Value::Object(base), Value::Object(top)) => {
            let mut out = base.clone();
            for (key, value) in top {
                let merged = match out.get(key) {
                    Some(old) => overlay(old, value),
                    None => value.clone(),
                };
                out.insert(key.clone(), merged);
            }
            Value::Object(out)
        }
        (_, top) => top.clone(),
    }
}

// ----- export

/// Everything this hub would write into a site file: every instance with its config,
/// every name and note people gave a device, and every scene. Never a credential.
pub fn export(conn: &Connection) -> rusqlite::Result<SiteFile> {
    let instances = inventory::instance_specs(conn)?
        .into_iter()
        .map(|spec| SiteInstance {
            id: spec.id,
            plugin: spec.plugin,
            enabled: (!spec.enabled).then_some(false),
            config: spec.config,
        })
        .collect();
    let mut devices: BTreeMap<String, SiteDevice> = BTreeMap::new();
    {
        let mut statement = conn.prepare("SELECT target, name FROM device_names ORDER BY target")?;
        let names = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
        for row in names {
            let (target, name) = row?;
            devices.insert(target.clone(), SiteDevice { target, name: Some(name), notes: None });
        }
    }
    for (target, lines) in notes::all(conn)? {
        devices.entry(target.clone()).or_insert_with(|| SiteDevice { target, name: None, notes: None }).notes =
            Some(lines);
    }
    let scenes = scenes::all(conn)?
        .into_iter()
        .map(|scene| SiteScene {
            id: scene.id,
            name: Some(scene.name),
            position: Some(scene.position),
            percent: Some(scene.percent),
            hue: Some(scene.hue),
            overrides: Some(scene.overrides),
            extras: scene.extras.as_object().is_some_and(|extras| !extras.is_empty()).then_some(scene.extras),
        })
        .collect();
    Ok(SiteFile { site_version: SITE_VERSION, hub: None, instances, devices: devices.into_values().collect(), scenes })
}

/// A JSON value as TOML. TOML has no null: a key whose value is null is left out,
/// which a plugin reads as not set. A null in a list cannot be written.
fn to_toml_value(value: &Value, at: &str) -> Result<Option<toml::Value>, SiteError> {
    Ok(Some(match value {
        Value::Null => return Ok(None),
        Value::Bool(flag) => toml::Value::Boolean(*flag),
        Value::Number(number) => match (number.as_i64(), number.as_f64()) {
            (Some(integer), _) => toml::Value::Integer(integer),
            (None, Some(_)) if number.is_u64() => return refuse(format!("{at} is too large for a site file")),
            (None, Some(float)) => toml::Value::Float(float),
            (None, None) => return refuse(format!("{at} is not a number a site file can hold")),
        },
        Value::String(text) => toml::Value::String(text.clone()),
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for (n, item) in items.iter().enumerate() {
                match to_toml_value(item, &format!("{at}[{n}]"))? {
                    Some(item) => out.push(item),
                    None => return refuse(format!("{at}[{n}] is empty, and a list in a site file cannot hold that")),
                }
            }
            toml::Value::Array(out)
        }
        Value::Object(object) => toml::Value::Table(to_toml_table(object, at)?),
    }))
}

fn to_toml_table(object: &Map<String, Value>, at: &str) -> Result<toml::Table, SiteError> {
    let mut table = toml::Table::new();
    for (key, value) in object {
        if let Some(value) = to_toml_value(value, &format!("{at}.{key}"))? {
            table.insert(key.clone(), value);
        }
    }
    Ok(table)
}

/// A key as TOML writes it: bare when it can be, quoted otherwise.
fn toml_key(key: &str) -> String {
    let bare = !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if bare {
        key.to_string()
    } else {
        toml::Value::String(key.to_string()).to_string()
    }
}

/// A value on one line: lists as `[...]`, tables as `{ key = value, ... }`.
fn inline(value: &toml::Value) -> String {
    match value {
        toml::Value::Array(items) => format!("[{}]", items.iter().map(inline).collect::<Vec<_>>().join(", ")),
        toml::Value::Table(table) if table.is_empty() => "{}".to_string(),
        toml::Value::Table(table) => {
            let pairs: Vec<String> =
                table.iter().map(|(key, value)| format!("{} = {}", toml_key(key), inline(value))).collect();
            format!("{{ {} }}", pairs.join(", "))
        }
        scalar => scalar.to_string(),
    }
}

/// `key = value`. A list of more than one table, or of more than one line of text,
/// is written one item to a line.
fn assignment(out: &mut String, key: &str, value: &toml::Value) {
    match value {
        toml::Value::Array(items) if items.len() > 1 && items.iter().any(|item| item.is_table() || item.is_str()) => {
            out.push_str(&format!("{} = [\n", toml_key(key)));
            for item in items {
                out.push_str(&format!("    {},\n", inline(item)));
            }
            out.push_str("]\n");
        }
        value => out.push_str(&format!("{} = {}\n", toml_key(key), inline(value))),
    }
}

impl SiteFile {
    /// The file's text, laid out as `docs/configuration.md` shows it. Reading it back gives the
    /// same site. `[hub]` is never written.
    pub fn to_toml(&self) -> Result<String, SiteError> {
        let text = |text: &str| toml::Value::String(text.to_string());
        let mut out = String::from(
            "# Swifty Home site file: this house's integrations, device names and notes, and scenes.\n\
             # Written by `swifty config export`, applied with `swifty config import`. It holds no credential.\n\n",
        );
        out.push_str(&format!("site_version = {}\n", self.site_version));
        for instance in &self.instances {
            out.push_str("\n[[instance]]\n");
            assignment(&mut out, "id", &text(&instance.id));
            assignment(&mut out, "plugin", &text(&instance.plugin));
            if let Some(enabled) = instance.enabled {
                assignment(&mut out, "enabled", &toml::Value::Boolean(enabled));
            }
            out.push_str("[instance.config]\n");
            let config = instance.config.as_object().cloned().unwrap_or_default();
            for (key, value) in to_toml_table(&config, &instance.id)? {
                assignment(&mut out, &key, &value);
            }
        }
        for device in &self.devices {
            out.push_str("\n[[device]]\n");
            assignment(&mut out, "target", &text(&device.target));
            if let Some(name) = &device.name {
                assignment(&mut out, "name", &text(name));
            }
            if let Some(lines) = &device.notes {
                assignment(&mut out, "notes", &toml::Value::Array(lines.iter().map(|line| text(line)).collect()));
            }
        }
        for scene in &self.scenes {
            out.push_str("\n[[scene]]\n");
            assignment(&mut out, "id", &text(&scene.id));
            if let Some(name) = &scene.name {
                assignment(&mut out, "name", &text(name));
            }
            for (key, number) in [
                ("position", scene.position),
                ("percent", scene.percent.map(i64::from)),
                ("hue", scene.hue.map(i64::from)),
            ] {
                if let Some(number) = number {
                    assignment(&mut out, key, &toml::Value::Integer(number));
                }
            }
            if let Some(overrides) = &scene.overrides {
                let rows = overrides
                    .iter()
                    .map(|level| {
                        let mut row = toml::Table::new();
                        row.insert("target".into(), text(&level.target));
                        row.insert("percent".into(), toml::Value::Integer(i64::from(level.percent)));
                        row.insert("hue".into(), toml::Value::Integer(i64::from(level.hue)));
                        toml::Value::Table(row)
                    })
                    .collect();
                assignment(&mut out, "overrides", &toml::Value::Array(rows));
            }
            if let Some(Value::Object(extras)) = &scene.extras {
                assignment(&mut out, "extras", &toml::Value::Table(to_toml_table(extras, &scene.id)?));
            }
        }
        // What was written must read back as written.
        SiteFile::parse(&out).map_err(|error| SiteError(format!("the site file would not read back: {error}")))?;
        Ok(out)
    }
}

// ----- applying

/// What applying the names, notes and scenes of a site file did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Arranged {
    /// Targets whose name was set.
    pub names: Vec<String>,
    /// Targets whose notes were set or removed.
    pub notes: Vec<String>,
    /// Scenes that changed or were added.
    pub scenes: Vec<String>,
    /// Targets named that this hub has no device for yet. Their names and notes are
    /// kept, for when one appears.
    pub unknown_targets: Vec<String>,
}

/// Stores the names and notes of `[[device]]` and the keys of `[[scene]]`. A scene
/// this hub does not have yet needs a name, a position, a level and a hue.
pub fn apply_arrangements(conn: &Connection, site: &SiteFile) -> Result<Arranged, SiteError> {
    let mut done = Arranged::default();
    for device in &site.devices {
        if !inventory::device_exists(conn, &device.target)? {
            done.unknown_targets.push(device.target.clone());
        }
        if let Some(name) = &device.name {
            inventory::set_device_name(conn, &device.target, name)?;
            done.names.push(device.target.clone());
        }
        if let Some(lines) = &device.notes {
            notes::set(conn, &device.target, lines)?;
            done.notes.push(device.target.clone());
        }
    }
    for wanted in &site.scenes {
        let stored = scenes::get(conn, &wanted.id)?;
        let mut scene = match &stored {
            Some(scene) => scene.clone(),
            None => {
                let (Some(name), Some(position), Some(percent), Some(hue)) =
                    (&wanted.name, wanted.position, wanted.percent, wanted.hue)
                else {
                    return refuse(format!(
                        "scene {} is not on this hub, so the site file must give its name, position, percent and hue",
                        wanted.id
                    ));
                };
                Scene {
                    id: wanted.id.clone(),
                    name: name.clone(),
                    position,
                    percent,
                    hue,
                    overrides: Vec::new(),
                    extras: empty_object(),
                }
            }
        };
        if let Some(name) = &wanted.name {
            scene.name = name.clone();
        }
        if let Some(position) = wanted.position {
            scene.position = position;
        }
        if let Some(percent) = wanted.percent {
            scene.percent = percent;
        }
        if let Some(hue) = wanted.hue {
            scene.hue = hue;
        }
        if let Some(overrides) = &wanted.overrides {
            scene.overrides = overrides.clone();
        }
        if let Some(extras) = &wanted.extras {
            scene.extras = extras.clone();
        }
        if let Some(problem) = scene.problem() {
            return refuse(format!("scene {}: {problem}", scene.id));
        }
        if stored.as_ref() != Some(&scene) {
            scenes::save(conn, &scene)?;
            done.scenes.push(scene.id.clone());
        }
    }
    Ok(done)
}

/// Stores the devices each instance's config names, as the running hub does when it
/// loads ([`App::sync_directory`](crate::app::App::sync_directory)), so they have
/// their targets now. `registry` answers for the plugins; it is never asked to reach a
/// device. Returns the targets that were added.
pub fn store_declared_devices(conn: &Connection, registry: &Registry, now: i64) -> rusqlite::Result<Vec<String>> {
    let stored = inventory::stored_devices(conn)?;
    let mut added = Vec::new();
    for spec in inventory::instance_specs(conn)? {
        let adopted: Vec<_> = stored
            .iter()
            .filter(|device| device.instance == spec.id && device.origin == Origin::Adopted && !device.pending)
            .map(|device| device.record())
            .collect();
        if let Some(declared) = registry.declared(&spec, &adopted) {
            added.extend(inventory::store_config_devices(conn, now, &spec.id, &declared)?);
        }
    }
    Ok(added)
}

/// One row of `plugin_instances`, to write.
#[derive(Debug, Clone, Copy)]
pub struct InstanceRow<'a> {
    pub id: &'a str,
    pub plugin: &'a str,
    pub config_version: u32,
    pub enabled: bool,
    pub config: &'a Value,
}

/// Writes or replaces one instance row. `by` and `now` go into a new row's
/// `created_by` and `created_at`; a replaced row keeps them. A replaced row's
/// `updated_at` always moves forward, as [`inventory::touch_instance`] moves it, so a
/// running hub in another process that loads the instances again builds that instance
/// anew, whatever the clock says. Move `instances_rev` in the same transaction.
pub fn store_instance(conn: &Connection, now: i64, by: &str, row: InstanceRow<'_>) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO plugin_instances (id, plugin, config, config_version, enabled, created_at, created_by, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?6)
         ON CONFLICT(id) DO UPDATE SET plugin = excluded.plugin, config = excluded.config,
             config_version = excluded.config_version, enabled = excluded.enabled,
             updated_at = MAX(plugin_instances.updated_at + 1, excluded.updated_at)",
        params![row.id, row.plugin, row.config.to_string(), row.config_version, row.enabled, now, by],
    )?;
    Ok(())
}

/// Removes an instance and everything that names its devices: their room places and
/// favourites. Devices and candidates go with the row. Names, notes and audit rows
/// stay. The secrets are removed by the caller once this has committed.
fn remove_instance(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM room_devices WHERE target IN (SELECT target FROM devices WHERE instance_id = ?1)",
        params![id],
    )?;
    conn.execute(
        "DELETE FROM favourites WHERE target IN (SELECT target FROM devices WHERE instance_id = ?1)",
        params![id],
    )?;
    conn.execute("DELETE FROM devices WHERE instance_id = ?1", params![id])?;
    conn.execute("DELETE FROM device_candidates WHERE instance_id = ?1", params![id])?;
    conn.execute("DELETE FROM plugin_instances WHERE id = ?1", params![id])?;
    Ok(())
}

/// An audit row for something the command line did.
pub fn cli_event(action: &str, target: &str, params: Value, actor: &str, now: i64) -> audit::Event {
    audit::Event {
        ts: now,
        action: action.to_string(),
        target: target.to_string(),
        params,
        credential_ref: "-".to_string(),
        session_ref: "local".to_string(),
        user_ref: "-".to_string(),
        device: actor.to_string(),
        channel: "cli".to_string(),
        step_up: "-".to_string(),
        result: "ok".to_string(),
        rtt_ms: None,
        run_id: None,
    }
}

/// How to import.
pub struct ImportOptions<'a> {
    /// Also remove the instances, names and notes the file does not list.
    pub prune: bool,
    /// Who runs it, for the audit row: the Unix user name.
    pub actor: &'a str,
    pub now: i64,
    /// The audit key, so the row extends the signed head.
    pub audit_key: Option<&'a AuditKey>,
    /// `swifty.toml`, to say where it differs from `[hub]`.
    pub config: Option<&'a Config>,
}

/// What an import did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub added: Vec<String>,
    pub updated: Vec<String>,
    pub unchanged: Vec<String>,
    pub removed: Vec<String>,
    pub arranged: Arranged,
    pub pruned_names: Vec<String>,
    pub pruned_notes: Vec<String>,
    /// Devices the new configs named, by target.
    pub devices_added: Vec<String>,
    /// Where `swifty.toml` differs from what `[hub]` pins. Import does not change it.
    pub hub_differs: Vec<String>,
}

/// Where `swifty.toml` differs from what the site file's `[hub]` pins.
pub fn hub_differences(site: &SiteFile, config: Option<&Config>) -> Vec<String> {
    let Some(pins) = site.agent_pins() else {
        return Vec::new();
    };
    let agent = config.and_then(|config| config.assistant.as_ref()).and_then(|assistant| assistant.agent.as_ref());
    let Some(agent) = agent else {
        return vec!["the site file pins [hub.assistant.agent], and swifty.toml has no [assistant.agent]".to_string()];
    };
    pins.keys()
        .into_iter()
        .filter_map(|(key, wanted)| {
            let have = match key {
                "assistant.agent.session_prefix" => &agent.session_prefix,
                "assistant.agent.client_header" => &agent.client_header,
                _ => &agent.client_name,
            };
            (have != wanted).then(|| format!("swifty.toml has {key} = {have:?}, the site file pins {wanted:?}"))
        })
        .collect()
}

/// Applies a site file: its instances (checked by their plugins first), then names,
/// notes and scenes, and with `prune` removes what it does not list. One transaction
/// and one audit row; the running hub reloads when it sees `instances_rev` move.
/// Credentials are never touched, except that a pruned instance's are removed.
pub fn import(
    store: &Store,
    registry: &Registry,
    site: &SiteFile,
    options: &ImportOptions<'_>,
) -> Result<ImportReport, SiteError> {
    site.check()?;
    let plugins: Vec<&'static dyn Plugin> = registry.plugins().collect();
    for instance in &site.instances {
        check_instance(&plugins, &instance.id, &instance.plugin, &instance.config)?;
    }
    let mut report = ImportReport { hub_differs: hub_differences(site, options.config), ..ImportReport::default() };
    store.tx(|tx| -> Result<(), SiteError> {
        let stored: BTreeMap<String, crate::runtime::InstanceSpec> =
            inventory::instance_specs(tx)?.into_iter().map(|spec| (spec.id.clone(), spec)).collect();
        for instance in &site.instances {
            let enabled = instance.enabled.unwrap_or(true);
            let plugin = plugins.iter().find(|p| p.manifest().id == instance.plugin).expect("checked above");
            let version = plugin.manifest().config_version;
            match stored.get(&instance.id) {
                Some(old) if old.plugin != instance.plugin => {
                    return refuse(format!(
                        "instance {} is a {} instance here, and the site file says {}. Remove it first: \
                         swifty plugin remove {}",
                        instance.id, old.plugin, instance.plugin, instance.id
                    ));
                }
                Some(old) if old.config == instance.config && old.enabled == enabled => {
                    report.unchanged.push(instance.id.clone());
                    continue;
                }
                Some(_) => report.updated.push(instance.id.clone()),
                None => report.added.push(instance.id.clone()),
            }
            let row = InstanceRow {
                id: &instance.id,
                plugin: &instance.plugin,
                config_version: version,
                enabled,
                config: &instance.config,
            };
            store_instance(tx, options.now, options.actor, row)?;
        }
        if options.prune {
            for id in stored.keys().filter(|id| site.instance(id).is_none()) {
                remove_instance(tx, id)?;
                report.removed.push(id.clone());
            }
        }
        report.devices_added = store_declared_devices(tx, registry, options.now)?;
        report.arranged = apply_arrangements(tx, site)?;
        if options.prune {
            let listed = |target: &str, name: bool| {
                site.devices
                    .iter()
                    .any(|d| d.target == target && if name { d.name.is_some() } else { d.notes.is_some() })
            };
            let names: Vec<String> = {
                let mut statement = tx.prepare("SELECT target FROM device_names ORDER BY target")?;
                let rows = statement.query_map([], |row| row.get(0))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            for target in names.into_iter().filter(|target| !listed(target, true)) {
                tx.execute("DELETE FROM device_names WHERE target = ?1", params![target])?;
                report.pruned_names.push(target);
            }
            for target in notes::all(tx)?.into_keys().filter(|target| !listed(target, false)) {
                notes::remove(tx, &target)?;
                report.pruned_notes.push(target);
            }
        }
        inventory::bump_instances_rev(tx)?;
        let params = json!({
            "added": report.added,
            "updated": report.updated,
            "removed": report.removed,
            "names": report.arranged.names.len() + report.pruned_names.len(),
            "notes": report.arranged.notes.len() + report.pruned_notes.len(),
            "scenes": report.arranged.scenes,
            "prune": options.prune,
        });
        audit::append(tx, &cli_event(IMPORT_ACTION, "-", params, options.actor, options.now), options.audit_key)?;
        Ok(())
    })?;
    for list in [&mut report.added, &mut report.updated, &mut report.unchanged, &mut report.devices_added] {
        list.sort();
    }
    for id in &report.removed {
        if let Err(error) = registry.secret_store().remove_instance(id) {
            tracing::warn!(instance = %id, kind = ?error.kind(), "the secrets of a removed instance could not be removed");
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNERS: &str = r#"
site_version = 1

[hub.assistant.agent]
session_prefix = "house"

[[instance]]
id = "wiz"
plugin = "wiz"
[instance.config]
broadcast = "192.0.2.255:38899"
bulbs = [{ id = "bulb1", address = "192.0.2.21" }]

[[instance]]
id = "sonos"
plugin = "sonos"
[instance.config]

[[device]]
target = "vm101"
name = "Storage"
notes = ["The shared folders go offline while this is down."]

[[scene]]
id = "evening"
percent = 40
hue = 34
overrides = [{ target = "bulb3", percent = 0, hue = 0 }]
"#;

    #[test]
    fn a_site_file_is_read_strictly() {
        let site = SiteFile::parse(OWNERS).unwrap();
        assert_eq!(site.instances.len(), 2);
        assert_eq!(site.instance("sonos").unwrap().config, json!({}));
        assert_eq!(site.agent_pins().unwrap().keys(), [("assistant.agent.session_prefix", "house")]);
        assert_eq!(site.devices[0].notes.as_ref().unwrap().len(), 1);
        assert_eq!(site.scenes[0].overrides.as_ref().unwrap()[0].target, "bulb3");

        for (bad, says) in [
            ("site_version = 2", "site_version"),
            ("site_version = 1\nhouse = 1", "unknown field"),
            ("site_version = 1\n[hub]\nlisten = \"x\"", "unknown field"),
            ("site_version = 1\n[hub.assistant.agent]\n", "pins nothing"),
            ("site_version = 1\n[[instance]]\nid = \"Wiz\"\nplugin = \"wiz\"", "instance id"),
            ("site_version = 1\n[[instance]]\nid = \"a\"\nplugin = \"wiz\"\n[[instance]]\nid = \"a\"\nplugin = \"hue\"", "twice"),
            ("site_version = 1\n[[instance]]\nid = \"a\"\nplugin = \"\"", "no plugin"),
            ("site_version = 1\n[[device]]\ntarget = \"Bad Target\"", "not a device target"),
            ("site_version = 1\n[[device]]\ntarget = \"tv\"\nname = \" padded\"", "surrounding"),
            ("site_version = 1\n[[scene]]\nid = \"evening\"\npercent = 140", "percent"),
            ("site_version = 1\n[[scene]]\nid = \"evening\"\nextras = 3", "extras must be a table"),
            ("site_version = 1\n[[scene]]\nid = \"evening\"\nhue = \"red\"", "wrong type"),
        ] {
            let error = SiteFile::parse(bad).unwrap_err().0;
            assert!(error.contains(says), "{bad}: {error}");
        }
    }

    #[test]
    fn a_value_from_the_file_is_never_quoted_back() {
        let error = SiteFile::parse("site_version = 1\n[[device]]\ntarget = \"tv\"\nname = 42\n").unwrap_err().0;
        assert!(error.contains("line 4"), "{error}");
        let pasted = "site_version = 1\n[[device]]\ntarget = \"tv\"\nnotes = \"hunter2-secret\"\n";
        let error = SiteFile::parse(pasted).unwrap_err().0;
        assert!(!error.contains("hunter2"), "{error}");
    }

    #[test]
    fn an_overlay_replaces_only_the_keys_it_gives() {
        let base = json!({ "url": "http://old", "bulbs": [1, 2], "nested": { "a": 1, "b": 2 } });
        let top = json!({ "url": "http://new", "nested": { "b": 3 }, "extra": true });
        assert_eq!(
            overlay(&base, &top),
            json!({ "url": "http://new", "bulbs": [1, 2], "nested": { "a": 1, "b": 3 }, "extra": true })
        );
        assert_eq!(overlay(&base, &json!({})), base);
    }

    #[test]
    fn a_written_site_reads_back_the_same() {
        let mut site = SiteFile::parse(OWNERS).unwrap();
        site.hub = None;
        site.instances[0].enabled = Some(false);
        site.instances[0].config["port"] = json!(38899);
        site.instances[0].config["nothing"] = Value::Null;
        site.scenes[0].extras = Some(json!({ "tv_input": "HDMI 1", "volume_cap": 30 }));
        let text = site.to_toml().unwrap();
        let back = SiteFile::parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        site.instances[0].config.as_object_mut().unwrap().remove("nothing");
        assert_eq!(back, site, "{text}");
        assert!(!text.contains("[hub"), "{text}");
    }

    #[test]
    fn a_list_with_a_hole_cannot_be_written() {
        let site = SiteFile {
            site_version: 1,
            instances: vec![SiteInstance {
                id: "a".into(),
                plugin: "mock".into(),
                enabled: None,
                config: json!({ "rows": [1, null] }),
            }],
            ..SiteFile::default()
        };
        assert!(site.to_toml().unwrap_err().0.contains("a.rows[1]"));
    }
}
