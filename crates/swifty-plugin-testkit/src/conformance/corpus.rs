//! Every operation the hub's catalog can send to a device, with the catalog's
//! ranges and their edges, for checks 4 and 5.
//!
//! The testkit does not link the hub, so the ranges and word lists below are a
//! copy of the hub's catalog (`crates/swifty-hub/src/catalog/`) and of the class
//! and capability table of the plugin contract. The test kit's
//! `tests/catalog_ranges.rs` reads the catalog's source and fails when they
//! differ, so a range changed there must be changed here too.

use std::collections::BTreeMap;

use swifty_plugin_api::{Capability, DeviceClass, DeviceDecl, Manifest, Op, ParamKind, Scalar};

/// `light.brightness` and `lights.brightness`: percent.
pub const PERCENT: (i64, i64) = (1, 100);
/// `light.hue`: degrees.
pub const HUE: (i64, i64) = (0, 359);
/// `speaker.volume`.
pub const VOLUME: (i64, i64) = (0, 100);
/// The cap `scene.movie` puts on the first speaker with a volume control.
pub const MOVIE_VOLUME_CAP: u8 = 30;
/// `ac.temperature`: degrees Celsius.
pub const CELSIUS: (i64, i64) = (17, 30);
/// `pets.feed`: grams.
pub const GRAMS: (i64, i64) = (10, 50);
/// `tv.input`.
pub const TV_INPUTS: &[&str] = &["HDMI 1", "HDMI 2", "HDMI 3", "HDMI 4", "Live TV"];
/// `tv.key`.
pub const TV_KEYS: &[&str] = &[
    "up",
    "down",
    "left",
    "right",
    "ok",
    "back",
    "home",
    "exit",
    "volume_up",
    "volume_down",
    "mute",
    "channel_up",
    "channel_down",
    "play",
    "pause",
];
/// `ac.mode`.
pub const AC_MODES: &[&str] = &["auto", "cool", "heat", "dry", "fan"];
/// `ac.fan`.
pub const AC_FANS: &[&str] = &["auto", "quiet", "low", "medium_low", "medium", "medium_high", "high"];

/// The ends of `range`, one step inside each, the middle, and each of `extra`
/// that is inside the range, in order and once each.
pub fn edges(range: (i64, i64), extra: &[i64]) -> Vec<i64> {
    let (min, max) = range;
    let mut values = vec![min, min.saturating_add(1), min + (max - min) / 2, max.saturating_sub(1), max];
    values.extend_from_slice(extra);
    values.retain(|value| (min..=max).contains(value));
    values.sort_unstable();
    values.dedup();
    values
}

fn power(on: fn(bool) -> Op) -> Vec<Op> {
    vec![on(true), on(false)]
}

/// Every operation the catalog can send to this device: the standard actions
/// its class and capabilities admit, and the custom actions of `manifest` that
/// it declares a `Custom` capability for.
pub fn ops_for(manifest: &Manifest, decl: &DeviceDecl) -> Vec<Op> {
    let mut ops = Vec::new();
    for capability in &decl.capabilities {
        ops.extend(standard(decl.class, capability));
    }
    for action in manifest.actions {
        let declared = decl
            .capabilities
            .iter()
            .any(|capability| matches!(capability, Capability::Custom { action: id } if id == action.id));
        if action.class == decl.class && declared {
            for args in argument_sets(action.params) {
                ops.push(Op::Custom { action: action.id.to_string(), args });
            }
        }
    }
    let mut unique: Vec<Op> = Vec::with_capacity(ops.len());
    for op in ops {
        if !unique.contains(&op) {
            unique.push(op);
        }
    }
    unique
}

