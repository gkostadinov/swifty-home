//! `swifty.toml`, version 2: what the process needs before it can open the
//! database, and operator policy.
//!
//! Read-only to the service. Written by `swifty init`, `swifty migrate`,
//! `swifty go-live` or a person. Every section refuses unknown keys, and an error
//! names the line and the key and never repeats the value, because a secret
//! pasted into the file would otherwise reach the log. Secrets are never values
//! here: plugin credentials live in the secret store, and the hub's own keys in
//! files this file names.
//!
//! A version 1 file is never read as this one. [`Config::load`] recognises it and
//! answers [`ConfigError::Legacy`], which tells the person to run `swifty migrate`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::shared::{
    check_host_networks, default_host_networks, default_listen, parse_error, AssistantConfig, AuditConfig, ConfigError,
    HealthConfig, SessionConfig, SpeechConfig, TimingConfig, WebauthnConfig, MAX_DEVICE_TIMEOUT_MS,
};

/// The version this build reads and writes.
pub const CONFIG_VERSION: u32 = 2;
/// Where everything the hub writes lives when nothing else is said.
pub const DEFAULT_DATA_DIR: &str = "/var/lib/swifty-home";
/// The database's file name inside the data directory.
pub const DATABASE_FILE: &str = "home.db";
/// The secret store's directory name inside the data directory.
pub const SECRETS_DIR: &str = "secrets";

const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 1_500;
const DEFAULT_READ_TIMEOUT_MS: u64 = 2_500;
const MAX_RP_NAME: usize = 64;

/// Keys only a version 1 file has. A file with one of these and no
/// `config_version` is a version 1 file.
const LEGACY_KEYS: &[&str] = &["devices", "console_vm", "console", "machines", "lights", "agents"];

/// Version 2 of the configuration.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Must be 2.
    pub config_version: u32,
    /// Only `live = true` lets a command reach a device. Anything else is dry-run.
    #[serde(default)]
    pub live: bool,
    /// An IP address and port. Keep it on loopback, behind a TLS reverse proxy.
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Everything the hub writes lives under here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<PathBuf>,
    /// Default `<data_dir>/home.db`. One of `data_dir` and `database` is required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<PathBuf>,
    /// Default `<data_dir>/secrets`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets_dir: Option<PathBuf>,
    /// The device this hub itself runs on, if it is one of its own devices. Stopping
    /// it needs an extra acknowledgement, and it cannot be removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs_on: Option<String>,
    /// Required, with no default: passkeys are bound to these for good.
    pub webauthn: WebauthnConfig,
    #[serde(default)]
    pub session: SessionConfig,
    #[serde(default)]
    pub timing: TimingConfig,
    #[serde(default)]
    pub network: NetworkConfig,
    #[serde(default, skip_serializing_if = "is_default")]
    pub audit: AuditConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech: Option<SpeechConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant: Option<AssistantConfig>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub health: HealthConfig,
}

/// How long devices may take, and where a further Proxmox host may be.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
    #[serde(default = "default_read_timeout_ms")]
    pub read_timeout_ms: u64,
    /// A further Proxmox host must have an address inside one of these.
    #[serde(default = "default_host_networks")]
    pub allowed_host_networks: Vec<String>,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            connect_timeout_ms: DEFAULT_CONNECT_TIMEOUT_MS,
            read_timeout_ms: DEFAULT_READ_TIMEOUT_MS,
            allowed_host_networks: default_host_networks(),
        }
    }
}

impl NetworkConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        for (name, value) in
            [("network.connect_timeout_ms", self.connect_timeout_ms), ("network.read_timeout_ms", self.read_timeout_ms)]
        {
            if value == 0 || value > MAX_DEVICE_TIMEOUT_MS {
                return invalid(format!("{name} must be between 1 and {MAX_DEVICE_TIMEOUT_MS}"));
            }
        }
        check_host_networks("network.allowed_host_networks", &self.allowed_host_networks)
    }
}

fn default_connect_timeout_ms() -> u64 {
    DEFAULT_CONNECT_TIMEOUT_MS
}

fn default_read_timeout_ms() -> u64 {
    DEFAULT_READ_TIMEOUT_MS
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

fn invalid<T>(message: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError::Invalid(message.into()))
}

