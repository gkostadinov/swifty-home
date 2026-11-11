//! What the console shows before an action runs, and the warnings that go with it.
//!
//! The words come from the action, the device's name in the index, the owner's
//! notes on the device (such as what goes offline while it is down) and, for a
//! scene, the stored scene. Nothing here knows one particular house.

use serde_json::Value;
use swifty_plugin_api::DeviceClass;

use super::plan::{movie_input, movie_speaker, movie_tv, volume_cap};
use super::ActionDef;
use crate::runtime::index::DeviceIndex;
use crate::scenes::Scene;

/// What the console shows before an action runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Description {
    pub label: String,
    pub target_name: String,
    pub what: String,
    pub consequences: Vec<String>,
    pub affected: Vec<String>,
    pub warning: Option<String>,
}

/// What is known about the target, besides the action itself.
#[derive(Debug, Clone, Copy)]
pub struct Facts<'a> {
    /// Where device names come from.
    pub index: &'a DeviceIndex,
    /// The device this hub runs on, if it is one of its own.
    pub runs_on: Option<&'a str>,
    /// The owner's notes on the target.
    pub notes: &'a [String],
    /// The stored scene, when the target is one.
    pub scene: Option<&'a Scene>,
}

/// True when this action on this target takes the console itself offline: it
/// stops or restarts the VM this hub runs on.
pub fn takes_console_offline(def: &ActionDef, target: &str, runs_on: Option<&str>) -> bool {
    matches!(def.id, "vm.stop" | "vm.restart" | "vm.force_stop") && runs_on == Some(target)
}

pub const NODE_REBOOT_WARNING: &str = "Everything on the host goes offline while it reboots: every VM on it, and this console if it runs there. That takes a few minutes.";
pub const NODE_SHUTDOWN_WARNING: &str = "Everything on the host goes offline: every VM on it, and this console if it runs there. A shutdown cannot be undone from this page: the host has to be powered on by hand.";

/// What the person must be told about this action, every time it is shown.
pub fn node_power_warning(action: &str) -> Option<&'static str> {
    match action {
        "host.reboot" => Some(NODE_REBOOT_WARNING),
        "host.shutdown" => Some(NODE_SHUTDOWN_WARNING),
        _ => None,
    }
}

pub fn console_offline_warning(def: &ActionDef) -> String {
    let how = if def.id == "vm.restart" {
        "goes offline until the VM is back"
    } else {
        "goes offline and you will need its host to start it again"
    };
    format!("This console runs on this VM. The console {how}.")
}

/// Whether this hub runs on the host `target` or on a VM carried with it.
fn runs_on_host(facts: &Facts<'_>, target: &str) -> bool {
    let Some(runs_on) = facts.runs_on else {
        return false;
    };
    let host = facts.index.get(target);
    runs_on == target
        || facts.index.get(runs_on).is_some_and(|device| {
            device.class == DeviceClass::Vm && host.is_some_and(|host| host.instance == device.instance)
        })
}

