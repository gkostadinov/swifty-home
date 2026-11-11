//! The instance config: what `[devices.toshiba]` held, with the sign-in moved out to the
//! secret slot `login`, and the client identity as two fields of its own.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::Value;
use swifty_plugin_api::ConfigProblem;
use url::{Host, Url};

use crate::identity;

/// Where the cloud's REST API is.
pub const DEFAULT_API_URL: &str = "https://mobileapi.toshibahomeaccontrols.com";
/// The hub that takes commands. A token for any other hub is refused.
pub const DEFAULT_HUB_URL: &str = "https://toshibasmaciothubprod.azure-devices.net";
/// How long one request to the cloud may take to answer, in milliseconds.
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;
pub const MAX_TIMEOUT_MS: u64 = 30_000;
pub const MAX_AIRCONS: usize = 8;
pub const MAX_AIRCON_ID: usize = 64;
pub const MAX_AIRCON_UNIQUE_ID: usize = 128;
pub const MAX_NAME: usize = 64;
pub const MAX_CLIENT_SUFFIX: usize = 64;
pub const MAX_USER_AGENT: usize = 256;

/// Toshiba air conditioners on Wi-Fi, on one account of Toshiba's Home AC Control cloud.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToshibaConfig {
    #[serde(default = "default_api_url")]
    pub api_url: String,
    /// The hub that takes commands. A token for any other hub is refused.
    #[serde(default = "default_hub_url")]
    pub hub_url: String,
    /// How long one request to the cloud may take to answer.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// Joined to the account's e-mail, the hub's client id at the cloud. See [`identity::CLIENT_SUFFIX`].
    #[serde(default = "default_client_suffix")]
    pub client_suffix: String,
    /// What the hub says it is. See [`identity::USER_AGENT`].
    #[serde(default = "default_user_agent")]
    pub user_agent: String,
    pub units: Vec<AirconConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AirconConfig {
    /// The `Id` the cloud lists for this unit.
    pub id: String,
    /// The `DeviceUniqueId` the cloud lists for this unit. Commands are addressed to it.
    pub unique_id: String,
    pub name: Option<String>,
}

impl AirconConfig {
    /// The unit's local id, which is also the target it has always had.
    pub fn target(&self) -> String {
        format!("ac-{}", self.id.to_ascii_lowercase())
    }
}

fn default_api_url() -> String {
    DEFAULT_API_URL.to_string()
}

fn default_hub_url() -> String {
    DEFAULT_HUB_URL.to_string()
}

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

fn default_client_suffix() -> String {
    identity::CLIENT_SUFFIX.to_string()
}

fn default_user_agent() -> String {
    identity::USER_AGENT.to_string()
}

fn problem(field: &str, message: impl Into<String>) -> ConfigProblem {
    ConfigProblem { field: field.to_string(), message: message.into() }
}

/// What serde found wrong, under the field it names. A config holds no secret, so its
/// words can be shown.
fn shape_problem(error: serde_json::Error) -> ConfigProblem {
    let message = error.to_string();
    let field = message.split('`').nth(1).unwrap_or("config").to_string();
    ConfigProblem { field, message }
}

/// A Toshiba unit id: letters, digits and hyphens, of a sensible length.
pub fn is_aircon_id(id: &str) -> bool {
    (8..=MAX_AIRCON_ID).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// A unique id goes into a message built by hand, so it holds no quote, backslash or space.
pub fn is_plain_unique_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_AIRCON_UNIQUE_ID
        && id.bytes().all(|b| b.is_ascii_graphic() && !matches!(b, b'"' | b'\\'))
}

/// A name a person would write: no edge spaces, no control characters, not too long.
pub fn is_plain_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.trim() == name
        && !name.chars().any(char::is_control)
        && name.chars().count() <= MAX_NAME
}

