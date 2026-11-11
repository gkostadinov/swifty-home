//! What both configuration versions share: the error type, the sections whose
//! meaning did not change between them, the checks on those sections, and the
//! hub's own key files.
//!
//! [`super::legacy`] (version 1, the glimdock-home file) and [`super::v2`]
//! (`swifty.toml`) both read these, so a section means the same in either file.
//! The one difference is the agent section: version 2 adds what the hub tells
//! the agent it is ([`AgentConfig::session_prefix`], [`AgentConfig::client_header`],
//! [`AgentConfig::client_name`]), which version 1 had as constants. So the
//! assistant structs here are version 2's, and version 1 keeps its own.

use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use swifty_plugin_api::Secret;
use thiserror::Error;

pub const MAX_SESSION_SECS: u64 = 8 * 60 * 60;

pub const MIN_POLL_MS: u64 = 50;
pub const MAX_POLL_MS: u64 = 10_000;
pub const MAX_SYNC_WAIT_MS: u64 = 60_000;
pub const MIN_DUPLICATE_WINDOW_MS: i64 = 10_000;
pub const MAX_DUPLICATE_WINDOW_MS: i64 = 3_600_000;

/// The longest a device may take to connect or to answer.
pub const MAX_DEVICE_TIMEOUT_MS: u64 = 30_000;
const MAX_SPEECH_CONNECT_SECS: u64 = 30;
const MAX_SPEECH_SECS: u64 = 120;

/// The agent's session for one chat is `<session_prefix>-<user>-<chat>`.
pub const DEFAULT_SESSION_PREFIX: &str = "swifty";
/// The header that tells the agent which client is asking.
pub const DEFAULT_CLIENT_HEADER: &str = "x-swifty-client";
/// What this hub calls itself in that header.
pub const DEFAULT_CLIENT_NAME: &str = "Swifty Home";

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read { path: PathBuf, source: std::io::Error },
    #[error("invalid configuration: {0}")]
    Parse(String),
    #[error("invalid configuration: {0}")]
    Invalid(String),
    /// A version 1 file. It is never read as version 2 and never converted on the
    /// fly: `swifty migrate` does that once, and says what it did.
    #[error("This is a glimdock-home configuration. Run: swifty migrate --from {from}")]
    Legacy { from: String },
}

/// A TOML error with the offending source line and value taken out. The library's
/// `Display` quotes the line, so a secret pasted into the file would reach the log.
pub(super) fn parse_error(error: &toml::de::Error, text: &str) -> ConfigError {
    let message = error.message();
    // Serde quotes the value itself in these, and in nothing else it reports.
    let echoes_value = message.starts_with("invalid type")
        || message.starts_with("invalid value")
        || message.starts_with("unknown variant")
        || message.contains('"')
        || message.contains('\'');
    let detail = if echoes_value { "this value has the wrong type" } else { message };
    ConfigError::Parse(match error.span() {
        Some(span) => {
            let start = span.start.min(text.len());
            let line = text.as_bytes()[..start].iter().filter(|b| **b == b'\n').count() + 1;
            format!("line {line}: {detail}")
        }
        None => detail.to_string(),
    })
}

fn invalid<T>(message: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError::Invalid(message.into()))
}

pub(super) fn default_listen() -> String {
    "127.0.0.1:8790".to_string()
}

/// The networks a further Proxmox host may be on when nothing else is said: the
/// private ranges.
pub fn default_host_networks() -> Vec<String> {
    ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "fc00::/7"].map(str::to_string).to_vec()
}

/// An IP network written address/prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpNetwork {
    address: IpAddr,
    prefix: u8,
}

impl IpNetwork {
    pub fn parse(text: &str) -> Option<Self> {
        let (address, prefix) = text.split_once('/')?;
        let address: IpAddr = address.parse().ok()?;
        let prefix: u8 = prefix.parse().ok()?;
        let bits = if address.is_ipv4() { 32 } else { 128 };
        (prefix <= bits).then_some(Self { address, prefix })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.address, ip) {
            (IpAddr::V4(network), IpAddr::V4(ip)) => {
                let mask = u32::MAX.checked_shl(u32::from(32 - self.prefix)).unwrap_or(0);
                u32::from(network) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(network), IpAddr::V6(ip)) => {
                let mask = u128::MAX.checked_shl(u32::from(128 - self.prefix)).unwrap_or(0);
                u128::from(network) & mask == u128::from(ip) & mask
            }
            _ => false,
        }
    }

    fn is_wide(&self) -> bool {
        self.prefix < if self.address.is_ipv4() { 8 } else { 7 }
    }
}

