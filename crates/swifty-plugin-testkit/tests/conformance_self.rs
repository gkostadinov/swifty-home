//! The conformance suite run against a tiny plugin defined in `lamp/`: lamps on UDP.
//! It must pass as written, and each way of breaking it must be reported.

mod lamp;

use lamp::{Faults, Lamp, MANIFEST};
use serde_json::json;
use swifty_plugin_api::{
    ConfigField, CustomAction, DeviceClass, Expect, FieldKind, Manifest, ParamDef, ParamKind, SecretSlot, Tier,
    API_VERSION,
};
use swifty_plugin_testkit::conformance::{config_problems, manifest_problems, report, run, Fixture, MockDevice};
use swifty_plugin_testkit::trap::{Trap, Traps};

static GOOD: Lamp = Lamp::good();

#[tokio::test]
async fn a_conforming_plugin_passes() {
    run(&GOOD, lamp::fixture().await).await;
}

#[tokio::test]
async fn the_conforming_plugin_is_put_through_every_check() {
    let report = report(&GOOD, lamp::fixture().await).await;
    assert!(report.passed(), "{report}");
    assert!(report.not_run.is_empty(), "every check ran:\n{report}");
}

#[tokio::test]
#[should_panic(expected = "fails conformance")]
async fn a_failing_plugin_panics_with_its_problems() {
    static BAD: Lamp = Lamp::with(Faults { accepts_anything: true, host_optional: false, migrate_changes: false });
    run(&BAD, Fixture { config: json!({ "host": "192.0.2.1" }), ..Fixture::empty() }).await;
}

#[test]
fn the_manifest_checks_catch_each_mistake() {
    assert_eq!(manifest_problems(&MANIFEST, Some(env!("CARGO_PKG_VERSION"))), Vec::<String>::new());
    // Without cargo's version, a crate version is still required.
    assert_eq!(manifest_problems(&MANIFEST, None), Vec::<String>::new());

    let broken = |change: fn(&mut Manifest), needle: &str| {
        let mut manifest = MANIFEST.clone();
        change(&mut manifest);
        let problems = manifest_problems(&manifest, Some(env!("CARGO_PKG_VERSION")));
        assert!(problems.iter().any(|p| p.contains(needle)), "expected a problem with {needle:?}, got {problems:?}");
    };
    broken(|m| m.id = "Lamp", "the id");
    broken(|m| m.id = "l", "the id");
    broken(|m| m.version = "9.9.9", "the crate is");
    broken(|m| m.api = API_VERSION + 1, "plugin API");
    broken(|m| m.secrets = &[SecretSlot { slot: "Key", label: "K", help: "h", required: true }], "secret slot");
    broken(|m| m.secrets = &[SecretSlot { slot: "host", label: "K", help: "h", required: true }], "both a config key");
    broken(
        |m| {
            static ACTIONS: [CustomAction; 1] = [CustomAction {
                id: "other.flash",
                label: "Flash",
                description: "Flashes.",
                consequences: &[],
                class: DeviceClass::Light,
                tier: Tier::Read,
                params: &[ParamDef { name: "n", kind: ParamKind::Int { min: 5, max: 1 }, required: true, help: "h" }],
                idempotent: true,
                confirm_timeout_ms: 1,
                expect: Expect::Unreadable,
            }];
            m.actions = &ACTIONS;
        },
        "must be named `lamp.<verb>`",
    );
    broken(
        |m| {
            static ACTIONS: [CustomAction; 1] = [CustomAction {
                id: "lamp.look",
                label: "Look",
                description: "Looks.",
                consequences: &[],
                class: DeviceClass::Light,
                tier: Tier::SafeDirection,
                params: &[],
                idempotent: true,
                confirm_timeout_ms: 1,
                expect: Expect::Unreadable,
            }];
            m.actions = &ACTIONS;
        },
        "asks for tier safe_direction",
    );
    broken(
        |m| {
            static ACTIONS: [CustomAction; 1] = [CustomAction {
                id: "lamp.flash",
                label: "Flash",
                description: "Flashes.",
                consequences: &[],
                class: DeviceClass::Light,
                tier: Tier::Comfort,
                params: &[ParamDef { name: "n", kind: ParamKind::Enum(&[]), required: true, help: "h" }],
                idempotent: true,
                confirm_timeout_ms: 1,
                expect: Expect::Unreadable,
            }];
            m.actions = &ACTIONS;
        },
        "has no options",
    );
    broken(|m| m.vocabulary = &[("Lamb", "lamp")], "lower-case words");
    broken(|m| m.vocabulary = &[("off", "lamp")], "voice grammar");
    broken(|m| m.nouns = &[("all", DeviceClass::Light)], "voice grammar");
    broken(|m| m.nouns = &[("lumo", DeviceClass::Printer)], "does not offer");
    broken(
        |m| {
            static BAD: [ConfigField; 1] = [ConfigField {
                key: "host",
                label: "Host",
                help: "h",
                kind: FieldKind::Choice { options: &["a"] },
                required: true,
                default: Some("b"),
            }];
            m.config = &BAD;
        },
        "required and has a default",
    );
}

