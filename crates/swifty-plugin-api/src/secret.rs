//! Credentials, kept out of logs and out of anything that serialises.

use std::collections::BTreeMap;
use std::fmt;

use crate::PluginError;

/// A credential. `Debug` and `Display` print `"<redacted>"`. It is not `Serialize`.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    /// Wraps a value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value, for the one place that sends it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// The secrets of one instance, by slot.
#[derive(Clone, Default)]
pub struct Secrets(BTreeMap<String, Secret>);

impl Secrets {
    /// The secret in this slot, if it holds one.
    pub fn get(&self, slot: &str) -> Option<&Secret> {
        self.0.get(slot)
    }

    /// The secret in this slot, or an error that names the slot and nothing else.
    pub fn require(&self, slot: &str) -> Result<&Secret, PluginError> {
        self.get(slot).ok_or_else(|| PluginError(format!("The secret {slot} is not set.")))
    }

    /// Secrets from slot and value pairs. For tests.
    pub fn of<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self(pairs.into_iter().map(|(slot, value)| (slot.into(), Secret(value.into()))).collect())
    }
}

/// Lists the slots, never the values.
impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.0.iter()).finish()
    }
}