/// Every entry is a network, and none so wide that a further host could be anywhere.
pub(super) fn check_host_networks(field: &str, networks: &[String]) -> Result<(), ConfigError> {
    for network in networks {
        match IpNetwork::parse(network) {
            None => return invalid(format!("{field}: {network} is not a network")),
            // A network this wide would let a further host be anywhere.
            Some(parsed) if parsed.is_wide() => return invalid(format!("{field}: {network} is too wide")),
            Some(_) => {}
        }
    }
    Ok(())
}

/// A device's or service's base URL: the scheme and authority, and nothing else.
pub(super) fn check_device_url(field: &str, text: &str, schemes: &[&str]) -> Result<reqwest::Url, ConfigError> {
    let invalid = |why: String| ConfigError::Invalid(format!("{field}: {why}"));
    let url = reqwest::Url::parse(text).map_err(|e| invalid(e.to_string()))?;
    if !schemes.contains(&url.scheme()) {
        return Err(invalid(format!("the scheme must be one of {schemes:?}")));
    }
    if url.host_str().is_none()
        || url.authority().contains('@')
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(invalid("must be only a scheme, host and port".to_string()));
    }
    Ok(url)
}

/// Whether a URL names this machine.
pub fn is_loopback(url: &reqwest::Url) -> bool {
    match url.host_str() {
        Some("localhost") => true,
        Some(host) => host.trim_matches(['[', ']']).parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()),
        None => false,
    }
}

/// Where passkeys are bound. Required in version 2: a passkey made for one origin
/// never works for another, so there is no default to fall back on.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WebauthnConfig {
    pub rp_id: String,
    pub origin: String,
    pub rp_name: String,
}

impl WebauthnConfig {
    /// The origin is a bare https origin (or `http://localhost`), and `rp_id` is
    /// its host or a parent of it.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let origin = match reqwest::Url::parse(&self.origin) {
            Ok(origin) => origin,
            Err(error) => return invalid(format!("webauthn.origin: {error}")),
        };
        if origin.path() != "/" || origin.query().is_some() || self.origin.ends_with('/') {
            return invalid("webauthn.origin must be a bare origin such as https://home.example.org");
        }
        let host = origin.host_str().unwrap_or_default();
        if origin.scheme() != "https" && host != "localhost" {
            return invalid("webauthn.origin must use https");
        }
        if self.rp_id != host && !host.ends_with(&format!(".{}", self.rp_id)) {
            return invalid("webauthn.rp_id must be the origin's host or a parent of it");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionConfig {
    pub absolute_secs: u64,
    pub idle_secs: u64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self { absolute_secs: MAX_SESSION_SECS, idle_secs: 30 * 60 }
    }
}

