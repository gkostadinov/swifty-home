//! The test house and the demo home, as the hub loads them.

mod common;

use common::{connect, connect_house, with_view};
use serde_json::json;
use swifty_plugin_api::{Audience, CapKind, Capability, DeviceClass, Op, Plugin, Readiness};
use swifty_plugin_mock::{
    demo_home, legacy_house, legacy_house_with_vms, House, MockDevices, MockPlugin, Origin, Status, LEGACY_VMS, PLUGIN,
};

const INSTANCES: [&str; 13] = [
    "wiz",
    "hue",
    "sonos",
    "webos",
    "moonraker",
    "plex",
    "camera",
    "petkit",
    "proxmox",
    "collector",
    "router",
    "tuya",
    "toshiba",
];

/// Every device of today's house: target, instance, class and who may see it.
fn expected() -> Vec<(String, &'static str, DeviceClass, Audience)> {
    use Audience::*;
    use DeviceClass::*;
    let mut devices: Vec<(String, &str, DeviceClass, Audience)> =
        (1..=4).map(|n| (format!("bulb{n}"), "wiz", Light, Family)).collect();
    for (target, instance, class, audience) in [
        ("hue", "hue", Bridge, Family),
        ("sonos", "sonos", Speaker, Family),
        ("sub", "sonos", Speaker, Family),
        ("tv", "webos", Tv, Family),
        ("printer", "moonraker", Printer, Owner),
        ("plex", "plex", Service, Owner),
        ("camera", "camera", Camera, Family),
        ("feeder", "petkit", Feeder, Family),
        ("litter", "petkit", Litter, Family),
        ("host", "proxmox", Host, Owner),
        ("collector", "collector", Telemetry, Owner),
        ("router", "router", Router, Owner),
    ] {
        devices.push((target.to_string(), instance, class, audience));
    }
    devices.extend(LEGACY_VMS.iter().map(|(vmid, _)| (format!("vm{vmid}"), "proxmox", Vm, Owner)));
    devices
}

/// Each instance's devices as the plugin declares them: (instance, target hint, class).
fn declared(house: &House) -> Vec<(String, String, DeviceClass)> {
    let mut all = Vec::new();
    for instance in &house.instances {
        let adopted = house.adopted(&instance.id);
        for decl in with_view(&instance.id, &instance.config, &[], &adopted, |view| PLUGIN.devices(view)) {
            all.push((instance.id.clone(), decl.target_hint.unwrap(), decl.class));
        }
    }
    all
}

#[test]
fn the_legacy_house_has_one_ready_mock_instance_per_old_kind() {
    let house = legacy_house();
    let ids: Vec<&str> = house.instances.iter().map(|instance| instance.id.as_str()).collect();
    assert_eq!(ids, INSTANCES);
    for instance in &house.instances {
        assert_eq!(instance.plugin, "mock");
        assert_eq!(instance.config_version, 1);
        assert!(instance.enabled);
        PLUGIN.validate(&instance.config).unwrap();
        let readiness = with_view(&instance.id, &instance.config, &[], &[], |view| PLUGIN.readiness(view));
        assert_eq!(readiness, Readiness::Ready, "{}", instance.id);
    }
}

#[test]
fn the_legacy_house_has_todays_twenty_devices_with_their_targets_classes_and_audiences() {
    let house = legacy_house();
    let expected = expected();
    assert_eq!(expected.len(), 20);
    assert_eq!(house.devices.len(), 20);
    let mut declared = declared(&house);
    declared.sort();
    let mut stored: Vec<(String, String, DeviceClass)> =
        house.devices.iter().map(|device| (device.instance.clone(), device.target.clone(), device.class)).collect();
    stored.sort();
    assert_eq!(declared, stored, "the plugin declares exactly the stored devices");
    for (target, instance, class, audience) in expected {
        let device = house.device(&target).unwrap_or_else(|| panic!("{target} is missing"));
        assert_eq!((device.instance.as_str(), device.class), (instance, class), "{target}");
        assert_eq!(device.class.default_audience(), audience, "{target}");
        assert_eq!(device.local_id, target, "local ids are today's targets");
        assert_eq!(device.status, Status::Active);
        let origin = if class == DeviceClass::Vm { Origin::Adopted } else { Origin::Config };
        assert_eq!(device.origin, origin, "{target}");
    }
}

