//! The instance config: one row per speaker.
//!
//! `{ "speakers": [{ "id": "sonos", "url": "http://192.0.2.30:1400" },
//!                 { "id": "sub", "url": "http://192.0.2.31:1400", "read_only": true }] }`

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::Value;
use swifty_plugin_api::ConfigProblem;
use url::Url;

/// More speakers than any one home has. Bounds what a config can make the hub hold.
pub const MAX_SPEAKERS: usize = 32;

/// The speakers of one instance.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SonosConfig {
    /// One row per speaker, in the order they are shown.
    pub speakers: Vec<SpeakerConfig>,
}

/// One speaker.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeakerConfig {
    /// The device's local id, which is also the target it asks for.
    pub id: String,
    /// `http://<IP address>:1400`. A real speaker checks the Host header and
    /// answers HTTP 400 to a request addressed by name.
    pub url: String,
    /// Only ever read, never commanded. A subwoofer has no transport of its own.
    #[serde(default)]
    pub read_only: bool,
}

fn problem(field: impl Into<String>, message: impl Into<String>) -> ConfigProblem {
    ConfigProblem { field: field.into(), message: message.into() }
}

/// `[a-z0-9][a-z0-9_-]{0,63}`: a device's local id.
pub fn is_local_id(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && text.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// A device's base URL: the scheme and authority, and nothing else.
pub fn check_device_url(field: &str, text: &str, schemes: &[&str]) -> Result<Url, ConfigProblem> {
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

impl SonosConfig {
    /// Parses and checks a config document.
    pub fn parse(config: &Value) -> Result<Self, ConfigProblem> {
        let parsed: Self = serde_json::from_value(config.clone()).map_err(|e| problem("config", e.to_string()))?;
        parsed.check()?;
        Ok(parsed)
    }

    fn check(&self) -> Result<(), ConfigProblem> {
        if self.speakers.is_empty() {
            return Err(problem("speakers", "name at least one speaker"));
        }
        if self.speakers.len() > MAX_SPEAKERS {
            return Err(problem("speakers", format!("at most {MAX_SPEAKERS} speakers")));
        }
        let mut ids = BTreeSet::new();
        for (row, speaker) in self.speakers.iter().enumerate() {
            if !is_local_id(&speaker.id) {
                return Err(problem(format!("speakers[{row}].id"), "must match [a-z0-9][a-z0-9_-]{0,63}"));
            }
            if !ids.insert(speaker.id.as_str()) {
                return Err(problem(format!("speakers[{row}].id"), format!("{} is used twice", speaker.id)));
            }
            check_device_url(&format!("speakers[{row}].url"), &speaker.url, &["http"])?;
        }
        Ok(())
    }

    /// The speaker with this local id.
    pub fn speaker(&self, id: &str) -> Option<&SpeakerConfig> {
        self.speakers.iter().find(|speaker| speaker.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn speakers_are_checked_row_by_row() {
        let parse = |speakers: Value| SonosConfig::parse(&json!({ "speakers": speakers }));
        assert!(parse(json!([{ "id": "sonos", "url": "http://192.0.2.30:1400" }])).is_ok());
        assert!(parse(json!([
            { "id": "sonos", "url": "http://192.0.2.30:1400" },
            { "id": "sub", "url": "http://192.0.2.31:1400", "read_only": true },
        ]))
        .is_ok());
        assert!(parse(json!([])).is_err(), "no speaker");
        assert!(parse(json!([{ "id": "Sonos", "url": "http://192.0.2.30:1400" }])).is_err(), "upper case");
        assert!(parse(json!([
            { "id": "sonos", "url": "http://192.0.2.30:1400" },
            { "id": "sonos", "url": "http://192.0.2.31:1400" },
        ]))
        .is_err());
        for url in ["https://192.0.2.30:1400", "http://192.0.2.30:1400/x", "http://u@192.0.2.30", "nonsense"] {
            let error = parse(json!([{ "id": "sonos", "url": url }])).unwrap_err();
            assert_eq!(error.field, "speakers[0].url", "{url}");
        }
        assert!(parse(json!([{ "id": "sonos", "url": "http://192.0.2.30:1400", "name": "x" }])).is_err());
        assert!(SonosConfig::parse(&json!({})).unwrap_err().message.contains("speakers"));
    }
}