#[test]
fn the_config_checks_catch_each_mistake() {
    let config = json!({ "host": "192.0.2.1", "lamps": [{ "id": "one" }] });
    assert_eq!(config_problems(&GOOD, &config), Vec::<String>::new());

    let anything = Lamp::with(Faults { accepts_anything: true, ..Faults::default() });
    let problems = config_problems(&anything, &config);
    assert!(problems.iter().any(|p| p.contains("unknown key `conformance_unknown_key` validates")), "{problems:?}");
    assert!(problems.iter().any(|p| p.contains("without the required key `host`")), "{problems:?}");
    assert!(problems.iter().any(|p| p.contains("a row of `lamps` without the required `id`")), "{problems:?}");

    let optional = Lamp::with(Faults { host_optional: true, ..Faults::default() });
    let problems = config_problems(&optional, &config);
    assert_eq!(problems, vec!["a config without the required key `host` validates".to_string()]);

    let migrating = Lamp::with(Faults { migrate_changes: true, ..Faults::default() });
    let problems = config_problems(&migrating, &config);
    assert_eq!(problems, vec!["migrate changes a config that is already at config_version".to_string()]);

    let problems = config_problems(&GOOD, &json!({ "host": 5 }));
    assert!(problems[0].starts_with("the fixture's config does not validate"), "{problems:?}");
    let problems = config_problems(&GOOD, &json!({ "host": "192.0.2.1", "colour": "red" }));
    assert!(problems[0].starts_with("the fixture's config does not validate"), "{problems:?}");
    assert_eq!(config_problems(&GOOD, &json!([])), vec!["the fixture's config is not a JSON object".to_string()]);
}

#[test]
fn cargo_states_the_version_of_the_crate_under_test() {
    assert_eq!(std::env::var("CARGO_PKG_VERSION").as_deref(), Ok(env!("CARGO_PKG_VERSION")));
}

#[tokio::test]
async fn a_trap_counts_connections_and_datagrams_and_answers_nothing() {
    let trap = Trap::start().await;
    assert_eq!(trap.traffic(), 0);
    let mut stream = tokio::net::TcpStream::connect(trap.addr).await.unwrap();
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    socket.send_to(b"hello", trap.addr).await.unwrap();
    for _ in 0..200 {
        if trap.traffic() == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(trap.connections.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(trap.datagrams.load(std::sync::atomic::Ordering::SeqCst), 1);
    // Nothing comes back on the connection.
    use tokio::io::AsyncReadExt;
    let mut buffer = [0u8; 8];
    let read = tokio::time::timeout(std::time::Duration::from_millis(100), stream.read(&mut buffer)).await;
    assert!(read.is_err(), "the trap answered");

    let traps = Traps::start(2).await;
    assert_eq!(traps.addrs().len(), 2);
    assert_eq!(MockDevice::traffic(&traps), 0);
}
