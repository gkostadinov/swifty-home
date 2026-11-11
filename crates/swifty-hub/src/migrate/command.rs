//! `swifty migrate`, step by step.
//!
//! 1. Refuses while something answers on the old `listen` address, when the target
//!    directories already hold a database or any file it would write, or when the
//!    new files cannot be given to their owner.
//! 2. Reads the old file with the frozen version 1 parser, and converts it. The site
//!    file, if given, is laid over the converted instances, and its `[hub]` over the
//!    new `swifty.toml`. What the old service compiled in must be pinned there.
//! 3. Reads the old database as immutable, verifies its audit chain with the old key
//!    and notes the checkpoint. A chain that does not verify stops everything.
//! 4. to 11. are first rehearsed on a copy in memory, so whatever would refuse does
//!    so before anything is written. `--dry-run` stops there and prints the plan.
//!    Otherwise, for real: the directories, the database by online backup, the key
//!    files and any further host's files with their modes, the current schema, the instances
//!    and their secrets, the site file's names, notes and scenes, every open session
//!    ended, the old router's rows removed, one audit row `system.migrate`, the chain
//!    verified again against the checkpoint, and `swifty.toml` written last. A
//!    failure removes everything this run wrote.
//!
//! Nothing under the old directories is written, ever.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use swifty_net::Timeouts;
use swifty_plugin_api::{Plugin, Readiness};

use super::snapshot::{self, Rehearsal};
use super::{map_legacy_to, required_pins, FileKind, Instance, Mapped, Pin, Places};
use crate::admin::files::{self, Created, Ownership};
use crate::admin::site::{self, Arranged, InstanceRow, SiteError, SiteFile};
use crate::audit::{self, AuditKey, Checkpoint, VerifyOptions};
use crate::config::legacy;
use crate::inventory;
use crate::runtime::{health, Registry};
use crate::secrets::{FileSecretStore, MemorySecretStore, SecretStore};
use crate::store::{Store, SCHEMA_VERSION};

/// The audit action of a migration.
pub const ACTION: &str = "system.migrate";
/// Who created the instance rows a migration writes.
const CREATED_BY: &str = "migrate";
/// The old house's router. It had no adapter, and no plugin carries it now.
const LEGACY_ROUTER: &str = "router";
/// The tables whose rows name a device by its target, and what a person calls them.
const TARGET_TABLES: [(&str, &str); 5] = [
    ("favourites", "favourites"),
    ("room_devices", "room places"),
    ("device_names", "names"),
    ("device_notes", "notes"),
    ("learned_phrases", "learned phrases"),
];
const OLD_SERVICE: &str = "glimdock-home";
const NEW_SERVICE: &str = "swifty-home";

/// Why a migration stopped, and what to do about it. It never shows a credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrateError {
    pub message: String,
    pub hint: Option<String>,
}

impl MigrateError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into(), hint: None }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl fmt::Display for MigrateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for MigrateError {}

impl From<rusqlite::Error> for MigrateError {
    fn from(error: rusqlite::Error) -> Self {
        Self::new(format!("the database: {error}"))
    }
}

impl From<SiteError> for MigrateError {
    fn from(error: SiteError) -> Self {
        Self::new(error.0)
    }
}

fn io_error(what: &str, path: &Path) -> impl FnOnce(io::Error) -> MigrateError {
    let text = format!("cannot {what} {}", path.display());
    move |error| {
        let hint = (error.kind() == io::ErrorKind::PermissionDenied).then_some("Run it with sudo.");
        let error = MigrateError::new(format!("{text}: {error}"));
        match hint {
            Some(hint) => error.hint(hint),
            None => error,
        }
    }
}

/// What to migrate, and how.
pub struct Request<'a> {
    /// The old `config.toml`.
    pub from: PathBuf,
    /// The site file and where it was read from.
    pub site: Option<(PathBuf, SiteFile)>,
    /// The new `swifty.toml`. Its directory gets the hub's key files.
    pub config_path: PathBuf,
    /// The new data directory: the database and the secret store.
    pub data_dir: PathBuf,
    /// The plugins of this build. Every instance must be one of theirs.
    pub plugins: Vec<&'static dyn Plugin>,
    /// Who the new files belong to.
    pub ownership: Ownership,
    /// Why they cannot be given to their owner, when that is so: a real run refuses,
    /// a dry-run says so.
    pub ownership_problem: Option<String>,
    /// Who runs it, for the audit row: the Unix user name.
    pub actor: String,
    pub now: i64,
    /// Ok when this process could listen on the address. Listening is the binary's
    /// own business, so it is handed in.
    pub listen_probe: &'a dyn Fn(&str) -> io::Result<()>,
    pub dry_run: bool,
}

/// One instance, as the migration left it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceLine {
    pub id: String,
    pub plugin: String,
    /// "ready", or why not.
    pub readiness: String,
    pub ready: bool,
    /// The keys the site file set, with their values.
    pub from_site: Vec<String>,
    /// The secret slots that hold a value. Never the value.
    pub slots: Vec<String>,
    /// Made from the site file alone, with no section in the old file.
    pub site_only: bool,
}

