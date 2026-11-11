//! The instance config: where the Plex server is.
//!
//! The token is not here. It lives in the secret slot `token`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use swifty_plugin_api::ConfigProblem;
use url::Url;

/// One Plex Media Server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlexConfig {
    /// The server's base URL: a scheme, a host and a port, nothing else.
    pub url: String,
}

impl PlexConfig {
    /// Parses and checks a config document.
    pub fn parse(config: &Value) -> Result<Self, ConfigProblem> {
        let parsed: Self = serde_json::from_value(config.clone()).map_err(problem)?;
        parsed.check()?;
        Ok(parsed)
    }

    /// The rules beyond the shape.
    pub fn check(&self) -> Result<(), ConfigProblem> {
        check_device_url("url", &self.url).map(|_| ())
    }

    /// The config as the hub stores it.
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

fn invalid(field: &str, message: &str) -> ConfigProblem {
    ConfigProblem { field: field.to_string(), message: message.to_string() }
}

/// A serde error as a problem with the field it names, when it names one.
fn problem(error: serde_json::Error) -> ConfigProblem {
    let message = error.to_string();
    let named = message.starts_with("missing field") || message.starts_with("unknown field");
    let field = named.then(|| message.split('`').nth(1)).flatten().unwrap_or("config").to_string();
    ConfigProblem { field, message }
}

/// A device's base URL: the scheme and authority, and nothing else.
pub fn check_device_url(field: &str, text: &str) -> Result<Url, ConfigProblem> {
    let url = Url::parse(text).map_err(|e| invalid(field, &e.to_string()))?;
    if !["http", "https"].contains(&url.scheme()) {
        return Err(invalid(field, "the scheme must be http or https"));
    }
    if url.host_str().is_none()
        || url.authority().contains('@')
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(invalid(field, "must be only a scheme, host and port"));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn device_credentials_cannot_be_inline() {
        let refused = PlexConfig::parse(&json!({ "url": "http://p", "token": "inline" })).unwrap_err();
        assert_eq!(refused.field, "token");
        assert!(!refused.to_string().contains("inline"));
    }

    #[test]
    fn the_url_is_only_a_scheme_host_and_port() {
        for good in ["http://192.0.2.20:32400", "https://plex.example.org:32400", "http://p"] {
            assert!(PlexConfig::parse(&json!({ "url": good })).is_ok(), "{good}");
        }
        for bad in [
            "ftp://192.0.2.20",
            "http://192.0.2.20:32400/web",
            "http://user@192.0.2.20",
            "http://192.0.2.20/?X-Plex-Token=x",
            "http://192.0.2.20/#x",
            "plex",
        ] {
            assert_eq!(PlexConfig::parse(&json!({ "url": bad })).unwrap_err().field, "url", "{bad}");
        }
    }

    #[test]
    fn a_missing_or_unknown_field_is_named() {
        assert_eq!(PlexConfig::parse(&json!({})).unwrap_err().field, "url");
        assert_eq!(PlexConfig::parse(&json!({ "url": "http://p", "extra": 1 })).unwrap_err().field, "extra");
        assert_eq!(PlexConfig::parse(&json!({ "url": 5 })).unwrap_err().field, "config");
    }
}
