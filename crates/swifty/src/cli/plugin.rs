//! `swifty plugin`: the integrations of this hub, and `plugin new` for a new plugin
//! crate.
//!
//! - `list` and `info` read the database and the names of the filled secret slots.
//!   They change nothing and contact nothing. With `--json` they print the documents
//!   `GET /api/v1/plugins` and `GET /api/v1/plugins/{plugin}` answer with.
//! - `add` and `setup` run the plugin's setup flow here in the terminal, through the
//!   hub's own setup engine, as the local
//!   administrator: audited with channel `cli`, session `local` and the Unix user as
//!   the device. The steps are the ones the web wizard shows ([`super::wizard`]).
//! - `remove` lists what goes, asks, and removes it the way `plugin.remove` does.
//! - `secret set` stores one secret read from standard input.
//!
//! Every change moves `instances_rev`, and a running hub loads it within a few
//! seconds. A setup or a secret that changes an instance in place moves the
//! instance's own `updated_at` too, so the hub connects that instance anew.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use clap::{Args, Subcommand};
use serde_json::{json, Value};
use swifty_hub::actions::Actor;
use swifty_hub::app::App;
use swifty_hub::audit::{self, AuditKey};
use swifty_hub::catalog::{self, Executor};
use swifty_hub::clock::{Clock, SystemClock};
use swifty_hub::config::Config;
use swifty_hub::inventory;
use swifty_hub::plugin_actions::{self, reach_text, Removal};
use swifty_hub::policy::Role;
use swifty_hub::runtime::{contained, InstanceSpec, Registry, StoredDevice};
use swifty_hub::secrets::{is_slot_name, FileSecretStore, SecretStore, MAX_VALUE_BYTES};
use swifty_hub::setup::Opener;
use swifty_hub::store::Store;
use swifty_hub::voice::{self, PluginWords};
use swifty_plugin_api::{FieldKind, Health, Manifest, Plugin, Reach, Readiness, Secret as Credential};

use super::terminal::{self, without_line_ending, Console, Terminal};
use super::wizard::{self, refused, yes_or_no, Answers, Wizard};
use super::{load_hub_config, CliError};
use crate::privileges;
use crate::scaffold::{self, Layout, NewPlugin};