/// One device, by target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceLine {
    pub target: String,
    pub class: String,
    pub instance: String,
    pub name: String,
    pub pending: bool,
}

/// One file of the old install and its copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copied {
    pub what: &'static str,
    pub from: PathBuf,
    pub to: PathBuf,
}

/// What a migration did, or with `--dry-run` would do.
#[derive(Debug, Clone)]
pub struct Report {
    pub dry_run: bool,
    pub from: PathBuf,
    pub site: Option<PathBuf>,
    pub config_path: PathBuf,
    pub places: Places,
    /// What step 1 found.
    pub checks: Vec<String>,
    /// The old schema version.
    pub old_schema: i64,
    /// Rows in the old audit log, and its checkpoint (none for an empty log).
    pub old_rows: i64,
    pub checkpoint: Option<String>,
    pub keyed: bool,
    pub copies: Vec<Copied>,
    pub instances: Vec<InstanceLine>,
    /// Placeholder instances the upgrade left that nothing filled.
    pub unfilled: Vec<String>,
    pub devices: Vec<DeviceLine>,
    /// `swifty.toml`: what it keeps, and what the site file pinned.
    pub listen: String,
    pub origin: String,
    pub rp_id: String,
    pub runs_on: Option<String>,
    pub pins: Vec<String>,
    pub arranged: Option<Arranged>,
    pub sessions_ended: usize,
    pub router_rows: Vec<(String, usize)>,
    pub dropped: Vec<String>,
    /// The audit checkpoint after the migration, with its own row.
    pub head: String,
    /// Who will own the new files, or why they cannot, as a sentence.
    pub ownership: String,
}

/// The old configuration converted, with the site file laid over it.
struct Converted {
    mapped: Mapped,
    from_site: BTreeMap<String, Vec<String>>,
    site_only: BTreeSet<String>,
    disabled: BTreeSet<String>,
    pins: Vec<String>,
}

/// What the database steps did, on the rehearsal or for real.
struct Applied {
    instances: Vec<InstanceLine>,
    unfilled: Vec<String>,
    devices: Vec<DeviceLine>,
    arranged: Option<Arranged>,
    sessions_ended: usize,
    router_rows: Vec<(String, usize)>,
    head: String,
}

/// What the database steps need.
struct Context<'a> {
    converted: &'a Converted,
    site: Option<&'a SiteFile>,
    plugins: &'a [&'static dyn Plugin],
    key: Option<&'a AuditKey>,
    checkpoint: Option<&'a Checkpoint>,
    old_schema: i64,
    actor: &'a str,
    now: i64,
    timeouts: Timeouts,
}

