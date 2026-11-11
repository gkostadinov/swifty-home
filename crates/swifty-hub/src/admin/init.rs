//! First run: the configuration, the directories, the audit key and the
//! database, and the integrations a new hub starts with (the demo home).
//!
//! [`load_config`] reads `swifty.toml` (version 2, described in `docs/configuration.md`)
//! for `swifty serve`, `swifty config check` and `swifty doctor`, with the errors
//! the command line words for a person.

use std::fmt::Write as _;
use std::io::Write as _;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use rusqlite::params;
use serde_json::{json, Value};
use swifty_plugin_api::DeviceDecl;

use crate::audit::{self, AuditKey};
use crate::config::{Config, ConfigError};
use crate::crypto::{random_bytes, random_token};
use crate::inventory;
use crate::passkeys::Passkeys;
use crate::setup::SETUP_ACTION;
use crate::store::Store;

/// The only configuration version this build writes and reads.
pub const CONFIG_VERSION: u32 = 2;
/// Where the configuration lives unless `--config` says otherwise.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/swifty-home/swifty.toml";
/// Where the hub keeps what it writes unless `--data-dir` says otherwise.
pub const DEFAULT_DATA_DIR: &str = "/var/lib/swifty-home";
/// The address people use when nothing else is given: this machine.
pub const DEFAULT_ORIGIN: &str = "http://localhost:8790";
/// Where the hub listens when nothing else is given: loopback only.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8790";
/// The audit key's file name, next to the configuration.
pub const AUDIT_KEY_FILE: &str = "audit.key";
/// The database's file name, in the data directory.
pub const DATABASE_FILE: &str = "home.db";
/// The secret store's directory name, in the data directory.
pub const SECRETS_DIR: &str = "secrets";

const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 1_500;
const DEFAULT_READ_TIMEOUT_MS: u64 = 2_500;
const DEFAULT_HOST_NETWORKS: [&str; 4] = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "fc00::/7"];

// ----- reading swifty.toml

/// Why a configuration file cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum ConfigFileError {
    /// The file could not be read.
    #[error("cannot read {path}: {source}")]
    Read { path: PathBuf, source: std::io::Error },
    /// A glimdock-home file. It is never guessed at; `swifty migrate` converts it.
    #[error("this is a glimdock-home configuration")]
    Legacy,
    /// Something in it is wrong. The text names the line or the key, never a value.
    #[error("{0}")]
    Invalid(String),
}

impl From<ConfigError> for ConfigFileError {
    fn from(error: ConfigError) -> Self {
        match error {
            ConfigError::Read { path, source } => Self::Read { path, source },
            ConfigError::Legacy { .. } => Self::Legacy,
            ConfigError::Parse(message) | ConfigError::Invalid(message) => Self::Invalid(message),
        }
    }
}

/// Reads and checks `swifty.toml`, version 2: the file the hub runs on.
pub fn load_config(path: &Path) -> Result<Config, ConfigFileError> {
    Config::load(path).map_err(ConfigFileError::from)
}

/// Checks the text of `swifty.toml`.
pub fn parse_config(text: &str) -> Result<Config, ConfigFileError> {
    Config::parse(text).map_err(ConfigFileError::from)
}

// ----- planning a first run

/// What `swifty init` was asked for.
#[derive(Debug, Clone)]
pub struct InitOptions {
    /// Where to write `swifty.toml`.
    pub config_path: PathBuf,
    /// The address people will use, as typed. Empty means [`DEFAULT_ORIGIN`].
    pub origin: String,
    /// Where to listen. `None` follows the origin on this machine, or [`DEFAULT_LISTEN`].
    pub listen: Option<String>,
    /// The directory the hub writes into.
    pub data_dir: PathBuf,
    /// `live = true` from the start.
    pub live: bool,
    /// Who runs it, for the audit row: the Unix user name.
    pub actor: String,
}

/// What `swifty init` will write, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitPlan {
    pub config_path: PathBuf,
    pub origin: String,
    pub rp_id: String,
    pub listen: String,
    pub data_dir: PathBuf,
    pub audit_key: PathBuf,
    pub live: bool,
    pub actor: String,
    /// Sentences worth saying before anything is written.
    pub warnings: Vec<String>,
}

impl InitPlan {
    pub fn database(&self) -> PathBuf {
        self.data_dir.join(DATABASE_FILE)
    }

