//! The instance config: what `[devices.petkit]` held, with the login moved out to the
//! secret slot `login`, and the client identity as fields of its own.

use serde::Deserialize;
use serde_json::Value;
use swifty_plugin_api::ConfigProblem;
use url::{Host, Url};

use crate::identity;

/// Where Petkit lists its regional servers.
pub const DEFAULT_PASSPORT_URL: &str = "https://passport.petkt.com/";
pub const DEFAULT_TIMEZONE: &str = "UTC";
pub const DEFAULT_POLL_SECS: u64 = 60;
pub const MIN_POLL_SECS: u64 = 15;
pub const MAX_POLL_SECS: u64 = 600;
pub const MAX_REGION: usize = 32;
pub const MAX_TIMEZONE: usize = 64;
pub const MAX_OFFSET_MINUTES: i32 = 840;
pub const MAX_IDENTITY: usize = 64;
pub const MAX_USER_AGENT: usize = 200;

/// The Petkit D4 feeder and T4 litter box on one Petkit account.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PetkitConfig {
    /// Your country as the Petkit app has it ("DE", "US"), or "china".
    pub region: String,
    /// An IANA name, sent to Petkit as the time zone of this account.
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// Minutes east of UTC for that zone, now. The date of a feed and the time shown
    /// for the last one use it. It does not follow daylight saving time by itself.
    #[serde(default)]
    pub utc_offset_minutes: i32,
    /// Feeding and cleaning are refused until this is true, so the readings can be
    /// checked against the real devices first.
    #[serde(default)]
    pub commands: bool,
    /// Only needed when the account has more than one feeder or litter box.
    pub feeder_id: Option<u64>,
    pub litter_id: Option<u64>,
    /// Where Petkit lists its regional servers. Only a test changes this.
    #[serde(default = "default_passport_url")]
    pub passport_url: String,
    /// How often a reading is fetched from Petkit when nothing was just commanded.
    #[serde(default = "default_poll_secs")]
    pub poll_secs: u64,
    /// See [`identity::APP_VERSION`].
    #[serde(default = "default_app_version")]
    pub app_version: String,
    /// See [`identity::USER_AGENT`].
    #[serde(default = "default_user_agent")]
    pub user_agent: String,
    /// See [`identity::OS_VERSION`].
    #[serde(default = "default_os_version")]
    pub os_version: String,
    /// See [`identity::PHONE_MODEL`].
    #[serde(default = "default_phone_model")]
    pub phone_model: String,
    /// See [`identity::PHONE_BRAND`].
    #[serde(default = "default_phone_brand")]
    pub phone_brand: String,
}

fn default_timezone() -> String {
    DEFAULT_TIMEZONE.to_string()
}

fn default_passport_url() -> String {
    DEFAULT_PASSPORT_URL.to_string()
}

fn default_poll_secs() -> u64 {
    DEFAULT_POLL_SECS
}

fn default_app_version() -> String {
    identity::APP_VERSION.to_string()
}

fn default_user_agent() -> String {
    identity::USER_AGENT.to_string()
}

fn default_os_version() -> String {
    identity::OS_VERSION.to_string()
}

fn default_phone_model() -> String {
    identity::PHONE_MODEL.to_string()
}

fn default_phone_brand() -> String {
    identity::PHONE_BRAND.to_string()
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

/// Whether a URL names this machine.
pub fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// A region as the Petkit app names it: letters, digits, spaces and hyphens.
pub fn is_region(region: &str) -> bool {
    !region.is_empty()
        && region.len() <= MAX_REGION
        && region.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-'))
}

/// An IANA zone name, such as `Europe/Berlin`.
pub fn is_timezone(zone: &str) -> bool {
    !zone.is_empty()
        && zone.len() <= MAX_TIMEZONE
        && zone.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '+'))
}

/// An identity string goes into headers and into the login's client description, which
/// is quoted with `'`, and `X-Client` uses `(`, `;` and `)`. So it keeps to plain characters.
fn is_identity(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_IDENTITY
        && text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-' | '/'))
        && text.trim() == text
}