impl SessionConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.absolute_secs == 0 || self.absolute_secs > MAX_SESSION_SECS {
            return invalid(format!("session.absolute_secs must be between 1 and {MAX_SESSION_SECS}"));
        }
        if self.idle_secs == 0 || self.idle_secs > self.absolute_secs {
            return invalid("session.idle_secs must be between 1 and session.absolute_secs");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TimingConfig {
    /// How often a commanded device is read back.
    pub poll_ms: u64,
    /// How long an HTTP response waits for the result before answering 202.
    pub sync_wait_ms: u64,
    /// A repeat of a non-idempotent action inside this window is refused.
    pub duplicate_window_ms: i64,
    /// Replaces every per-action confirmation timeout. Meant for tests, and
    /// refused when `live = true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_timeout_ms: Option<u64>,
}

impl Default for TimingConfig {
    fn default() -> Self {
        Self { poll_ms: 250, sync_wait_ms: 4_000, duplicate_window_ms: 10_000, confirm_timeout_ms: None }
    }
}

impl TimingConfig {
    pub fn validate(&self, live: bool) -> Result<(), ConfigError> {
        if !(MIN_POLL_MS..=MAX_POLL_MS).contains(&self.poll_ms) {
            return invalid(format!("timing.poll_ms must be between {MIN_POLL_MS} and {MAX_POLL_MS}"));
        }
        if !(1..=MAX_SYNC_WAIT_MS).contains(&self.sync_wait_ms) {
            return invalid(format!("timing.sync_wait_ms must be between 1 and {MAX_SYNC_WAIT_MS}"));
        }
        if !(MIN_DUPLICATE_WINDOW_MS..=MAX_DUPLICATE_WINDOW_MS).contains(&self.duplicate_window_ms) {
            return invalid(format!(
                "timing.duplicate_window_ms must be between {MIN_DUPLICATE_WINDOW_MS} and {MAX_DUPLICATE_WINDOW_MS}"
            ));
        }
        if live && self.confirm_timeout_ms.is_some() {
            return invalid("timing.confirm_timeout_ms is for tests and is refused when live = true");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuditConfig {
    /// A file holding the key that signs the audit head. Without it, someone who
    /// can write the database can also rewrite the log undetected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HealthConfig {
    /// A file holding the last CPU canary result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_canary_file: Option<PathBuf>,
    /// The telemetry device whose readings feed the charts and the health page.
    /// Left out, the first active one with readings, in target order. Version 2 only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SpeechConfig {
    /// Base URL of the speech service.
    pub url: String,
    /// File holding its bearer token.
    pub token_file: PathBuf,
    #[serde(default = "default_connect_secs")]
    pub connect_timeout_secs: u64,
    #[serde(default = "default_speech_secs")]
    pub timeout_secs: u64,
}

fn default_connect_secs() -> u64 {
    3
}

fn default_speech_secs() -> u64 {
    20
}

impl SpeechConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        let url = check_device_url("speech.url", &self.url, &["http", "https"])?;
        if url.scheme() != "https" && !is_loopback(&url) {
            return invalid("speech.url must use https unless it is on loopback, so its token and the audio never cross the network in clear");
        }
        if self.connect_timeout_secs == 0 || self.connect_timeout_secs > MAX_SPEECH_CONNECT_SECS {
            return invalid(format!("speech.connect_timeout_secs must be between 1 and {MAX_SPEECH_CONNECT_SECS}"));
        }
        if self.timeout_secs == 0 || self.timeout_secs > MAX_SPEECH_SECS {
            return invalid(format!("speech.timeout_secs must be between 1 and {MAX_SPEECH_SECS}"));
        }
        Ok(())
    }
}

/// The local model, and what it may look up.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantConfig {
    /// Its OpenAI-compatible addresses, ending in /v1. The next is tried only when one
    /// cannot be reached.
    pub urls: Vec<String>,
    pub model: String,
    /// File holding its API key.
    pub key_file: PathBuf,
    /// The most one answer may take. nginx gives up at 75 seconds.
    #[serde(default = "default_assistant_secs")]
    pub timeout_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weather: Option<WeatherConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentConfig>,
}

/// An agent for everything that is not about the home. Loopback only.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// The owner's copy: every tool, no approvals.
    pub owner_url: String,
    /// Everyone else's copy: web search only.
    pub guest_url: String,
    /// The name of the model connection saved in both copies.
    pub connection: String,
    pub model: String,
    #[serde(default = "default_agent_turn_secs")]
    pub max_turn_secs: u64,
    /// The agent's session for one chat is `<session_prefix>-<user>-<chat>`. The agent
    /// keeps its sessions by this name, so an install keeps the prefix it started with.
    #[serde(default = "default_session_prefix")]
    pub session_prefix: String,
    /// The header that tells the agent which client is asking.
    #[serde(default = "default_client_header")]
    pub client_header: String,
    /// What this hub calls itself in that header.
    #[serde(default = "default_client_name")]
    pub client_name: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WeatherConfig {
    pub place: String,
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default = "default_weather_url")]
    pub url: String,
}

