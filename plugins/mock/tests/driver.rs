//! The driver and the pure calls: behaviours, refusals, confirmation, the
//! extensions, discovery and readiness.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use common::{connect, connect_house, context, with_view, Adopted};
use serde_json::{json, Value};
use swifty_plugin_api::{
    CapKind, Capability, DeviceClass, DeviceDecl, DeviceError, DeviceRecord, HealthStatus, HistoryRange, Op, Plugin,
    Readiness, Scalar, Secrets, State,
};
use swifty_plugin_mock::{
    legacy_house, Behavior, MockDevices, MockPlugin, CANNOT, CODE_MISSING, MANIFEST, NOT_HERE, PLUGIN,
};

fn level(level: i64) -> Op {
    Op::Custom {
        action: "mock.set_level".to_string(),
        args: BTreeMap::from([("level".to_string(), Scalar::Int(level))]),
    }
}

fn one_of_each() -> Value {
    let classes = [
        "light",
        "bridge",
        "plug",
        "aircon",
        "speaker",
        "tv",
        "feeder",
        "litter",
        "printer",
        "vm",
        "host",
        "router",
        "camera",
        "service",
        "telemetry",
        "other",
    ];
    json!({ "devices": classes.iter().map(|class| json!({ "id": class, "class": class })).collect::<Vec<_>>() })
}

/// An op each class can do, and whether its read-back needs the state from before.
fn op_for(class: &str) -> Option<Op> {
    Some(match class {
        "light" => Op::LightSet { percent: 40, hue: 120 },
        "plug" => Op::PlugPower { on: true },
        "aircon" => Op::AirconTemp { celsius: 25 },
        "speaker" => Op::Volume { level: 33 },
        "tv" => Op::TvInput { input: "HDMI 3".to_string() },
        "feeder" => Op::PetFeed { grams: 20 },
        "litter" => Op::LitterClean,
        "printer" => Op::PrintStart,
        "vm" => Op::VmShutdown,
        "host" => Op::HostShutdown,
        "router" => Op::WireguardRestart,
        "other" => level(7),
        _ => return None,
    })
}

#[tokio::test]
async fn every_class_takes_an_op_and_reads_back_confirmed() {
    let config = one_of_each();
    let driver = connect(&PLUGIN, "mock", &config);
    let expect = MANIFEST.actions[0].expect;
    for row in config["devices"].as_array().unwrap() {
        let id = row["id"].as_str().unwrap();
        let before = driver.read_state(id).await.unwrap();
        let Some(op) = op_for(id) else { continue };
        assert_eq!(with_view("mock", &config, &[], &[], |view| PLUGIN.refusal(view, id, &op)), None, "{id}");
        driver.command(id, &op).await.unwrap();
        let after = driver.read_state(id).await.unwrap();
        let confirmed = match &op {
            Op::Custom { args, .. } => expect.is_met(args, &after, Some(&before)),
            _ => op.is_confirmed_by(&after, Some(&before)),
        };
        assert!(confirmed, "{id}: {op:?} then {after:?}");
    }
}

#[tokio::test]
async fn the_custom_action_is_confirmed_by_its_expect() {
    let config = json!({ "devices": [{ "id": "gadget", "class": "other" }] });
    let driver = connect(&PLUGIN, "mock", &config);
    let action = &MANIFEST.actions[0];
    assert_eq!(action.id, "mock.set_level");
    let before = driver.read_state("gadget").await.unwrap();
    assert_eq!(before, State::Custom { fields: BTreeMap::from([("level".to_string(), Scalar::Int(0))]) });
    let Op::Custom { args, .. } = level(4) else { unreachable!() };
    assert!(!action.expect.is_met(&args, &before, None));
    driver.command("gadget", &level(4)).await.unwrap();
    assert!(action.expect.is_met(&args, &driver.read_state("gadget").await.unwrap(), None));
}