/// "a", "a and b", "a, b and c".
fn listed(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// What Movie does in this house: the TV and the speaker by name, with the scene's
/// input and volume cap.
fn movie_what(scene: &Scene, index: &DeviceIndex) -> String {
    let mut parts = vec!["Dims the lights in the TV's room".to_string()];
    if let Some(tv) = movie_tv(index) {
        parts.push(match movie_input(scene, tv) {
            Some(input) => format!("turns {} on at {input}", tv.name),
            None => format!("turns {} on", tv.name),
        });
    }
    if let (Some(speaker), Some(cap)) = (movie_speaker(index), volume_cap(scene)) {
        parts.push(format!("caps the volume of {} at {cap}", speaker.name));
    }
    format!(
        "{}. That is the room holding the TV, or else the one called Living room. With neither, it dims every light.",
        listed(&parts)
    )
}

pub fn describe(def: &ActionDef, target: &str, params: &Value, facts: &Facts<'_>) -> Description {
    let name = facts.index.get(target).map_or(target, |device| device.name.as_str()).to_string();
    let mut description = Description {
        label: def.label.to_string(),
        target_name: name.clone(),
        what: def.description.to_string(),
        consequences: Vec::new(),
        affected: Vec::new(),
        warning: None,
    };
    // What this action does to this device, before the owner's notes on it.
    let mut own: Vec<String> = Vec::new();
    match def.id {
        "vm.start" | "vm.stop" | "vm.restart" | "vm.force_stop" => {
            let (verb, line) = match def.id {
                "vm.start" => ("Start", format!("{name} starts.")),
                "vm.stop" => ("Stop", format!("{name} shuts down.")),
                "vm.restart" => ("Restart", format!("{name} restarts.")),
                _ => ("Force-stop", format!("{name} stops at once.")),
            };
            description.label = format!("{verb} {name}");
            own.push(line);
            description.affected.push(name.clone());
        }
        "host.shutdown" | "host.reboot" => {
            description.affected = vec![name.clone(), "every VM on it".to_string()];
            if runs_on_host(facts, target) {
                description.affected.push("this console".to_string());
            }
            description.warning = node_power_warning(def.id).map(str::to_string);
        }
        "router.wireguard_restart" => {
            description.affected = vec!["every WireGuard session".to_string(), "this page, if remote".to_string()];
        }
        "pets.feed" => {
            let grams = params.get("grams").and_then(Value::as_i64).unwrap_or(0);
            description.label = format!("Feed {grams} g");
            description.what = format!("{name} dispenses {grams} g now.");
        }
        "passkey.revoke" => description.target_name = "Passkey".to_string(),
        "passkey.register" => {
            if let Some(name) = params.get("name").and_then(Value::as_str) {
                description.target_name = name.to_string();
            }
        }
        "scene.apply" | "scene.movie" => {
            if let Some(scene) = facts.scene.filter(|scene| scene.id == target) {
                description.target_name = scene.name.clone();
                if def.id == "scene.movie" {
                    description.what = movie_what(scene, facts.index);
                }
            }
        }
        _ => {}
    }
    description.consequences = own;
    description.consequences.extend(facts.notes.iter().cloned());
    description.consequences.extend(def.consequences.iter().map(|line| line.to_string()));
    if takes_console_offline(def, target, facts.runs_on) {
        description.warning = Some(console_offline_warning(def));
    }
    description
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use swifty_plugin_api::Capability;

    use super::*;
    use crate::catalog::custom::tests::{device, names, register};
    use crate::catalog::find;
    use crate::catalog::tests::{house, scene};
    use crate::runtime::index::legacy_seeded;

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|line| line.to_string()).collect()
    }

    #[test]
    fn a_note_on_a_device_appears_where_an_action_on_it_is_confirmed() {
        let index = house();
        let notes = lines(&["The shared folders go offline while this is down.", "Backups run at night."]);
        let facts = Facts { index: &index, runs_on: None, notes: &notes, scene: None };
        let text = describe(find("vm.stop").unwrap(), "vm101", &json!({}), &facts);
        assert_eq!(text.label, "Stop VM 101");
        assert_eq!(text.target_name, "VM 101");
        assert_eq!(text.consequences[0], "VM 101 shuts down.");
        assert_eq!(text.consequences[1..], notes[..]);
        assert_eq!(text.affected, ["VM 101"]);
        assert_eq!(text.warning, None, "this hub runs on none of its devices here");

        let force = describe(find("vm.force_stop").unwrap(), "vm101", &json!({}), &facts);
        assert_eq!(
            force.consequences.last().unwrap(),
            "Anything not yet written to disk is lost.",
            "the action's own come last"
        );
        let tv = describe(find("tv.power").unwrap(), "tv", &json!({ "on": false }), &facts);
        assert_eq!(tv.target_name, "TV");
        assert!(tv.consequences.contains(&notes[0]), "notes are by target, whatever the device is");
        let none = Facts { notes: &[], ..facts };
        assert_eq!(describe(find("vm.stop").unwrap(), "vm101", &json!({}), &none).consequences, ["VM 101 shuts down."]);
    }

    #[test]
    fn a_renamed_device_is_described_by_its_new_name() {
        let renamed = DeviceIndex::new(legacy_seeded().all().cloned().map(|mut device| {
            if device.target == "vm100" {
                device.name = "Build box".to_string();
            }
            if device.target == "feeder" {
                device.name = "Cat feeder".to_string();
            }
            device
        }));
        let facts = Facts { index: &renamed, runs_on: None, notes: &[], scene: None };
        let start = describe(find("vm.start").unwrap(), "vm100", &json!({}), &facts);
        assert_eq!((start.label.as_str(), start.consequences[0].as_str()), ("Start Build box", "Build box starts."));
        let feed = describe(find("pets.feed").unwrap(), "feeder", &json!({ "grams": 20 }), &facts);
        assert_eq!((feed.label.as_str(), feed.what.as_str()), ("Feed 20 g", "Cat feeder dispenses 20 g now."));
    }

    #[test]
    fn the_host_says_what_goes_with_it_and_whether_this_console_does() {
        let index = house();
        let notes = lines(&["Every VM on it stops while it is down."]);
        let on_a_vm = Facts { index: &index, runs_on: Some("vm102"), notes: &notes, scene: None };
        let text = describe(find("host.shutdown").unwrap(), "host", &json!({}), &on_a_vm);
        assert_eq!(text.target_name, "Host");
        assert_eq!(text.affected, ["Host", "every VM on it", "this console"]);
        assert_eq!(text.consequences[0], notes[0]);
        assert_eq!(text.warning.as_deref(), Some(NODE_SHUTDOWN_WARNING));
        let elsewhere = Facts { runs_on: None, ..on_a_vm };
        let text = describe(find("host.reboot").unwrap(), "host", &json!({}), &elsewhere);
        assert_eq!(text.affected, ["Host", "every VM on it"]);
        assert_eq!(text.warning.as_deref(), Some(NODE_REBOOT_WARNING));
        let on_the_host = Facts { runs_on: Some("host"), ..on_a_vm };
        assert!(describe(find("host.reboot").unwrap(), "host", &json!({}), &on_the_host)
            .affected
            .contains(&"this console".to_string()));
    }

    #[test]
    fn movie_names_the_tv_the_input_the_speaker_and_the_cap() {
        let index = legacy_seeded();
        let movie = scene("movie");
        let facts = Facts { index: &index, runs_on: None, notes: &[], scene: Some(&movie) };
        let text = describe(find("scene.movie").unwrap(), "movie", &json!({}), &facts);
        assert_eq!(text.target_name, "Movie");
        assert!(
            text.what.starts_with(
                "Dims the lights in the TV's room, turns TV on at HDMI 1 and caps the volume of Sound bar at 30."
            ),
            "{}",
            text.what
        );
        let lights_only = DeviceIndex::new(index.of_class(DeviceClass::Light).cloned());
        let facts = Facts { index: &lights_only, ..facts };
        let text = describe(find("scene.movie").unwrap(), "movie", &json!({}), &facts);
        assert!(text.what.starts_with("Dims the lights in the TV's room. That is"), "{}", text.what);
        let evening = scene("evening");
        let text =
            describe(find("scene.apply").unwrap(), "evening", &json!({}), &Facts { scene: Some(&evening), ..facts });
        assert_eq!(text.target_name, "Evening");
    }

    #[test]
    fn a_custom_action_is_described_from_its_manifest_and_the_notes() {
        register();
        let index = DeviceIndex::new([device("gadget", "floors", DeviceClass::Light, names("floors.ok"))]);
        let notes = lines(&["It hums."]);
        let facts = Facts { index: &index, runs_on: None, notes: &notes, scene: None };
        let text = describe(find("floors.ok").unwrap(), "gadget", &json!({}), &facts);
        assert_eq!((text.label.as_str(), text.what.as_str()), ("Test action", "Does one test thing."));
        assert_eq!(text.target_name, "The gadget");
        assert_eq!(text.consequences, ["It hums.", "Nothing that matters."]);
        assert!(matches!(index.get("gadget").unwrap().capabilities[0], Capability::Custom { .. }));
    }
}