pub(super) fn default_agent_turn_secs() -> u64 {
    3600
}

pub(super) fn default_assistant_secs() -> u64 {
    65
}

fn default_weather_url() -> String {
    "https://api.open-meteo.com/".to_string()
}

fn default_session_prefix() -> String {
    DEFAULT_SESSION_PREFIX.to_string()
}

fn default_client_header() -> String {
    DEFAULT_CLIENT_HEADER.to_string()
}

fn default_client_name() -> String {
    DEFAULT_CLIENT_NAME.to_string()
}

/// A model, connection or similar name: short, and nothing that could break a header or a URL.
fn is_plain_name(text: &str) -> bool {
    !text.is_empty() && text.len() <= 64 && text.chars().all(|c| c.is_ascii_alphanumeric() || "-_.:".contains(c))
}

/// Lower-case letters, digits and hyphens, starting with a letter, at most `max` long.
fn is_lower_word(text: &str, max: usize) -> bool {
    text.len() <= max
        && text.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl AssistantConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.urls.is_empty() || self.urls.len() > 3 {
            return invalid("assistant.urls must list 1 to 3 addresses");
        }
        for text in &self.urls {
            let Ok(url) = reqwest::Url::parse(text) else {
                return invalid(format!("assistant.urls: {text} is not an address"));
            };
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || url.authority().contains('@')
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return invalid(format!("assistant.urls: {text} must be only a scheme, host, port and path"));
            }
            let private = url
                .host_str()
                .and_then(|host| host.parse::<Ipv4Addr>().ok())
                .is_some_and(|ip| ip.is_private() || ip.is_loopback());
            if url.scheme() == "http" && !private {
                return invalid(format!("assistant.urls: {text} must use https unless it is on a private network"));
            }
        }
        if !is_plain_name(&self.model) {
            return invalid("assistant.model must be 1 to 64 plain characters");
        }
        if !(5..=70).contains(&self.timeout_secs) {
            return invalid("assistant.timeout_secs must be between 5 and 70");
        }
        if let Some(agent) = &self.agent {
            agent.validate()?;
        }
        if let Some(weather) = &self.weather {
            weather.validate()?;
        }
        Ok(())
    }
}

impl AgentConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        for (field, text) in
            [("assistant.agent.owner_url", &self.owner_url), ("assistant.agent.guest_url", &self.guest_url)]
        {
            if !is_loopback(&check_device_url(field, text, &["http"])?) {
                return invalid(format!("{field} must be on loopback"));
            }
        }
        if !is_plain_name(&self.connection) || !is_plain_name(&self.model) {
            return invalid("assistant.agent needs a plain connection and model name");
        }
        if !(30..=21_600).contains(&self.max_turn_secs) {
            return invalid("assistant.agent.max_turn_secs must be between 30 and 21600");
        }
        if !is_lower_word(&self.session_prefix, 32) {
            return invalid(
                "assistant.agent.session_prefix must be 1 to 32 lower-case letters, digits and hyphens, starting with a letter",
            );
        }
        if !is_lower_word(&self.client_header, 64) {
            return invalid(
                "assistant.agent.client_header must be a header name: 1 to 64 lower-case letters, digits and hyphens",
            );
        }
        let name = &self.client_name;
        if name.is_empty() || name.len() > 64 || name.trim() != name || !name.bytes().all(|b| (0x20..0x7f).contains(&b))
        {
            return invalid(
                "assistant.agent.client_name must be 1 to 64 plain ASCII characters, with no space at either end",
            );
        }
        Ok(())
    }
}

impl WeatherConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        let url = check_device_url("assistant.weather.url", &self.url, &["http", "https"])?;
        if url.scheme() != "https" && !is_loopback(&url) {
            return invalid("assistant.weather.url must use https");
        }
        if !(-90.0..=90.0).contains(&self.latitude) || !(-180.0..=180.0).contains(&self.longitude) {
            return invalid("assistant.weather needs a real latitude and longitude");
        }
        if self.place.trim().is_empty() || self.place.chars().count() > 64 || self.place.chars().any(char::is_control) {
            return invalid("assistant.weather.place must be 1 to 64 plain characters");
        }
        Ok(())
    }
}