    pub fn secrets_dir(&self) -> PathBuf {
        self.data_dir.join(SECRETS_DIR)
    }
}

/// Turns what a person typed into an origin and its relying party id.
///
/// `home.example.org` becomes `https://home.example.org`, `localhost:8790`
/// becomes `http://localhost:8790`, and a trailing slash goes.
pub fn normalize_origin(typed: &str) -> Result<(String, String), String> {
    let typed = typed.trim();
    let typed = if typed.is_empty() { DEFAULT_ORIGIN } else { typed };
    let with_scheme = if typed.contains("://") {
        typed.to_string()
    } else if typed.starts_with("localhost") {
        format!("http://{typed}")
    } else {
        format!("https://{typed}")
    };
    let not_an_address = || format!("{typed} is not a web address. Use one such as https://home.example.org");
    let url = reqwest::Url::parse(&with_scheme).map_err(|_| not_an_address())?;
    let host = url.host_str().ok_or_else(not_an_address)?.to_string();
    if url.authority().contains('@') || url.query().is_some() || url.fragment().is_some() {
        return Err(format!("{typed} must be only a scheme, a name and perhaps a port"));
    }
    if url.path() != "/" {
        return Err(format!("{typed} must not have a path. Use the address of the site itself"));
    }
    if host.parse::<IpAddr>().is_ok() || host.starts_with('[') {
        return Err(format!(
            "{typed} is an IP address, and passkeys need a name. Use http://localhost:8790 on this machine, \
             or a name such as https://home.example.org"
        ));
    }
    match url.scheme() {
        "https" => {}
        "http" if host == "localhost" => {}
        "http" => {
            return Err(format!(
                "{typed} uses http, and passkeys need https away from this machine. \
                 Use https://{host}, or http://localhost:8790 to try it here"
            ))
        }
        _ => return Err(not_an_address()),
    }
    Ok((url.origin().ascii_serialization(), host))
}

/// Checks the options and works out every path, without touching anything.
pub fn plan(options: &InitOptions) -> Result<InitPlan, String> {
    let (origin, rp_id) = normalize_origin(&options.origin)?;
    let mut warnings = Vec::new();
    let origin_port = reqwest::Url::parse(&origin).ok().and_then(|url| url.port());
    let listen = match &options.listen {
        Some(listen) => listen.trim().to_string(),
        None => match origin_port {
            Some(port) if rp_id == "localhost" => format!("127.0.0.1:{port}"),
            _ => DEFAULT_LISTEN.to_string(),
        },
    };
    let address: SocketAddr = listen
        .parse()
        .map_err(|_| format!("{listen} is not an address to listen on. Use one such as {DEFAULT_LISTEN}"))?;
    if !address.ip().is_loopback() {
        warnings.push(format!(
            "The hub will listen on {listen}, which other machines can reach. Keep it on 127.0.0.1 behind a \
             TLS reverse proxy unless you know you need this."
        ));
    }
    if rp_id == "localhost" && origin_port.is_none() {
        warnings.push(format!(
            "{origin} means port 80, but the hub listens on {listen}. Put a proxy on port 80, or use \
             http://localhost:{} instead.",
            address.port()
        ));
    }
    if rp_id == "localhost" {
        warnings.push(
            "Passkeys made for localhost work in a browser on this machine only. For phones, use a name \
             with https, such as https://home.example.org."
                .to_string(),
        );
    }
    let config_path = options.config_path.clone();
    let config_dir = config_path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let plan = InitPlan {
        audit_key: config_dir.join(AUDIT_KEY_FILE),
        config_path,
        origin,
        rp_id,
        listen,
        data_dir: options.data_dir.clone(),
        live: options.live,
        actor: options.actor.clone(),
        warnings,
    };
    // What will be written must pass the same check as `swifty config check`.
    parse_config(&render(&plan)).map_err(|error| format!("the configuration would not be valid: {error}"))?;
    Ok(plan)
}

/// A TOML string, quoted and escaped as TOML needs.
fn quoted(text: &str) -> String {
    toml::Value::String(text.to_string()).to_string()
}