/// What a device target may be: `[a-z0-9][a-z0-9_-]{0,63}`.
fn is_target(text: &str) -> bool {
    text.len() <= 64
        && text.as_bytes().first().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

impl Config {
    /// Reads and checks a file. A version 1 file gives [`ConfigError::Legacy`],
    /// naming this path in the command to run.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text =
            std::fs::read_to_string(path).map_err(|source| ConfigError::Read { path: path.to_path_buf(), source })?;
        Self::parse_from(&text, &path.display().to_string())
    }

    /// Reads and checks a file's text.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        Self::parse_from(text, "<that file>")
    }

    fn parse_from(text: &str, from: &str) -> Result<Self, ConfigError> {
        let table: toml::Table = toml::from_str(text).map_err(|error| parse_error(&error, text))?;
        match table.get("config_version") {
            None if LEGACY_KEYS.iter().any(|key| table.contains_key(*key)) => {
                return Err(ConfigError::Legacy { from: from.to_string() });
            }
            None => return invalid("config_version is missing: a version 2 file says config_version = 2"),
            Some(toml::Value::Integer(2)) => {}
            Some(toml::Value::Integer(version)) => {
                return invalid(format!("config_version {version} is not one this build reads; it reads 2"));
            }
            Some(_) => return invalid("config_version must be the number 2"),
        }
        let config: Config = toml::from_str(text).map_err(|error| parse_error(&error, text))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.config_version != CONFIG_VERSION {
            return invalid("config_version must be 2");
        }
        if self.listen.parse::<SocketAddr>().is_err() {
            return invalid("listen must be an IP address and a port, such as 127.0.0.1:8790");
        }
        let empty = |path: &Option<PathBuf>| path.as_ref().is_some_and(|path| path.as_os_str().is_empty());
        if self.data_dir.is_none() && self.database.is_none() {
            return invalid("data_dir is required (or database)");
        }
        for (name, path) in
            [("data_dir", &self.data_dir), ("database", &self.database), ("secrets_dir", &self.secrets_dir)]
        {
            if empty(path) {
                return invalid(format!("{name} cannot be empty"));
            }
        }
        if self.runs_on.as_deref().is_some_and(|target| !is_target(target)) {
            return invalid("runs_on must be a device target: lower-case letters, digits, _ and -");
        }
        if self.health.telemetry.as_deref().is_some_and(|target| !is_target(target)) {
            return invalid("health.telemetry must be a device target: lower-case letters, digits, _ and -");
        }
        self.webauthn.validate()?;
        let name = &self.webauthn.rp_name;
        if name.trim().is_empty() || name.chars().count() > MAX_RP_NAME || name.chars().any(char::is_control) {
            return invalid(format!("webauthn.rp_name must be 1 to {MAX_RP_NAME} plain characters"));
        }
        self.session.validate()?;
        self.timing.validate(self.live)?;
        self.network.validate()?;
        if let Some(speech) = &self.speech {
            speech.validate()?;
        }
        if let Some(assistant) = &self.assistant {
            assistant.validate()?;
        }
        Ok(())
    }

    /// The directory the database and the secret store default to: `data_dir`, or
    /// else the database's own directory.
    pub fn data_path(&self) -> PathBuf {
        match (&self.data_dir, &self.database) {
            (Some(dir), _) => dir.clone(),
            (None, Some(database)) => database.parent().map(Path::to_path_buf).unwrap_or_default(),
            (None, None) => PathBuf::new(),
        }
    }

    /// `database`, or `<data_dir>/home.db`.
    pub fn database_path(&self) -> PathBuf {
        self.database.clone().unwrap_or_else(|| self.data_path().join(DATABASE_FILE))
    }

    /// `secrets_dir`, or `<data_dir>/secrets`.
    pub fn secrets_path(&self) -> PathBuf {
        self.secrets_dir.clone().unwrap_or_else(|| self.data_path().join(SECRETS_DIR))
    }

    pub fn session_absolute_ms(&self) -> i64 {
        self.session.absolute_secs as i64 * 1000
    }

    pub fn session_idle_ms(&self) -> i64 {
        self.session.idle_secs as i64 * 1000
    }

    /// The file's text, as `swifty init` and `swifty migrate` write it. Reading it
    /// back gives the same configuration.
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        let body = toml::to_string(self).map_err(|error| ConfigError::Invalid(format!("cannot write: {error}")))?;
        Ok(format!("# Swifty Home configuration, version 2.\n\n{body}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = "config_version = 2\ndata_dir = \"/var/lib/swifty-home\"\n[webauthn]\nrp_id = \"home.example.org\"\norigin = \"https://home.example.org\"\nrp_name = \"Swifty Home\"\n";

    #[test]
    fn the_paths_default_to_the_data_directory() {
        let config = Config::parse(MINIMAL).unwrap();
        let data = Path::new("/var/lib/swifty-home");
        assert_eq!(config.database_path(), data.join("home.db"));
        assert_eq!(config.secrets_path(), data.join("secrets"));

        let only_database = MINIMAL.replace("data_dir = \"/var/lib/swifty-home\"", "database = \"/srv/home/hub.db\"");
        let config = Config::parse(&only_database).unwrap();
        assert_eq!(config.database_path(), Path::new("/srv/home/hub.db"));
        assert_eq!(config.secrets_path(), Path::new("/srv/home").join("secrets"));

        let both = MINIMAL.replace("data_dir", "secrets_dir = \"/srv/keys\"\ndata_dir");
        assert_eq!(Config::parse(&both).unwrap().secrets_path(), Path::new("/srv/keys"));
    }

    #[test]
    fn a_target_is_lower_case_and_short() {
        for good in ["vm102", "host", "two-vm200", "a_b", "0"] {
            assert!(is_target(good), "{good}");
        }
        for bad in ["", "VM102", "-vm", "_vm", "vm 102", "vm/102", &"a".repeat(65)] {
            assert!(!is_target(bad), "{bad}");
        }
    }
}