#[derive(Debug, Subcommand)]
pub enum PluginCommand {
    /// List the integrations, with readiness, health and device counts.
    List(ListArgs),
    /// Show what a plugin reaches, its settings, its secrets and its actions.
    Info(InfoArgs),
    /// Add an integration. Runs the plugin's setup here in the terminal.
    Add(AddArgs),
    /// Run the setup of an integration again: a new pairing, a changed address.
    Setup(SetupArgs),
    /// Remove an integration, its devices and its secrets. Audit rows stay.
    Remove {
        /// The integration, such as hue.
        instance: String,
        /// Do not ask first.
        #[arg(long)]
        yes: bool,
    },
    /// Set one secret of an integration from standard input.
    #[command(subcommand)]
    Secret(SecretCommand),
    /// Write a new plugin crate from the template, ready to build and test.
    New(NewArgs),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Every plugin in this build, not only the ones in use.
    #[arg(long)]
    pub available: bool,
    /// Print JSON instead of text: what `GET /api/v1/plugins` answers.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct InfoArgs {
    /// The plugin, such as hue.
    pub plugin: String,
    /// Print JSON instead of text: the manifest, as `GET /api/v1/plugins/{plugin}` answers.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
#[command(after_help = "\
Each step of the setup is shown in turn. A secret is read without being shown.
With standard input from a file or a pipe, each answer is one line of it.

Example:
  sudo swifty plugin add hue --set url=http://192.0.2.10")]
pub struct AddArgs {
    /// The plugin, such as hue.
    pub plugin: String,
    /// The id of the new integration. Default: the plugin's id.
    #[arg(long, value_name = "ID")]
    pub instance: Option<String>,
    /// A setting, given ahead: key=value. Can be repeated. Never a secret.
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub settings: Vec<String>,
    /// Take every prefilled value and every ticked choice without asking.
    #[arg(long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct SetupArgs {
    /// The integration, such as hue.
    pub instance: String,
    /// A setting, given ahead: key=value. Can be repeated. Never a secret.
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub settings: Vec<String>,
    /// Take every prefilled value and every ticked choice without asking.
    #[arg(long)]
    pub yes: bool,
}

#[derive(Debug, Subcommand)]
pub enum SecretCommand {
    /// Read the secret from standard input and store it.
    #[command(after_help = "\
At a terminal, the value is typed without being shown. From a file or a pipe, all
of standard input is the value, without its last line ending:
  sudo swifty plugin secret set hue app_key < hue-key.txt")]
    Set {
        /// The integration, such as hue.
        instance: String,
        /// The secret's slot, such as app_key.
        slot: String,
    },
}

#[derive(Debug, Args)]
#[command(after_help = "\
Example:
  swifty plugin new acme-blinds --class plug --name \"Acme Blinds\"

Run inside a swifty-home checkout, the crate goes in plugins/ and builds with the
workspace. Anywhere else, it goes in the current directory.")]
pub struct NewArgs {
    /// The plugin's id: lower-case letters, digits and dashes, such as acme-blinds.
    pub id: String,
    /// The kinds of device it offers, separated by commas, such as plug or light,plug.
    #[arg(long, value_delimiter = ',', required = true, value_name = "CLASS")]
    pub class: Vec<String>,
    /// The name people see. Default: made from the id ("Acme Blinds").
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
    /// It reaches the devices through the vendor's servers, with an access token.
    #[arg(long)]
    pub cloud: bool,
    /// The directory to create the crate in.
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,
}

pub fn run(config: &Path, command: PluginCommand) -> Result<ExitCode, CliError> {
    match command {
        PluginCommand::List(args) => list(config, &args),
        PluginCommand::Info(args) => info(config, &args),
        PluginCommand::Add(args) => {
            let answers = Answers::new(&args.settings, args.yes)?;
            let start = Start::Add { plugin: args.plugin, instance: args.instance };
            run_setup(config, start, answers)
        }
        PluginCommand::Setup(args) => {
            let answers = Answers::new(&args.settings, args.yes)?;
            run_setup(config, Start::Again { instance: args.instance }, answers)
        }
        PluginCommand::Remove { instance, yes } => remove(config, &instance, yes),
        PluginCommand::Secret(SecretCommand::Set { instance, slot }) => set_secret(config, &instance, &slot),
        PluginCommand::New(args) => new(args, Path::new(".")),
    }
}

// ----- the hub's files

/// The hub's configuration, for a command that works on its files.
pub struct Hub {
    pub config: Config,
    /// Who ran the command, for the audit log: the Unix user, as it was before the
    /// command became the owner of the data.
    pub user: String,
}

/// Reads the configuration and, run as root, becomes the owner of the data
/// directory. There must be a database: `swifty init` makes it.
pub fn open_hub(config_path: &Path) -> Result<Hub, CliError> {
    let user = privileges::user_name();
    let loaded = load_hub_config(config_path)?;
    privileges::become_owner_of(&loaded.data_dir)?;
    let database = loaded.hub.database_path();
    if !database.is_file() {
        return Err(CliError::new(format!("there is no database at {}.", database.display()))
            .hint("Set up the hub first: sudo swifty init"));
    }
    Ok(Hub { config: loaded.hub, user })
}

fn database_error(error: impl std::fmt::Display) -> CliError {
    CliError::new(format!("the database could not be used: {error}"))
}

fn secret_store(config: &Config) -> Arc<dyn SecretStore> {
    Arc::new(FileSecretStore::new(config.secrets_path()))
}

/// The instances and devices as stored, loaded into a registry that contacts
/// nothing: readiness and refusals come from the plugins' own pure calls.
pub struct Loaded {
    pub registry: Registry,
    pub specs: Vec<InstanceSpec>,
    pub devices: Vec<StoredDevice>,
    /// The mode in the configuration: the one the hub runs in once started.
    pub live: bool,
}

impl Loaded {
    /// Reads the database without writing to it.
    pub fn read(config: &Config) -> Result<Self, CliError> {
        let store = Store::open_for_inspection(&config.database_path()).map_err(CliError::new)?;
        let (specs, devices) = store
            .with(|conn| -> rusqlite::Result<_> {
                Ok((inventory::instance_specs(conn)?, inventory::stored_devices(conn)?))
            })
            .map_err(database_error)?;
        let registry =
            Registry::new(false, crate::plugins::compiled_in(), secret_store(config), (&config.network).into());
        registry.load(specs.clone(), devices.clone());
        Ok(Self { registry, specs, devices, live: config.live })
    }
}

/// The plugin of this build with this id.
fn find_plugin(id: &str) -> Result<&'static dyn Plugin, CliError> {
    crate::plugins::compiled_in().into_iter().find(|plugin| plugin.manifest().id == id).ok_or_else(|| {
        CliError::new(format!("this build of Swifty Home has no plugin {id}."))
            .hint("The plugins in this build: swifty plugin list --available")
    })
}

/// Who the command line is in the audit log and to the setup engine: the local
/// administrator, with channel `cli`, session `local` and the Unix user as the device.
pub fn opener(user: &str) -> Opener {
    Opener {
        actor: Actor {
            session_ref: "local".to_string(),
            credential_id: "-".to_string(),
            device: user.to_string(),
            user_id: "-".to_string(),
            role: Role::Owner,
        },
        channel: "cli".to_string(),
        step_up: "local".to_string(),
    }
}

