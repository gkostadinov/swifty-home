//! The instance config: where the node is and which of its paths to use.
//!
//! The access token is not here. It lives in the secret slot `token`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use swifty_plugin_api::ConfigProblem;
use url::Url;

/// The longest path the config accepts.
pub const MAX_PATH: usize = 256;

/// One Glimdock node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlimdockConfig {
    /// The node's base URL: a scheme, a host and a port, nothing else.
    pub url: String,
    /// The path of the node's snapshot endpoint.
    pub snapshot_path: String,
    /// The path of the node's own agent pairing endpoint. Without it, pairing says so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_path: Option<String>,
}

impl GlimdockConfig {
    /// Parses and checks a config document.
    pub fn parse(config: &Value) -> Result<Self, ConfigProblem> {
        let parsed: Self = serde_json::from_value(config.clone()).map_err(problem)?;
        parsed.check()?;
        Ok(parsed)
    }

    /// The rules beyond the shape.
    pub fn check(&self) -> Result<(), ConfigProblem> {
        check_device_url("url", &self.url)?;
        if !is_plain_path(&self.snapshot_path) {
            return Err(invalid("snapshot_path", "must be a plain absolute path"));
        }
        if let Some(path) = &self.pairing_path {
            if !is_plain_path(path) {
                return Err(invalid("pairing_path", "must be a plain absolute path"));
            }
        }
        Ok(())
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

/// An absolute path on the node. A leading `//` is a scheme-relative URL: it
/// would name another host, and the node's token would go there.
pub fn is_plain_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains(['?', '#', '\\'])
        && !path.contains("..")
        && path.len() <= MAX_PATH
        && !path.chars().any(char::is_control)
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

    fn config(path: &str) -> Result<GlimdockConfig, ConfigProblem> {
        GlimdockConfig::parse(&json!({ "url": "http://c", "snapshot_path": path }))
    }

    #[test]
    fn a_scheme_relative_snapshot_path_is_refused() {
        assert!(config("/api/snapshot").is_ok());
        assert!(config("//127.0.0.1:52409/stolen").is_err());
        assert!(config("//evil.example/stolen").is_err());
    }

    #[test]
    fn collector_and_tv_settings_are_checked() {
        assert!(config("/snapshot").is_ok());
        assert!(config("snapshot").is_err());
        assert!(config("/a?x=1").is_err());
        assert!(config("/a/../b").is_err());
    }

    #[test]
    fn discovery_and_pairing_settings_are_checked() {
        let pairing = |path: &str| {
            GlimdockConfig::parse(&json!({ "url": "http://c", "snapshot_path": "/s", "pairing_path": path }))
        };
        assert!(pairing("/pair").is_ok());
        assert!(pairing("//evil.example/pair").is_err());
    }

    #[test]
    fn the_token_cannot_be_inline() {
        let inline = json!({ "url": "http://c", "snapshot_path": "/s", "token": "inline" });
        let refused = GlimdockConfig::parse(&inline).unwrap_err();
        assert_eq!(refused.field, "token");
        assert!(!refused.to_string().contains("inline"));
    }

    #[test]
    fn the_url_is_only_a_scheme_host_and_port() {
        for good in ["http://192.0.2.10", "https://node.example.org:8443", "http://192.0.2.10:8080/"] {
            assert!(check_device_url("url", good).is_ok(), "{good}");
        }
        for bad in [
            "ftp://192.0.2.10",
            "http://192.0.2.10/api",
            "http://user@192.0.2.10",
            "http://192.0.2.10/?x=1",
            "http://192.0.2.10/#x",
            "not a url",
        ] {
            assert_eq!(check_device_url("url", bad).unwrap_err().field, "url", "{bad}");
        }
    }

    #[test]
    fn a_missing_or_unknown_field_is_named() {
        assert_eq!(GlimdockConfig::parse(&json!({ "snapshot_path": "/s" })).unwrap_err().field, "url");
        let unknown = json!({ "url": "http://c", "snapshot_path": "/s", "extra": 1 });
        assert_eq!(GlimdockConfig::parse(&unknown).unwrap_err().field, "extra");
        let wrong_type = json!({ "url": 5, "snapshot_path": "/s" });
        assert_eq!(GlimdockConfig::parse(&wrong_type).unwrap_err().field, "config");
    }

    #[test]
    fn the_stored_form_leaves_out_an_absent_pairing_path() {
        let config = config("/s").unwrap();
        assert_eq!(config.to_value(), json!({ "url": "http://c", "snapshot_path": "/s" }));
        assert_eq!(GlimdockConfig::parse(&config.to_value()).unwrap(), config);
    }
}