/// Runs `swifty migrate`. With `dry_run`, writes nothing anywhere and reports what it
/// would do; otherwise does it, or leaves everything as it was.
pub fn run(request: Request<'_>) -> Result<Report, MigrateError> {
    let config_dir = request
        .config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let places = Places { config_dir, data_dir: request.data_dir.clone() };

    // 2, first: step 1 needs the old address.
    let old = legacy::Config::load(&request.from).map_err(|error| {
        MigrateError::new(format!("cannot use {} as a glimdock-home configuration: {error}", request.from.display()))
    })?;

    // 1. Refusals.
    let mut checks = Vec::new();
    match (request.listen_probe)(&old.listen) {
        Ok(()) => checks.push(format!("nothing answers on {}, where the old hub listened", old.listen)),
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
            return Err(MigrateError::new(format!(
                "something still answers on {}, the address glimdock-home listens on",
                old.listen
            ))
            .hint(format!("Stop the old hub first: sudo systemctl stop {OLD_SERVICE}")));
        }
        Err(error) => {
            return Err(MigrateError::new(format!(
                "cannot listen on {}, where the new hub will listen: {error}",
                old.listen
            )));
        }
    }
    refuse_old_directories(&request, &old, &places)?;
    let mapped = map_legacy_to(&old, &places, |path| std::fs::read_to_string(path))
        .map_err(|error| MigrateError::new(format!("the old configuration cannot be converted: {error}")))?;
    check_targets_free(&request.config_path, &places, &mapped)?;
    checks.push(format!(
        "nothing of a Swifty Home install is in {} or {} yet",
        places.config_dir.display(),
        places.data_dir.display()
    ));
    let ownership = match (&request.ownership_problem, request.ownership) {
        (Some(problem), _) if !request.dry_run => {
            return Err(MigrateError::new(problem.clone())
                .hint("Install Swifty Home first without starting it: install.sh --no-start --no-init"));
        }
        (Some(problem), _) => format!("the new files have no owner to go to yet: {problem}; a real run refuses"),
        (None, Ownership::Current) => "the new files will belong to whoever runs swifty migrate".to_string(),
        (None, Ownership::Service { uid, gid }) => format!(
            "the new files will belong to user {uid} and group {gid} (the configuration directory and \
             swifty.toml stay root's)"
        ),
    };

    let site = request.site.as_ref().map(|(_, site)| site);
    let converted = convert(&old, mapped, site, &request.plugins)?;

    // 3. The old database and its audit chain.
    let source = snapshot::open_old(&old.database)?;
    let key = old
        .audit
        .key_file
        .as_deref()
        .map(AuditKey::load)
        .transpose()
        .map_err(|error| MigrateError::new(format!("the old audit key: {error}")))?;
    let rehearsal = Rehearsal::copy(&source)?;
    let old_schema = snapshot::old_version(rehearsal.conn(), &old.database)?;
    let before =
        audit::verify_with(rehearsal.conn(), VerifyOptions { key: key.as_ref(), expected: None, allow_empty: true })?;
    if !before.ok {
        return Err(MigrateError::new(format!(
            "the old audit log does not verify{}: {}. Nothing was changed",
            before.first_bad_seq.map(|seq| format!(" at row {seq}")).unwrap_or_default(),
            before.problem.as_deref().unwrap_or("unknown problem")
        ))
        .hint("Find out why with the old binary's audit-verify before moving anything."));
    }
    let checkpoint: Option<Checkpoint> =
        if before.empty { None } else { Some(before.checkpoint.parse().map_err(MigrateError::new)?) };
    let host_files = further_host_files(rehearsal.conn(), &old, &places)?;

    // 4 to 11, rehearsed on the copy in memory.
    let timeouts: Timeouts = (&converted.mapped.config.network).into();
    let context = Context {
        converted: &converted,
        site,
        plugins: &request.plugins,
        key: key.as_ref(),
        checkpoint: checkpoint.as_ref(),
        old_schema,
        actor: &request.actor,
        now: request.now,
        timeouts,
    };
    let rehearsed = {
        let store = Store::open(rehearsal.path())?;
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::default());
        apply(&store, secrets, &context)?
    };
    drop(rehearsal);

    let applied = if request.dry_run {
        rehearsed
    } else {
        let mut created = Created::default();
        match install(&request, &places, &converted, &source, &host_files, &context, &mut created) {
            Ok(applied) => {
                created.keep();
                applied
            }
            Err(error) => {
                created.undo();
                return Err(error);
            }
        }
    };

    let mut copies = vec![Copied { what: "database", from: old.database.clone(), to: places.database() }];
    for file in &converted.mapped.files {
        let what = match file.kind {
            FileKind::AuditKey => "audit key",
            FileKind::SpeechToken => "speech token",
            FileKind::AssistantKey => "assistant key",
            FileKind::Database | FileKind::FurtherHostFiles => continue,
        };
        copies.push(Copied { what, from: file.from.clone(), to: file.to.clone() });
    }
    copies.extend(host_files.copies.iter().map(|(from, to)| Copied {
        what: "further host file",
        from: from.clone(),
        to: to.clone(),
    }));
    let mut dropped: Vec<String> =
        converted.mapped.dropped.iter().map(|dropped| format!("{}: {}", dropped.what, dropped.why)).collect();
    dropped.extend(host_files.missing.iter().cloned());
    let config = &converted.mapped.config;
    Ok(Report {
        dry_run: request.dry_run,
        from: request.from.clone(),
        site: request.site.as_ref().map(|(path, _)| path.clone()),
        config_path: request.config_path.clone(),
        places,
        checks,
        old_schema,
        old_rows: before.rows,
        checkpoint: checkpoint.map(|checkpoint| format!("{}:{}", checkpoint.seq, checkpoint.hash)),
        keyed: before.keyed,
        copies,
        instances: applied.instances,
        unfilled: applied.unfilled,
        devices: applied.devices,
        listen: config.listen.clone(),
        origin: config.webauthn.origin.clone(),
        rp_id: config.webauthn.rp_id.clone(),
        runs_on: config.runs_on.clone(),
        pins: converted.pins.clone(),
        arranged: applied.arranged,
        sessions_ended: applied.sessions_ended,
        router_rows: applied.router_rows,
        dropped,
        head: applied.head,
        ownership,
    })
}

/// The new directories may not be the old ones: nothing of the old install is written.
fn refuse_old_directories(request: &Request<'_>, old: &legacy::Config, places: &Places) -> Result<(), MigrateError> {
    let real = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut old_dirs = Vec::new();
    for path in [Some(request.from.as_path()), Some(old.database.as_path()), old.audit.key_file.as_deref()]
        .into_iter()
        .flatten()
    {
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            old_dirs.push(real(parent));
        }
    }
    for new in [&places.config_dir, &places.data_dir] {
        let new = real(new);
        if old_dirs.iter().any(|old| new.starts_with(old)) {
            return Err(MigrateError::new(format!(
                "{} is where the old install keeps its files, and swifty migrate never writes there",
                new.display()
            ))
            .hint("Give the new install its own directories: --config /etc/swifty-home/swifty.toml --data-dir /var/lib/swifty-home"));
        }
    }
    Ok(())
}

