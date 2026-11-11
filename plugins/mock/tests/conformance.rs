//! The conformance suite, on the plugin as shipped and on a bound one, and its
//! config check on every instance of both homes.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::json;
use swifty_plugin_api::{DeviceClass, DeviceDecl, Op, Scalar, SetupInput, State};
use swifty_plugin_mock::{demo_home, legacy_house, Behavior, MockDevices, MockPlugin, PLUGIN};
use swifty_plugin_testkit::conformance::{config_problems, run_every_check, Fault, Fixture, MockDevice};

/// The fixture's devices, by local id.
const DEVICES: [&str; 3] = ["lamp", "tv", "gadget"];

/// The mock's devices live in memory, so the device the suite watches is the
/// world itself: whatever a driver asks of it is traffic, a command it records
/// is an exchange, and its behaviours stand in for silence and for answers that
/// cannot be read. The world is found when it is needed, because the plugin as
/// shipped makes one when the suite connects it.
struct World {
    find: Box<dyn Fn() -> Option<Arc<MockDevices>> + Send + Sync>,
    /// The states a hostile name replaced, to put back.
    saved: Mutex<Vec<(String, State)>>,
}

impl World {
    fn of(find: impl Fn() -> Option<Arc<MockDevices>> + Send + Sync + 'static) -> Self {
        Self { find: Box::new(find), saved: Mutex::new(Vec::new()) }
    }

    fn behave(&self, behavior: Behavior) -> bool {
        let Some(world) = (self.find)() else { return false };
        for device in DEVICES {
            world.set_behavior(device, behavior.clone());
        }
        true
    }
}

impl MockDevice for World {
    fn traffic(&self) -> usize {
        (self.find)().map_or(0, |world| world.total_commands() + world.total_reads())
    }

    fn exchanges(&self) -> usize {
        (self.find)().map_or(0, |world| world.total_commands())
    }

    fn set_silent(&self, silent: bool) -> bool {
        self.behave(if silent { Behavior::Silent } else { Behavior::Normal })
    }

    fn set_fault(&self, fault: Option<Fault>) -> bool {
        self.behave(if fault.is_some() { Behavior::Garbles } else { Behavior::Normal })
    }

    /// As the TV's input, a field of the gadget, and the name of a device the
    /// instance can discover.
    fn set_device_text(&self, text: Option<&str>) -> bool {
        let Some(world) = (self.find)() else { return false };
        let mut saved = self.saved.lock().unwrap();
        match text {
            Some(text) => {
                for device in ["tv", "gadget"] {
                    if let Some(state) = world.state(device) {
                        saved.push((device.to_string(), state));
                    }
                }
                world.add_state("tv", State::Tv { on: true, input: text.to_string() });
                let fields = BTreeMap::from([
                    ("level".to_string(), Scalar::Int(0)),
                    (text.to_string(), Scalar::Text(text.to_string())),
                ]);
                world.add_state("gadget", State::Custom { fields });
                world.add_discoverable(
                    "mock",
                    DeviceDecl {
                        local_id: "found".to_string(),
                        target_hint: None,
                        class: DeviceClass::Light,
                        name: text.to_string(),
                        capabilities: Vec::new(),
                        attrs: json!({}),
                    },
                );
            }
            None => {
                for (device, state) in saved.drain(..) {
                    world.add_state(&device, state);
                }
            }
        }
        true
    }
}

fn fixture(device: World) -> Fixture {
    Fixture {
        config: json!({
            "devices": [
                { "id": "lamp", "class": "light", "name": "Lamp", "target": "lamp" },
                { "id": "tv", "class": "tv", "capabilities": [{ "cap": "power" }, { "cap": "inputs", "options": "HDMI 1, HDMI 2" }] },
                { "id": "gadget", "class": "other" },
            ],
        }),
        device: Box::new(device),
        ops: vec![
            ("lamp".to_string(), Op::LightSet { percent: 40, hue: 120 }),
            ("tv".to_string(), Op::TvInput { input: "HDMI 2".to_string() }),
            (
                "gadget".to_string(),
                Op::Custom {
                    action: "mock.set_level".to_string(),
                    args: BTreeMap::from([("level".to_string(), Scalar::Int(7))]),
                },
            ),
        ],
        setup: vec![
            SetupInput::Chosen { ids: vec!["lamp".to_string(), "gadget".to_string()] },
            SetupInput::Values { values: BTreeMap::new() },
        ],
        ..Fixture::empty()
    }
}

#[tokio::test]
async fn conformance() {
    run_every_check(&PLUGIN, fixture(World::of(|| MockPlugin::world_of("mock")))).await;
}

#[tokio::test]
async fn a_bound_plugin_conforms_too() {
    let world = MockDevices::new();
    let seen = world.clone();
    run_every_check(MockPlugin::bound(&world), fixture(World::of(move || Some(seen.clone())))).await;
}

#[test]
fn every_instance_of_both_homes_passes_the_config_check() {
    for house in [legacy_house(), demo_home()] {
        for instance in &house.instances {
            let problems = config_problems(&PLUGIN, &instance.config);
            assert!(problems.is_empty(), "{}: {problems:?}", instance.id);
        }
    }
}
