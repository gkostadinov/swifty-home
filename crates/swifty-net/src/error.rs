//! What can go wrong when a device is asked to do something or read.

use serde::{Deserialize, Serialize};

/// Why a device did not do what it was asked, or could not be read.
///
/// Every helper in this crate returns it, and so does every driver. Its text is
/// fixed, or is device-supplied text made safe, and never comes from the HTTP
/// client, whose messages can include a URL.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceError {
    /// No reply within the connect or read timeout.
    #[error("no reply from the device")]
    Timeout,
    /// The device could not be reached at all.
    #[error("device unreachable: {0}")]
    Unreachable(String),
    /// The device answered and refused.
    #[error("device refused: {0}")]
    Rejected(String),
    /// The driver is switched off or cannot do this.
    #[error("{0}")]
    Disabled(String),
}

impl DeviceError {
    /// True when the device may simply not have heard us.
    pub fn is_silence(&self) -> bool {
        matches!(self, DeviceError::Timeout | DeviceError::Unreachable(_))
    }

    /// The answer for something this device or driver cannot do at all.
    pub fn unsupported() -> Self {
        Self::Disabled("This device cannot do that.".into())
    }
}