#[test]
fn the_legacy_house_keeps_todays_capabilities() {
    let house = legacy_house();
    let caps = |target: &str| -> Vec<CapKind> {
        let device = house.device(target).unwrap();
        let adopted = house.adopted(&device.instance);
        let spec = house.instance(&device.instance).unwrap();
        let decls = with_view(&spec.id, &spec.config, &[], &adopted, |view| PLUGIN.devices(view));
        decls
            .into_iter()
            .find(|decl| decl.local_id == target)
            .unwrap()
            .capabilities
            .iter()
            .map(Capability::kind)
            .collect()
    };
    use CapKind::*;
    assert_eq!(caps("bulb1"), [Power, Brightness, Color]);
    assert_eq!(caps("sonos"), [Volume, Mute, NightMode, Transport]);
    assert!(caps("sub").is_empty(), "the subwoofer is read-only");
    assert!(caps("hue").is_empty() && caps("camera").is_empty() && caps("plex").is_empty());
    assert_eq!(caps("tv"), [Power, Inputs, Keys]);
    assert_eq!(caps("feeder"), [Dispense]);
    assert_eq!(caps("litter"), [Clean]);
    assert_eq!(caps("printer"), [PrintJob, EmergencyStop]);
    assert_eq!(caps("host"), [NodePower, History]);
    assert_eq!(caps("vm200"), [Lifecycle, Console, History]);
    assert_eq!(caps("router"), [Tunnel]);
    assert_eq!(caps("collector"), [Metrics, AgentPairing]);
}

#[test]
fn the_vms_are_generic_and_can_be_replaced() {
    let house = legacy_house();
    let vms: Vec<(String, String, serde_json::Value)> = house
        .devices
        .iter()
        .filter(|device| device.class == DeviceClass::Vm)
        .map(|device| (device.target.clone(), device.name.clone(), device.attrs.clone()))
        .collect();
    assert_eq!(
        vms,
        [
            ("vm200".to_string(), "alpha".to_string(), json!({ "host": "primary", "vmid": 200 })),
            ("vm201".to_string(), "bravo".to_string(), json!({ "host": "primary", "vmid": 201 })),
            ("vm202".to_string(), "charlie".to_string(), json!({ "host": "primary", "vmid": 202 })),
            ("vm203".to_string(), "delta".to_string(), json!({ "host": "primary", "vmid": 203 })),
        ]
    );

    let other = legacy_house_with_vms(&[(150, "web"), (151, "files")]);
    let targets: Vec<&str> =
        other.devices.iter().filter(|d| d.class == DeviceClass::Vm).map(|d| d.target.as_str()).collect();
    assert_eq!(targets, ["vm150", "vm151"]);
    assert_eq!(other.notes.iter().map(|note| note.target.as_str()).collect::<Vec<_>>(), ["host", "vm150"]);
    assert_eq!(other.devices.len(), 18);
}

/// Today's scene table: (bulb, level, hue), where level 0 means off.
const SCENE_EVENING: [(u8, u8, u16); 4] = [(1, 40, 34), (2, 35, 30), (3, 0, 0), (4, 25, 14)];
const SCENE_FOCUS: [(u8, u8, u16); 4] = [(1, 90, 52), (2, 90, 52), (3, 90, 52), (4, 70, 52)];
const SCENE_MOVIE: [(u8, u8, u16); 4] = [(1, 10, 14), (2, 8, 14), (3, 0, 0), (4, 0, 0)];
const SCENE_ALL_OFF: [(u8, u8, u16); 4] = [(1, 0, 0), (2, 0, 0), (3, 0, 0), (4, 0, 0)];

