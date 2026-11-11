//! The checks behind `swifty doctor`, each with its fix.
//!
//! Every check here reads files and the database and nothing else: no socket is
//! opened and no secret value is read, apart from the audit key, which verifying
//! the log needs. Each instance is checked with its plugin's pure calls (is the
//! plugin in this build, does its config validate, is it ready), which read which
//! secret slots hold a value and never a value. Health calls to live instances and
//! the reverse proxy (it needs API v1) are listed as skipped.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Serialize;

use swifty_plugin_api::{InstanceView, Plugin, Readiness};

use super::init::{load_config, ConfigFileError};
use crate::audit::{self, AuditKey, VerifyOptions};
use crate::config::Config;
use crate::inventory;
use crate::runtime::{contained, Origin};
use crate::secrets::{FileSecretStore, SecretStore};
use crate::store::Store;

/// 2024-01-01 in milliseconds. A clock before it is certainly wrong.
const CLOCK_FLOOR_MS: i64 = 1_704_067_200_000;

/// How a check came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Fine.
    Ok,
    /// Works, but worth a look.
    Warn,
    /// Broken. `swifty doctor` exits with 1.
    Error,
    /// Not checked in this build, or not applicable.
    Skip,
}

/// One check: what it looked at, what it found and what to do.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: String,
    pub status: Status,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Check {
    pub fn new(name: &str, status: Status, detail: impl Into<String>) -> Self {
        Self { name: name.to_string(), status, detail: detail.into(), fix: None }
    }

    pub fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

/// Every check, in the order they ran.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    pub config: PathBuf,
    pub checks: Vec<Check>,
}

impl Report {
    fn push(&mut self, check: Check) {
        self.checks.push(check);
    }

    pub fn count(&self, status: Status) -> usize {
        self.checks.iter().filter(|check| check.status == status).count()
    }

    pub fn has_errors(&self) -> bool {
        self.count(Status::Error) > 0
    }
}

/// Runs every check, with the plugins of this build for the instances.
pub fn run(config_path: &Path, now_ms: i64, plugins: &[&'static dyn Plugin]) -> Report {
    let mut report = Report { config: config_path.to_path_buf(), checks: Vec::new() };
    let file = match load_config(config_path) {
        Ok(file) => {
            let mode = if file.live { "live: commands reach devices" } else { "dry-run: no device is reached" };
            report.push(Check::new("config", Status::Ok, format!("version 2, {mode}")));
            file
        }
        Err(error) => {
            report.push(config_problem(config_path, &error));
            report.push(clock(now_ms));
            return report;
        }
    };
    report.push(listen(&file));
    report.push(origin(&file));
    report.checks.extend(files(config_path, &file));
    let database = database(&file);
    let opened = database.status == Status::Ok;
    report.push(database);
    report.push(if opened { audit_chain(&file) } else { Check::new("audit", Status::Skip, "no database to check") });
    report.push(secrets(&file));
    let key_files = [
        ("speech", file.speech.as_ref().map(|speech| speech.token_file.clone())),
        ("assistant", file.assistant.as_ref().map(|assistant| assistant.key_file.clone())),
    ];
    for (name, path) in key_files {
        if let Some(path) = path {
            report.push(key_file(name, &path));
        }
    }
    report.push(if opened { instances(&file, plugins) } else { Check::new("plugins", Status::Skip, "no database") });
    if file.live {
        report.push(Check::new(
            "health",
            Status::Skip,
            "Health calls to each instance are made by the running hub, which shows them on the plugin page.",
        ));
    }
    report.push(proxy(&file));
    report.push(clock(now_ms));
    report
}

/// The reverse proxy in front of the hub, which the doctor does not call: it runs where
/// the hub runs, and the proxy is what other machines reach. It says how to check it.
fn proxy(file: &Config) -> Check {
    let origin = file.webauthn.origin.trim_end_matches('/');
    let host = reqwest::Url::parse(origin).ok().and_then(|url| url.host_str().map(str::to_string)).unwrap_or_default();
    if matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]") {
        return Check::new("proxy", Status::Skip, "no reverse proxy: the address is this machine");
    }
    Check::new(
        "proxy",
        Status::Skip,
        format!("not checked from here: the proxy is what other machines reach at {origin}"),
    )
    .with_fix(format!("From a phone or another computer: curl -sf {origin}/api/v1/meta"))
}

