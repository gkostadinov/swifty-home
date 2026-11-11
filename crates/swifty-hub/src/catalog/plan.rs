//! What an action does, as one operation for each device it touches.
//!
//! The lights an action on every light reaches are the lights in the device index.
//! A house scene is a stored row: one level for every light, the lights with a
//! level of their own, and for Movie the TV's input and the speaker's volume cap.
//! A custom action is one [`Op::Custom`] for its one device.

use std::collections::BTreeMap;

use serde_json::Value;
use swifty_plugin_api::{CapKind, Capability, DeviceClass, Op, Scalar};

use super::{ActionDef, Invalid};
use crate::runtime::index::{DeviceIndex, IndexedDevice};
use crate::scenes::Scene;

/// One operation for one device. The registry finds what carries the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub target: String,
    pub op: Op,
}

/// A step for a target the action admitted, or for one a plan names itself. The
/// registry refuses a target that no instance carries.
fn step(target: &str, op: Op) -> Step {
    Step { target: target.to_string(), op }
}

/// What a plan needs to know besides the request.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    pub index: &'a DeviceIndex,
    /// The lights an action on every light reaches, in order (see [`every_light`]).
    /// Empty for any other action.
    pub lights: &'a [String],
    /// The stored scene the target names, for the scene actions.
    pub scene: Option<&'a Scene>,
}

/// Whether this action means every light there is, rather than one device.
pub fn reaches_every_light(def: &ActionDef) -> bool {
    matches!(def.id, "lights.power" | "lights.brightness" | "scene.apply" | "scene.movie")
}

/// Whether this action sets lights to the levels of a stored scene.
pub fn applies_a_scene(def: &ActionDef) -> bool {
    matches!(def.id, "scene.apply" | "scene.movie")
}

/// Every active light in the index, in the order an action on every light reaches
/// them: those the index has of its own first, in target order, then those in
/// `added` (the lights added after a discovery) in the order the store added them.
pub fn every_light(index: &DeviceIndex, added: &[String]) -> Vec<String> {
    let lights: Vec<&str> = index.of_class(DeviceClass::Light).map(|device| device.target.as_str()).collect();
    let mut every: Vec<String> = lights
        .iter()
        .filter(|light| !added.iter().any(|known| known == *light))
        .map(|light| light.to_string())
        .collect();
    every.extend(added.iter().filter(|known| lights.contains(&known.as_str())).cloned());
    every
}

/// Expands a group target to the devices it stands for: every light, or every VM.
pub fn expand_target(index: &DeviceIndex, target: &str) -> Vec<String> {
    let of = |class| index.of_class(class).map(|device| device.target.clone()).collect();
    match target {
        "all_bulbs" => of(DeviceClass::Light),
        "all_vms" => of(DeviceClass::Vm),
        other => vec![other.to_string()],
    }
}

/// The TV Movie turns on: the first one that can be switched on.
pub fn movie_tv(index: &DeviceIndex) -> Option<&IndexedDevice> {
    index.of_class(DeviceClass::Tv).find(|device| device.has(CapKind::Power))
}

/// The speaker whose volume Movie caps: the first one with a volume control.
pub fn movie_speaker(index: &DeviceIndex) -> Option<&IndexedDevice> {
    index.of_class(DeviceClass::Speaker).find(|device| device.has(CapKind::Volume))
}

/// The input a scene selects on the TV, if the scene names one the TV has.
pub fn movie_input<'a>(scene: &'a Scene, tv: &IndexedDevice) -> Option<&'a str> {
    let input = scene.extras.get("tv_input").and_then(Value::as_str)?;
    let offered = tv.capabilities.iter().any(|capability| match capability {
        Capability::Inputs { inputs } => inputs.iter().any(|known| known == input),
        _ => false,
    });
    offered.then_some(input)
}

/// The volume a scene caps the speaker at, if it gives one from 0 to 100.
pub fn volume_cap(scene: &Scene) -> Option<u8> {
    scene.extras.get("volume_cap").and_then(Value::as_u64).filter(|cap| *cap <= 100).map(|cap| cap as u8)
}

