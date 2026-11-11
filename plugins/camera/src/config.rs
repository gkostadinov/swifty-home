//! The instance config: where the camera is.
//!
//! The camera's login is not here. It lives in the secret slot `login`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use swifty_plugin_api::ConfigProblem;

/// The RTSP port when the config names none.
pub const DEFAULT_PORT: u16 = 554;

/// One IP camera.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraConfig {
    /// The camera's IP address or host name.
    pub host: String,
    /// Its RTSP port.
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

impl CameraConfig {
    /// Parses and checks a config document.
    pub fn parse(config: &Value) -> Result<Self, ConfigProblem> {
        let parsed: Self = serde_json::from_value(config.clone()).map_err(problem)?;
        parsed.check()?;
        Ok(parsed)
    }

    /// The rules beyond the shape.
    pub fn check(&self) -> Result<(), ConfigProblem> {
        if !is_hostname(&self.host) {
            return Err(ConfigProblem {
                field: "host".to_string(),
                message: "must be a plain host name or IP".to_string(),
            });
        }
        Ok(())
    }

    /// The config as the hub stores it.
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// A serde error as a problem with the field it names, when it names one.
fn problem(error: serde_json::Error) -> ConfigProblem {
    let message = error.to_string();
    let named = message.starts_with("missing field") || message.starts_with("unknown field");
    let field = named.then(|| message.split('`').nth(1)).flatten().unwrap_or("config").to_string();
    ConfigProblem { field, message }
}

/// A host name, an IPv4 address or a bare IPv6 address. Nothing that could
/// carry a path, a user or a second header line.
pub fn is_hostname(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 253
        && text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_host_must_be_plain() {
        for good in ["192.0.2.30", "camera.example.org", "fd00::30", "cam-1"] {
            assert!(CameraConfig::parse(&json!({ "host": good })).is_ok(), "{good}");
        }
        for bad in ["", "rtsp://192.0.2.30", "192.0.2.30/stream", "user@192.0.2.30", "a b", "x\r\nCSeq: 2"] {
            assert_eq!(CameraConfig::parse(&json!({ "host": bad })).unwrap_err().field, "host", "{bad:?}");
        }
    }

    #[test]
    fn the_port_defaults_to_rtsp() {
        assert_eq!(CameraConfig::parse(&json!({ "host": "192.0.2.30" })).unwrap().port, 554);
        assert_eq!(CameraConfig::parse(&json!({ "host": "192.0.2.30", "port": 8554 })).unwrap().port, 8554);
        assert!(CameraConfig::parse(&json!({ "host": "192.0.2.30", "port": 70000 })).is_err());
    }

    #[test]
    fn device_credentials_cannot_be_inline() {
        let refused = CameraConfig::parse(&json!({ "host": "192.0.2.30", "login": "viewer:inline" })).unwrap_err();
        assert_eq!(refused.field, "login");
        assert!(!refused.to_string().contains("inline"));
    }

    #[test]
    fn a_missing_or_unknown_field_is_named() {
        assert_eq!(CameraConfig::parse(&json!({})).unwrap_err().field, "host");
        assert_eq!(CameraConfig::parse(&json!({ "host": "h", "extra": 1 })).unwrap_err().field, "extra");
    }
}