#[test]
fn the_scenes_reproduce_todays_per_bulb_levels() {
    let house = legacy_house();
    let ids: Vec<&str> = house.scenes.iter().map(|scene| scene.id.as_str()).collect();
    assert_eq!(ids, ["evening", "focus", "all_off", "movie"]);
    for (id, table) in
        [("evening", SCENE_EVENING), ("focus", SCENE_FOCUS), ("all_off", SCENE_ALL_OFF), ("movie", SCENE_MOVIE)]
    {
        let scene = house.scenes.iter().find(|scene| scene.id == id).unwrap();
        // A light without an override takes the scene's own level, as bulb1 does today.
        assert_eq!((scene.percent, scene.hue), (table[0].1, table[0].2), "{id}");
        for (bulb, percent, hue) in table {
            let target = format!("bulb{bulb}");
            let level = scene
                .overrides
                .iter()
                .find(|o| o.target == target)
                .map_or((scene.percent, scene.hue), |o| (o.percent, o.hue));
            assert_eq!(level, (percent, hue), "{id} {target}");
        }
    }
    let movie = house.scenes.iter().find(|scene| scene.id == "movie").unwrap();
    assert_eq!(movie.extras, json!({ "tv_input": "HDMI 1", "volume_cap": 30 }));
}

#[test]
fn the_notes_are_generic_and_name_devices_that_exist() {
    let house = legacy_house();
    assert!(!house.notes.is_empty());
    for note in &house.notes {
        assert!(house.device(&note.target).is_some(), "{}", note.target);
    }
}

#[tokio::test]
async fn a_bound_world_drives_the_legacy_house_with_todays_states() {
    let world = MockDevices::new();
    let plugin = MockPlugin::bound(&world);
    let house = legacy_house();
    let wiz = connect_house(plugin, &house, "wiz");
    let before = wiz.read_state("bulb3").await.unwrap();
    assert_eq!(before, swifty_plugin_api::State::Light { on: false, bri: 70, hue: 205, color: true });
    wiz.command("bulb3", &Op::LightPower { on: true }).await.unwrap();
    assert!(Op::LightPower { on: true }.is_confirmed_by(&wiz.read_state("bulb3").await.unwrap(), None));

    let proxmox = connect_house(plugin, &house, "proxmox");
    let vm = format!("vm{}", LEGACY_VMS[0].0);
    proxmox.command(&vm, &Op::VmShutdown).await.unwrap();
    assert!(Op::VmShutdown.is_confirmed_by(&proxmox.read_state(&vm).await.unwrap(), None));
    assert_eq!(world.command_count("bulb3") + world.command_count(&vm), 2);
    assert_eq!(world.total_reads(), 3);
}

#[test]
fn the_demo_home_is_one_mock_instance_with_unhinted_devices() {
    let house = demo_home();
    assert_eq!(house.instances.len(), 1);
    let instance = &house.instances[0];
    assert_eq!((instance.id.as_str(), instance.plugin.as_str()), ("mock", "mock"));
    assert!(house.devices.is_empty() && house.scenes.is_empty() && house.notes.is_empty());
    let decls = with_view("mock", &instance.config, &[], &[], |view| PLUGIN.devices(view));
    assert_eq!(decls.len(), 8);
    assert!(decls.iter().all(|decl| decl.target_hint.is_none()), "the hub names demo devices");
    let gadget = decls.iter().find(|decl| decl.class == DeviceClass::Other).unwrap();
    assert_eq!(gadget.capabilities, [Capability::Custom { action: "mock.set_level".to_string() }]);
}

#[tokio::test]
async fn each_demo_instance_gets_a_world_of_its_own() {
    let config = demo_home().instances[0].config.clone();
    let first = connect(&PLUGIN, "mock", &config);
    let second = connect(&PLUGIN, "mock2", &config);
    first.command("plug", &Op::PlugPower { on: true }).await.unwrap();
    assert_eq!(first.read_state("plug").await.unwrap(), swifty_plugin_api::State::Plug { on: true });
    assert_eq!(second.read_state("plug").await.unwrap(), swifty_plugin_api::State::Plug { on: false });
}