/// The level a light is given by an action on several lights or a scene: the one
/// asked for, raised to the lowest its plugin says it dims to. Such a light is
/// set as low as it goes, as it would set itself, instead of refusing the whole
/// action. 0 stays 0: off.
pub fn within_reach(device: &IndexedDevice, percent: u8) -> u8 {
    let floor = device
        .capabilities
        .iter()
        .find_map(|capability| match capability {
            Capability::Brightness { min } => Some(*min),
            _ => None,
        })
        .unwrap_or(0);
    if percent == 0 {
        0
    } else {
        percent.max(floor)
    }
}

/// The step that puts one light at a level: off at 0, otherwise as much of the
/// brightness and colour as the light can take.
fn set_light(index: &DeviceIndex, target: &str, (percent, hue): (u8, u16)) -> Option<Step> {
    let device = index.get(target)?;
    let percent = within_reach(device, percent);
    let op = if percent == 0 {
        device.has(CapKind::Power).then_some(Op::LightPower { on: false })?
    } else if device.has(CapKind::Brightness) && device.has(CapKind::Color) {
        Op::LightSet { percent, hue }
    } else if device.has(CapKind::Brightness) {
        Op::LightBrightness { percent }
    } else {
        device.has(CapKind::Power).then_some(Op::LightPower { on: true })?
    };
    Some(step(target, op))
}

/// Each light at the scene's level for it: its own, or the scene's.
fn scene_steps(scene: &Scene, context: &Context<'_>) -> Vec<Step> {
    context.lights.iter().filter_map(|light| set_light(context.index, light, scene.level_for(light))).collect()
}

/// What Movie does once the lights are set: the first TV on, at the scene's input,
/// and the first speaker's volume capped. Each only where there is one.
fn movie_tail(scene: &Scene, index: &DeviceIndex) -> Vec<Step> {
    let mut steps = Vec::new();
    if let Some(tv) = movie_tv(index) {
        steps.push(step(&tv.target, Op::TvPower { on: true }));
        if let Some(input) = movie_input(scene, tv) {
            steps.push(step(&tv.target, Op::TvInput { input: input.to_string() }));
        }
    }
    if let (Some(speaker), Some(max)) = (movie_speaker(index), volume_cap(scene)) {
        steps.push(step(&speaker.target, Op::VolumeCap { max }));
    }
    steps
}

/// Movie kept to one room: each of these lights takes the scene's own level, then
/// the TV and the speaker as everywhere.
pub fn movie_in_room(scene: &Scene, index: &DeviceIndex, lights: &[String]) -> Vec<Step> {
    let mut steps: Vec<Step> =
        lights.iter().filter_map(|light| set_light(index, light, (scene.percent, scene.hue))).collect();
    steps.extend(movie_tail(scene, index));
    steps
}

fn int(params: &Value, name: &str) -> Result<i64, Invalid> {
    params.get(name).and_then(Value::as_i64).ok_or_else(|| Invalid(format!("{name} is required")))
}

fn word(params: &Value, name: &str) -> Result<String, Invalid> {
    params.get(name).and_then(Value::as_str).map(str::to_string).ok_or_else(|| Invalid(format!("{name} is required")))
}

fn boolean(params: &Value, name: &str) -> Result<bool, Invalid> {
    params.get(name).and_then(Value::as_bool).ok_or_else(|| Invalid(format!("{name} is required")))
}

/// A custom action's checked parameters, as the plain values its operation carries.
fn custom_op(def: &ActionDef, params: &Value) -> Result<Op, Invalid> {
    let mut args = BTreeMap::new();
    for (name, value) in params.as_object().into_iter().flatten() {
        let scalar = match value {
            Value::Bool(flag) => Scalar::Bool(*flag),
            Value::Number(number) => {
                Scalar::Int(number.as_i64().ok_or_else(|| Invalid(format!("{name} is not whole")))?)
            }
            Value::String(text) => Scalar::Text(text.clone()),
            _ => return Err(Invalid(format!("{name} is not a plain value"))),
        };
        args.insert(name.clone(), scalar);
    }
    Ok(Op::Custom { action: def.id.to_string(), args })
}