/// The text of `swifty.toml` for this plan: the file `docs/configuration.md` describes, with comments.
pub fn render(plan: &InitPlan) -> String {
    let data_dir = plan.data_dir.to_string_lossy();
    let separator = if data_dir.ends_with('/') || data_dir.ends_with('\\') { "" } else { "/" };
    let networks: Vec<String> = DEFAULT_HOST_NETWORKS.iter().map(|n| quoted(n)).collect();
    let mut out = String::new();
    let _ = write!(
        out,
        "# Swifty Home configuration, version 2. Written by `swifty init`.
# Check it with `swifty config check`, and everything else with `swifty doctor`.
config_version = 2

# Dry-run is the default: no plugin is connected and no socket is opened.
# Only the exact line `live = true` lets a command reach a device.
# Change it with `sudo swifty go-live` and `sudo swifty go-dry`.
live = {live}

# Keep this on loopback, behind a TLS reverse proxy.
listen = {listen}

# Everything the hub writes lives under here.
data_dir = {data}
# database    = \"{data_dir}{separator}{database}\"     default: <data_dir>/{database}
# secrets_dir = \"{data_dir}{separator}{secrets}\"     default: <data_dir>/{secrets}

# The device this hub itself runs on, if it is one of its own devices. Stopping
# it needs an extra acknowledgement, and it cannot be removed.
# runs_on = \"vm102\"

# Required. There is no default: passkeys are bound to these for good.
# Changing them later means enrolling every passkey again.
[webauthn]
origin  = {origin}
rp_id   = {rp_id}
rp_name = \"Swifty Home\"

[session]
absolute_secs = 28800
idle_secs     = 1800

[timing]
poll_ms             = 250
sync_wait_ms        = 4000
duplicate_window_ms = 10000

[network]
connect_timeout_ms = {connect}
read_timeout_ms    = {read}
# A further Proxmox host must have an address inside one of these.
allowed_host_networks = [{networks}]

[audit]
key_file = {key}

# Optional sections, unchanged in meaning: [speech], [assistant], [assistant.agent],
# [assistant.weather], [health].
",
        live = plan.live,
        listen = quoted(&plan.listen),
        data = quoted(&data_dir),
        database = DATABASE_FILE,
        secrets = SECRETS_DIR,
        origin = quoted(&plan.origin),
        rp_id = quoted(&plan.rp_id),
        connect = DEFAULT_CONNECT_TIMEOUT_MS,
        read = DEFAULT_READ_TIMEOUT_MS,
        networks = networks.join(", "),
        key = quoted(&plan.audit_key.to_string_lossy()),
    );
    out
}

// ----- running a first run

/// Why a first run stopped. Each says what to do about it.
#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("{} already exists. `swifty init` never replaces a configuration. Check it with `swifty config check`, or move it away to start again.", .0.display())]
    ConfigExists(PathBuf),
    #[error("{} already holds a database. `swifty init` sets up a new install only. Move the database away to start again.", .0.display())]
    DatabaseExists(PathBuf),
    #[error("{} exists but is not a usable audit key: {reason}. Move it away and run `swifty init` again.", path.display())]
    BadAuditKey { path: PathBuf, reason: String },
    #[error("cannot {what} {}: {source}", path.display())]
    Io { what: &'static str, path: PathBuf, source: std::io::Error },
    #[error("cannot create the database {}: {reason}", path.display())]
    Database { path: PathBuf, reason: String },
}

/// What a first run did.
#[derive(Debug, Clone)]
pub struct InitOutcome {
    /// Every file and directory it made, in order.
    pub created: Vec<PathBuf>,
    /// What was there already and was kept as it is.
    pub kept: Vec<PathBuf>,
    /// The one-time token for the first passkey, and when it expires (ms).
    pub enroll_token: String,
    pub enroll_expires_at: i64,
    /// The targets of the devices the starting integrations brought, in order.
    pub devices: Vec<String>,
}

/// An integration a new hub starts with, such as the demo home's: stored by the
/// first run in the same transaction as the rest of the database, as a finished
/// setup would store it. The command line makes it with the plugin, which the hub
/// library does not know.
#[derive(Debug, Clone)]
pub struct StartingInstance {
    /// The instance id: `[a-z][a-z0-9]{0,15}`.
    pub id: String,
    /// The plugin it is an instance of, and that plugin's version, for the audit row.
    pub plugin: String,
    pub version: String,
    /// Its config, which the plugin's `validate` accepted.
    pub config: Value,
    pub config_version: u32,
    /// What the plugin declares for that config: stored as its devices at once, so
    /// they are listed before the hub first starts.
    pub devices: Vec<DeviceDecl>,
}