/// The app of a command, beside a running hub on the same database.
fn command_app(config: Config, plugins: Vec<&'static dyn Plugin>, live: bool) -> Result<App, CliError> {
    let registry = Registry::new(live, plugins, secret_store(&config), (&config.network).into());
    App::for_command_line(config, Arc::new(SystemClock), registry)
        .map_err(|error| CliError::new(format!("cannot open the hub's database: {error}")))
}

fn runtime() -> Result<tokio::runtime::Runtime, CliError> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|error| CliError::new(format!("cannot start: {error}")))
}

/// Columns of text, each as wide as its widest cell, the last one as it is.
pub fn table(rows: &[Vec<String>]) -> Vec<String> {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..columns)
        .map(|column| rows.iter().filter_map(|row| row.get(column)).map(|cell| cell.chars().count()).max().unwrap_or(0))
        .collect();
    rows.iter()
        .map(|row| {
            let mut line = String::from("  ");
            for (column, cell) in row.iter().enumerate() {
                if column + 1 == row.len() {
                    line.push_str(cell);
                } else {
                    line.push_str(&format!("{cell:<width$}  ", width = widths[column]));
                }
            }
            line.trim_end().to_string()
        })
        .collect()
}

fn mode_line(live: bool) -> &'static str {
    if live {
        "live: commands reach real devices"
    } else {
        "dry-run: no device is contacted"
    }
}

// ----- list

fn has_setup(plugin: &dyn Plugin) -> bool {
    contained(|| plugin.setup().is_some()).unwrap_or(false)
}

/// Why another instance of this plugin cannot be added now, if it cannot.
fn why_not(plugin: &dyn Plugin, instances: &[&str]) -> Option<String> {
    let manifest = plugin.manifest();
    if !has_setup(plugin) {
        return Some(format!("{} has no guided setup.", manifest.name));
    }
    match instances.first() {
        Some(existing) if !manifest.multi_instance => {
            Some(format!("{} is set up already as {existing}, and one is all it takes.", manifest.name))
        }
        _ => None,
    }
}

/// A plugin as `GET /api/v1/plugins` lists it.
fn plugin_summary(plugin: &dyn Plugin, specs: &[InstanceSpec]) -> Value {
    let manifest = plugin.manifest();
    let instances: Vec<&str> =
        specs.iter().filter(|spec| spec.plugin == manifest.id).map(|spec| spec.id.as_str()).collect();
    let why_not = why_not(plugin, &instances);
    json!({
        "id": manifest.id,
        "name": manifest.name,
        "version": manifest.version,
        "summary": manifest.summary,
        "reach": manifest.reach,
        "reach_text": reach_text(plugin),
        "network": manifest.network,
        "classes": manifest.classes,
        "multi_instance": manifest.multi_instance,
        "setup": has_setup(plugin),
        "instances": instances,
        "can_add": why_not.is_none(),
        "why_not": why_not,
    })
}

/// An instance as `GET /api/v1/plugins` lists it. Health is what the running hub
/// polls; this process has not, so it is unknown here.
fn instance_summary(loaded: &Loaded, spec: &InstanceSpec) -> Value {
    let plugin = loaded.registry.plugin(&spec.plugin);
    let (readiness, unavailable, health) = match loaded.registry.instance(&spec.id) {
        Some(instance) => (instance.readiness(), instance.unavailable(false), instance.last_health()),
        None => {
            let reason = format!("This build of Swifty Home has no plugin {}.", spec.plugin);
            (Readiness::Disabled(reason.clone()), Some(reason), Health::unknown())
        }
    };
    let secrets: Vec<String> = loaded.registry.secret_store().slots(&spec.id).unwrap_or_default().into_iter().collect();
    let devices: Vec<Value> = loaded
        .devices
        .iter()
        .filter(|device| device.instance == spec.id)
        .map(|device| json!({ "target": device.target, "name": device.name, "class": device.class, "pending": device.pending }))
        .collect();
    json!({
        "id": spec.id,
        "plugin": spec.plugin,
        "plugin_name": plugin.map(|plugin| plugin.manifest().name),
        "in_build": plugin.is_some(),
        "enabled": spec.enabled,
        "config_version": spec.config_version,
        "readiness": readiness,
        "unavailable": unavailable,
        "health": health,
        "secrets": secrets,
        "devices": devices,
        "setup_open": Value::Null,
    })
}

/// What `GET /api/v1/plugins` answers, from the files: no setup is open in this process.
pub fn plugins_document(loaded: &Loaded) -> Value {
    let plugins: Vec<Value> = loaded.registry.plugins().map(|plugin| plugin_summary(plugin, &loaded.specs)).collect();
    let instances: Vec<Value> = loaded.specs.iter().map(|spec| instance_summary(loaded, spec)).collect();
    json!({ "live": loaded.live, "plugins": plugins, "instances": instances, "setups_open": 0 })
}