/// Each stored instance, put to its plugin: the plugin is in this build, its config
/// validates, and it is ready. Reads which slots hold a value, never a value.
fn instances(file: &Config, plugins: &[&'static dyn Plugin]) -> Check {
    let read = Store::open_for_inspection(&file.database_path()).and_then(|store| {
        store
            .with(|conn| -> rusqlite::Result<_> {
                Ok((inventory::instance_specs(conn)?, inventory::stored_devices(conn)?))
            })
            .map_err(|error| error.to_string())
    });
    let Ok((specs, devices)) = read else {
        return Check::new("plugins", Status::Skip, "The integrations could not be read from this database.");
    };
    let store = FileSecretStore::new(file.secrets_path());
    let (mut problems, mut waiting) = (Vec::new(), Vec::new());
    for spec in &specs {
        let Some(plugin) = plugins.iter().find(|plugin| plugin.manifest().id == spec.plugin) else {
            problems.push(format!("{} needs the plugin {}, which this build does not have", spec.id, spec.plugin));
            continue;
        };
        if let Err(problem) = contained(|| plugin.validate(&spec.config))
            .and_then(|checked| checked.map_err(|problem| problem.to_string()))
        {
            problems.push(format!("{}: {problem}", spec.id));
            continue;
        }
        let secrets_present = store.slots(&spec.id).unwrap_or_default();
        let adopted: Vec<_> = devices
            .iter()
            .filter(|device| device.instance == spec.id && device.origin == Origin::Adopted && !device.pending)
            .map(|device| device.record())
            .collect();
        let view =
            InstanceView { id: &spec.id, config: &spec.config, secrets_present: &secrets_present, adopted: &adopted };
        match contained(|| plugin.readiness(&view)) {
            Ok(Readiness::Ready) => {}
            Ok(Readiness::NeedsSetup(reason) | Readiness::Disabled(reason)) | Err(reason) => {
                waiting.push(format!("{}: {reason}", spec.id))
            }
        }
    }
    let built = format!("{} plugins in this build", plugins.len());
    if !problems.is_empty() {
        return Check::new("plugins", Status::Error, format!("{built}. {}.", problems.join("; ")))
            .with_fix("Set the integration up again with `swifty plugin setup <id>`, or remove it.");
    }
    if !waiting.is_empty() {
        return Check::new("plugins", Status::Warn, format!("{built}. Waiting: {}.", waiting.join("; ")))
            .with_fix("Finish each one's setup with `swifty plugin setup <id>`.");
    }
    Check::new("plugins", Status::Ok, format!("{built}, {} integration(s) set up and ready.", specs.len()))
}

fn config_problem(path: &Path, error: &ConfigFileError) -> Check {
    let shown = path.display();
    match error {
        ConfigFileError::Read { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
            Check::new("config", Status::Error, format!("{shown} does not exist."))
                .with_fix("Create it with `sudo swifty init`, or name another file with --config.")
        }
        ConfigFileError::Read { source, .. } => {
            Check::new("config", Status::Error, format!("cannot read {shown}: {source}"))
                .with_fix("Run it as root (sudo), or as the user the service runs as.")
        }
        ConfigFileError::Legacy => Check::new("config", Status::Error, "This is a glimdock-home configuration.")
            .with_fix(format!("Run: sudo swifty migrate --from {shown}")),
        ConfigFileError::Invalid(message) => Check::new("config", Status::Error, message.clone())
            .with_fix(format!("Edit {shown}, then run `swifty config check`.")),
    }
}

fn listen(file: &Config) -> Check {
    match file.listen.parse::<SocketAddr>() {
        Ok(address) if address.ip().is_loopback() => {
            Check::new("listen", Status::Ok, format!("{address}, loopback only"))
        }
        Ok(address) => Check::new("listen", Status::Warn, format!("{address} can be reached from other machines."))
            .with_fix("Set listen = \"127.0.0.1:8790\" and put a TLS reverse proxy in front."),
        Err(_) => Check::new("listen", Status::Error, format!("{} is not an address to listen on.", file.listen))
            .with_fix("Set listen = \"127.0.0.1:8790\"."),
    }
}

fn origin(file: &Config) -> Check {
    let origin = &file.webauthn.origin;
    let Ok(url) = reqwest::Url::parse(origin) else {
        return Check::new("origin", Status::Error, format!("{origin} is not an address."));
    };
    if url.host_str() == Some("localhost") {
        let listen_port = file.listen.parse::<SocketAddr>().ok().map(|a| a.port());
        let origin_port = url.port_or_known_default();
        if listen_port.is_some() && origin_port != listen_port {
            return Check::new(
                "origin",
                Status::Warn,
                format!("{origin} is not where the hub listens ({}).", file.listen),
            )
            .with_fix("Make the port of webauthn.origin and of listen the same, or put a proxy in between.");
        }
        return Check::new("origin", Status::Ok, format!("{origin}: passkeys work in a browser on this machine only"));
    }
    Check::new("origin", Status::Ok, format!("{origin}, relying party {}", file.webauthn.rp_id))
}

/// Owners and modes, where the platform has them.
fn files(config_path: &Path, file: &Config) -> Vec<Check> {
    #[cfg(unix)]
    {
        unix_files(config_path, file)
    }
    #[cfg(not(unix))]
    {
        let _ = (config_path, file);
        vec![Check::new("files", Status::Skip, "File modes are checked on Linux only.")]
    }
}

#[cfg(unix)]
fn unix_files(config_path: &Path, file: &Config) -> Vec<Check> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let mut problems: Vec<Check> = Vec::new();
    let mode = |path: &Path| std::fs::metadata(path).ok().map(|m| (m.permissions().mode() & 0o777, m.uid()));

    if let Some((bits, _)) = mode(config_path) {
        if bits & 0o002 != 0 {
            problems.push(
                Check::new("files", Status::Error, format!("{} can be changed by anyone.", config_path.display()))
                    .with_fix(format!("sudo chmod 0640 {}", config_path.display())),
            );
        } else if bits & 0o004 != 0 {
            problems.push(
                Check::new("files", Status::Warn, format!("{} can be read by anyone.", config_path.display()))
                    .with_fix(format!("sudo chmod 0640 {}", config_path.display())),
            );
        }
    }
    let data = file.data_path();
    match mode(&data) {
        None => problems.push(
            Check::new("files", Status::Error, format!("the data directory {} does not exist.", data.display()))
                .with_fix(format!("sudo install -d -m 0700 -o swifty-home -g swifty-home {}", data.display())),
        ),
        Some((bits, _)) if bits & 0o077 != 0 => problems.push(
            Check::new("files", Status::Warn, format!("{} can be read by other users ({bits:04o}).", data.display()))
                .with_fix(format!("sudo chmod 0700 {}", data.display())),
        ),
        Some((_, owner)) => {
            if let Some(database_owner) = mode(&file.database_path()).map(|(_, uid)| uid) {
                if database_owner != owner {
                    problems.push(
                        Check::new(
                            "files",
                            Status::Error,
                            format!("{} has another owner than its directory.", file.database_path().display()),
                        )
                        .with_fix(format!(
                            "sudo chown -R --reference={} {}",
                            data.display(),
                            data.display()
                        )),
                    );
                }
            }
        }
    }
    if let Some(key) = &file.audit.key_file {
        match mode(key) {
            None => problems.push(
                Check::new("files", Status::Error, format!("the audit key {} does not exist.", key.display()))
                    .with_fix("Restore it from a backup. Without it the audit log cannot be verified."),
            ),
            Some((bits, _)) if bits & 0o077 != 0 => problems.push(
                Check::new(
                    "files",
                    Status::Error,
                    format!("the audit key {} can be read by other users.", key.display()),
                )
                .with_fix(format!("sudo chmod 0600 {}", key.display())),
            ),
            Some(_) => {}
        }
    }
    if problems.is_empty() {
        problems.push(Check::new("files", Status::Ok, "owners and modes are as they should be"));
    }
    problems
}

