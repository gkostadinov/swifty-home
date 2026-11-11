//! Devices as a plugin describes them, and as the hub stores them.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Capability, DeviceClass};

/// A device as a plugin describes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DeviceDecl {
    /// Stable within the instance. `[a-z0-9][a-z0-9_-]{0,63}`. It is what
    /// `Driver::command` and `read_state` receive.
    pub local_id: String,
    /// The public id the plugin would like. The hub decides.
    pub target_hint: Option<String>,
    /// What kind of thing it is.
    pub class: DeviceClass,
    /// A name for it, until someone gives it another.
    pub name: String,
    /// What it can do.
    pub capabilities: Vec<Capability>,
    /// What the plugin needs to find the device again: address, MAC, VM id.
    /// Stored by the hub when the device is adopted. Never a secret.
    pub attrs: Value,
}

/// A device the hub has stored for an instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceRecord {
    /// The public id the hub gave it. It never changes.
    pub target: String,
    /// The id the plugin knows it by.
    pub local_id: String,
    /// What kind of thing it is.
    pub class: DeviceClass,
    /// Its name.
    pub name: String,
    /// What the plugin stored to find it again.
    pub attrs: Value,
}