/// One line on how an instance is: ready, or why not.
fn status_text(readiness: &Readiness, unavailable: Option<&str>) -> String {
    match (readiness, unavailable) {
        (Readiness::NeedsSetup(reason), _) => format!("needs setup: {reason}"),
        (Readiness::Disabled(reason), _) => format!("disabled: {reason}"),
        (Readiness::Ready, Some(reason)) => format!("unavailable: {reason}"),
        (Readiness::Ready, None) => "ready".to_string(),
    }
}

fn reach_word(manifest: &Manifest) -> String {
    match manifest.reach {
        Reach::Local if manifest.network.is_empty() => "nothing outside the hub".to_string(),
        Reach::Local => "your network".to_string(),
        Reach::Cloud { vendor, official: true } => format!("{vendor}'s servers"),
        Reach::Cloud { vendor, official: false } => format!("{vendor}'s servers (unofficial)"),
    }
}

fn list(config_path: &Path, args: &ListArgs) -> Result<ExitCode, CliError> {
    let hub = open_hub(config_path)?;
    let loaded = Loaded::read(&hub.config)?;
    if args.json {
        let document = plugins_document(&loaded);
        println!("{}", serde_json::to_string_pretty(&document).map_err(|e| e.to_string())?);
        return Ok(ExitCode::SUCCESS);
    }
    if args.available {
        println!("Plugins in this build");
        println!();
        let mut rows = vec![vec!["ID".to_string(), "NAME".into(), "REACHES".into(), "IN USE".into()]];
        for plugin in loaded.registry.plugins() {
            let manifest = plugin.manifest();
            let instances: Vec<&str> =
                loaded.specs.iter().filter(|spec| spec.plugin == manifest.id).map(|spec| spec.id.as_str()).collect();
            let in_use = if instances.is_empty() { "-".to_string() } else { instances.join(", ") };
            rows.push(vec![manifest.id.to_string(), manifest.name.to_string(), reach_word(manifest), in_use]);
        }
        for line in table(&rows) {
            println!("{line}");
        }
        println!();
        println!("What one reaches and needs: swifty plugin info <plugin>");
        println!("Add one: sudo swifty plugin add <plugin>");
        return Ok(ExitCode::SUCCESS);
    }
    println!("Integrations ({})", mode_line(loaded.live));
    println!();
    if loaded.specs.is_empty() {
        println!("  None yet. Add one: sudo swifty plugin add <plugin>");
    } else {
        let mut rows = vec![vec!["ID".to_string(), "PLUGIN".into(), "DEVICES".into(), "STATUS".into()]];
        for spec in &loaded.specs {
            let plugin = loaded.registry.plugin(&spec.plugin);
            let name = plugin.map_or_else(|| spec.plugin.clone(), |plugin| plugin.manifest().name.to_string());
            let mine: Vec<&StoredDevice> = loaded.devices.iter().filter(|device| device.instance == spec.id).collect();
            let waiting = mine.iter().filter(|device| device.pending).count();
            let devices = match waiting {
                0 => mine.len().to_string(),
                n => format!("{} + {n} waiting", mine.len() - n),
            };
            let status = match loaded.registry.instance(&spec.id) {
                Some(instance) => status_text(&instance.readiness(), instance.unavailable(false).as_deref()),
                None => format!("not in this build: it needs the plugin {}", spec.plugin),
            };
            rows.push(vec![spec.id.clone(), name, devices, status]);
        }
        for line in table(&rows) {
            println!("{line}");
        }
    }
    println!();
    println!("Health is checked by the running hub; the Integrations page in the app shows it.");
    println!("Every plugin in this build: swifty plugin list --available");
    Ok(ExitCode::SUCCESS)
}

// ----- info