/// Today's `check_petkit`, with the credentials file gone and the identity added.
fn check(petkit: &PetkitConfig) -> Result<(), ConfigProblem> {
    if !is_region(&petkit.region) {
        return Err(problem("region", "region must be your country as the Petkit app has it, such as DE, or china"));
    }
    if !is_timezone(&petkit.timezone) {
        return Err(problem("timezone", "timezone must be an IANA name such as Europe/Berlin"));
    }
    if !(-MAX_OFFSET_MINUTES..=MAX_OFFSET_MINUTES).contains(&petkit.utc_offset_minutes) {
        return Err(problem("utc_offset_minutes", "utc_offset_minutes must be between -840 and 840"));
    }
    if !(MIN_POLL_SECS..=MAX_POLL_SECS).contains(&petkit.poll_secs) {
        return Err(problem("poll_secs", format!("poll_secs must be between {MIN_POLL_SECS} and {MAX_POLL_SECS}")));
    }
    if petkit.feeder_id == Some(0) || petkit.litter_id == Some(0) {
        let field = if petkit.feeder_id == Some(0) { "feeder_id" } else { "litter_id" };
        return Err(problem(field, "feeder_id and litter_id cannot be 0"));
    }
    let field = "passport_url";
    let url = Url::parse(&petkit.passport_url).map_err(|error| problem(field, format!("{field}: {error}")))?;
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
        return Err(problem(field, "passport_url must use https, so the login never crosses the network in clear"));
    }
    for (field, value) in [
        ("app_version", &petkit.app_version),
        ("os_version", &petkit.os_version),
        ("phone_model", &petkit.phone_model),
        ("phone_brand", &petkit.phone_brand),
    ] {
        if !is_identity(value) {
            return Err(problem(
                field,
                format!("{field} must be 1 to {MAX_IDENTITY} letters, digits, spaces and . _ - /"),
            ));
        }
    }
    let agent = &petkit.user_agent;
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
    Ok(())
}

/// Reads and checks a config document.
pub fn parse(config: &Value) -> Result<PetkitConfig, ConfigProblem> {
    let petkit: PetkitConfig = serde_json::from_value(config.clone()).map_err(shape_problem)?;
    check(&petkit)?;
    Ok(petkit)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn petkit(extra: Value) -> Result<PetkitConfig, ConfigProblem> {
        let mut config = json!({ "region": "DE" });
        for (key, value) in extra.as_object().unwrap() {
            config[key] = value.clone();
        }
        parse(&config)
    }

    #[test]
    fn the_petkit_section_is_checked_and_its_login_is_never_inline() {
        assert!(petkit(json!({})).is_ok());
        assert!(petkit(json!({ "timezone": "Europe/Paris", "utc_offset_minutes": 120, "commands": true })).is_ok());
        assert!(petkit(json!({ "feeder_id": 12345, "litter_id": 67890, "poll_secs": 120 })).is_ok());
        for bad in [
            json!({ "region": "" }),
            json!({ "region": "B/G" }),
            json!({ "timezone": "Europe Paris" }),
            json!({ "utc_offset_minutes": 900 }),
            json!({ "poll_secs": 5 }),
            json!({ "poll_secs": 601 }),
            json!({ "feeder_id": 0 }),
            json!({ "passport_url": "http://passport.example" }),
            json!({ "passport_url": "https://passport.example/x" }),
            json!({ "md5": "inline" }),
            json!({ "username": "inline" }),
            json!({ "login": "inline" }),
            json!({ "credentials_file": "/c" }),
        ] {
            assert!(petkit(bad.clone()).is_err(), "{bad}");
        }
        assert!(parse(&json!({})).is_err(), "a region is needed");
        assert!(petkit(json!({ "passport_url": "http://127.0.0.1:9" })).is_ok(), "loopback is for tests");
    }

    #[test]
    fn commands_start_off_and_the_identity_has_defaults_that_can_be_changed() {
        let defaults = petkit(json!({})).unwrap();
        assert!(!defaults.commands);
        assert_eq!(
            (defaults.app_version.as_str(), defaults.user_agent.as_str(), defaults.phone_model.as_str()),
            (identity::APP_VERSION, identity::USER_AGENT, identity::PHONE_MODEL)
        );
        let newer = petkit(json!({ "app_version": "14.0.0", "user_agent": "okhttp/4.12.0", "os_version": "17" }));
        assert_eq!(newer.unwrap().app_version, "14.0.0");
        for bad in [
            json!({ "app_version": "" }),
            json!({ "app_version": "1'2" }),
            json!({ "os_version": "16;1" }),
            json!({ "phone_model": "a(b)" }),
            json!({ "phone_brand": " padded" }),
            json!({ "user_agent": "line\nbreak" }),
        ] {
            let error = petkit(bad.clone()).unwrap_err();
            let key = bad.as_object().unwrap().keys().next().unwrap().clone();
            assert_eq!(error.field, key, "{bad}");
        }
    }
}
