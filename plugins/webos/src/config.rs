//! The instance config: where the TV is, its certificate pin, and how to wake it.
//! The client key is a secret (slot `client_key`) and never part of this.

// Address types only: every socket is opened by `swifty-net`.
use swifty_net::SocketAddr;

use serde::Deserialize;
use serde_json::Value;
use swifty_plugin_api::ConfigProblem;
use url::Url;

/// One TV.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebosConfig {
    /// `ws://<address>:3000` or `wss://<address>:3001`.
    pub url: String,
    /// The SHA-256 of the TV's certificate, in hex. Required for wss, which is all some TVs offer.
    pub cert_sha256: Option<String>,
    /// The TV's MAC address. SSAP cannot wake a TV that is off, so without this
    /// turning it on is refused.
    pub wol_mac: Option<String>,
    /// Where the Wake-on-LAN packet goes, as IP:port. `255.255.255.255:9` when not set.
    pub wol_broadcast: Option<String>,
}

fn problem(field: &str, message: impl Into<String>) -> ConfigProblem {
    ConfigProblem { field: field.to_string(), message: message.into() }
}

/// A MAC address: six pairs of hex digits, separated by `:` or `-`.
pub fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = text.split([':', '-']).collect();
    let parts: [&str; 6] = parts.try_into().ok()?;
    let mut mac = [0u8; 6];
    for (slot, part) in mac.iter_mut().zip(parts) {
        if part.len() != 2 {
            return None;
        }
        *slot = u8::from_str_radix(part, 16).ok()?;
    }
    Some(mac)
}

/// Whether `pin` is a SHA-256 in hex.
pub fn is_pin(pin: &str) -> bool {
    pin.len() == 64 && pin.chars().all(|c| c.is_ascii_hexdigit())
}

/// A device's base URL: the scheme and authority, and nothing else.
fn check_device_url(field: &str, text: &str, schemes: &[&str]) -> Result<Url, ConfigProblem> {
    let url = Url::parse(text).map_err(|e| problem(field, e.to_string()))?;
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

impl WebosConfig {
    /// Parses and checks a config document.
    pub fn parse(config: &Value) -> Result<Self, ConfigProblem> {
        let parsed: Self = serde_json::from_value(config.clone()).map_err(|e| problem("config", e.to_string()))?;
        parsed.check()?;
        Ok(parsed)
    }

    fn check(&self) -> Result<(), ConfigProblem> {
        let url = check_device_url("url", &self.url, &["ws", "wss"])?;
        match (url.scheme(), &self.cert_sha256) {
            ("wss", Some(pin)) if is_pin(pin) => {}
            ("wss", _) => return Err(problem("cert_sha256", "must be the certificate's SHA-256 in hex")),
            ("ws", Some(_)) => return Err(problem("cert_sha256", "only applies to wss")),
            _ => {}
        }
        if let Some(mac) = &self.wol_mac {
            if parse_mac(mac).is_none() {
                return Err(problem("wol_mac", "is not a MAC address"));
            }
        }
        if let Some(broadcast) = &self.wol_broadcast {
            if broadcast.parse::<SocketAddr>().is_err() {
                return Err(problem("wol_broadcast", "must be IP:port"));
            }
        }
        Ok(())
    }

    /// The URL, already checked.
    pub fn url(&self) -> Result<Url, String> {
        Url::parse(&self.url).map_err(|_| "url is not valid".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The TV half of the hub's config test `collector_and_tv_settings_are_checked`.
    #[test]
    fn tv_settings_are_checked() {
        let tv = |extra: Value| {
            let mut config = json!({ "url": "ws://t:3000" });
            config.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            WebosConfig::parse(&config)
        };
        assert!(tv(json!({ "wol_mac": "aa:bb:cc:dd:ee:ff" })).is_ok());
        assert!(tv(json!({ "wol_mac": "aa:bb:cc" })).is_err());
        assert!(tv(json!({ "wol_broadcast": "nowhere" })).is_err());
        assert_eq!(parse_mac("AA-bb-CC-dd-EE-FF"), Some([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]));
        assert!(WebosConfig::parse(&json!({ "url": "http://t:3000" })).is_err());
    }

    /// As the hub's config test `device_credentials_cannot_be_inline`: the key is a secret slot.
    #[test]
    fn the_client_key_cannot_be_inline() {
        let inline = json!({ "url": "ws://t:3000", "client_key": "inline" });
        let problem = WebosConfig::parse(&inline).unwrap_err();
        assert!(problem.message.contains("client_key"), "{problem}");
    }
}