/// What `GET /api/v1/plugins/{plugin}` answers: the manifest, each custom action's
/// effective tier, and whether another instance can be added.
pub fn manifest_document(
    plugin: &'static dyn Plugin,
    plugins: &[&'static dyn Plugin],
    specs: &[InstanceSpec],
) -> Value {
    catalog::register_plugins(plugins);
    let words: Vec<PluginWords> = plugins.iter().map(|plugin| PluginWords::from(plugin.manifest())).collect();
    let dropped = voice::register_plugin_words(&words);
    let manifest = plugin.manifest();
    let mut full = serde_json::to_value(manifest).unwrap_or(Value::Null);
    if let Some(actions) = full.get_mut("actions").and_then(Value::as_array_mut) {
        for action in actions {
            let tier = action.get("id").and_then(Value::as_str).and_then(catalog::find).map(|def| def.tier.wire());
            action["tier_effective"] = tier.unwrap_or(Value::Null);
        }
    }
    let summary = plugin_summary(plugin, specs);
    for key in ["reach_text", "setup", "instances", "can_add", "why_not"] {
        full[key] = summary[key].clone();
    }
    full["dropped_words"] = dropped
        .iter()
        .filter(|word| word.plugin == manifest.id)
        .map(|word| json!({ "word": word.word, "why": word.why }))
        .collect();
    full
}

fn kind_text(kind: &FieldKind) -> String {
    match kind {
        FieldKind::Text { max_len } => format!("text, up to {max_len} characters"),
        FieldKind::Host => "an address or host name".to_string(),
        FieldKind::Url { schemes } => format!("a URL ({})", schemes.join(", ")),
        FieldKind::HostPort => "host:port".to_string(),
        FieldKind::Port => "a port".to_string(),
        FieldKind::Int { min, max } => format!("a number from {min} to {max}"),
        FieldKind::Bool => "yes or no".to_string(),
        FieldKind::Choice { options } => format!("one of {}", options.join(", ")),
        FieldKind::Mac => "a MAC address".to_string(),
        FieldKind::Rows { fields } => {
            let keys: Vec<&str> = fields.iter().map(|field| field.key).collect();
            format!("a list, each with {}", keys.join(", "))
        }
    }
}

fn info(config_path: &Path, args: &InfoArgs) -> Result<ExitCode, CliError> {
    let plugin = find_plugin(&args.plugin)?;
    // The integrations of this hub, where there is one to read. A plugin is described
    // without one too.
    let specs = match open_hub(config_path) {
        Ok(hub) => Loaded::read(&hub.config).map(|loaded| loaded.specs).unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    let plugins = crate::plugins::compiled_in();
    let document = manifest_document(plugin, &plugins, &specs);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&document).map_err(|e| e.to_string())?);
        return Ok(ExitCode::SUCCESS);
    }
    let manifest = plugin.manifest();
    let say = |indent: &str, text: &str| {
        for line in wizard::wrap(text, 76 - indent.len()) {
            println!("{indent}{line}");
        }
    };
    println!("{} ({}), version {}", manifest.name, manifest.id, manifest.version);
    say("  ", manifest.summary);
    println!();
    println!("What it reaches");
    for line in reach_text(plugin) {
        say("  ", &line);
    }
    println!();
    let classes: Vec<String> = manifest
        .classes
        .iter()
        .filter_map(|class| serde_json::to_value(class).ok().and_then(|v| v.as_str().map(str::to_string)))
        .collect();
    println!("Devices:  {}", classes.join(", "));
    if has_setup(plugin) {
        println!("Setup:    guided, in the app or with: sudo swifty plugin add {}", manifest.id);
    } else {
        println!("Setup:    none guided; its settings go in with `swifty config import`");
    }
    let instances: Vec<&str> =
        specs.iter().filter(|spec| spec.plugin == manifest.id).map(|spec| spec.id.as_str()).collect();
    println!("In use:   {}", if instances.is_empty() { "no".to_string() } else { instances.join(", ") });
    if let Some(why) = why_not(plugin, &instances).filter(|_| has_setup(plugin)) {
        say("          ", &why);
    }
    if !manifest.config.is_empty() {
        println!();
        println!("Settings");
        for field in manifest.config {
            let need = if field.required { ", required" } else { "" };
            let default = field.default.map(|value| format!(", default {value}")).unwrap_or_default();
            println!("  {}: {}{need}{default}", field.key, kind_text(&field.kind));
            say("    ", field.help);
        }
    }
    if !manifest.secrets.is_empty() {
        println!();
        println!("Secrets (in the secret store; never shown, logged or audited)");
        for slot in manifest.secrets {
            let need = if slot.required { ", required" } else { "" };
            println!("  {}: {}{need}", slot.slot, slot.label);
            say("    ", slot.help);
        }
    }
    if !manifest.actions.is_empty() {
        println!();
        println!("Actions it adds");
        for action in manifest.actions {
            let effective = catalog::find(action.id).map(|def| def.tier).unwrap_or(action.tier);
            let tier = if effective == action.tier {
                effective.name().to_string()
            } else {
                format!("{} (the hub's floor; the plugin asks for {})", effective.name(), action.tier.name())
            };
            println!("  {}: {}, {tier}", action.id, action.label);
            say("    ", action.description);
        }
    }
    Ok(ExitCode::SUCCESS)
}

// ----- add and setup

/// Which setup to open.
pub enum Start {
    /// A new instance of a plugin, under this id or the next free one.
    Add { plugin: String, instance: Option<String> },
    /// A stored instance, from its stored config.
    Again { instance: String },
}

