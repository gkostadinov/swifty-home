//! The plugin traits: [`Plugin`] for what is pure, [`Driver`] for what is live.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::examples::text;
use crate::{
    net, ConfigProblem, ConsoleGrant, DeviceDecl, DeviceError, DeviceRecord, Examples, ExtraHost, HistoryRange,
    Manifest, Op, PluginError, Secrets, SeenMachine, SetupFlow, State,
};

/// One integration. Implemented by a unit struct and exported as
/// `pub static PLUGIN: MyPlugin = MyPlugin;`
pub trait Plugin: Send + Sync + 'static {
    /// What the plugin says about itself.
    fn manifest(&self) -> &'static Manifest;

    /// Checks a config document: shape, ranges, addresses. Pure.
    fn validate(&self, config: &Value) -> Result<(), ConfigProblem>;

    /// Brings a config stored by an older version of this plugin up to
    /// `manifest().config_version`. Pure.
    fn migrate(&self, _from_version: u32, config: Value) -> Result<Value, ConfigProblem> {
        Ok(config)
    }

    /// Whether this instance can work at all, from its config and from which
    /// secret slots hold a value. Pure.
    fn readiness(&self, instance: &InstanceView<'_>) -> Readiness;

    /// Every device this instance offers right now: the ones its config names
    /// and one for each adopted record it recognises. Pure.
    fn devices(&self, instance: &InstanceView<'_>) -> Vec<DeviceDecl>;

    /// Why `op` will not be done on `device`, if it will not. Pure, so dry-run
    /// gives the same answer as live. `device` is the device's local id.
    fn refusal(&self, instance: &InstanceView<'_>, device: &str, op: &Op) -> Option<String>;

    /// Builds the driver. Called only by a hub that is live, at most once per
    /// instance load. Reads nothing but `ctx`.
    fn connect(&self, ctx: LiveContext) -> Result<Arc<dyn Driver>, PluginError>;

    /// The guided setup for a new instance, or for setting an existing one up again.
    fn setup(&self) -> Option<Box<dyn SetupFlow>> {
        None
    }
}

/// What the pure calls may look at.
pub struct InstanceView<'a> {
    /// The instance's id.
    pub id: &'a str,
    /// The instance's config.
    pub config: &'a Value,
    /// The secret slots that hold a value. Never the values.
    pub secrets_present: &'a BTreeSet<String>,
    /// Devices adopted for this instance and still active.
    pub adopted: &'a [DeviceRecord],
}

/// Whether an instance can work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "status", content = "reason", rename_all = "snake_case")]
pub enum Readiness {
    /// It can.
    Ready,
    /// Configured but not usable yet: not paired, a credential is missing.
    /// Its devices are listed, disabled, with this reason.
    NeedsSetup(String),
    /// Cannot work with this config. Same listing.
    Disabled(String),
}

/// What a live driver is built from.
pub struct LiveContext {
    /// The instance's id.
    pub instance_id: String,
    /// The instance's config.
    pub config: Value,
    /// The instance's credentials.
    pub secrets: Secrets,
    /// Proof that the hub is live, for the socket helpers.
    pub grant: net::LiveGrant,
    /// How long to wait to connect and for each reply.
    pub timeouts: net::Timeouts,
    /// The adopted devices, kept current by the hub. A driver refuses anything
    /// that is not in it, on its own, as adapters do today.
    pub directory: Arc<dyn Directory>,
}

/// The adopted devices of one instance, as the hub keeps them.
pub trait Directory: Send + Sync {
    /// The adopted device with this local id.
    fn get(&self, local_id: &str) -> Option<DeviceRecord>;
    /// Every adopted device.
    fn all(&self) -> Vec<DeviceRecord>;
}

/// The device accepted a command. This is not success: that needs a read-back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandAck;

/// A live connection to one instance's devices. Implementations must set
/// connect and read timeouts on every socket and must never retry a command:
/// the engine owns retries, because only it knows whether an action is idempotent.
#[async_trait]
pub trait Driver: Send + Sync + 'static {
    /// Sends one validated operation, once. `Ok` means the device took it.
    async fn command(&self, device: &str, op: &Op) -> Result<CommandAck, DeviceError>;

    /// Reads the device's real current state.
    async fn read_state(&self, device: &str) -> Result<State, DeviceError>;

    /// Why a background poll must leave the devices alone for now (rate limits).
    fn poll_refusal(&self) -> Option<String> {
        None
    }

    /// Looks for devices that could be adopted. Nothing is adopted here.
    async fn discover(&self, _window: Duration) -> Result<Vec<DeviceDecl>, DeviceError> {
        Err(DeviceError::unsupported())
    }

    /// How the connection to the devices is doing.
    async fn health(&self) -> Health {
        Health::unknown()
    }

    /// Extensions. A plugin returns `Some` for the ones it implements.
    fn machines(&self) -> Option<&dyn MachineHost> {
        None
    }

    /// The agent pairing extension, if this driver has it.
    fn agents(&self) -> Option<&dyn AgentPairing> {
        None
    }
}

/// Proxmox-like hosts: VM lists, stored charts, consoles, further endpoints.
#[async_trait]
pub trait MachineHost: Send + Sync {
    /// The VMs the hosts report.
    async fn list_machines(&self) -> Result<Vec<SeenMachine>, DeviceError>;
    /// Stored chart samples for one host or VM.
    async fn history(&self, device: &str, range: HistoryRange) -> Result<Vec<Value>, DeviceError>;
    /// Asks for a console on one VM.
    async fn open_console(&self, device: &str) -> Result<ConsoleGrant, DeviceError>;
    /// Opens the socket for a console that was granted.
    async fn connect_console(&self, grant: &ConsoleGrant) -> Result<net::ConsoleLink, DeviceError>;
    /// Adds a further host. Reads local files and sends nothing.
    fn add_host(&self, spec: &ExtraHost) -> Result<(), String>;
    /// Stops using a further host. Sends nothing.
    fn remove_host(&self, id: &str);
}

/// Glimdock-like collectors that pair their own agents.
#[async_trait]
pub trait AgentPairing: Send + Sync {
    /// Starts pairing an agent with this name, and hands back what the collector said.
    async fn pair_agent(&self, name: &str) -> Result<Value, DeviceError>;
}

/// How an instance's connection to its devices is doing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Health {
    /// The verdict.
    pub status: HealthStatus,
    /// One sentence on why, when there is one.
    pub detail: Option<String>,
}

impl Health {
    /// Nothing is known yet.
    pub fn unknown() -> Self {
        Self { status: HealthStatus::Unknown, detail: None }
    }
}

/// The verdict of a health check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    /// Everything answers.
    Ok,
    /// Some of it answers.
    Degraded,
    /// Nothing answers.
    Down,
    /// Not known yet.
    Unknown,
}

impl Examples for Readiness {
    fn examples() -> Vec<Self> {
        vec![
            Readiness::Ready,
            Readiness::NeedsSetup(text("The hub is not paired yet. Run the setup and press its button.")),
            Readiness::Disabled(text("The configuration is not valid: address: must be an IP address.")),
        ]
    }
}

impl Examples for Health {
    fn examples() -> Vec<Self> {
        vec![
            Health { status: HealthStatus::Ok, detail: None },
            Health { status: HealthStatus::Degraded, detail: Some(text("2 of 3 blinds answer.")) },
            Health { status: HealthStatus::Down, detail: Some(text("The hub refused the access token.")) },
            Health::unknown(),
        ]
    }
}
