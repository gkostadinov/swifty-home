//! Errors that cross the boundary.

use serde::Serialize;

use crate::examples::text;
use crate::Examples;

/// Why a device did not do what it was asked, or could not be read. It is
/// defined next to the socket helpers, which return it too, and is the same type.
pub use swifty_net::DeviceError;

/// Why an instance could not be built. Shown on the plugin page. Fixed text, never a secret.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[error("{0}")]
pub struct PluginError(pub String);

/// What is wrong with a config document: the field and a sentence.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq, Serialize)]
#[error("{field}: {message}")]
pub struct ConfigProblem {
    /// The key of the field, or a dotted path to it.
    pub field: String,
    /// What is wrong with it, as one sentence.
    pub message: String,
}

impl Examples for DeviceError {
    fn examples() -> Vec<Self> {
        vec![
            DeviceError::Timeout,
            DeviceError::Unreachable(text("the connection failed")),
            DeviceError::Rejected(text("the device sent an unreadable reply")),
            DeviceError::Disabled(text("This device cannot do that.")),
        ]
    }
}

impl Examples for PluginError {
    fn examples() -> Vec<Self> {
        vec![PluginError(text("The secret token is not set."))]
    }
}

impl Examples for ConfigProblem {
    fn examples() -> Vec<Self> {
        vec![ConfigProblem { field: text("devices[0].address"), message: text("must be an IP address") }]
    }
}