/// Opens a setup on this app and runs it in the terminal until it is done or fails.
/// Returns the summary. A setup that stops early is abandoned and leaves nothing.
/// `open` holds the id of the setup while it runs, for whoever has to abandon it
/// from outside (the Ctrl+C handler).
pub async fn run_setup_with(
    app: &App,
    opener: &Opener,
    term: &mut dyn Terminal,
    start: &Start,
    answers: Answers,
    open: &Mutex<Option<String>>,
) -> Result<String, CliError> {
    let view = match start {
        Start::Add { plugin, instance } => app.setup.begin(app, opener, plugin, instance.as_deref()).await,
        Start::Again { instance } => app.setup.begin_again(app, opener, instance).await,
    }
    .map_err(refused)?;
    *open.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(view.setup_id.clone());
    if matches!(start, Start::Add { instance: None, .. }) {
        term.say(&format!("  The new integration is called {}.", view.instance));
    }
    let summary = Wizard::new(app, &opener.actor, term, answers).run(view).await;
    *open.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    summary
}

/// What `plugin add` and `plugin setup` say before the first step.
fn introduce(term: &mut dyn Terminal, plugin: &dyn Plugin, start: &Start, live: bool) {
    let manifest = plugin.manifest();
    match start {
        Start::Add { .. } => term.say(&format!("Add {} ({})", manifest.name, manifest.id)),
        Start::Again { instance } => term.say(&format!("Set up {instance} again ({})", manifest.name)),
    }
    for line in wizard::wrap(manifest.summary, 74) {
        term.say(&format!("  {line}"));
    }
    for sentence in reach_text(plugin) {
        for line in wizard::wrap(&sentence, 74) {
            term.say(&format!("  {line}"));
        }
    }
    let reaches_out = !manifest.network.is_empty() || matches!(manifest.reach, Reach::Cloud { .. });
    if !live && reaches_out {
        term.say("  The hub is in dry-run: a step that has to reach a device stops there and says so.");
        term.say("  To set it up for real: sudo swifty go-live, then restart the hub.");
    }
}

/// Stops a setup cleanly on Ctrl+C: the echo comes back, the setup is abandoned so it
/// leaves nothing, and the process ends.
fn stop_on_ctrl_c(app: App, actor: Actor, open: Arc<Mutex<Option<String>>>) {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_err() {
            return;
        }
        terminal::restore();
        let id = open.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
        if let Some(id) = id {
            let _ = app.setup.abandon(&app, &actor, &id).await;
        }
        eprintln!();
        eprintln!("Stopped. Nothing was stored.");
        std::process::exit(130);
    });
}

fn run_setup(config_path: &Path, start: Start, answers: Answers) -> Result<ExitCode, CliError> {
    let hub = open_hub(config_path)?;
    let plugin = match &start {
        Start::Add { plugin, .. } => find_plugin(plugin)?,
        Start::Again { instance } => {
            let specs = Loaded::read(&hub.config)?.specs;
            let spec = specs.iter().find(|spec| &spec.id == instance).ok_or_else(|| {
                CliError::new(format!("there is no integration {instance} here."))
                    .hint("The integrations of this hub: swifty plugin list")
            })?;
            find_plugin(&spec.plugin).map_err(|_| {
                CliError::new(format!(
                    "this build of Swifty Home has no plugin {}, so {instance} cannot be set up here.",
                    spec.plugin
                ))
            })?
        }
    };
    let live = hub.config.live;
    let opener = opener(&hub.user);
    let mut term = Console::new();
    introduce(&mut term, plugin, &start, live);
    let runtime = runtime()?;
    runtime.block_on(async move {
        // Only this plugin: the command loads, and in a live hub connects, nothing else.
        let app = command_app(hub.config, vec![plugin], live)?;
        let open = Arc::new(Mutex::new(None));
        stop_on_ctrl_c(app.clone(), opener.actor.clone(), open.clone());
        run_setup_with(&app, &opener, &mut term, &start, answers, &open).await?;
        term.say("  A running hub picks it up within a few seconds; a stopped one when it starts.");
        term.say("  Its devices: swifty device list");
        Ok(ExitCode::SUCCESS)
    })
}

// ----- remove