/// Whether a URL names this machine.
pub fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// A cloud's address: only a scheme, host and port, and https unless it is this machine.
fn check_cloud_url(field: &str, text: &str) -> Result<(), ConfigProblem> {
    let url = Url::parse(text).map_err(|error| problem(field, format!("{field}: {error}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(problem(field, format!("{field}: the scheme must be http or https")));
    }
    if url.host_str().is_none()
        || url.authority().contains('@')
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(problem(field, format!("{field}: must be only a scheme, host and port")));
    }
    if url.scheme() != "https" && !is_loopback(&url) {
        return Err(problem(field, format!("{field} must use https unless it is this machine")));
    }
    Ok(())
}

/// Today's `check_toshiba`, with the credentials file gone.
fn check(toshiba: &ToshibaConfig) -> Result<(), ConfigProblem> {
    check_cloud_url("api_url", &toshiba.api_url)?;
    check_cloud_url("hub_url", &toshiba.hub_url)?;
    if toshiba.timeout_ms == 0 || toshiba.timeout_ms > MAX_TIMEOUT_MS {
        return Err(problem("timeout_ms", format!("timeout_ms must be 1 to {MAX_TIMEOUT_MS}")));
    }
    let suffix = &toshiba.client_suffix;
    if suffix.is_empty() || suffix.len() > MAX_CLIENT_SUFFIX || !suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(problem("client_suffix", format!("client_suffix must be 1 to {MAX_CLIENT_SUFFIX} hex digits")));
    }
    let agent = &toshiba.user_agent;
    if agent.trim().is_empty()
        || agent.trim() != agent
        || agent.len() > MAX_USER_AGENT
        || !agent.bytes().all(|b| (0x20..0x7f).contains(&b))
    {
        return Err(problem(
            "user_agent",
            format!("user_agent must be 1 to {MAX_USER_AGENT} plain ASCII characters, with no space at either end"),
        ));
    }
    if toshiba.units.is_empty() || toshiba.units.len() > MAX_AIRCONS {
        return Err(problem("units", format!("units must list 1 to {MAX_AIRCONS} air conditioners")));
    }
    let mut targets = HashSet::new();
    for unit in &toshiba.units {
        let id = &unit.id;
        if !is_aircon_id(id) {
            return Err(problem("units", "units: an id must be 8 to 64 letters, digits and hyphens"));
        }
        if !targets.insert(unit.target()) {
            return Err(problem("units", format!("units: {id} is listed twice")));
        }
        if !is_plain_unique_id(&unit.unique_id) {
            return Err(problem(
                "units",
                format!("units: the unique_id of {id} must be 1 to {MAX_AIRCON_UNIQUE_ID} plain characters"),
            ));
        }
        if unit.name.as_deref().is_some_and(|name| !is_plain_name(name)) {
            return Err(problem("units", format!("units: the name of {id} must be 1 to {MAX_NAME} plain characters")));
        }
    }
    Ok(())
}

