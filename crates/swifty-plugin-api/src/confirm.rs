//! How the hub tells that an operation really took effect.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::{Examples, Op, Scalar, State};

/// Lights that do not go below this brightness. A lower request is sent as this,
/// and reading it back counts as the request having worked.
pub const MIN_LIGHT_DIMMING: u8 = 10;

fn brightness_matches(reported: u8, requested: u8) -> bool {
    reported == requested || (requested < MIN_LIGHT_DIMMING && reported == MIN_LIGHT_DIMMING)
}

impl Op {
    /// Whether this sets one light.
    pub fn is_light_op(&self) -> bool {
        matches!(self, Op::LightPower { .. } | Op::LightBrightness { .. } | Op::LightHue { .. } | Op::LightSet { .. })
    }

    /// Whether this sets something on an air conditioner.
    pub fn is_aircon_op(&self) -> bool {
        matches!(self, Op::AirconPower { .. } | Op::AirconMode { .. } | Op::AirconTemp { .. } | Op::AirconFan { .. })
    }

    /// Whether nothing can show this worked. A key press leaves no state to read back.
    pub fn cannot_be_read_back(&self) -> bool {
        matches!(self, Op::TvKey { .. })
    }

    /// Whether confirming this operation needs the state from before it ran.
    pub fn needs_baseline(&self) -> bool {
        matches!(self, Op::PetFeed { .. } | Op::LitterClean | Op::VmReboot | Op::HostReboot | Op::WireguardRestart)
    }

    /// Whether the device is expected to stop answering once it has this. Whatever
    /// it says on the way down is not a refusal of the command it already took.
    pub fn takes_the_device_away(&self) -> bool {
        matches!(self, Op::HostShutdown | Op::HostReboot)
    }

    /// Whether `state` shows this operation has really taken effect. The body
    /// is the read-back rule the adapters had before this crate, unchanged.
    /// `Custom` is always false here: the engine confirms it with the action's
    /// `Expect`.
    pub fn is_confirmed_by(&self, state: &State, baseline: Option<&State>) -> bool {
        use State as S;
        match (self, state) {
            (Op::LightPower { on }, S::Light { on: now, .. }) => now == on,
            // Zero means off to a Hue light. WiZ sends it as its lowest level.
            (Op::LightBrightness { percent }, S::Light { on, bri, .. }) => {
                (*on && brightness_matches(*bri, *percent)) || (*percent == 0 && !*on)
            }
            (Op::LightHue { hue }, S::Light { on, hue: now, color, .. }) => *on && *color && now == hue,
            (Op::LightSet { percent, hue }, S::Light { on, bri, hue: now, color }) => {
                (*on && *color && brightness_matches(*bri, *percent) && now == hue) || (*percent == 0 && !*on)
            }
            (Op::PlugPower { on }, S::Plug { on: now }) => now == on,
            (Op::AirconPower { on }, S::Aircon { on: now, .. }) => now == on,
            (Op::AirconMode { mode }, S::Aircon { mode: now, .. }) => now.as_ref() == Some(mode),
            (Op::AirconTemp { celsius }, S::Aircon { target, .. }) => *target == Some(i16::from(*celsius)),
            (Op::AirconFan { fan }, S::Aircon { fan: now, .. }) => now.as_ref() == Some(fan),
            (Op::Volume { level }, S::Speaker { vol, .. }) => vol == level,
            (Op::VolumeCap { max }, S::Speaker { vol, .. }) => vol <= max,
            (Op::Mute { muted }, S::Speaker { mute, .. }) => mute == muted,
            (Op::NightMode { on }, S::Speaker { night, .. }) => *night == Some(*on),
            (Op::Play, S::Speaker { playing, .. }) => *playing,
            (Op::Pause, S::Speaker { playing, .. }) => !*playing,
            (Op::TvPower { on }, S::Tv { on: now, .. }) => now == on,
            (Op::TvInput { input }, S::Tv { on, input: now }) => *on && now == input,
            (Op::PetFeed { .. }, S::Feeder { portions, .. }) => match baseline {
                Some(S::Feeder { portions: before, .. }) => portions > before,
                _ => false,
            },
            (Op::LitterClean, S::Litter { busy, cycles, .. }) => match baseline {
                Some(S::Litter { cycles: before, .. }) => *busy || cycles > before,
                _ => false,
            },
            (Op::PrintStart | Op::PrintResume, S::Printer { state, .. }) => state == "printing",
            (Op::PrintPause, S::Printer { state, .. }) => state == "paused",
            (Op::PrintEmergencyStop, S::Printer { state, .. }) => state == "stopped",
            (Op::VmStart, S::Vm { state, .. }) => state == "running",
            (Op::VmShutdown | Op::VmForceStop, S::Vm { state, .. }) => state == "stopped",
            (Op::VmReboot, S::Vm { state, uptime_s }) => match baseline {
                Some(S::Vm { uptime_s: before, .. }) => state == "running" && uptime_s < before,
                _ => false,
            },
            (Op::HostShutdown, S::Host { up, .. }) => !*up,
            (Op::HostReboot, S::Host { up, uptime_s }) => match baseline {
                Some(S::Host { uptime_s: before, .. }) => *up && uptime_s < before,
                _ => false,
            },
            (Op::WireguardRestart, S::Router { wireguard_up, uptime_s }) => match baseline {
                Some(S::Router { uptime_s: before, .. }) => *wireguard_up && uptime_s < before,
                _ => false,
            },
            _ => false,
        }
    }
}