/// What one capability of a device of `class` lets the catalog send.
fn standard(class: DeviceClass, capability: &Capability) -> Vec<Op> {
    use DeviceClass as C;
    let numbers = |range, extra: &[i64]| edges(range, extra);
    match (class, capability) {
        (C::Light, Capability::Power) => power(|on| Op::LightPower { on }),
        (C::Light, Capability::Brightness { min }) => {
            let min = i64::from(*min);
            let mut ops: Vec<Op> = numbers(PERCENT, &[min - 1, min, min + 1])
                .into_iter()
                .map(|percent| Op::LightBrightness { percent: percent as u8 })
                .collect();
            // Scenes set a level and a colour together, on every light.
            let mut levels = vec![PERCENT.0, min, PERCENT.1];
            levels.retain(|percent| (PERCENT.0..=PERCENT.1).contains(percent));
            levels.sort_unstable();
            levels.dedup();
            for percent in levels {
                for hue in [HUE.0, HUE.1] {
                    ops.push(Op::LightSet { percent: percent as u8, hue: hue as u16 });
                }
            }
            ops
        }
        (C::Light, Capability::Color) => {
            numbers(HUE, &[180]).into_iter().map(|hue| Op::LightHue { hue: hue as u16 }).collect()
        }
        (C::Plug, Capability::Power) => power(|on| Op::PlugPower { on }),
        (C::Aircon, Capability::Power) => power(|on| Op::AirconPower { on }),
        (C::Aircon, Capability::Mode { .. }) => {
            AC_MODES.iter().map(|mode| Op::AirconMode { mode: mode.to_string() }).collect()
        }
        (C::Aircon, Capability::Temperature { min, max }) => {
            let (min, max) = (i64::from(*min), i64::from(*max));
            numbers(CELSIUS, &[min - 1, min, max, max + 1])
                .into_iter()
                .map(|celsius| Op::AirconTemp { celsius: celsius as u8 })
                .collect()
        }
        (C::Aircon, Capability::Fan { .. }) => {
            AC_FANS.iter().map(|fan| Op::AirconFan { fan: fan.to_string() }).collect()
        }
        (C::Speaker, Capability::Volume) => {
            let mut ops: Vec<Op> =
                numbers(VOLUME, &[]).into_iter().map(|level| Op::Volume { level: level as u8 }).collect();
            ops.push(Op::VolumeCap { max: MOVIE_VOLUME_CAP });
            ops
        }
        (C::Speaker, Capability::Mute) => power(|muted| Op::Mute { muted }),
        (C::Speaker, Capability::NightMode) => power(|on| Op::NightMode { on }),
        (C::Speaker, Capability::Transport) => vec![Op::Play, Op::Pause],
        (C::Tv, Capability::Power) => power(|on| Op::TvPower { on }),
        (C::Tv, Capability::Inputs { .. }) => {
            TV_INPUTS.iter().map(|input| Op::TvInput { input: input.to_string() }).collect()
        }
        (C::Tv, Capability::Keys { .. }) => TV_KEYS.iter().map(|key| Op::TvKey { key: key.to_string() }).collect(),
        (C::Feeder, Capability::Dispense { grams }) => {
            let declared: Vec<i64> = grams.iter().map(|g| i64::from(*g)).collect();
            numbers(GRAMS, &declared).into_iter().map(|grams| Op::PetFeed { grams: grams as u8 }).collect()
        }
        (C::Litter, Capability::Clean) => vec![Op::LitterClean],
        (C::Printer, Capability::PrintJob) => vec![Op::PrintStart, Op::PrintPause, Op::PrintResume],
        (C::Printer, Capability::EmergencyStop) => vec![Op::PrintEmergencyStop],
        (C::Vm, Capability::Lifecycle) => vec![Op::VmStart, Op::VmShutdown, Op::VmReboot, Op::VmForceStop],
        (C::Host, Capability::NodePower) => vec![Op::HostShutdown, Op::HostReboot],
        (C::Router, Capability::Tunnel) => vec![Op::WireguardRestart],
        _ => Vec::new(),
    }
}

/// The values the catalog would let through for one parameter: both booleans,
/// the edges of a range, every option, the shortest and longest label.
fn values(kind: &ParamKind) -> Vec<Scalar> {
    match kind {
        ParamKind::Bool => vec![Scalar::Bool(true), Scalar::Bool(false)],
        ParamKind::Int { min, max } => edges((*min, *max), &[]).into_iter().map(Scalar::Int).collect(),
        ParamKind::Enum(options) => options.iter().map(|option| Scalar::Text(option.to_string())).collect(),
        ParamKind::Label { max_len } => {
            let mut labels = vec![Scalar::Text("a".to_string())];
            if *max_len > 1 {
                labels.push(Scalar::Text("a".repeat(*max_len)));
            }
            labels
        }
    }
}