fn database(file: &Config) -> Check {
    let path = file.database_path();
    if !path.exists() {
        return Check::new("database", Status::Error, format!("there is no database at {}.", path.display()))
            .with_fix("Run `sudo swifty init` for a new install, or set data_dir to where the database is.");
    }
    match Store::open_for_inspection(&path) {
        Ok(_) => Check::new("database", Status::Ok, format!("{} opens, and its schema is current", path.display())),
        Err(reason) => Check::new("database", Status::Error, reason)
            .with_fix("Start the hub once so it can bring the schema up to date, or restore a backup."),
    }
}

fn audit_chain(file: &Config) -> Check {
    let key = match file.audit.key_file.as_deref().map(AuditKey::load).transpose() {
        Ok(key) => key,
        Err(error) => {
            return Check::new("audit", Status::Error, error.to_string())
                .with_fix("Restore the audit key, readable by the service user only (mode 0600).")
        }
    };
    let store = match Store::open_for_inspection(&file.database_path()) {
        Ok(store) => store,
        Err(reason) => return Check::new("audit", Status::Error, reason),
    };
    let options = VerifyOptions { key: key.as_ref(), expected: None, allow_empty: true };
    match store.with(|conn| audit::verify_with(conn, options)) {
        Ok(report) if report.ok && report.empty => Check::new("audit", Status::Warn, "the audit log is empty")
            .with_fix("Nothing to do on a hub nobody has used yet."),
        Ok(report) if report.ok && key.is_none() => Check::new(
            "audit",
            Status::Warn,
            format!("{} row(s) verify, but the head is not signed: no audit key is set", report.rows),
        )
        .with_fix("Set [audit] key_file to a file of at least 32 random characters, mode 0600."),
        Ok(report) if report.ok => {
            Check::new("audit", Status::Ok, format!("{} row(s) verify; checkpoint {}", report.rows, report.checkpoint))
        }
        Ok(report) => Check::new(
            "audit",
            Status::Error,
            report.problem.unwrap_or_else(|| "the audit log does not verify".to_string()),
        )
        .with_fix("Run `swifty audit verify` for the details, and compare with a checkpoint you exported earlier."),
        Err(error) => Check::new("audit", Status::Error, error.to_string()),
    }
}