/// Turns a validated action into the device operations that carry it out. The
/// values have already passed [`ActionDef::validate_params`].
pub fn plan(def: &ActionDef, target: &str, params: &Value, context: &Context<'_>) -> Result<Vec<Step>, Invalid> {
    let one = |op| Ok(vec![step(target, op)]);
    if def.is_custom() {
        return one(custom_op(def, params)?);
    }
    let lights_with = |cap: CapKind| {
        context.lights.iter().filter(move |light| context.index.get(light).is_some_and(|device| device.has(cap)))
    };
    match def.id {
        "light.power" => one(Op::LightPower { on: boolean(params, "on")? }),
        "light.brightness" => one(Op::LightBrightness { percent: int(params, "percent")? as u8 }),
        "light.hue" => one(Op::LightHue { hue: int(params, "hue")? as u16 }),
        "lights.power" => {
            let on = boolean(params, "on")?;
            Ok(lights_with(CapKind::Power).map(|light| step(light, Op::LightPower { on })).collect())
        }
        "lights.brightness" => {
            let percent = int(params, "percent")? as u8;
            let at = |light: &String| context.index.get(light).map_or(percent, |device| within_reach(device, percent));
            Ok(lights_with(CapKind::Brightness)
                .map(|light| step(light, Op::LightBrightness { percent: at(light) }))
                .collect())
        }
        "speaker.volume" => one(Op::Volume { level: int(params, "level")? as u8 }),
        "speaker.mute" => one(Op::Mute { muted: boolean(params, "muted")? }),
        "speaker.night" => one(Op::NightMode { on: boolean(params, "on")? }),
        "plug.power" => one(Op::PlugPower { on: boolean(params, "on")? }),
        "ac.power" => one(Op::AirconPower { on: boolean(params, "on")? }),
        "ac.mode" => one(Op::AirconMode { mode: word(params, "mode")? }),
        "ac.temperature" => one(Op::AirconTemp { celsius: int(params, "celsius")? as u8 }),
        "ac.fan" => one(Op::AirconFan { fan: word(params, "fan")? }),
        "speaker.play" => one(Op::Play),
        "speaker.pause" => one(Op::Pause),
        "tv.input" => one(Op::TvInput { input: word(params, "input")? }),
        "tv.power" => one(Op::TvPower { on: boolean(params, "on")? }),
        "tv.key" => one(Op::TvKey { key: word(params, "key")? }),
        "scene.apply" | "scene.movie" => {
            let scene = context
                .scene
                .filter(|scene| scene.id == target)
                .ok_or_else(|| Invalid(format!("no scene {target}")))?;
            let mut steps = scene_steps(scene, context);
            if def.id == "scene.movie" {
                steps.extend(movie_tail(scene, context.index));
            }
            if steps.is_empty() {
                return Err(Invalid("there is nothing here for this scene to set".to_string()));
            }
            Ok(steps)
        }
        "pets.feed" => one(Op::PetFeed { grams: int(params, "grams")? as u8 }),
        "pets.clean" => one(Op::LitterClean),
        "print.start" => one(Op::PrintStart),
        "print.pause" => one(Op::PrintPause),
        "print.resume" => one(Op::PrintResume),
        "print.estop" => one(Op::PrintEmergencyStop),
        "vm.start" => one(Op::VmStart),
        "vm.stop" => one(Op::VmShutdown),
        "vm.restart" => one(Op::VmReboot),
        "vm.force_stop" => one(Op::VmForceStop),
        "host.shutdown" => one(Op::HostShutdown),
        "host.reboot" => one(Op::HostReboot),
        "router.wireguard_restart" => one(Op::WireguardRestart),
        other => Err(Invalid(format!("{other} has no device plan"))),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::catalog::custom::tests::device;
    use crate::catalog::find;
    use crate::catalog::tests::{house, plan_in, scene, HUE_LIGHT, WIZ_LIGHT};
    use crate::runtime::index::legacy_seeded;
    use crate::scenes::house_scenes;

    fn targets(steps: &[Step]) -> Vec<&str> {
        steps.iter().map(|step| step.target.as_str()).collect()
    }

    fn movie(index: &DeviceIndex, scene: &Scene) -> Vec<Step> {
        let lights = every_light(index, &[]);
        plan(find("scene.movie").unwrap(), "movie", &json!({}), &Context { index, lights: &lights, scene: Some(scene) })
            .unwrap()
    }

    #[test]
    fn a_scene_is_planned_from_its_stored_level_and_overrides() {
        let index = house();
        let added = [WIZ_LIGHT.to_string(), HUE_LIGHT.to_string()];
        let evening = plan_in(&index, find("scene.apply").unwrap(), "evening", &json!({}), &added).unwrap();
        let ops: Vec<(&str, Op)> = evening.iter().map(|step| (step.target.as_str(), step.op.clone())).collect();
        assert_eq!(
            ops,
            [
                ("bulb1", Op::LightSet { percent: 40, hue: 34 }),
                ("bulb2", Op::LightSet { percent: 35, hue: 30 }),
                ("bulb3", Op::LightPower { on: false }),
                ("bulb4", Op::LightSet { percent: 25, hue: 14 }),
                (WIZ_LIGHT, Op::LightSet { percent: 40, hue: 34 }),
                (HUE_LIGHT, Op::LightSet { percent: 40, hue: 34 }),
            ]
        );

        // A new install's scene has one level for every light.
        let uniform = house_scenes().into_iter().find(|scene| scene.id == "evening").unwrap();
        let lights = every_light(&index, &added);
        let context = Context { index: &index, lights: &lights, scene: Some(&uniform) };
        let steps = plan(find("scene.apply").unwrap(), "evening", &json!({}), &context).unwrap();
        assert!(steps.iter().all(|step| step.op == Op::LightSet { percent: 40, hue: 34 }), "{steps:?}");

        // A scene that is not stored, or another one, is not planned.
        let none = Context { scene: None, ..context };
        assert!(plan(find("scene.apply").unwrap(), "evening", &json!({}), &none).is_err());
        assert!(plan(find("scene.apply").unwrap(), "focus", &json!({}), &context).is_err());
    }

    #[test]
    fn a_light_gets_as_much_of_a_scene_as_it_can_take() {
        let index = DeviceIndex::new([
            device("switch-only", "wiz", DeviceClass::Light, vec![Capability::Power]),
            device("dim-only", "wiz", DeviceClass::Light, vec![Capability::Power, Capability::Brightness { min: 1 }]),
            device("nothing", "wiz", DeviceClass::Light, Vec::new()),
        ]);
        let lights = every_light(&index, &[]);
        let plan_scene = |id: &str| {
            let scene = scene(id);
            let context = Context { index: &index, lights: &lights, scene: Some(&scene) };
            plan(find("scene.apply").unwrap(), id, &json!({}), &context).unwrap()
        };
        let focus = plan_scene("focus");
        assert_eq!(targets(&focus), ["dim-only", "switch-only"], "a light with no control at all is left alone");
        assert_eq!(focus[0].op, Op::LightBrightness { percent: 90 });
        assert_eq!(focus[1].op, Op::LightPower { on: true });
        let off = plan_scene("all_off");
        assert_eq!(targets(&off), ["dim-only", "switch-only"]);
        assert!(off.iter().all(|step| step.op == Op::LightPower { on: false }));
    }

    #[test]
    fn movie_turns_on_the_first_tv_and_caps_the_first_speaker_with_a_volume() {
        let index = legacy_seeded();
        let steps = movie(&index, &scene("movie"));
        assert_eq!(targets(&steps), ["bulb1", "bulb2", "bulb3", "bulb4", "tv", "tv", "sonos"]);
        // Bulb 2's own level is 8 %, and a WiZ bulb dims to 10 % at the lowest.
        assert_eq!(steps[1].op, Op::LightSet { percent: 10, hue: 14 });
        assert_eq!(steps[4].op, Op::TvPower { on: true });
        assert_eq!(steps[5].op, Op::TvInput { input: "HDMI 1".to_string() });
        assert_eq!(steps[6].op, Op::VolumeCap { max: 30 }, "the subwoofer has no volume of its own");

        // An input the TV does not have, or no cap, is left out. So is a TV or speaker that is not here.
        let mut other = scene("movie");
        other.extras = json!({ "tv_input": "HDMI 9" });
        assert_eq!(targets(&movie(&index, &other))[4..], ["tv"]);
        let lights_only = DeviceIndex::new(index.of_class(DeviceClass::Light).cloned());
        assert_eq!(targets(&movie(&lights_only, &scene("movie"))), ["bulb1", "bulb2", "bulb3", "bulb4"]);

        // Kept to a room, each light there takes the scene's own level.
        let in_room = movie_in_room(&scene("movie"), &index, &["bulb2".to_string()]);
        assert_eq!(in_room[0].op, Op::LightSet { percent: 10, hue: 14 });
        assert_eq!(targets(&in_room), ["bulb2", "tv", "tv", "sonos"]);
    }

    #[test]
    fn a_level_below_what_a_light_dims_to_is_raised_to_it() {
        let dims = |id: &str, min: u8| {
            let capabilities = vec![Capability::Power, Capability::Brightness { min }, Capability::Color];
            device(id, "wiz", DeviceClass::Light, capabilities)
        };
        let index = DeviceIndex::new([dims("deep", 1), dims("shallow", 25)]);
        assert_eq!(within_reach(index.get("deep").unwrap(), 8), 8);
        assert_eq!(within_reach(index.get("shallow").unwrap(), 8), 25);
        assert_eq!(within_reach(index.get("shallow").unwrap(), 60), 60);
        assert_eq!(within_reach(index.get("shallow").unwrap(), 0), 0, "off stays off");

        let lights = every_light(&index, &[]);
        let context = Context { index: &index, lights: &lights, scene: None };
        let steps = plan(find("lights.brightness").unwrap(), "all_bulbs", &json!({ "percent": 5 }), &context).unwrap();
        let ops: Vec<&Op> = steps.iter().map(|step| &step.op).collect();
        assert_eq!(ops, [&Op::LightBrightness { percent: 5 }, &Op::LightBrightness { percent: 25 }]);
        // Asked of one light, the level is the person's to choose and its plugin's to refuse.
        let one = plan(find("light.brightness").unwrap(), "shallow", &json!({ "percent": 5 }), &context).unwrap();
        assert_eq!(one[0].op, Op::LightBrightness { percent: 5 });
    }

    #[test]
    fn a_scene_with_nothing_to_set_is_refused() {
        let index = DeviceIndex::default();
        let context = Context { index: &index, lights: &[], scene: Some(&scene("evening")) };
        assert!(plan(find("scene.apply").unwrap(), "evening", &json!({}), &context).is_err());
        let movie = scene("movie");
        assert!(plan(find("scene.movie").unwrap(), "movie", &json!({}), &Context { scene: Some(&movie), ..context })
            .is_err());
    }

    #[test]
    fn a_group_stands_for_every_device_of_its_class() {
        let index = house();
        assert_eq!(expand_target(&index, "all_bulbs"), ["bulb1", "bulb2", "bulb3", "bulb4", HUE_LIGHT, WIZ_LIGHT]);
        assert_eq!(expand_target(&index, "all_vms"), ["ac-vm12345678", "vm100", "vm101", "vm102", "vm103"]);
        assert_eq!(expand_target(&index, "tv"), ["tv"]);
    }
}