/// Argument sets for a custom action: every parameter at its first value, then
/// each other value of each parameter in turn, then the required ones alone.
pub fn argument_sets(params: &[swifty_plugin_api::ParamDef]) -> Vec<BTreeMap<String, Scalar>> {
    let choices: Vec<(&str, bool, Vec<Scalar>)> =
        params.iter().map(|param| (param.name, param.required, values(&param.kind))).collect();
    if choices.iter().any(|(_, required, values)| *required && values.is_empty()) {
        return Vec::new();
    }
    let base: BTreeMap<String, Scalar> = choices
        .iter()
        .filter_map(|(name, _, values)| values.first().map(|value| (name.to_string(), value.clone())))
        .collect();
    let mut sets = vec![base.clone()];
    for (name, _, values) in &choices {
        for value in values.iter().skip(1) {
            let mut set = base.clone();
            set.insert(name.to_string(), value.clone());
            sets.push(set);
        }
    }
    let required: BTreeMap<String, Scalar> = base
        .iter()
        .filter(|(name, _)| choices.iter().any(|(n, r, _)| n == name && *r))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if required.len() != base.len() {
        sets.push(required);
    }
    sets
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use swifty_plugin_api::{CustomAction, Expect, ParamDef, Reach, Tier, API_VERSION};

    fn decl(class: DeviceClass, capabilities: Vec<Capability>) -> DeviceDecl {
        DeviceDecl { local_id: "d".into(), target_hint: None, class, name: "D".into(), capabilities, attrs: json!({}) }
    }

    static ACTIONS: [CustomAction; 1] = [CustomAction {
        id: "acme.blink",
        label: "Blink",
        description: "Blinks.",
        consequences: &[],
        class: DeviceClass::Light,
        tier: Tier::Comfort,
        params: &[
            ParamDef { name: "times", kind: ParamKind::Int { min: 1, max: 3 }, required: true, help: "h" },
            ParamDef { name: "fast", kind: ParamKind::Bool, required: false, help: "h" },
        ],
        idempotent: false,
        confirm_timeout_ms: 1_000,
        expect: Expect::Unreadable,
    }];

    static MANIFEST: Manifest = Manifest {
        id: "acme",
        name: "Acme",
        version: "0.1.0",
        api: API_VERSION,
        summary: "s",
        reach: Reach::Local,
        network: &[],
        classes: &[DeviceClass::Light],
        config_version: 1,
        config: &[],
        secrets: &[],
        actions: &ACTIONS,
        vocabulary: &[],
        nouns: &[],
        multi_instance: false,
    };

    #[test]
    fn edges_cover_the_ends_and_stay_inside() {
        assert_eq!(edges((1, 100), &[9, 10, 11, 0, 101]), vec![1, 2, 9, 10, 11, 50, 99, 100]);
        assert_eq!(edges((0, 1), &[]), vec![0, 1]);
    }

    #[test]
    fn a_light_gets_the_catalog_ranges_and_its_own_floor() {
        let ops =
            ops_for(&MANIFEST, &decl(DeviceClass::Light, vec![Capability::Power, Capability::Brightness { min: 10 }]));
        assert!(ops.contains(&Op::LightPower { on: true }) && ops.contains(&Op::LightPower { on: false }));
        for percent in [1, 9, 10, 11, 100] {
            assert!(ops.contains(&Op::LightBrightness { percent }), "{percent}");
        }
        assert!(ops.contains(&Op::LightSet { percent: 10, hue: 359 }));
        assert!(!ops.iter().any(|op| matches!(op, Op::LightHue { .. })), "no Color, no light.hue");
        // The custom action needs its capability.
        assert!(!ops.iter().any(|op| matches!(op, Op::Custom { .. })));
        let ops = ops_for(
            &MANIFEST,
            &decl(DeviceClass::Light, vec![Capability::Color, Capability::Custom { action: "acme.blink".into() }]),
        );
        assert!(ops.contains(&Op::LightHue { hue: 359 }));
        let customs: Vec<_> = ops.iter().filter(|op| matches!(op, Op::Custom { .. })).collect();
        // Base, times 2 and 3, fast false, and times alone.
        assert_eq!(customs.len(), 5, "{customs:?}");
    }

    #[test]
    fn a_class_without_the_capability_gets_nothing() {
        assert!(ops_for(&MANIFEST, &decl(DeviceClass::Plug, vec![Capability::Brightness { min: 1 }])).is_empty());
        let tv = ops_for(&MANIFEST, &decl(DeviceClass::Tv, vec![Capability::Power, Capability::Keys { keys: vec![] }]));
        assert_eq!(tv.len(), 2 + TV_KEYS.len());
    }
}
