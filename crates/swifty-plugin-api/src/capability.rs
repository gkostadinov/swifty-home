//! The capability model: what kind of thing a device is, and what it can do.

use serde::{Deserialize, Serialize};

use crate::examples::text;
use crate::Examples;

/// What kind of thing a device is. It picks the card in the app, the default
/// audience, the nouns voice understands and which catalog actions can name it.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum DeviceClass {
    /// A light that can be switched, dimmed and perhaps coloured.
    Light,
    /// A bridge that other devices are reached through, such as a Hue Bridge.
    Bridge,
    /// A smart socket.
    Plug,
    /// An air conditioner.
    Aircon,
    /// A speaker or sound bar.
    Speaker,
    /// A television.
    Tv,
    /// A pet feeder.
    Feeder,
    /// A litter box that cleans itself.
    Litter,
    /// A 3D printer.
    Printer,
    /// A virtual machine.
    Vm,
    /// A host that runs virtual machines.
    Host,
    /// A router, and the tunnel it runs.
    Router,
    /// A camera.
    Camera,
    /// A service on the network that is only checked for reachability.
    Service,
    /// A source of readings, such as a node's sensors.
    Telemetry,
    /// Anything the hub has no class for.
    Other,
}

/// Who may see a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Audience {
    /// Everyone in the house, the owner included.
    Family,
    /// The owner only.
    Owner,
}

impl DeviceClass {
    /// Who may see a device of this class. Lights, music, TV, pets and the
    /// camera are for the family. Everything else starts owner-only.
    pub fn default_audience(self) -> Audience {
        use DeviceClass::*;
        match self {
            Light | Bridge | Speaker | Tv | Feeder | Litter | Camera => Audience::Family,
            _ => Audience::Owner,
        }
    }

    /// Whether any standard action on this class is critical. A device of such
    /// a class always starts pending and needs a passkey step-up to approve.
    pub fn needs_approval(self) -> bool {
        matches!(self, DeviceClass::Vm | DeviceClass::Host | DeviceClass::Router)
    }
}

/// One thing a particular device can do. The lists narrow the catalog's own
/// ranges for this device and tell the app which controls to draw.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "cap", rename_all = "snake_case")]
pub enum Capability {
    /// Light, Plug, Aircon or Tv power.
    Power,
    /// Dimming. WiZ: 10.
    Brightness {
        /// The lowest level the device goes down to, in percent.
        min: u8,
    },
    /// Colour, as a hue angle 0..=359.
    Color,
    /// Speaker volume.
    Volume,
    /// Speaker mute.
    Mute,
    /// Speaker night mode.
    NightMode,
    /// Speaker play and pause.
    Transport,
    /// TV inputs.
    Inputs {
        /// The inputs this TV has.
        inputs: Vec<String>,
    },
    /// Remote keys.
    Keys {
        /// The keys this device accepts.
        keys: Vec<String>,
    },
    /// Aircon modes.
    Mode {
        /// The modes this unit accepts.
        modes: Vec<String>,
    },
    /// Target temperature, Celsius.
    Temperature {
        /// The lowest target.
        min: u8,
        /// The highest target.
        max: u8,
    },
    /// Fan speeds.
    Fan {
        /// The speeds this unit accepts.
        speeds: Vec<String>,
    },
    /// Food out of a feeder.
    Dispense {
        /// The portions the feeder accepts, in grams.
        grams: Vec<u8>,
    },
    /// A litter box cleaning cycle.
    Clean,
    /// Starting, pausing and resuming a print.
    PrintJob,
    /// Stopping a printer at once.
    EmergencyStop,
    /// VM start, shutdown, reboot, force stop.
    Lifecycle,
    /// Host shutdown, reboot.
    NodePower,
    /// VPN tunnel restart.
    Tunnel,
    /// A telemetry snapshot.
    Metrics,
    /// Stored charts ([`MachineHost::history`](crate::MachineHost::history)).
    History,
    /// VNC console ([`MachineHost`](crate::MachineHost)).
    Console,
    /// The [`AgentPairing`](crate::AgentPairing) extension.
    AgentPairing,
    /// A plugin-declared action id.
    Custom {
        /// The action's id, `<plugin id>.<verb>`.
        action: String,
    },
}