/// Step 1: the new install is not there yet. Nothing is ever replaced.
fn check_targets_free(config_path: &Path, places: &Places, mapped: &Mapped) -> Result<(), MigrateError> {
    let taken = |path: &Path| std::fs::symlink_metadata(path).is_ok();
    let again =
        "If an earlier attempt left it, move it aside (the old install is untouched) and run swifty migrate again.";
    if taken(config_path) {
        return Err(MigrateError::new(format!(
            "{} already exists, and swifty migrate sets up a new install only",
            config_path.display()
        ))
        .hint(again));
    }
    let database = places.database();
    for path in [database.clone(), snapshot::beside(&database, "-wal"), snapshot::beside(&database, "-journal")] {
        if taken(&path) {
            return Err(
                MigrateError::new(format!("{} already holds a database", places.data_dir.display())).hint(again)
            );
        }
    }
    if places.data_dir.is_dir() {
        let entries = std::fs::read_dir(&places.data_dir).map_err(io_error("read", &places.data_dir))?;
        for entry in entries {
            let entry = entry.map_err(io_error("read", &places.data_dir))?;
            let path = entry.path();
            let empty_secrets = entry.file_name() == crate::config::v2::SECRETS_DIR
                && path.is_dir()
                && std::fs::read_dir(&path).map_err(io_error("read", &path))?.next().is_none();
            if !empty_secrets {
                return Err(MigrateError::new(format!(
                    "{} is not empty: {} is in it",
                    places.data_dir.display(),
                    path.display()
                ))
                .hint(again));
            }
        }
    }
    for file in mapped.files.iter().filter(|file| file.kind != FileKind::Database) {
        if taken(&file.to) {
            return Err(MigrateError::new(format!("{} already exists", file.to.display())).hint(again));
        }
    }
    Ok(())
}

/// The keys of a site instance's config, each with its value, for the report.
fn shown_keys(config: &Value) -> Vec<String> {
    let Some(object) = config.as_object() else {
        return Vec::new();
    };
    object
        .iter()
        .map(|(key, value)| {
            let mut text = value.to_string();
            if text.chars().count() > 72 {
                text = text.chars().take(69).collect::<String>() + "...";
            }
            format!("{key} = {text}")
        })
        .collect()
}

