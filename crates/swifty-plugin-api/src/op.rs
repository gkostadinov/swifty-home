//! The operations a driver can be asked to perform.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::examples::text;
use crate::Examples;

/// A plain value in a custom action's arguments or a custom state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(untagged)]
pub enum Scalar {
    /// `true` or `false`.
    Bool(bool),
    /// A whole number.
    Int(i64),
    /// Text.
    Text(String),
}

/// Every state change a driver can be asked to make. The values were
/// range-checked by the catalog. There is no variant carrying a raw command,
/// URL, path or payload, and there never will be.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Turns one light on or off.
    LightPower {
        /// On or off.
        on: bool,
    },
    /// Sets one light's brightness and turns it on. Zero turns it off where
    /// the light allows it.
    LightBrightness {
        /// Brightness in percent.
        percent: u8,
    },
    /// Sets one light's colour and turns it on.
    LightHue {
        /// Colour as a hue angle in degrees, 0..=359.
        hue: u16,
    },
    /// Sets one light's brightness and colour at once. Zero brightness turns it off.
    LightSet {
        /// Brightness in percent.
        percent: u8,
        /// Colour as a hue angle in degrees, 0..=359.
        hue: u16,
    },
    /// Switches a smart socket.
    PlugPower {
        /// On or off.
        on: bool,
    },
    /// Switches an air conditioner.
    AirconPower {
        /// On or off.
        on: bool,
    },
    /// Sets an air conditioner's mode.
    AirconMode {
        /// One of the modes the unit accepts.
        mode: String,
    },
    /// Sets an air conditioner's target temperature.
    AirconTemp {
        /// The target, in degrees Celsius.
        celsius: u8,
    },
    /// Sets an air conditioner's fan speed.
    AirconFan {
        /// One of the speeds the unit accepts.
        fan: String,
    },
    /// Sets a speaker's volume.
    Volume {
        /// Volume from 0 to 100.
        level: u8,
    },
    /// Lowers a speaker's volume to at most this, and leaves a lower one alone.
    VolumeCap {
        /// The highest volume to keep.
        max: u8,
    },
    /// Mutes or unmutes a speaker.
    Mute {
        /// Muted or not.
        muted: bool,
    },
    /// Switches a speaker's night mode.
    NightMode {
        /// On or off.
        on: bool,
    },
    /// Resumes playback.
    Play,
    /// Pauses playback.
    Pause,
    /// Switches a TV.
    TvPower {
        /// On or off.
        on: bool,
    },
    /// Switches a TV to another input.
    TvInput {
        /// One of the TV's inputs.
        input: String,
    },
    /// Presses one key on a TV's remote.
    TvKey {
        /// One of the keys the TV accepts.
        key: String,
    },
    /// Dispenses food from a feeder.
    PetFeed {
        /// One of the portions the feeder accepts, in grams.
        grams: u8,
    },
    /// Starts a litter box cleaning cycle.
    LitterClean,
    /// Starts the print that is loaded.
    PrintStart,
    /// Pauses the current print.
    PrintPause,
    /// Resumes a paused print.
    PrintResume,
    /// Stops the printer at once.
    PrintEmergencyStop,
    /// Starts a VM.
    VmStart,
    /// Shuts a VM down cleanly.
    VmShutdown,
    /// Reboots a VM.
    VmReboot,
    /// Stops a VM at once, as pulling the plug would.
    VmForceStop,
    /// Shuts a host down.
    HostShutdown,
    /// Reboots a host.
    HostReboot,
    /// Restarts a router's VPN tunnel.
    WireguardRestart,
    /// An action the plugin declared in its manifest. `args` passed the
    /// declared parameter schema.
    Custom {
        /// The action's id, `<plugin id>.<verb>`.
        action: String,
        /// The arguments, by parameter name.
        args: BTreeMap<String, Scalar>,
    },
}

impl Examples for Op {
    fn examples() -> Vec<Self> {
        vec![
            Op::LightPower { on: true },
            Op::LightBrightness { percent: 40 },
            Op::LightHue { hue: 210 },
            Op::LightSet { percent: 60, hue: 30 },
            Op::PlugPower { on: true },
            Op::AirconPower { on: true },
            Op::AirconMode { mode: text("cool") },
            Op::AirconTemp { celsius: 22 },
            Op::AirconFan { fan: text("auto") },
            Op::Volume { level: 25 },
            Op::VolumeCap { max: 30 },
            Op::Mute { muted: true },
            Op::NightMode { on: true },
            Op::Play,
            Op::Pause,
            Op::TvPower { on: true },
            Op::TvInput { input: text("HDMI 1") },
            Op::TvKey { key: text("ok") },
            Op::PetFeed { grams: 10 },
            Op::LitterClean,
            Op::PrintStart,
            Op::PrintPause,
            Op::PrintResume,
            Op::PrintEmergencyStop,
            Op::VmStart,
            Op::VmShutdown,
            Op::VmReboot,
            Op::VmForceStop,
            Op::HostShutdown,
            Op::HostReboot,
            Op::WireguardRestart,
            Op::Custom {
                action: text("acme-blinds.calibrate"),
                args: [(text("speed"), Scalar::Int(2)), (text("quiet"), Scalar::Bool(true))].into(),
            },
        ]
    }
}