/// A capability without its details, for "does this device have X".
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum CapKind {
    /// [`Capability::Power`].
    Power,
    /// [`Capability::Brightness`].
    Brightness,
    /// [`Capability::Color`].
    Color,
    /// [`Capability::Volume`].
    Volume,
    /// [`Capability::Mute`].
    Mute,
    /// [`Capability::NightMode`].
    NightMode,
    /// [`Capability::Transport`].
    Transport,
    /// [`Capability::Inputs`].
    Inputs,
    /// [`Capability::Keys`].
    Keys,
    /// [`Capability::Mode`].
    Mode,
    /// [`Capability::Temperature`].
    Temperature,
    /// [`Capability::Fan`].
    Fan,
    /// [`Capability::Dispense`].
    Dispense,
    /// [`Capability::Clean`].
    Clean,
    /// [`Capability::PrintJob`].
    PrintJob,
    /// [`Capability::EmergencyStop`].
    EmergencyStop,
    /// [`Capability::Lifecycle`].
    Lifecycle,
    /// [`Capability::NodePower`].
    NodePower,
    /// [`Capability::Tunnel`].
    Tunnel,
    /// [`Capability::Metrics`].
    Metrics,
    /// [`Capability::History`].
    History,
    /// [`Capability::Console`].
    Console,
    /// [`Capability::AgentPairing`].
    AgentPairing,
    /// [`Capability::Custom`].
    Custom,
}

impl Examples for DeviceClass {
    fn examples() -> Vec<Self> {
        use DeviceClass::*;
        vec![
            Light, Bridge, Plug, Aircon, Speaker, Tv, Feeder, Litter, Printer, Vm, Host, Router, Camera, Service,
            Telemetry, Other,
        ]
    }
}

impl Examples for Capability {
    fn examples() -> Vec<Self> {
        let words = |list: &[&str]| list.iter().map(|word| text(word)).collect::<Vec<_>>();
        vec![
            Capability::Power,
            Capability::Brightness { min: 10 },
            Capability::Color,
            Capability::Volume,
            Capability::Mute,
            Capability::NightMode,
            Capability::Transport,
            Capability::Inputs { inputs: words(&["HDMI 1", "HDMI 2", "Live TV"]) },
            Capability::Keys { keys: words(&["up", "down", "ok", "back"]) },
            Capability::Mode { modes: words(&["auto", "cool", "heat"]) },
            Capability::Temperature { min: 17, max: 30 },
            Capability::Fan { speeds: words(&["auto", "low", "high"]) },
            Capability::Dispense { grams: vec![10, 20, 30] },
            Capability::Clean,
            Capability::PrintJob,
            Capability::EmergencyStop,
            Capability::Lifecycle,
            Capability::NodePower,
            Capability::Tunnel,
            Capability::Metrics,
            Capability::History,
            Capability::Console,
            Capability::AgentPairing,
            Capability::Custom { action: text("acme-blinds.calibrate") },
        ]
    }
}

impl Capability {
    /// This capability without its details.
    pub fn kind(&self) -> CapKind {
        match self {
            Capability::Power => CapKind::Power,
            Capability::Brightness { .. } => CapKind::Brightness,
            Capability::Color => CapKind::Color,
            Capability::Volume => CapKind::Volume,
            Capability::Mute => CapKind::Mute,
            Capability::NightMode => CapKind::NightMode,
            Capability::Transport => CapKind::Transport,
            Capability::Inputs { .. } => CapKind::Inputs,
            Capability::Keys { .. } => CapKind::Keys,
            Capability::Mode { .. } => CapKind::Mode,
            Capability::Temperature { .. } => CapKind::Temperature,
            Capability::Fan { .. } => CapKind::Fan,
            Capability::Dispense { .. } => CapKind::Dispense,
            Capability::Clean => CapKind::Clean,
            Capability::PrintJob => CapKind::PrintJob,
            Capability::EmergencyStop => CapKind::EmergencyStop,
            Capability::Lifecycle => CapKind::Lifecycle,
            Capability::NodePower => CapKind::NodePower,
            Capability::Tunnel => CapKind::Tunnel,
            Capability::Metrics => CapKind::Metrics,
            Capability::History => CapKind::History,
            Capability::Console => CapKind::Console,
            Capability::AgentPairing => CapKind::AgentPairing,
            Capability::Custom { .. } => CapKind::Custom,
        }
    }
}
