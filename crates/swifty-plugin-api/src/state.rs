//! What a device reports about itself.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::examples::text;
use crate::{Examples, Scalar};

/// What a device reports. The JSON is what API v1 returns under `state`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum State {
    /// `color` is false in white or scene mode, where `hue` is only a tint.
    Light {
        /// Whether it is on.
        on: bool,
        /// Brightness in percent.
        bri: u8,
        /// Colour as a hue angle in degrees.
        hue: u16,
        /// Whether the light is showing a colour, so `hue` can confirm a colour change.
        color: bool,
    },
    /// A bridge that other devices are reached through.
    Bridge {
        /// Whether it answered.
        reachable: bool,
    },
    /// A smart socket.
    Plug {
        /// Whether it is on.
        on: bool,
    },
    /// An air conditioner. Each of the others is absent when the unit gives no
    /// usable value for it.
    Aircon {
        /// Whether it is on.
        on: bool,
        /// The mode it is in.
        mode: Option<String>,
        /// The target temperature, in degrees Celsius.
        target: Option<i16>,
        /// The fan speed.
        fan: Option<String>,
        /// The temperature indoors, in degrees Celsius.
        indoor: Option<i16>,
        /// The temperature outdoors, in degrees Celsius.
        outdoor: Option<i16>,
    },
    /// A speaker. `night` is absent when the speaker has no night mode or
    /// would not say.
    Speaker {
        /// Volume from 0 to 100.
        vol: u8,
        /// Whether it is muted.
        mute: bool,
        /// Whether it is playing.
        playing: bool,
        /// What it is playing from.
        source: String,
        /// Whether night mode is on.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        night: Option<bool>,
    },
    /// A television.
    Tv {
        /// Whether it is on.
        on: bool,
        /// The input it shows.
        input: String,
    },
    /// A pet feeder.
    Feeder {
        /// When it last dispensed, as the device says it.
        last: Option<String>,
        /// How many portions it has dispensed. It only grows.
        portions: u32,
    },
    /// A litter box that cleans itself.
    Litter {
        /// When it last cleaned, as the device says it.
        last: Option<String>,
        /// Whether a cycle is running.
        busy: bool,
        /// How many cycles it has run. It only grows.
        cycles: u32,
    },
    /// A 3D printer.
    Printer {
        /// What it is doing: "printing", "paused", "stopped", and so on.
        state: String,
        /// Nozzle temperature, in degrees Celsius.
        nozzle: f64,
        /// Bed temperature, in degrees Celsius.
        bed: f64,
        /// The layer it is on.
        layer: u32,
        /// How many layers the print has.
        layers: u32,
        /// How far the print is, in percent.
        pct: f64,
    },
    /// A virtual machine.
    Vm {
        /// What it is doing: "running", "stopped", and so on.
        state: String,
        /// Seconds since it started.
        uptime_s: u64,
    },
    /// A host that runs virtual machines.
    Host {
        /// Whether it answered.
        up: bool,
        /// Seconds since it started.
        uptime_s: u64,
    },
    /// A router.
    Router {
        /// Whether its VPN tunnel is up.
        wireguard_up: bool,
        /// Seconds since it started.
        uptime_s: u64,
    },
    /// A camera.
    Camera {
        /// Whether it is streaming.
        streaming: bool,
    },
    /// A service on the network that is only checked for reachability.
    Service {
        /// Whether it answered.
        reachable: bool,
    },
    /// Readings, as the source sent them.
    Telemetry {
        /// The readings.
        data: serde_json::Value,
    },
    /// For a class the hub has no shape for. Custom actions confirm against it.
    Custom {
        /// The fields the device reports, by name.
        fields: BTreeMap<String, Scalar>,
    },
}

impl Examples for State {
    fn examples() -> Vec<Self> {
        vec![
            State::Light { on: true, bri: 40, hue: 210, color: true },
            State::Bridge { reachable: true },
            State::Plug { on: false },
            State::Aircon {
                on: true,
                mode: Some(text("cool")),
                target: Some(22),
                fan: Some(text("auto")),
                indoor: Some(24),
                outdoor: None,
            },
            State::Speaker { vol: 25, mute: false, playing: true, source: text("tv"), night: Some(false) },
            State::Tv { on: true, input: text("HDMI 1") },
            State::Feeder { last: Some(text("2026-01-01 08:00")), portions: 12 },
            State::Litter { last: None, busy: false, cycles: 7 },
            State::Printer { state: text("printing"), nozzle: 210.5, bed: 60.0, layer: 12, layers: 240, pct: 5.0 },
            State::Vm { state: text("running"), uptime_s: 3600 },
            State::Host { up: true, uptime_s: 86_400 },
            State::Router { wireguard_up: true, uptime_s: 1200 },
            State::Camera { streaming: true },
            State::Service { reachable: true },
            State::Telemetry { data: serde_json::json!({ "cpu_temp": 48.5, "memory": 0.42 }) },
            State::Custom {
                fields: [(text("position"), Scalar::Int(40)), (text("calibrated"), Scalar::Bool(true))].into(),
            },
        ]
    }
}