fn io<'a>(what: &'static str, path: &'a Path) -> impl FnOnce(std::io::Error) -> InitError + 'a {
    move |source| InitError::Io { what, path: path.to_path_buf(), source }
}

/// Creates a directory and its missing parents. The last one gets `mode` on Unix.
fn make_dir(path: &Path, mode: u32, outcome: &mut InitOutcome) -> Result<(), InitError> {
    if path.is_dir() {
        outcome.kept.push(path.to_path_buf());
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    builder.create(path).map_err(io("create", path))?;
    outcome.created.push(path.to_path_buf());
    Ok(())
}

/// Writes a new file with `mode` on Unix. An existing file is never replaced.
fn write_new(path: &Path, text: &str, mode: u32) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let mut file = options.open(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

/// A random version 4 UUID, written as text.
fn user_handle() -> String {
    let mut bytes = random_bytes::<16>();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = hex::encode(bytes);
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// The files SQLite keeps beside a database in WAL mode.
fn database_files(database: &Path) -> Vec<PathBuf> {
    let mut files = vec![database.to_path_buf()];
    for suffix in ["-wal", "-shm"] {
        let mut name = database.as_os_str().to_owned();
        name.push(suffix);
        files.push(PathBuf::from(name));
    }
    files
}

/// Writes the configuration, the directories, the audit key and the database,
/// appends one audit row and issues the token for the first passkey.
///
/// The configuration is written last, so a run that stops half way leaves no
/// configuration behind and can simply be run again.
pub fn run(plan: &InitPlan, now_ms: i64) -> Result<InitOutcome, InitError> {
    run_with(plan, now_ms, &[])
}

/// As [`run`], and stores these integrations with their devices in the same
/// transaction as the first audit row: the demo home of `swifty init --demo`. Each
/// gets a `plugin.setup` audit row of its own, as a finished setup does.
pub fn run_with(plan: &InitPlan, now_ms: i64, home: &[StartingInstance]) -> Result<InitOutcome, InitError> {
    if plan.config_path.exists() {
        return Err(InitError::ConfigExists(plan.config_path.clone()));
    }
    let database = plan.database();
    if database.exists() {
        return Err(InitError::DatabaseExists(database));
    }
    let mut outcome = InitOutcome {
        created: Vec::new(),
        kept: Vec::new(),
        enroll_token: String::new(),
        enroll_expires_at: 0,
        devices: Vec::new(),
    };
    let config_dir = plan.config_path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    make_dir(config_dir, 0o750, &mut outcome)?;
    make_dir(&plan.data_dir, 0o700, &mut outcome)?;
    make_dir(&plan.secrets_dir(), 0o700, &mut outcome)?;

    if plan.audit_key.exists() {
        AuditKey::load(&plan.audit_key)
            .map_err(|error| InitError::BadAuditKey { path: plan.audit_key.clone(), reason: error.to_string() })?;
        outcome.kept.push(plan.audit_key.clone());
    } else {
        write_new(&plan.audit_key, &format!("{}\n", random_token()), 0o600).map_err(io("write", &plan.audit_key))?;
        outcome.created.push(plan.audit_key.clone());
    }
    let key = AuditKey::load(&plan.audit_key)
        .map_err(|error| InitError::BadAuditKey { path: plan.audit_key.clone(), reason: error.to_string() })?;

    let mut pending = plan.config_path.as_os_str().to_owned();
    pending.push(".new");
    let pending = PathBuf::from(pending);
    let _ = std::fs::remove_file(&pending);
    write_new(&pending, &render(plan), 0o640).map_err(io("write", &pending))?;

    let finish = || -> Result<(String, i64, Vec<String>), InitError> {
        let failed = |reason: String| InitError::Database { path: database.clone(), reason };
        let store = Store::open(&database).map_err(|e| failed(e.to_string()))?;
        let event = |action: &str, target: &str, params: Value, result: &str| audit::Event {
            ts: now_ms,
            action: action.to_string(),
            target: target.to_string(),
            params,
            credential_ref: "-".to_string(),
            session_ref: "local".to_string(),
            user_ref: "-".to_string(),
            device: plan.actor.clone(),
            channel: "cli".to_string(),
            step_up: "-".to_string(),
            result: result.to_string(),
            rtt_ms: None,
            run_id: None,
        };
        let devices = store
            .tx(|tx| {
                // The owner's WebAuthn user handle, as the hub makes it on its first start.
                tx.execute("INSERT OR IGNORE INTO meta (key, value) VALUES ('owner_id', ?1)", [user_handle()])?;
                let init = event("system.init", "-", json!({ "origin": plan.origin, "live": plan.live }), "ok");
                audit::append(tx, &init, Some(&key))?;
                let mut devices = Vec::new();
                for instance in home {
                    devices.extend(store_starting_instance(tx, now_ms, instance)?);
                    let params = json!({
                        "instance": instance.id,
                        "plugin": instance.plugin,
                        "version": instance.version,
                        "secrets": [],
                        "devices": instance.devices.len(),
                    });
                    audit::append(tx, &event(SETUP_ACTION, &instance.id, params, "done"), Some(&key))?;
                }
                if !home.is_empty() {
                    inventory::bump_instances_rev(tx)?;
                }
                Ok(devices)
            })
            .map_err(|e: rusqlite::Error| failed(e.to_string()))?;
        let (token, expires_at) = Passkeys::issue_enroll_token(&store, now_ms).map_err(|e| failed(e.message))?;
        Ok((token, expires_at, devices))
    };
    match finish() {
        Ok((token, expires_at, devices)) => {
            outcome.created.push(database.clone());
            outcome.enroll_token = token;
            outcome.enroll_expires_at = expires_at;
            outcome.devices = devices;
        }
        Err(error) => {
            let _ = std::fs::remove_file(&pending);
            for file in database_files(&database) {
                let _ = std::fs::remove_file(file);
            }
            return Err(error);
        }
    }
    std::fs::rename(&pending, &plan.config_path).map_err(io("write", &plan.config_path))?;
    outcome.created.push(plan.config_path.clone());
    Ok(outcome)
}

/// The instance row and the devices its config names, under the targets the hub
/// gives them. Returns those targets.
fn store_starting_instance(
    tx: &rusqlite::Connection,
    now: i64,
    instance: &StartingInstance,
) -> rusqlite::Result<Vec<String>> {
    tx.execute(
        "INSERT INTO plugin_instances (id, plugin, config, config_version, enabled, created_at, created_by, updated_at)
         VALUES (?1, ?2, ?3, ?4, 1, ?5, '-', ?5)",
        params![instance.id, instance.plugin, instance.config.to_string(), instance.config_version, now],
    )?;
    inventory::store_config_devices(tx, now, &instance.id, &instance.devices)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(dir: &Path) -> InitOptions {
        InitOptions {
            config_path: dir.join("etc").join("swifty.toml"),
            origin: String::new(),
            listen: None,
            data_dir: dir.join("data"),
            live: false,
            actor: "tester".to_string(),
        }
    }

    #[test]
    fn origins_are_normalised_and_checked() {
        let ok = |typed: &str| normalize_origin(typed).unwrap();
        assert_eq!(ok(""), ("http://localhost:8790".to_string(), "localhost".to_string()));
        assert_eq!(ok("home.example.org"), ("https://home.example.org".to_string(), "home.example.org".to_string()));
        assert_eq!(
            ok("https://home.example.org/"),
            ("https://home.example.org".to_string(), "home.example.org".to_string())
        );
        assert_eq!(ok("localhost:9000").0, "http://localhost:9000");
        assert_eq!(ok("HTTPS://Home.Example.org:8443").0, "https://home.example.org:8443");
        for bad in [
            "http://home.example.org",
            "https://192.0.2.10",
            "https://[2001:db8::1]",
            "https://home.example.org/app",
            "https://user@home.example.org",
            "https://home.example.org?x=1",
            "ftp://home.example.org",
            "https://",
        ] {
            assert!(normalize_origin(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_rendered_file_is_the_spec_file_and_checks() {
        let dir = Path::new("/srv/test");
        let plan = plan(&InitOptions { origin: "home.example.org".into(), ..options(dir) }).unwrap();
        let text = render(&plan);
        let checked = parse_config(&text).unwrap();
        assert_eq!(checked.config_version, 2);
        assert!(!checked.live);
        assert_eq!(checked.listen, "127.0.0.1:8790");
        assert_eq!(checked.webauthn.origin, "https://home.example.org");
        assert_eq!(checked.webauthn.rp_id, "home.example.org");
        assert_eq!(checked.database_path(), dir.join("data").join("home.db"));
        assert_eq!(checked.secrets_path(), dir.join("data").join("secrets"));
        assert_eq!(checked.audit.key_file.as_deref(), Some(dir.join("etc").join("audit.key").as_path()));
        for line in
            ["config_version = 2", "live = false", "[webauthn]", "[session]", "[timing]", "[network]", "[audit]"]
        {
            assert!(text.lines().any(|l| l == line), "{line}");
        }
    }

    #[test]
    fn a_localhost_origin_with_a_port_sets_the_listen_port() {
        let dir = Path::new("/srv/test");
        let plan = plan(&InitOptions { origin: "http://localhost:9000".into(), ..options(dir) }).unwrap();
        assert_eq!(plan.listen, "127.0.0.1:9000");
        let given = plan_with_listen(dir, "0.0.0.0:8790");
        assert_eq!(given.listen, "0.0.0.0:8790");
        assert!(given.warnings.iter().any(|w| w.contains("other machines can reach")));
    }

    fn plan_with_listen(dir: &Path, listen: &str) -> InitPlan {
        plan(&InitOptions { listen: Some(listen.into()), ..options(dir) }).unwrap()
    }

    #[test]
    fn a_legacy_file_is_named_as_one() {
        for legacy in ["database = 'home.db'\n[devices]\n", "console_vm = 102\ndatabase = 'home.db'\n"] {
            assert!(matches!(parse_config(legacy), Err(ConfigFileError::Legacy)), "{legacy}");
        }
        let plain = parse_config("listen = '127.0.0.1:8790'\n").err().unwrap().to_string();
        assert!(plain.contains("config_version is missing"), "{plain}");
    }

    #[test]
    fn version_2_rules_are_applied() {
        let dir = Path::new("/srv/test");
        let good = render(&plan(&options(dir)).unwrap());
        let refused = |text: String| parse_config(&text).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(refused(good.replace("config_version = 2", "config_version = 3")).contains("reads 2"));
        assert!(refused(good.replace("[webauthn]", "[webauthn_old]")).contains("unknown field"));
        assert!(refused(format!("{good}\nsurprise = 1\n")).contains("unknown field"));
        assert!(refused(good.replace("data_dir =", "# data_dir =")).contains("data_dir is required"));
        assert!(refused(good.replace("connect_timeout_ms = 1500", "connect_timeout_ms = 0"))
            .contains("network.connect_timeout_ms"));
        assert!(refused(good.replace("\"10.0.0.0/8\"", "\"10.0.0.0/4\"")).contains("network.allowed_host_networks"));
        assert!(refused(good.replace("# runs_on = \"vm102\"", "runs_on = \"VM 102\"")).contains("runs_on"));
        assert!(parse_config(&good.replace("# runs_on = \"vm102\"", "runs_on = \"vm102\"")).is_ok());
        // The hub's own rules, with their messages.
        assert!(refused(good.replace("idle_secs     = 1800", "idle_secs     = 0")).contains("session.idle_secs"));
        assert!(refused(good.replace("rp_id   = \"localhost\"", "rp_id   = \"example.org\"")).contains("rp_id"));
        // A value pasted where it does not belong is never repeated.
        let pasted = refused(good.replace("live = false", "live = \"s3cr3t-value\""));
        assert!(pasted.contains("line") && !pasted.contains("s3cr3t"), "{pasted}");
    }

    #[test]
    fn the_new_agent_keys_are_read_by_the_hub() {
        let dir = Path::new("/srv/test");
        let good = render(&plan(&options(dir)).unwrap());
        let assistant = "[assistant]\nurls = [\"http://127.0.0.1:8080/v1\"]\nmodel = \"local\"\nkey_file = \"/srv/test/assistant.key\"\n\
                         [assistant.agent]\nowner_url = \"http://127.0.0.1:9001\"\nguest_url = \"http://127.0.0.1:9002\"\n\
                         connection = \"local\"\nmodel = \"local\"\nsession_prefix = \"swifty\"\nclient_header = \"x-swifty-client\"\n";
        let checked = parse_config(&format!("{good}\n{assistant}")).unwrap();
        let agent = checked.assistant.unwrap().agent.unwrap();
        assert_eq!((agent.session_prefix.as_str(), agent.client_header.as_str()), ("swifty", "x-swifty-client"));
        let bad = assistant.replace("session_prefix = \"swifty\"", "session_prefix = \"a\\nb\"");
        assert!(parse_config(&format!("{good}\n{bad}")).is_err());
    }

    #[test]
    fn a_first_run_writes_everything_once() {
        let dir = tempfile::tempdir().unwrap();
        let plan = plan(&options(dir.path())).unwrap();
        let outcome = run(&plan, 1_700_000_000_000).unwrap();
        assert!(plan.config_path.is_file());
        assert!(plan.audit_key.is_file());
        assert!(plan.database().is_file());
        assert!(plan.secrets_dir().is_dir());
        assert_eq!(outcome.enroll_token.len(), 43);
        let checked = load_config(&plan.config_path).unwrap();
        assert_eq!(checked.data_dir.as_deref(), Some(dir.path().join("data").as_path()));

        // The audit log has the init row, signed with the new key.
        let key = AuditKey::load(&plan.audit_key).unwrap();
        let store = Store::open_for_inspection(&plan.database()).unwrap();
        let options = audit::VerifyOptions { key: Some(&key), expected: None, allow_empty: false };
        let report = store.with(|conn| audit::verify_with(conn, options)).unwrap();
        assert!(report.ok && report.rows == 1 && report.keyed, "{report:?}");
        drop(store);

        assert!(matches!(run(&plan, 1), Err(InitError::ConfigExists(_))));
        std::fs::remove_file(&plan.config_path).unwrap();
        assert!(matches!(run(&plan, 1), Err(InitError::DatabaseExists(_))));
    }

    #[test]
    fn a_first_run_can_start_with_a_home_and_says_so_in_the_audit_log() {
        let dir = tempfile::tempdir().unwrap();
        let plan = plan(&options(dir.path())).unwrap();
        let lamp = DeviceDecl {
            local_id: "lamp".to_string(),
            target_hint: None,
            class: swifty_plugin_api::DeviceClass::Light,
            name: "Floor lamp".to_string(),
            capabilities: Vec::new(),
            attrs: json!({}),
        };
        let home = StartingInstance {
            id: "demo".to_string(),
            plugin: "mock".to_string(),
            version: "0.1.0".to_string(),
            config: json!({ "devices": [{ "id": "lamp", "class": "light" }] }),
            config_version: 1,
            devices: vec![lamp],
        };
        let outcome = run_with(&plan, 1_700_000_000_000, &[home]).unwrap();
        assert_eq!(outcome.devices, ["demo-lamp"]);

        let key = AuditKey::load(&plan.audit_key).unwrap();
        let store = Store::open_for_inspection(&plan.database()).unwrap();
        let options = audit::VerifyOptions { key: Some(&key), expected: None, allow_empty: false };
        let report = store.with(|conn| audit::verify_with(conn, options)).unwrap();
        assert!(report.ok && report.rows == 2 && report.keyed, "{report:?}");
        store
            .with(|conn| -> rusqlite::Result<()> {
                let specs = inventory::instance_specs(conn)?;
                assert_eq!(specs.len(), 1);
                assert_eq!((specs[0].id.as_str(), specs[0].plugin.as_str()), ("demo", "mock"));
                let devices = inventory::stored_devices(conn)?;
                assert_eq!(devices.len(), 1);
                assert_eq!((devices[0].target.as_str(), devices[0].name.as_str()), ("demo-lamp", "Floor lamp"));
                let (channel, result, params): (String, String, String) = conn.query_row(
                    "SELECT channel, result, params FROM audit_log WHERE action = 'plugin.setup'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
                assert_eq!((channel.as_str(), result.as_str()), ("cli", "done"));
                assert!(params.contains("\"devices\":1"), "{params}");
                assert_eq!(inventory::instances_rev(conn)?, 1);
                Ok(())
            })
            .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn files_and_directories_get_their_modes() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let plan = plan(&options(dir.path())).unwrap();
        run(&plan, 1).unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&plan.audit_key), 0o600);
        assert_eq!(mode(&plan.data_dir), 0o700);
        assert_eq!(mode(&plan.secrets_dir()), 0o700);
        assert_eq!(mode(&plan.config_path) & 0o007, 0);
    }
}
