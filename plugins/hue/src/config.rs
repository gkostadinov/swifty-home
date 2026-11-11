//! The config of a Hue instance: `{ "url": "http://192.0.2.10" }`.
//!
//! The application key the bridge hands out when its link button is pressed is
//! not in the config. It lives in the secret slot `app_key`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use swifty_plugin_api::ConfigProblem;
use url::Url;

/// The secret slot that holds the bridge's application key.
pub const APP_KEY: &str = "app_key";

/// The typed config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HueConfig {
    /// The bridge: a scheme, a host and a port, and nothing else.
    pub url: String,
}

fn problem(field: impl Into<String>, message: impl Into<String>) -> ConfigProblem {
    ConfigProblem { field: field.into(), message: message.into() }
}

/// The field a serde error is about: the first name in backticks, if any.
fn field_of(message: &str) -> String {
    message.split('`').nth(1).unwrap_or("config").to_string()
}

/// Parses and checks a config document.
pub fn parse(config: &Value) -> Result<HueConfig, ConfigProblem> {
    let parsed: HueConfig = serde_json::from_value(config.clone()).map_err(|error| {
        let message = error.to_string();
        problem(field_of(&message), message)
    })?;
    check_device_url("url", &parsed.url, &["http", "https"])?;
    Ok(parsed)
}

/// A device's base URL: the scheme and authority, and nothing else. Today's
/// check on `devices.hue.url`.
fn check_device_url(field: &str, text: &str, schemes: &[&str]) -> Result<Url, ConfigProblem> {
    let url = Url::parse(text).map_err(|error| problem(field, error.to_string()))?;
    if !schemes.contains(&url.scheme()) {
        return Err(problem(field, format!("the scheme must be one of {schemes:?}")));
    }
    if url.host_str().is_none()
        || url.authority().contains('@')
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(problem(field, "must be only a scheme, host and port"));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_bridge_url_is_only_a_scheme_host_and_port() {
        let hue = |url: &str| parse(&json!({ "url": url }));
        assert!(hue("http://192.0.2.1").is_ok());
        assert!(hue("https://192.0.2.1:8443/").is_ok());
        assert!(hue("http://192.0.2.1/api").is_err());
        assert!(hue("http://user:x@192.0.2.1").is_err());
        assert!(hue("http://192.0.2.1?a=b").is_err());
        assert!(hue("ftp://192.0.2.1").is_err());
        assert_eq!(hue("nowhere").unwrap_err().field, "url");
    }

    #[test]
    fn the_key_is_never_part_of_the_config() {
        let error = parse(&json!({ "url": "http://192.0.2.1", "key_file": "k" })).unwrap_err();
        assert_eq!(error.field, "key_file", "{error}");
        let error = parse(&json!({ "url": "http://192.0.2.1", "app_key": "inline" })).unwrap_err();
        assert_eq!(error.field, "app_key", "{error}");
        assert_eq!(parse(&json!({})).unwrap_err().field, "url");
    }
}
