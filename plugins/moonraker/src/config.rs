//! The instance config and its checks. An instance is one printer: Moonraker's
//! address. Its optional API key lives in the secret slot `api_key`, never here.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use swifty_plugin_api::ConfigProblem;
use url::Url;

/// The slot that holds Moonraker's API key, when it asks for one.
pub const KEY_SLOT: &str = "api_key";
/// The local id of the printer, and the target it would like.
pub const PRINTER: &str = "printer";

/// What an instance is configured with.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MoonrakerConfig {
    /// Moonraker's base URL: the scheme, the printer's address and the port, usually 7125.
    pub url: String,
}

/// The field a serde error names, which is the first one in backticks.
fn serde_problem(error: &serde_json::Error) -> ConfigProblem {
    let text = error.to_string();
    let field = text.split('`').nth(1).unwrap_or("config").to_string();
    ConfigProblem { field, message: text }
}

/// Parses and checks a config document.
pub fn parse(config: &Value) -> Result<MoonrakerConfig, ConfigProblem> {
    let parsed: MoonrakerConfig = serde_json::from_value(config.clone()).map_err(|error| serde_problem(&error))?;
    check_device_url("url", &parsed.url, &["http", "https"])?;
    Ok(parsed)
}

/// A device's base URL: the scheme and authority, and nothing else.
pub fn check_device_url(field: &str, text: &str, schemes: &[&str]) -> Result<Url, ConfigProblem> {
    let problem = |message: String| ConfigProblem { field: field.to_string(), message };
    let url = Url::parse(text).map_err(|error| problem(error.to_string()))?;
    if !schemes.contains(&url.scheme()) {
        return Err(problem(format!("the scheme must be one of {schemes:?}")));
    }
    if url.host_str().is_none()
        || url.authority().contains('@')
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(problem("must be only a scheme, host and port".to_string()));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_url_is_only_a_scheme_a_host_and_a_port() {
        assert!(parse(&json!({ "url": "http://192.0.2.30:7125" })).is_ok());
        assert!(parse(&json!({ "url": "https://192.0.2.30" })).is_ok());
        for bad in
            ["ftp://192.0.2.30", "http://u@192.0.2.30", "http://192.0.2.30/printer", "http://192.0.2.30?a=1", "x"]
        {
            assert_eq!(parse(&json!({ "url": bad })).unwrap_err().field, "url", "{bad}");
        }
        assert_eq!(parse(&json!({})).unwrap_err().field, "url");
        assert_eq!(
            parse(&json!({ "url": "http://192.0.2.30", "api_key_file": "/k" })).unwrap_err().field,
            "api_key_file"
        );
    }
}