/// Reads and checks a config document.
pub fn parse(config: &Value) -> Result<ToshibaConfig, ConfigProblem> {
    let toshiba: ToshibaConfig = serde_json::from_value(config.clone()).map_err(shape_problem)?;
    check(&toshiba)?;
    Ok(toshiba)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const UNIT: &str = "0a1b2c3d-1111-2222-3333-444455556666";

    fn unit() -> Value {
        json!({ "id": UNIT, "unique_id": "AB-12:cd" })
    }

    fn with(extra: Value, units: Value) -> Result<ToshibaConfig, ConfigProblem> {
        let mut config = json!({ "units": units });
        for (key, value) in extra.as_object().unwrap() {
            config[key] = value.clone();
        }
        parse(&config)
    }

    #[test]
    fn a_toshiba_unit_has_to_be_described_exactly() {
        let toshiba = |extra: Value| with(extra, json!([unit()]));
        assert!(toshiba(json!({})).is_ok());
        let settings = toshiba(json!({})).unwrap();
        assert_eq!(settings.api_url, "https://mobileapi.toshibahomeaccontrols.com");
        assert_eq!(settings.hub_url, "https://toshibasmaciothubprod.azure-devices.net");
        assert_eq!(settings.units[0].target(), "ac-0a1b2c3d-1111-2222-3333-444455556666");
        assert!(toshiba(json!({ "timeout_ms": 30000, "hub_url": "https://h.azure-devices.net" })).is_ok());
        assert!(toshiba(json!({ "api_url": "http://127.0.0.1:9" })).is_ok(), "this machine, for a test");
        for bad in [
            json!({ "api_url": "http://192.0.2.1" }),
            json!({ "hub_url": "http://hub.test" }),
            json!({ "api_url": "https://h.test/api" }),
            json!({ "timeout_ms": 0 }),
            json!({ "timeout_ms": 30001 }),
            json!({ "login": "inline" }),
            json!({ "credentials_file": "/etc/x/toshiba" }),
        ] {
            assert!(toshiba(bad.clone()).is_err(), "{bad}");
        }
        assert!(parse(&json!({})).is_err(), "nothing at all");
        let one = |body: Value| with(json!({}), json!([body]));
        for bad in [
            json!({ "id": "short", "unique_id": "x" }),
            json!({ "id": "has spaces here", "unique_id": "x" }),
            json!({ "id": "0a1b2c3d", "unique_id": "" }),
            json!({ "id": "0a1b2c3d", "unique_id": "has space" }),
            json!({ "id": "0a1b2c3d", "unique_id": "quo\"te" }),
            json!({ "id": "0a1b2c3d", "unique_id": "x", "name": "" }),
            json!({ "id": "0a1b2c3d", "unique_id": "x", "address": "192.0.2.1" }),
        ] {
            assert!(one(bad.clone()).is_err(), "{bad}");
        }
        assert!(one(json!({ "id": "0a1b2c3d", "unique_id": "x", "name": "Lounge" })).is_ok());
        let twice = json!([unit(), { "id": "0A1B2C3D-1111-2222-3333-444455556666", "unique_id": "y" }]);
        assert!(with(json!({}), twice).is_err(), "the same id in another case");
        assert!(with(json!({}), json!([])).is_err(), "nothing listed");
    }

    #[test]
    fn the_client_identity_has_a_new_default_and_can_be_pinned() {
        let defaults = with(json!({}), json!([unit()])).unwrap();
        assert_eq!(defaults.client_suffix, identity::CLIENT_SUFFIX);
        assert_eq!(defaults.user_agent, identity::USER_AGENT);
        let pinned = with(json!({ "client_suffix": "6f6c64687562" }), json!([unit()])).unwrap();
        assert_eq!(pinned.client_suffix, "6f6c64687562", "an install keeps the id the cloud knows");
        assert!(with(json!({ "user_agent": "Mozilla/5.0 (X11; Linux x86_64)" }), json!([unit()])).is_ok());
        for bad in [
            json!({ "client_suffix": "" }),
            json!({ "client_suffix": "not hex" }),
            json!({ "client_suffix": "a/b" }),
            json!({ "client_suffix": "a".repeat(65) }),
            json!({ "user_agent": "" }),
            json!({ "user_agent": " padded " }),
            json!({ "user_agent": "line\nbreak" }),
            json!({ "user_agent": "x".repeat(257) }),
        ] {
            let error = with(bad.clone(), json!([unit()])).unwrap_err();
            let key = bad.as_object().unwrap().keys().next().unwrap().clone();
            assert_eq!(error.field, key, "{bad}");
        }
    }

    #[test]
    fn a_problem_names_its_field() {
        let error = parse(&json!({ "units": [unit()], "mystery": true })).unwrap_err();
        assert_eq!(error.field, "mystery");
        let error = parse(&json!({})).unwrap_err();
        assert_eq!(error.field, "units");
    }
}