#[test]
fn refusals_follow_the_capabilities() {
    let config = json!({ "devices": [
        { "id": "lamp", "class": "light" },
        { "id": "dim", "class": "light", "capabilities": [{ "cap": "power" }, { "cap": "brightness", "min": 25 }] },
        { "id": "sub", "class": "speaker", "capabilities": [] },
        { "id": "speaker", "class": "speaker" },
        { "id": "tv", "class": "tv" },
        { "id": "ac", "class": "aircon" },
        { "id": "feeder", "class": "feeder" },
        { "id": "gadget", "class": "other" },
        { "id": "camera", "class": "camera" },
    ]});
    let refusal = |device: &str, op: Op| with_view("mock", &config, &[], &[], |view| PLUGIN.refusal(view, device, &op));
    let cannot = Some(CANNOT.to_string());
    assert_eq!(
        refusal("lamp", Op::LightBrightness { percent: 5 }),
        Some("This light dims to 10 % at the lowest.".into())
    );
    assert_eq!(refusal("lamp", Op::LightBrightness { percent: 10 }), None);
    assert_eq!(refusal("lamp", Op::LightHue { hue: 360 }), Some("A colour is a hue angle from 0 to 359.".into()));
    assert_eq!(refusal("lamp", Op::Volume { level: 10 }), cannot);
    assert_eq!(
        refusal("dim", Op::LightBrightness { percent: 20 }),
        Some("This light dims to 25 % at the lowest.".into())
    );
    assert_eq!(refusal("dim", Op::LightSet { percent: 50, hue: 10 }), cannot, "no colour");
    assert_eq!(refusal("sub", Op::Volume { level: 10 }), cannot, "read-only");
    assert_eq!(refusal("speaker", Op::Volume { level: 101 }), Some("Volume goes from 0 to 100.".into()));
    assert_eq!(refusal("speaker", Op::VolumeCap { max: 30 }), None);
    assert_eq!(refusal("tv", Op::TvInput { input: "HDMI 9".into() }), Some("This device has no such input.".into()));
    assert_eq!(refusal("tv", Op::TvKey { key: "eject".into() }), Some("This device has no such key.".into()));
    assert_eq!(refusal("tv", Op::TvKey { key: "ok".into() }), None);
    assert_eq!(refusal("ac", Op::AirconMode { mode: "turbo".into() }), Some("This device has no such mode.".into()));
    assert_eq!(refusal("ac", Op::AirconFan { fan: "gale".into() }), Some("This device has no such fan speed.".into()));
    assert_eq!(refusal("ac", Op::AirconTemp { celsius: 31 }), Some("This device takes 17 to 30 °C.".into()));
    assert_eq!(
        refusal("feeder", Op::PetFeed { grams: 15 }),
        Some("This feeder dispenses 10, 20, 30, 40, 50 g at a time.".into())
    );
    assert_eq!(
        refusal("gadget", level(11)),
        Some("mock.set_level takes one level, a whole number from 0 to 10.".into())
    );
    assert_eq!(refusal("gadget", Op::Custom { action: "mock.other".into(), args: BTreeMap::new() }), cannot);
    assert_eq!(refusal("camera", Op::TvPower { on: true }), cannot);
    assert_eq!(refusal("nobody", Op::PlugPower { on: true }), Some(NOT_HERE.to_string()));
}

#[tokio::test]
async fn what_is_refused_in_dry_run_is_refused_live_and_nothing_is_sent() {
    let world = MockDevices::new();
    let plugin = MockPlugin::bound(&world);
    let house = legacy_house();
    let sonos = house.instance("sonos").unwrap();
    let driver = connect_house(plugin, &house, "sonos");
    for (device, op) in [("sub", Op::Volume { level: 10 }), ("sonos", Op::Volume { level: 101 }), ("bulb1", Op::Play)] {
        let said = with_view("sonos", &sonos.config, &[], &[], |view| plugin.refusal(view, device, &op)).unwrap();
        match driver.command(device, &op).await {
            Err(DeviceError::Rejected(reason)) => assert_eq!(reason, said, "{device}"),
            other => panic!("{device}: {other:?}"),
        }
    }
    assert_eq!(world.total_commands(), 0, "a refused op is never sent");
}

#[tokio::test]
async fn each_behaviour_answers_as_it_says() {
    let world = MockDevices::new();
    let plugin = MockPlugin::bound(&world);
    let house = legacy_house();
    let wiz = connect_house(plugin, &house, "wiz");
    let on = Op::LightPower { on: true };

    world.set_behavior("bulb1", Behavior::Silent);
    assert_eq!(wiz.command("bulb1", &on).await, Err(DeviceError::Timeout));
    assert_eq!(wiz.read_state("bulb1").await, Err(DeviceError::Timeout));
    assert_eq!(world.command_count("bulb1"), 1, "silence is recorded once and not retried");

    world.set_behavior("bulb3", Behavior::AcceptsButIgnores);
    wiz.command("bulb3", &on).await.unwrap();
    assert!(!on.is_confirmed_by(&wiz.read_state("bulb3").await.unwrap(), None));

    world.set_behavior("bulb2", Behavior::Rejects("busy".to_string()));
    assert_eq!(
        wiz.command("bulb2", &Op::LightPower { on: false }).await,
        Err(DeviceError::Rejected("busy".to_string()))
    );

    world.set_behavior("bulb4", Behavior::Slow(40));
    let off = Op::LightPower { on: false };
    wiz.command("bulb4", &off).await.unwrap();
    assert!(!off.is_confirmed_by(&world.state("bulb4").unwrap(), None), "not yet");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(off.is_confirmed_by(&world.state("bulb4").unwrap(), None));

    world.set_behavior("bulb4", Behavior::Hang);
    let waited = tokio::time::timeout(Duration::from_millis(50), wiz.command("bulb4", &on)).await;
    assert!(waited.is_err(), "a hanging device never answers");

    world.set_behavior("bulb4", Behavior::Normal);
    assert_eq!(world.behavior("bulb4"), Behavior::Normal);
    assert_eq!(world.total_commands(), 5);
    assert_eq!(world.commands()[0], ("bulb1".to_string(), on));
}