/// The secret store: there, private, and every file in it private. No value is read.
fn secrets(file: &Config) -> Check {
    let root = file.secrets_path();
    if !root.is_dir() {
        return Check::new("secrets", Status::Warn, format!("{} does not exist yet.", root.display()))
            .with_fix(format!("sudo install -d -m 0700 -o swifty-home -g swifty-home {}", root.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut open = Vec::new();
        let mut slots = 0usize;
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            if std::fs::metadata(&dir).is_ok_and(|m| m.permissions().mode() & 0o077 != 0) {
                open.push(dir.clone());
            }
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(meta) = entry.metadata() else { continue };
                if meta.is_dir() {
                    stack.push(path);
                } else {
                    slots += 1;
                    if meta.permissions().mode() & 0o077 != 0 {
                        open.push(path);
                    }
                }
            }
        }
        if let Some(first) = open.first() {
            return Check::new(
                "secrets",
                Status::Error,
                format!(
                    "{} item(s) in the secret store can be read by other users, such as {}.",
                    open.len(),
                    first.display()
                ),
            )
            .with_fix(format!("sudo chmod -R go-rwx {}", root.display()));
        }
        Check::new("secrets", Status::Ok, format!("{} private, {slots} slot(s) filled", root.display()))
    }
    #[cfg(not(unix))]
    {
        Check::new("secrets", Status::Ok, format!("{} exists", root.display()))
    }
}

/// A key file a section names exists and is private. Its value is not read.
fn key_file(name: &str, path: &Path) -> Check {
    let field = if name == "speech" { "token_file" } else { "key_file" };
    let Ok(meta) = std::fs::metadata(path) else {
        let command = if name == "speech" { "swifty speech set-token" } else { "swifty assistant set-key" };
        return Check::new(name, Status::Error, format!("{} does not exist.", path.display()))
            .with_fix(format!("Write it with `sudo {command}`."));
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Check::new(name, Status::Error, format!("{} can be read by other users.", path.display()))
                .with_fix(format!("sudo chmod 0600 {}", path.display()));
        }
    }
    let _ = meta;
    Check::new(name, Status::Ok, format!("{field} {} is there and private", path.display()))
}

fn clock(now_ms: i64) -> Check {
    if now_ms < CLOCK_FLOOR_MS {
        return Check::new("clock", Status::Error, "the system clock is in the past.")
            .with_fix("Set the time, or turn on time sync: sudo timedatectl set-ntp true");
    }
    Check::new("clock", Status::Ok, "the system clock looks right")
}

#[cfg(test)]
mod tests {
    use super::super::init::{plan, run as init, InitOptions};
    use super::*;

    fn installed(dir: &Path) -> PathBuf {
        let options = InitOptions {
            config_path: dir.join("etc").join("swifty.toml"),
            origin: String::new(),
            listen: None,
            data_dir: dir.join("data"),
            live: false,
            actor: "tester".to_string(),
        };
        let plan = plan(&options).unwrap();
        init(&plan, CLOCK_FLOOR_MS + 1).unwrap();
        plan.config_path
    }