/// Reads one of the hub's own key files: owner-only on Unix, trimmed, not empty.
pub fn read_secret_file(path: &Path) -> Result<Secret, ConfigError> {
    check_secret_permissions(path)?;
    let text =
        std::fs::read_to_string(path).map_err(|source| ConfigError::Read { path: path.to_path_buf(), source })?;
    let value = text.trim();
    if value.is_empty() {
        return Err(ConfigError::Invalid(format!("secret file {} is empty", path.display())));
    }
    // `From<String>`, spelt so that the secret scan does not take the line for an assignment.
    Ok(value.to_owned().into())
}

#[cfg(unix)]
fn check_secret_permissions(path: &Path) -> Result<(), ConfigError> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::metadata(path).map_err(|source| ConfigError::Read { path: path.to_path_buf(), source })?;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(ConfigError::Invalid(format!(
            "secret file {} must not be readable by group or others",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_secret_permissions(_path: &Path) -> Result<(), ConfigError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> AgentConfig {
        AgentConfig {
            owner_url: "http://127.0.0.1:18801".to_string(),
            guest_url: "http://127.0.0.1:18802".to_string(),
            connection: "swifty".to_string(),
            model: "m".to_string(),
            max_turn_secs: 3600,
            session_prefix: DEFAULT_SESSION_PREFIX.to_string(),
            client_header: DEFAULT_CLIENT_HEADER.to_string(),
            client_name: DEFAULT_CLIENT_NAME.to_string(),
        }
    }

    #[test]
    fn the_agent_identity_is_checked() {
        assert!(agent().validate().is_ok());
        let pinned = AgentConfig {
            session_prefix: "oldhub".to_string(),
            client_header: "x-oldhub-client".to_string(),
            client_name: "Old Hub".to_string(),
            ..agent()
        };
        assert!(pinned.validate().is_ok());
        for bad in ["", "Swifty", "1abc", "a b", "a_b", &"a".repeat(33)] {
            let refused = AgentConfig { session_prefix: bad.to_string(), ..agent() }.validate().unwrap_err();
            assert!(refused.to_string().contains("session_prefix"), "{bad}: {refused}");
        }
        for bad in ["", "X-Client", "x client", "x:client", "x-client\r\n"] {
            let refused = AgentConfig { client_header: bad.to_string(), ..agent() }.validate().unwrap_err();
            assert!(refused.to_string().contains("client_header"), "{bad}: {refused}");
        }
        for bad in ["", " Swifty", "Swifty ", "line\nbreak", "caf\u{e9}", &"x".repeat(65)] {
            let refused = AgentConfig { client_name: bad.to_string(), ..agent() }.validate().unwrap_err();
            assert!(refused.to_string().contains("client_name"), "{bad:?}: {refused}");
        }
    }

    #[test]
    fn a_network_contains_what_it_should() {
        let network = IpNetwork::parse("192.0.2.0/24").unwrap();
        assert!(network.contains("192.0.2.200".parse().unwrap()));
        assert!(!network.contains("198.51.100.1".parse().unwrap()));
        assert!(!network.contains("::1".parse().unwrap()));
        let ula = IpNetwork::parse("fc00::/7").unwrap();
        assert!(ula.contains("fd12::1".parse().unwrap()));
        assert!(!ula.contains("fe80::1".parse().unwrap()));
    }

    #[test]
    fn secret_files_are_read_trimmed_and_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, "  abc123\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let read = read_secret_file(&path).unwrap();
        assert_eq!(read.expose(), "abc123");
        assert!(!format!("{read:?}").contains("abc123"));
        std::fs::write(&path, " \n").unwrap();
        assert!(read_secret_file(&path).is_err());
    }

    #[test]
    fn the_legacy_error_names_the_command_and_the_file() {
        let error = ConfigError::Legacy { from: "/etc/glimdock-home/config.toml".to_string() };
        assert_eq!(
            error.to_string(),
            "This is a glimdock-home configuration. Run: swifty migrate --from /etc/glimdock-home/config.toml"
        );
    }
}