#[tokio::test]
async fn a_disabled_instance_says_so_everywhere_and_sends_nothing() {
    let world = MockDevices::new();
    let plugin = MockPlugin::bound(&world);
    let house = legacy_house();
    let webos = house.instance("webos").unwrap();
    let driver = connect_house(plugin, &house, "webos");
    let reason = "The TV has not been paired.";
    world.set_disabled("webos", reason);
    assert_eq!(world.disabled("webos").as_deref(), Some(reason));
    let readiness = with_view("webos", &webos.config, &[], &[], |view| plugin.readiness(view));
    assert_eq!(readiness, Readiness::Disabled(reason.to_string()));
    let refusal =
        with_view("webos", &webos.config, &[], &[], |view| plugin.refusal(view, "tv", &Op::TvPower { on: false }));
    assert_eq!(refusal.as_deref(), Some(reason));
    assert_eq!(driver.command("tv", &Op::TvPower { on: false }).await, Err(DeviceError::Disabled(reason.to_string())));
    assert_eq!(driver.read_state("tv").await, Err(DeviceError::Disabled(reason.to_string())));
    assert_eq!(driver.health().await.status, HealthStatus::Down);
    assert_eq!(world.total_commands(), 0);
    // Other instances carry on.
    let wiz = house.instance("wiz").unwrap();
    assert_eq!(with_view("wiz", &wiz.config, &[], &[], |v| plugin.readiness(v)), Readiness::Ready);
}

#[tokio::test]
async fn a_config_can_disable_an_instance_or_require_its_access_code() {
    let disabled =
        json!({ "devices": [{ "id": "router", "class": "router" }], "disabled": "No router plugin is set up." });
    let readiness = with_view("router", &disabled, &[], &[], |view| PLUGIN.readiness(view));
    assert_eq!(readiness, Readiness::Disabled("No router plugin is set up.".to_string()));
    let driver = connect(&PLUGIN, "router", &disabled);
    assert!(matches!(driver.command("router", &Op::WireguardRestart).await, Err(DeviceError::Disabled(_))));

    let coded = json!({ "devices": [{ "id": "lamp", "class": "light" }], "require_code": true });
    let readiness = |slots: &[&str]| with_view("mock", &coded, slots, &[], |view| PLUGIN.readiness(view));
    assert_eq!(readiness(&[]), Readiness::NeedsSetup(CODE_MISSING.to_string()));
    assert_eq!(readiness(&["access_code"]), Readiness::Ready);
    let without = PLUGIN.connect(context("mock", &coded, Secrets::default(), Adopted::of(Vec::new())));
    assert_eq!(without.err().unwrap().to_string(), "The secret access_code is not set.");
    let with = Secrets::of([("access_code", "an example code")]);
    assert!(PLUGIN.connect(context("mock", &coded, with, Adopted::of(Vec::new()))).is_ok());

    let broken = json!({ "devices": "none" });
    assert!(matches!(with_view("mock", &broken, &[], &[], |view| PLUGIN.readiness(view)), Readiness::Disabled(_)));
    assert!(with_view("mock", &broken, &[], &[], |view| PLUGIN.devices(view)).is_empty());
    assert!(PLUGIN.connect(context("mock", &broken, Secrets::default(), Adopted::of(Vec::new()))).is_err());
}

#[tokio::test]
async fn a_device_outside_the_instance_is_refused_by_the_driver() {
    let driver = connect(&PLUGIN, "mock", &json!({ "devices": [{ "id": "lamp", "class": "light" }] }));
    assert_eq!(
        driver.command("bulb1", &Op::LightPower { on: true }).await,
        Err(DeviceError::Rejected(NOT_HERE.to_string()))
    );
    assert_eq!(driver.read_state("bulb1").await, Err(DeviceError::Unreachable(NOT_HERE.to_string())));
}