    #[test]
    fn a_fresh_install_has_no_errors() {
        let dir = tempfile::tempdir().unwrap();
        let config = installed(dir.path());
        let report = run(&config, CLOCK_FLOOR_MS + 1, &[]);
        assert!(!report.has_errors(), "{report:#?}");
        let names: Vec<&str> = report.checks.iter().map(|c| c.name.as_str()).collect();
        for name in ["config", "listen", "origin", "files", "database", "audit", "secrets", "plugins", "proxy", "clock"]
        {
            assert!(names.contains(&name), "{name} in {names:?}");
        }
        let audit = report.checks.iter().find(|c| c.name == "audit").unwrap();
        assert_eq!(audit.status, Status::Ok, "{audit:?}");
    }

    #[test]
    fn the_proxy_check_says_how_to_check_the_proxy() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = load_config(&installed(dir.path())).unwrap();
        let local = proxy(&file);
        assert_eq!(local.status, Status::Skip);
        assert!(local.detail.contains("no reverse proxy"), "{local:?}");
        file.webauthn.origin = "https://home.example.org".to_string();
        let behind = proxy(&file);
        assert_eq!(
            behind.fix.as_deref(),
            Some("From a phone or another computer: curl -sf https://home.example.org/api/v1/meta")
        );
    }

    #[test]
    fn each_problem_comes_with_a_fix() {
        let dir = tempfile::tempdir().unwrap();
        let missing = run(&dir.path().join("nothing.toml"), CLOCK_FLOOR_MS + 1, &[]);
        assert!(missing.has_errors());
        assert!(missing.checks[0].fix.as_deref().unwrap().contains("swifty init"));

        let legacy = dir.path().join("legacy.toml");
        std::fs::write(&legacy, "database = 'home.db'\nconsole_vm = 102\n").unwrap();
        let report = run(&legacy, CLOCK_FLOOR_MS + 1, &[]);
        assert!(report.checks[0].fix.as_deref().unwrap().contains("swifty migrate --from"));

        let config = installed(dir.path());
        std::fs::remove_file(dir.path().join("data").join("home.db")).unwrap();
        let report = run(&config, 0, &[]);
        let failed: Vec<&Check> = report.checks.iter().filter(|c| c.status == Status::Error).collect();
        assert!(failed.iter().any(|c| c.name == "database"), "{failed:?}");
        assert!(failed.iter().any(|c| c.name == "clock"), "{failed:?}");
        assert!(failed.iter().all(|c| c.fix.is_some()), "{failed:?}");
    }

    #[test]
    fn each_integration_is_put_to_its_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let config = installed(dir.path());
        let store = Store::open(&dir.path().join("data").join("home.db")).unwrap();
        let add = |id: &str, plugin: &str, config: &str| {
            store
                .with(|conn| {
                    conn.execute(
                        "INSERT INTO plugin_instances (id, plugin, config, config_version, enabled, created_at, created_by, updated_at)
                         VALUES (?1, ?2, ?3, 1, 1, 0, 'test', 0)",
                        rusqlite::params![id, plugin, config],
                    )
                })
                .unwrap()
        };
        let mock: &'static dyn Plugin = &swifty_plugin_mock::PLUGIN;
        let plugins = |report: &Report| report.checks.iter().find(|c| c.name == "plugins").unwrap().clone();

        add("demo", "mock", r#"{"devices":[{"id":"lamp","class":"light"}]}"#);
        let fine = plugins(&run(&config, CLOCK_FLOOR_MS + 1, &[mock]));
        assert_eq!(fine.status, Status::Ok, "{fine:?}");
        assert!(fine.detail.contains("1 plugins in this build"), "{fine:?}");

        add("coded", "mock", r#"{"devices":[],"require_code":true}"#);
        let waiting = plugins(&run(&config, CLOCK_FLOOR_MS + 1, &[mock]));
        assert_eq!(waiting.status, Status::Warn, "{waiting:?}");
        assert!(waiting.detail.contains("coded: The access code is not set"), "{waiting:?}");

        add("blinds", "acme", "{}");
        let missing = plugins(&run(&config, CLOCK_FLOOR_MS + 1, &[mock]));
        assert_eq!(missing.status, Status::Error, "{missing:?}");
        assert!(missing.detail.contains("blinds needs the plugin acme"), "{missing:?}");
        assert!(missing.fix.is_some());
    }

    #[test]
    fn a_listen_address_off_loopback_is_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let config = installed(dir.path());
        let text = std::fs::read_to_string(&config).unwrap().replace("127.0.0.1:8790", "0.0.0.0:8790");
        std::fs::write(&config, text).unwrap();
        let report = run(&config, CLOCK_FLOOR_MS + 1, &[]);
        let listen = report.checks.iter().find(|c| c.name == "listen").unwrap();
        assert_eq!(listen.status, Status::Warn);
    }
}
