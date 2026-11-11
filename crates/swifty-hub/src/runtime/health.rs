//! How the instances are doing, for the plugin page: what each one's last health
//! poll said, and why one cannot be used.

use serde::Serialize;
use swifty_plugin_api::{Health, Readiness};

use super::Registry;

/// One instance, as the plugin page shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InstanceStatus {
    pub id: String,
    pub plugin: String,
    pub enabled: bool,
    pub readiness: Readiness,
    /// Why its devices cannot be used, when they cannot.
    pub unavailable: Option<String>,
    /// What the last health poll said. Unknown until one ran, and in dry-run.
    pub health: Health,
}

/// Every loaded instance and how it is doing. Opens nothing.
pub fn statuses(registry: &Registry) -> Vec<InstanceStatus> {
    registry
        .instances()
        .iter()
        .map(|instance| InstanceStatus {
            id: instance.spec.id.clone(),
            plugin: instance.spec.plugin.clone(),
            enabled: instance.spec.enabled,
            readiness: instance.readiness(),
            unavailable: instance.unavailable(registry.is_live()),
            health: instance.last_health(),
        })
        .collect()
}