#[tokio::test]
async fn hosts_list_machines_and_history_and_a_collector_pairs_agents() {
    let world = MockDevices::new();
    let plugin = MockPlugin::bound(&world);
    let house = legacy_house();
    let proxmox = connect_house(plugin, &house, "proxmox");
    world.add_seen_machine("primary", 210, "echo");
    let machines = proxmox.machines().expect("a host offers machines");
    let seen = machines.list_machines().await.unwrap();
    assert_eq!((seen[0].host.as_str(), seen[0].vmid, seen[0].name.as_str()), ("primary", 210, "echo"));
    let history = machines.history("vm200", HistoryRange::parse("1h").unwrap()).await.unwrap();
    assert_eq!(history, vec![json!({ "time": 1, "cpu": 0.25 })]);
    assert_eq!(world.total_reads(), 1);
    assert!(machines.history("bulb1", HistoryRange::parse("1h").unwrap()).await.is_err());
    assert_eq!(machines.open_console("vm200").await.err(), Some(DeviceError::unsupported()));
    assert!(proxmox.agents().is_none());

    let collector = connect_house(plugin, &house, "collector");
    let paired = collector.agents().expect("the collector pairs agents").pair_agent("desk").await.unwrap();
    assert_eq!(paired, json!({ "name": "desk", "code": "MOCK-CODE" }));
    assert!(collector.machines().is_none());
    world.set_collector(json!({ "pkg_temp_c": 50 }));
    assert_eq!(
        collector.read_state("collector").await.unwrap(),
        State::Telemetry { data: json!({ "pkg_temp_c": 50 }) }
    );
}

#[tokio::test]
async fn a_discovered_device_is_offered_and_works_once_adopted() {
    let world = MockDevices::new();
    let plugin = MockPlugin::bound(&world);
    let config = json!({ "devices": [] });
    let adopted = Adopted::of(Vec::new());
    let driver = plugin.connect(context("lights", &config, Secrets::default(), adopted.clone())).unwrap();
    world.add_discoverable(
        "lights",
        DeviceDecl {
            local_id: "strip".to_string(),
            target_hint: None,
            class: DeviceClass::Light,
            name: "Strip".to_string(),
            capabilities: vec![Capability::Power],
            attrs: json!({ "address": "192.0.2.40" }),
        },
    );
    world.add_discoverable(
        "elsewhere",
        DeviceDecl {
            local_id: "x".into(),
            target_hint: None,
            class: DeviceClass::Plug,
            name: "X".into(),
            capabilities: vec![],
            attrs: json!({}),
        },
    );
    let found = driver.discover(Duration::from_millis(10)).await.unwrap();
    assert_eq!(found.len(), 1, "only this instance's candidates");
    assert_eq!(found[0].attrs, json!({ "address": "192.0.2.40", "capabilities": [{ "cap": "power" }] }));
    assert_eq!(
        driver.command("strip", &Op::LightPower { on: true }).await,
        Err(DeviceError::Rejected(NOT_HERE.to_string()))
    );

    let record = DeviceRecord {
        target: "lights-strip".to_string(),
        local_id: "strip".to_string(),
        class: DeviceClass::Light,
        name: "Strip".to_string(),
        attrs: found[0].attrs.clone(),
    };
    adopted.add(record.clone());
    let decls = with_view("lights", &config, &[], &[record], |view| plugin.devices(view));
    assert_eq!(decls[0].capabilities, [Capability::Power], "the adopted device keeps its capabilities");
    assert_eq!(decls[0].target_hint.as_deref(), Some("lights-strip"));
    driver.command("strip", &Op::LightPower { on: true }).await.unwrap();
    assert!(Op::LightPower { on: true }.is_confirmed_by(&driver.read_state("strip").await.unwrap(), None));
    assert_eq!(
        driver.command("strip", &Op::LightBrightness { percent: 50 }).await,
        Err(DeviceError::Rejected(CANNOT.to_string()))
    );
}

#[test]
fn the_manifest_offers_every_class_and_one_custom_action() {
    assert_eq!(MANIFEST.id, "mock");
    assert_eq!(MANIFEST.classes.len(), 16);
    assert_eq!(MANIFEST.actions.len(), 1);
    let action = &MANIFEST.actions[0];
    assert_eq!(action.class, DeviceClass::Other);
    assert!(MANIFEST.network.is_empty(), "the mock reaches nothing");
    let json = serde_json::to_value(&MANIFEST).unwrap();
    assert_eq!(json["actions"][0]["expect"], json!({ "expect": "field_equals_arg", "field": "level", "arg": "level" }));
    assert!(MANIFEST.secrets.iter().all(|slot| !slot.required));
    let kinds: Vec<CapKind> =
        swifty_plugin_mock::default_capabilities(DeviceClass::Other).iter().map(Capability::kind).collect();
    assert_eq!(kinds, [CapKind::Custom]);
}