/// How the hub confirms a custom action from `State::Custom`. Part of a
/// manifest, so it is `static` data.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "expect", rename_all = "snake_case")]
pub enum Expect {
    /// Nothing can show it worked. The run ends "sent", like a key press.
    Unreadable,
    /// `fields[field]` equals the argument named `arg`.
    FieldEqualsArg {
        /// The field of [`State::Custom`] to look at.
        field: &'static str,
        /// The argument of [`Op::Custom`] it must equal.
        arg: &'static str,
    },
    /// `fields[field]` is this boolean.
    FieldIs {
        /// The field of [`State::Custom`] to look at.
        field: &'static str,
        /// The value it must have.
        value: bool,
    },
    /// `fields[field]` is an integer greater than before the command. Needs a baseline.
    CounterIncreased {
        /// The field of [`State::Custom`] to look at.
        field: &'static str,
    },
}

impl Expect {
    /// Whether confirming needs the state from before the command ran.
    pub fn needs_baseline(&self) -> bool {
        matches!(self, Expect::CounterIncreased { .. })
    }

    /// Whether `state` shows a custom action with these arguments has really
    /// taken effect. Only a [`State::Custom`] can show it, and
    /// [`Expect::Unreadable`] never does.
    pub fn is_met(&self, args: &BTreeMap<String, Scalar>, state: &State, baseline: Option<&State>) -> bool {
        let field_of = |state: Option<&State>, field: &str| match state {
            Some(State::Custom { fields }) => fields.get(field).cloned(),
            _ => None,
        };
        match *self {
            Expect::Unreadable => false,
            Expect::FieldEqualsArg { field, arg } => match (field_of(Some(state), field), args.get(arg)) {
                (Some(now), Some(wanted)) => now == *wanted,
                _ => false,
            },
            Expect::FieldIs { field, value } => field_of(Some(state), field) == Some(Scalar::Bool(value)),
            Expect::CounterIncreased { field } => match (field_of(Some(state), field), field_of(baseline, field)) {
                (Some(Scalar::Int(now)), Some(Scalar::Int(before))) => now > before,
                _ => false,
            },
        }
    }
}

impl Examples for Expect {
    fn examples() -> Vec<Self> {
        vec![
            Expect::Unreadable,
            Expect::FieldEqualsArg { field: "position", arg: "position" },
            Expect::FieldIs { field: "calibrated", value: true },
            Expect::CounterIncreased { field: "cycles" },
        ]
    }
}