/// Says what goes with an instance, asks unless `yes`, and removes it. `None` when
/// the person said no.
pub async fn remove_with(
    app: &App,
    opener: &Opener,
    term: &mut dyn Terminal,
    instance: &str,
    yes: bool,
) -> Result<Option<Removal>, CliError> {
    let Some(goes) = plugin_actions::removal(app, instance).map_err(refused)? else {
        return Err(CliError::new(format!("there is no integration {instance} here."))
            .hint("The integrations of this hub: swifty plugin list"));
    };
    if let Some(refusal) =
        plugin_actions::refusal(app, Executor::PluginRemove, instance, &Value::Null).map_err(refused)?
    {
        return Err(refused(refusal));
    }
    let name = app.registry.plugin(&goes.plugin).map_or(goes.plugin.as_str(), |plugin| plugin.manifest().name);
    term.say(&format!("Remove {instance} ({name})"));
    for line in goes.lines() {
        for wrapped in wizard::wrap(&line, 74) {
            term.say(&format!("  {wrapped}"));
        }
    }
    term.say("  The audit log keeps every row.");
    if !yes {
        let Some(answer) = term.ask(&format!("Remove {instance}? [y/N]: "))? else {
            return Err(CliError::new("nothing was removed: there was no answer.")
                .hint("Run it in a terminal, or add --yes to remove without asking."));
        };
        if yes_or_no(&answer, false) != Some(true) {
            term.say("Nothing was removed.");
            return Ok(None);
        }
    }
    let removed = plugin_actions::remove(app, opener, instance).await.map_err(refused)?;
    term.say(&removed.summary());
    term.say("A running hub lets go of it within a few seconds.");
    Ok(Some(removed))
}

fn remove(config_path: &Path, instance: &str, yes: bool) -> Result<ExitCode, CliError> {
    let hub = open_hub(config_path)?;
    let opener = opener(&hub.user);
    let mut term = Console::new();
    runtime()?.block_on(async move {
        // Every plugin, so what goes is known for any instance; nothing is connected.
        let app = command_app(hub.config, crate::plugins::compiled_in(), false)?;
        match remove_with(&app, &opener, &mut term, instance, yes).await? {
            Some(_) => Ok(ExitCode::SUCCESS),
            None => Ok(ExitCode::FAILURE),
        }
    })
}

// ----- secret set

/// Whether the plugin declares this slot: as it is, or as a row of a `name/{id}` slot.
fn declares_slot(manifest: &Manifest, slot: &str) -> bool {
    is_slot_name(slot)
        && manifest.secrets.iter().any(|declared| match declared.slot.split_once('/') {
            Some((base, _)) => slot.split_once('/').is_some_and(|(given, row)| given == base && !row.is_empty()),
            None => declared.slot == slot,
        })
}

/// The value from standard input: typed without echo at a terminal, otherwise all
/// of it without its last line ending.
fn read_value(slot: &str) -> Result<String, CliError> {
    let value = if std::io::stdin().is_terminal() {
        Console::new().ask_hidden(&format!("Value for {slot} (hidden): "))?.unwrap_or_default()
    } else {
        let mut bytes = Vec::new();
        std::io::stdin()
            .lock()
            .take(MAX_VALUE_BYTES as u64 + 2)
            .read_to_end(&mut bytes)
            .map_err(|error| CliError::new(format!("cannot read standard input: {error}")))?;
        let text = String::from_utf8(bytes).map_err(|_| CliError::new("the value is not text (UTF-8)."))?;
        without_line_ending(&text).to_string()
    };
    if value.is_empty() {
        return Err(CliError::new(format!("no value was given for {slot}; nothing was stored."))
            .hint(format!("Pipe it in, such as: sudo swifty plugin secret set <integration> {slot} < file")));
    }
    if value.len() > MAX_VALUE_BYTES {
        return Err(CliError::new(format!(
            "the value is longer than a secret slot holds ({} KiB); nothing was stored.",
            MAX_VALUE_BYTES / 1024
        )));
    }
    Ok(value)
}