/// The site file over the converted configuration, with every pin the old install
/// needs, and every instance checked by its plugin.
fn convert(
    old: &legacy::Config,
    mut mapped: Mapped,
    site: Option<&SiteFile>,
    plugins: &[&'static dyn Plugin],
) -> Result<Converted, MigrateError> {
    const IN_SITE_FILE: &str = " in the site file (--site)";
    let mut pins = Vec::new();
    for pin in required_pins(old) {
        match pin {
            Pin::ToshibaClientSuffix => {
                let pinned = site
                    .and_then(|site| site.instance("toshiba"))
                    .and_then(|instance| instance.config.get("client_suffix"));
                if let Some(suffix) = pinned {
                    pins.push(format!("toshiba: client_suffix = {suffix}"));
                } else {
                    return Err(MigrateError::new(format!(
                        "the Toshiba cloud knows the old hub by a client id whose suffix glimdock-home compiled in, \
                         and it is not pinned{IN_SITE_FILE}"
                    ))
                    .hint(
                        "Add it to the site file, as the old hub sent it (hex):\n  [[instance]]\n  id = \"toshiba\"\n  \
                         plugin = \"toshiba\"\n  [instance.config]\n  client_suffix = \"...\"\n\
                         Pinning the plugin's own suffix instead registers this hub as a new client.",
                    ));
                }
            }
            Pin::AgentIdentity => {
                let agent = site.and_then(SiteFile::agent_pins);
                let missing: Vec<&str> = [
                    ("session_prefix", agent.is_some_and(|agent| agent.session_prefix.is_some())),
                    ("client_header", agent.is_some_and(|agent| agent.client_header.is_some())),
                    ("client_name", agent.is_some_and(|agent| agent.client_name.is_some())),
                ]
                .into_iter()
                .filter_map(|(key, given)| (!given).then_some(key))
                .collect();
                if !missing.is_empty() {
                    return Err(MigrateError::new(format!(
                        "the agent knows the old hub by an identity glimdock-home compiled in, and {} not pinned{IN_SITE_FILE}",
                        if missing.len() == 1 { format!("{} is", missing[0]) } else { format!("{} are", missing.join(", ")) },
                    ))
                    .hint(
                        "Add what the old hub sent to the site file:\n  [hub.assistant.agent]\n  session_prefix = \"...\"   \
                         (its chats were <prefix>-<user>-<chat>)\n  client_header = \"...\"    (the header's name, \
                         lower case)\n  client_name = \"...\"      (the header's value)\nPinning Swifty Home's own values \
                         instead starts the agent's chats afresh.",
                    ));
                }
            }
        }
    }

    let mut from_site = BTreeMap::new();
    let mut site_only = BTreeSet::new();
    let mut disabled = BTreeSet::new();
    if let Some(site) = site {
        for wanted in &site.instances {
            if wanted.enabled == Some(false) {
                disabled.insert(wanted.id.clone());
            }
            from_site.insert(wanted.id.clone(), shown_keys(&wanted.config));
            match mapped.instances.iter_mut().find(|instance| instance.id == wanted.id) {
                Some(instance) if instance.plugin != wanted.plugin => {
                    return Err(MigrateError::new(format!(
                        "the site file says instance {} is a {} instance, and the old configuration makes it a {} one",
                        wanted.id, wanted.plugin, instance.plugin
                    )));
                }
                Some(instance) => instance.config = site::overlay(&instance.config, &wanted.config),
                None => {
                    let version = plugins
                        .iter()
                        .find(|plugin| plugin.manifest().id == wanted.plugin)
                        .map_or(1, |plugin| plugin.manifest().config_version);
                    mapped.instances.push(Instance {
                        id: wanted.id.clone(),
                        plugin: wanted.plugin.clone(),
                        config_version: version,
                        config: wanted.config.clone(),
                    });
                    site_only.insert(wanted.id.clone());
                }
            }
        }
        if let Some(agent_pins) = site.agent_pins() {
            let Some(agent) = mapped.config.assistant.as_mut().and_then(|assistant| assistant.agent.as_mut()) else {
                return Err(MigrateError::new(
                    "the site file pins [hub.assistant.agent], and the old configuration has no [assistant.agent] \
                     for it to apply to",
                )
                .hint("Remove [hub.assistant.agent] from the site file given to swifty migrate."));
            };
            for (key, value) in agent_pins.keys() {
                let slot = match key {
                    "assistant.agent.session_prefix" => &mut agent.session_prefix,
                    "assistant.agent.client_header" => &mut agent.client_header,
                    _ => &mut agent.client_name,
                };
                *slot = value.to_string();
                pins.push(format!("{key} = {value:?}"));
            }
            mapped.config.validate().map_err(|error| {
                MigrateError::new(format!("the site file's [hub] gives swifty.toml a value it cannot take: {error}"))
            })?;
        }
    }
    for instance in &mapped.instances {
        site::check_instance(plugins, &instance.id, &instance.plugin, &instance.config)?;
    }
    Ok(Converted { mapped, from_site, site_only, disabled, pins })
}

/// The further Proxmox hosts' files: only the ones the stored hosts name.
#[derive(Debug, Default)]
struct HostFiles {
    copies: Vec<(PathBuf, PathBuf)>,
    missing: Vec<String>,
}

fn further_host_files(conn: &Connection, old: &legacy::Config, places: &Places) -> Result<HostFiles, MigrateError> {
    let mut found = HostFiles::default();
    let rows: Vec<(String, String, String)> = {
        let mut statement = conn.prepare("SELECT id, token_file, ca_file FROM proxmox_hosts ORDER BY id")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let source_dir = old.machines.secrets_dir.as_deref().filter(|_| old.devices.proxmox.is_some());
    let mut seen = BTreeSet::new();
    for (id, token_file, ca_file) in rows {
        let Some(dir) = source_dir else {
            found.missing.push(format!(
                "further host {id}: the old configuration has no [machines] secrets_dir with [devices.proxmox], \
                 so its files have no place; it stays unavailable until it is added again"
            ));
            continue;
        };
        for name in [token_file, ca_file] {
            let bare = Path::new(&name).file_name().is_some_and(|file| file == name.as_str())
                && !name.starts_with('.')
                && !name.is_empty();
            let from = dir.join(&name);
            if !bare || !from.is_file() {
                found.missing.push(format!(
                    "further host {id}: {} cannot be read; the host stays unavailable until its file is put in {}",
                    from.display(),
                    places.host_files().display()
                ));
                continue;
            }
            if seen.insert(name.clone()) {
                found.copies.push((from, places.host_files().join(&name)));
            }
        }
    }
    Ok(found)
}

/// How a stored readiness reads.
fn readiness_text(readiness: &Readiness) -> (bool, String) {
    match readiness {
        Readiness::Ready => (true, "ready".to_string()),
        Readiness::NeedsSetup(reason) => (false, format!("waits for setup: {reason}")),
        Readiness::Disabled(reason) => (false, format!("cannot work: {reason}")),
    }
}

/// Removes the rows that name the old router, unless a device has its target now.
fn remove_router_rows(conn: &Connection) -> rusqlite::Result<Vec<(String, usize)>> {
    if inventory::device_exists(conn, LEGACY_ROUTER)? {
        return Ok(Vec::new());
    }
    let mut removed = Vec::new();
    for (table, label) in TARGET_TABLES {
        let count = conn.execute(&format!("DELETE FROM {table} WHERE target = ?1"), params![LEGACY_ROUTER])?;
        if count > 0 {
            removed.push((label.to_string(), count));
        }
    }
    Ok(removed)
}

/// Steps 7, 9 and 10 on an upgraded database, then 11. The same on the rehearsal and
/// for real.
fn apply(store: &Store, secrets: Arc<dyn SecretStore>, context: &Context<'_>) -> Result<Applied, MigrateError> {
    let mapped = &context.converted.mapped;
    for value in &mapped.secrets {
        secrets.write(&value.instance, &value.slot, &value.value).map_err(|error| {
            MigrateError::new(format!("cannot store the {} of the {} instance: {error}", value.slot, value.instance))
        })?;
    }
    let registry = Registry::new(false, context.plugins.to_vec(), secrets.clone(), context.timeouts);
    let mut unfilled = Vec::new();
    let mut arranged = None;
    let mut sessions_ended = 0;
    let mut router_rows = Vec::new();
    store.tx(|tx| -> Result<(), MigrateError> {
        let placeholders = inventory::instance_specs(tx)?;
        for instance in &mapped.instances {
            if let Some(stored) = placeholders.iter().find(|spec| spec.id == instance.id) {
                if stored.plugin != instance.plugin {
                    return Err(MigrateError::new(format!(
                        "the database has an instance {} of plugin {}, and the configuration makes it {}",
                        instance.id, stored.plugin, instance.plugin
                    )));
                }
            }
            let row = InstanceRow {
                id: &instance.id,
                plugin: &instance.plugin,
                config_version: instance.config_version,
                enabled: !context.converted.disabled.contains(&instance.id),
                config: &instance.config,
            };
            site::store_instance(tx, context.now, CREATED_BY, row)?;
        }
        unfilled = placeholders
            .iter()
            .filter(|spec| !mapped.instances.iter().any(|instance| instance.id == spec.id))
            .map(|spec| spec.id.clone())
            .collect();
        inventory::bump_instances_rev(tx)?;
        site::store_declared_devices(tx, &registry, context.now)?;
        sessions_ended = tx.execute(
            "UPDATE sessions SET ended_at = ?1, end_reason = 'upgrade' WHERE ended_at IS NULL",
            params![context.now],
        )?;
        router_rows = remove_router_rows(tx)?;
        if let Some(site) = context.site {
            arranged = Some(site::apply_arrangements(tx, site)?);
        }
        let params = json!({
            "from": OLD_SERVICE,
            "schema": [context.old_schema, SCHEMA_VERSION],
            "checkpoint": context.checkpoint.map(|checkpoint| format!("{}:{}", checkpoint.seq, checkpoint.hash)),
            "instances": mapped.instances.iter().map(|instance| instance.id.as_str()).collect::<Vec<_>>(),
            "sessions_ended": sessions_ended,
            "removed": router_rows.iter().map(|(what, count)| json!({ "target": LEGACY_ROUTER, "rows": what, "count": count })).collect::<Vec<_>>(),
        });
        audit::append(tx, &site::cli_event(ACTION, "-", params, context.actor, context.now), context.key)?;
        Ok(())
    })?;

    // What the hub will make of it.
    let (specs, stored) = store.with(|conn| -> rusqlite::Result<_> {
        Ok((inventory::instance_specs(conn)?, inventory::stored_devices(conn)?))
    })?;
    registry.load(specs, stored.clone());
    let instances = health::statuses(&registry)
        .into_iter()
        .map(|status| {
            let (ready, readiness) = readiness_text(&status.readiness);
            let slots = secrets.slots(&status.id).map(|slots| slots.into_iter().collect()).unwrap_or_default();
            InstanceLine {
                from_site: context.converted.from_site.get(&status.id).cloned().unwrap_or_default(),
                site_only: context.converted.site_only.contains(&status.id),
                id: status.id,
                plugin: status.plugin,
                readiness,
                ready,
                slots,
            }
        })
        .collect();
    let devices = stored
        .into_iter()
        .map(|device| DeviceLine {
            class: serde_json::to_value(device.class)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_default(),
            target: device.target,
            instance: device.instance,
            name: device.name,
            pending: device.pending,
        })
        .collect();

    // 11. The chain again, against the checkpoint from step 3.
    let after = store.with(|conn| {
        audit::verify_with(conn, VerifyOptions { key: context.key, expected: context.checkpoint, allow_empty: false })
    })?;
    if !after.ok {
        return Err(MigrateError::new(format!(
            "after the migration the audit log does not verify: {}",
            after.problem.as_deref().unwrap_or("unknown problem")
        )));
    }
    Ok(Applied { instances, unfilled, devices, arranged, sessions_ended, router_rows, head: after.checkpoint })
}

/// Copies one of the old install's own files, with its mode.
fn copy_private(from: &Path, to: &Path, created: &mut Created) -> Result<(), MigrateError> {
    let bytes = std::fs::read(from).map_err(io_error("read", from))?;
    let mode = files::mode_of(from).map_err(io_error("read", from))?.unwrap_or(0o600) & 0o777;
    files::write_atomic(to, &bytes, mode, false).map_err(io_error("write", to))?;
    created.file(to);
    Ok(())
}

/// Gives a directory and everything in it to a user and group. Never follows a link.
fn give_tree(path: &Path, uid: u32, gid: u32) -> io::Result<()> {
    files::set_owner(path, uid, gid)?;
    if std::fs::symlink_metadata(path)?.is_dir() {
        for entry in std::fs::read_dir(path)? {
            give_tree(&entry?.path(), uid, gid)?;
        }
    }
    Ok(())
}

/// Steps 4 to 11 for real. Every path it makes is in `created`, for the caller to
/// remove on failure.
fn install(
    request: &Request<'_>,
    places: &Places,
    converted: &Converted,
    source: &Connection,
    host_files: &HostFiles,
    context: &Context<'_>,
    created: &mut Created,
) -> Result<Applied, MigrateError> {
    let mapped = &converted.mapped;
    // 4. Directories.
    let made_config = created.make_dir(&places.config_dir, 0o750).map_err(io_error("create", &places.config_dir))?;
    created.make_dir(&places.data_dir, 0o700).map_err(io_error("create", &places.data_dir))?;

    // 5. The database, by online backup, then the hub's own files.
    let database = places.database();
    let pending = places.data_dir.join(format!(
        ".{}.{}.migrating",
        crate::config::v2::DATABASE_FILE,
        hex::encode(crate::crypto::random_bytes::<4>())
    ));
    created.file(&pending);
    created.file(snapshot::beside(&pending, "-journal"));
    {
        let mut target = Connection::open(&pending)
            .map_err(|error| MigrateError::new(format!("cannot create {}: {error}", pending.display())))?;
        snapshot::backup(source, &mut target)
            .map_err(|error| MigrateError::new(format!("cannot copy the old database: {error}")))?;
    }
    std::fs::rename(&pending, &database).map_err(io_error("write", &database))?;
    for side in ["", "-wal", "-shm", "-journal"] {
        created.file(snapshot::beside(&database, side));
    }
    files::set_mode(&database, 0o600).map_err(io_error("protect", &database))?;
    for file in &mapped.files {
        if matches!(file.kind, FileKind::AuditKey | FileKind::SpeechToken | FileKind::AssistantKey) {
            copy_private(&file.from, &file.to, created)?;
        }
    }
    if !host_files.copies.is_empty() {
        created.make_dir(&places.host_files(), 0o700).map_err(io_error("create", &places.host_files()))?;
        for (from, to) in &host_files.copies {
            copy_private(from, to, created)?;
        }
    }

    // 6. Opening the copy brings it to the current schema.
    let store = Store::open(&database).map_err(|error| {
        MigrateError::new(format!("cannot bring the copy {} to schema {SCHEMA_VERSION}: {error}", database.display()))
    })?;

    // 7, 9, 10 and 11, with the secrets in the new store and the copied key.
    let secrets_root = mapped.config.secrets_path();
    created.make_dir(&secrets_root, 0o700).map_err(io_error("create", &secrets_root))?;
    for instance in mapped.secrets.iter().map(|value| value.instance.as_str()).collect::<BTreeSet<_>>() {
        created.dir(secrets_root.join(instance));
    }
    let key = mapped
        .config
        .audit
        .key_file
        .as_deref()
        .map(AuditKey::load)
        .transpose()
        .map_err(|error| MigrateError::new(format!("the copied audit key: {error}")))?;
    let context = Context { key: key.as_ref(), ..*context };
    let applied = apply(&store, Arc::new(FileSecretStore::new(&secrets_root)), &context)?;
    drop(store);

    // 8. swifty.toml, last, so a run that stopped half way leaves nothing to start on.
    let text = mapped.config.to_toml().map_err(|error| MigrateError::new(error.to_string()))?;
    let written = format!(
        "# Written by `swifty migrate` from {}. Check it with `swifty config check`.\n{text}",
        request.from.display()
    );
    files::write_atomic(&request.config_path, written.as_bytes(), 0o640, false)
        .map_err(io_error("write", &request.config_path))?;
    created.file(&request.config_path);

    if let Ownership::Service { uid, gid } = request.ownership {
        let give = |path: &Path, uid: u32| files::set_owner(path, uid, gid).map_err(io_error("give away", path));
        if made_config {
            give(&places.config_dir, 0)?;
        }
        give(&request.config_path, 0)?;
        for file in mapped.files.iter().filter(|file| file.kind != FileKind::Database) {
            if std::fs::symlink_metadata(&file.to).is_ok() && file.kind != FileKind::FurtherHostFiles {
                give(&file.to, uid)?;
            }
        }
        if !host_files.copies.is_empty() {
            give_tree(&places.host_files(), uid, gid).map_err(io_error("give away", &places.host_files()))?;
        }
        give_tree(&places.data_dir, uid, gid).map_err(io_error("give away", &places.data_dir))?;
    }
    Ok(applied)
}

impl Report {
    /// The report, for a person.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let mut line = |text: String| {
            out.push_str(&text);
            out.push('\n');
        };
        let would = |done: &str, plan: &str| if self.dry_run { plan.to_string() } else { done.to_string() };
        line(format!(
            "swifty migrate{}: {} to {}",
            if self.dry_run { " --dry-run (nothing is written)" } else { "" },
            self.from.display(),
            self.config_path.display()
        ));
        line(String::new());
        line("1. Checks".to_string());
        for check in &self.checks {
            line(format!("   {check}"));
        }
        line(format!("   {}", self.ownership));
        line(format!("2. {} was read with the glimdock-home parser.", self.from.display()));
        match &self.checkpoint {
            Some(checkpoint) => line(format!(
                "3. The old audit log verifies{}: {} rows, checkpoint {checkpoint}",
                if self.keyed { " with its key" } else { "" },
                self.old_rows
            )),
            None => line("3. The old audit log is empty: there is no checkpoint to keep.".to_string()),
        }
        line(format!(
            "4. {} {} (0750) and {} (0700).",
            would("Made", "Would make"),
            self.places.config_dir.display(),
            self.places.data_dir.display()
        ));
        line(format!("5. {} (the old files are only read):", would("Copied", "Would copy")));
        for copy in &self.copies {
            line(format!("   {:<18} {} -> {}", copy.what, copy.from.display(), copy.to.display()));
        }
        line(format!(
            "6. The copy {} schema {} (it was {}).",
            would("is at", "would be at"),
            SCHEMA_VERSION,
            self.old_schema
        ));
        line(format!("7. Integrations ({}):", self.instances.len()));
        for instance in &self.instances {
            let origin = if instance.site_only { "from the site file" } else { "from the old configuration" };
            line(format!("   {:<10} {:<10} {}, {origin}", instance.id, instance.plugin, instance.readiness));
            if !instance.slots.is_empty() {
                line(format!("              secrets: {}", instance.slots.join(", ")));
            }
            for key in &instance.from_site {
                line(format!("              site file: {key}"));
            }
        }
        for id in &self.unfilled {
            line(format!(
                "   {id:<10} has devices from the old database and no configuration: run swifty plugin setup {id}"
            ));
        }
        line(format!(
            "8. swifty.toml: listen {}, origin {}, rp_id {}{}",
            self.listen,
            self.origin,
            self.rp_id,
            self.runs_on.as_ref().map(|target| format!(", runs_on {target}")).unwrap_or_default()
        ));
        for pin in &self.pins {
            line(format!("   pinned by the site file: {pin}"));
        }
        match (&self.site, &self.arranged) {
            (Some(site), Some(arranged)) => {
                line(format!(
                    "9. The site file {}: {} names, {} notes, {} scenes {}.",
                    site.display(),
                    arranged.names.len(),
                    arranged.notes.len(),
                    arranged.scenes.len(),
                    would("applied", "to apply")
                ));
                if !arranged.names.is_empty() {
                    line(format!("   names: {}", arranged.names.join(", ")));
                }
                if !arranged.notes.is_empty() {
                    line(format!("   notes: {}", arranged.notes.join(", ")));
                }
                if !arranged.scenes.is_empty() {
                    line(format!("   scenes: {}", arranged.scenes.join(", ")));
                }
                if !arranged.unknown_targets.is_empty() {
                    line(format!(
                        "   no device has these targets yet (their names and notes are kept): {}",
                        arranged.unknown_targets.join(", ")
                    ));
                }
            }
            _ => line("9. No site file was given.".to_string()),
        }
        line(format!(
            "10. {} every open session ({}, reason: upgrade), {} the router's rows, {} one audit row {ACTION}.",
            would("Ended", "Would end"),
            self.sessions_ended,
            would("removed", "would remove"),
            would("appended", "would append")
        ));
        line(format!(
            "11. The audit log {} again{}; head {}",
            would("verifies", "would verify"),
            self.checkpoint.as_ref().map(|checkpoint| format!(" and still contain {checkpoint}")).unwrap_or_default(),
            self.head
        ));
        line(String::new());
        line(format!("Devices ({}), by target:", self.devices.len()));
        let width = self.devices.iter().map(|device| device.target.len()).max().unwrap_or(0);
        for device in &self.devices {
            line(format!(
                "   {:<width$} {:<10} {:<10} {}{}",
                device.target,
                device.class,
                device.instance,
                device.name,
                if device.pending { " (waits for approval)" } else { "" }
            ));
        }
        line(String::new());
        line("Dropped:".to_string());
        let mut dropped = Vec::new();
        if !self.router_rows.is_empty() {
            let rows: Vec<String> = self.router_rows.iter().map(|(what, count)| format!("{what} {count}")).collect();
            dropped
                .push(format!("the {LEGACY_ROUTER} device, which no plugin carries: its rows ({})", rows.join(", ")));
        } else {
            dropped.push(format!("the {LEGACY_ROUTER} device, which no plugin carries (no rows named it)"));
        }
        dropped.extend(self.dropped.iter().cloned());
        for item in dropped {
            line(format!("   {item}"));
        }
        line(String::new());
        if self.dry_run {
            line("Nothing was written. Run the same command without --dry-run to migrate.".to_string());
        } else {
            line("Done. Nothing of the old install was changed.".to_string());
            line(format!("   Switch over: sudo systemctl disable --now {OLD_SERVICE} && sudo systemctl enable --now {NEW_SERVICE}"));
            if let Some(checkpoint) = &self.checkpoint {
                line(format!(
                    "   Check:       sudo swifty doctor && sudo swifty audit verify --expect-head {checkpoint}"
                ));
            } else {
                line("   Check:       sudo swifty doctor && sudo swifty audit verify".to_string());
            }
            line(format!("   Roll back:   sudo systemctl disable --now {NEW_SERVICE} && sudo systemctl enable --now {OLD_SERVICE}"));
        }
        out
    }
}