fn set_secret(config_path: &Path, instance: &str, slot: &str) -> Result<ExitCode, CliError> {
    let hub = open_hub(config_path)?;
    let config = &hub.config;
    let store = Store::open(&config.database_path()).map_err(database_error)?;
    let spec = store
        .with(inventory::instance_specs)
        .map_err(database_error)?
        .into_iter()
        .find(|spec| spec.id == instance)
        .ok_or_else(|| {
            CliError::new(format!("there is no integration {instance} here."))
                .hint("The integrations of this hub: swifty plugin list")
        })?;
    let plugin = find_plugin(&spec.plugin)?;
    let manifest = plugin.manifest();
    if !declares_slot(manifest, slot) {
        let slots: Vec<&str> = manifest.secrets.iter().map(|declared| declared.slot).collect();
        let known = if slots.is_empty() { "none".to_string() } else { slots.join(", ") };
        return Err(
            CliError::new(format!("{} has no secret slot {slot}.", manifest.name)).hint(format!("Its slots: {known}"))
        );
    }
    let key = config
        .audit
        .key_file
        .as_deref()
        .map(AuditKey::load)
        .transpose()
        .map_err(|error| CliError::new(format!("the audit key cannot be used: {error}")))?;
    let value = read_value(slot)?;
    FileSecretStore::new(config.secrets_path())
        .write(instance, slot, &Credential::from(value))
        .map_err(|error| CliError::new(format!("the secret could not be stored ({:?}).", error.kind())))?;
    let opener = opener(&hub.user);
    let now = SystemClock.now_ms();
    let event = audit::Event {
        ts: now,
        action: "plugin.secret".to_string(),
        target: instance.to_string(),
        params: json!({ "instance": instance, "plugin": spec.plugin, "slot": slot }),
        credential_ref: opener.actor.credential_id.clone(),
        session_ref: opener.actor.session_ref.clone(),
        user_ref: opener.actor.user_id.clone(),
        device: opener.actor.device.clone(),
        channel: opener.channel.clone(),
        step_up: opener.step_up.clone(),
        result: "done".to_string(),
        rtt_ms: None,
        run_id: None,
    };
    // The instance's own revision moves with `instances_rev`: a running hub sees both
    // and connects this instance anew, with the new value.
    store
        .tx(|tx| -> rusqlite::Result<()> {
            inventory::touch_instance(tx, instance, now)?;
            inventory::bump_instances_rev(tx)?;
            audit::append(tx, &event, key.as_ref())?;
            Ok(())
        })
        .map_err(|error| {
            CliError::new(format!("the secret was stored, but the hub could not be told: {error}"))
                .hint("Restart the hub to use it: sudo systemctl restart swifty-home")
        })?;
    println!("Stored {slot} for {instance}.");
    println!("A running hub connects {instance} again with it within a few seconds.");
    Ok(ExitCode::SUCCESS)
}

// ----- new

/// Writes the crate and says what to do next.
pub fn new(args: NewArgs, here: &Path) -> Result<ExitCode, CliError> {
    let request = NewPlugin { id: args.id, classes: args.class, name: args.name, cloud: args.cloud };
    scaffold::check_id(&request.id).map_err(CliError::new)?;
    // Check everything before anything is created.
    scaffold::render(&request, Layout::Standalone).map_err(CliError::new)?;
    let placement = scaffold::place(&request.id, args.dir.as_deref(), here).map_err(CliError::new)?;
    let rendered = scaffold::render(&request, placement.layout).map_err(CliError::new)?;
    scaffold::write(&rendered, &placement.dir).map_err(CliError::new)?;

    let dir = placement.dir.display();
    println!("Created {} in {dir} ({} files).", rendered.crate_name, rendered.files.len());
    println!();
    println!("Next:");
    match placement.layout {
        Layout::Workspace => {
            println!("  cargo test -p {}     its tests pass as written", rendered.crate_name);
        }
        Layout::Standalone => {
            println!("  cd {dir} && cargo test");
            println!();
            println!("  It depends on swifty-plugin-api 0.1 and swifty-plugin-testkit 0.1 by version.");
            println!("  Until they are published, create the plugin inside a swifty-home checkout instead:");
            println!("  run the same command there, and it goes in plugins/.");
        }
    }
    println!("  Then read {}: it says what to change, in order.", placement.dir.join("DEVELOPING.md").display());
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_are_as_wide_as_their_widest_cell() {
        let rows = vec![
            vec!["ID".to_string(), "NAME".to_string(), "STATUS".to_string()],
            vec!["mock".to_string(), "Mock devices".to_string(), "ready".to_string()],
        ];
        assert_eq!(table(&rows), ["  ID    NAME          STATUS", "  mock  Mock devices  ready"]);
    }

    #[test]
    fn a_slot_is_one_the_plugin_declares() {
        let hue = find_plugin("hue").unwrap().manifest();
        assert!(declares_slot(hue, "app_key"));
        assert!(!declares_slot(hue, "other"));
        assert!(!declares_slot(hue, "../app_key"));
        let tuya = find_plugin("tuya").unwrap().manifest();
        let row_slot = tuya.secrets.iter().find_map(|slot| slot.slot.split_once('/')).map(|(base, _)| base);
        if let Some(base) = row_slot {
            assert!(declares_slot(tuya, &format!("{base}/socket1")));
            assert!(!declares_slot(tuya, &format!("{base}/")));
            assert!(!declares_slot(tuya, base));
        }
    }

    #[test]
    fn the_command_line_is_the_local_administrator() {
        let opener = opener("someone");
        assert_eq!(opener.channel, "cli");
        assert_eq!(opener.actor.session_ref, "local");
        assert_eq!(opener.actor.device, "someone");
        assert_eq!(opener.actor.role, Role::Owner);
    }

    #[test]
    fn an_instance_is_ready_or_says_why_not() {
        assert_eq!(status_text(&Readiness::Ready, None), "ready");
        assert_eq!(status_text(&Readiness::NeedsSetup("Pair it.".into()), None), "needs setup: Pair it.");
        assert_eq!(status_text(&Readiness::Ready, Some("Gone.")), "unavailable: Gone.");
    }
}
