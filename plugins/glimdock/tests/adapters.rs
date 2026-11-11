//! The driver, called directly, against a mock node on loopback. The tests for
//! the collector in the hub's former `tests/adapters.rs` and `tests/machines.rs`, under
//! the same names.

mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::*;
use serde_json::json;
use swifty_net::Http;
use swifty_plugin_api::{DeviceError, Driver, Op, Plugin, State};
use swifty_plugin_glimdock::{GlimdockConfig, GlimdockDriver, MANIFEST, PLUGIN};
use swifty_plugin_testkit::devices::{collector_handler, MockHttp, Seen, COLLECTOR_TOKEN, SNAPSHOT_PATH};

/// A node that also pairs agents, as the hub's test rig had it.
fn node_with_pairing(seen: &Seen) -> (u16, String) {
    if seen.method == "POST" && seen.path == PAIRING_PATH {
        if seen.header("authorization") != format!("Bearer {COLLECTOR_TOKEN}") {
            return (401, String::new());
        }
        return (200, json!({ "code": "PAIR-1234", "expires_in": 600 }).to_string());
    }
    collector_handler(seen)
}

#[tokio::test]
async fn the_collector_is_read_with_a_bearer_token_from_a_file() {
    let mock = MockHttp::start(collector_handler).await;
    let config = config(&mock.url, SNAPSHOT_PATH);
    let driver = PLUGIN.connect(live(config.clone(), COLLECTOR_TOKEN)).unwrap();
    assert_eq!(
        driver.read_state("collector").await.unwrap(),
        State::Telemetry { data: json!({ "pkg_temp_c": 64, "pkg_watts": 118 }) }
    );
    assert_eq!(mock.requests()[0].method, "GET");
    assert_eq!(mock.requests()[0].header("authorization"), format!("Bearer {COLLECTOR_TOKEN}"));
    let present = with_token();
    assert!(PLUGIN.refusal(&view(&config, &present), "collector", &Op::Play).is_some());
    assert!(matches!(driver.command("collector", &Op::Play).await, Err(DeviceError::Disabled(_))));
    assert_eq!(mock.requests().len(), 1);
}

#[tokio::test]
async fn threads_by_guest_memory_and_energy_arrive_whole_with_the_two_flat_readings() {
    let snapshot = json!({
        "host": {
            "cpu_pct": 20.0,
            "mem_total_bytes": 4096,
            "cpu_cores": [10.0, 20.0],
            "cpu_core_kinds": ["performance", "efficiency"],
        },
        "guests": [{ "id": 100, "name": "media", "cpu_affinity": [0], "cpu_count": 1, "mem_host_bytes": 900 }],
        "energy": {
            "since": 1794380000,
            "total_kwh": 2.5,
            "domains": [{ "id": "cpu-package", "label": "CPU package", "kwh": 2.5, "watts": 40.0 }],
            "scope": "components",
        },
    });
    let body = snapshot.to_string();
    let mock =
        MockHttp::start(
            move |seen| {
                if collector_handler(seen).0 == 200 {
                    (200, body.clone())
                } else {
                    (401, String::new())
                }
            },
        )
        .await;
    let driver = PLUGIN.connect(live(config(&mock.url, SNAPSHOT_PATH), COLLECTOR_TOKEN)).unwrap();
    let State::Telemetry { data } = driver.read_state("collector").await.unwrap() else { panic!("not telemetry") };
    for (path, want) in [
        ("/host/cpu_core_kinds", &snapshot["host"]["cpu_core_kinds"]),
        ("/guests", &snapshot["guests"]),
        ("/energy", &snapshot["energy"]),
        ("/energy_kwh", &json!(2.5)),
        ("/energy_watts", &json!(40.0)),
        ("/cores", &json!([10.0, 20.0])),
    ] {
        assert_eq!(data.pointer(path), Some(want), "{path}");
    }
}

#[tokio::test]
async fn the_collector_refuses_a_bad_token_and_bad_data() {
    let mock = MockHttp::start(collector_handler).await;
    let driver = PLUGIN.connect(live(config(&mock.url, SNAPSHOT_PATH), "wrong-token")).unwrap();
    assert!(matches!(driver.read_state("collector").await, Err(DeviceError::Rejected(_))));

    let garbage = MockHttp::start(|_| (200, "not json".to_string())).await;
    let driver = PLUGIN.connect(live(config(&garbage.url, SNAPSHOT_PATH), "wrong-token")).unwrap();
    assert!(matches!(driver.read_state("collector").await, Err(DeviceError::Rejected(_))));
}

// ----- every driver has a read timeout, and none retries

/// Generous, but far below the 120 seconds a hung mock would take.
const OUTER_LIMIT: Duration = Duration::from_secs(5);

async fn assert_gives_up<T: std::fmt::Debug>(
    name: &str,
    call: impl std::future::Future<Output = Result<T, DeviceError>>,
) {
    let started = Instant::now();
    let result = tokio::time::timeout(OUTER_LIMIT, call).await.unwrap_or_else(|_| panic!("{name} never gave up"));
    assert!(started.elapsed() < OUTER_LIMIT, "{name}");
    assert!(matches!(&result, Err(DeviceError::Timeout)), "{name}: {result:?}");
}

#[tokio::test]
async fn a_device_that_never_answers_times_out_and_is_asked_once() {
    let telemetry = MockHttp::start(collector_handler).await;
    telemetry.hang.store(true, Ordering::SeqCst);
    let driver = PLUGIN.connect(live(config(&telemetry.url, SNAPSHOT_PATH), COLLECTOR_TOKEN)).unwrap();
    assert_gives_up("collector", driver.read_state("collector")).await;
    assert_eq!(telemetry.requests().len(), 1);
}

#[tokio::test]
async fn a_scheme_relative_snapshot_path_cannot_send_the_token_elsewhere() {
    let collector = MockHttp::start(collector_handler).await;
    let elsewhere = MockHttp::start(|_| (200, "{}".to_string())).await;
    // Past the configuration check, to prove the driver refuses by itself.
    let config = GlimdockConfig {
        url: collector.url.clone(),
        snapshot_path: format!("//{}/stolen", elsewhere.url.trim_start_matches("http://")),
        pairing_path: Some(format!("//{}/stolen", elsewhere.url.trim_start_matches("http://"))),
    };
    assert!(config.check().is_err(), "the configuration check refuses it first");
    let driver = GlimdockDriver::new(grant(), &config, &COLLECTOR_TOKEN.to_string().into(), timeouts()).unwrap();
    assert!(matches!(driver.read_state("collector").await, Err(DeviceError::Disabled(_))));
    let pairing = driver.agents().unwrap();
    assert!(matches!(pairing.pair_agent("x").await, Err(DeviceError::Disabled(_))));
    assert_eq!(elsewhere.requests().len(), 0, "the token went to another host");
    assert_eq!(collector.requests().len(), 0);
}

#[test]
fn an_adapter_path_can_never_change_the_destination() {
    let http = Http::new(grant(), "http://127.0.0.1:9", timeouts(), None).unwrap();
    assert_eq!(http.url("/api/x").unwrap().as_str(), "http://127.0.0.1:9/api/x");
    for path in [
        "//192.0.2.1/x",
        "//127.0.0.1:10/x",
        "http://192.0.2.1/x",
        "https://127.0.0.1:9/x",
        "http://127.0.0.1:10/x",
        "\\\\192.0.2.1\\x",
    ] {
        assert!(matches!(http.url(path), Err(DeviceError::Disabled(_))), "{path}");
    }
}

#[tokio::test]
async fn nothing_listening_is_unreachable() {
    let mut context = live(config(NOTHING_LISTENS, SNAPSHOT_PATH), COLLECTOR_TOKEN);
    context.timeouts.connect = Duration::from_secs(3);
    let driver = PLUGIN.connect(context).unwrap();
    let error = driver.read_state("collector").await.unwrap_err();
    assert!(error.is_silence(), "{error:?}");
    assert!(!error.to_string().contains("127.0.0.1"), "{error}");
}

#[tokio::test]
async fn an_oversized_reply_is_refused() {
    let mock = MockHttp::start(|_| (200, "x".repeat(9 * 1024 * 1024))).await;
    let mut context = live(config(&mock.url, SNAPSHOT_PATH), COLLECTOR_TOKEN);
    context.timeouts.read = Duration::from_secs(5);
    let driver = PLUGIN.connect(context).unwrap();
    let error = driver.read_state("collector").await.unwrap_err();
    assert!(matches!(&error, DeviceError::Rejected(text) if text.contains("too much")), "{error:?}");
}

#[tokio::test]
async fn a_device_the_node_does_not_have_is_refused_without_a_request() {
    let mock = MockHttp::start(collector_handler).await;
    let driver = PLUGIN.connect(live(config(&mock.url, SNAPSHOT_PATH), COLLECTOR_TOKEN)).unwrap();
    assert!(matches!(driver.read_state("collector-2").await, Err(DeviceError::Disabled(_))));
    assert_eq!(mock.traffic(), 0);
}

#[tokio::test]
async fn nothing_it_says_carries_the_token() {
    let mock = MockHttp::start(|_| (500, "the token was wrong".to_string())).await;
    let driver = PLUGIN.connect(live(pairing_config(&mock.url, SNAPSHOT_PATH), COLLECTOR_TOKEN)).unwrap();
    let read = driver.read_state("collector").await.unwrap_err();
    let pair = driver.agents().unwrap().pair_agent("x").await.unwrap_err();
    for error in [read, pair] {
        assert!(!format!("{error} {error:?}").contains(COLLECTOR_TOKEN), "{error:?}");
        assert!(matches!(&error, DeviceError::Rejected(text) if text.contains("500")), "{error:?}");
    }
    assert!(mock.requests().iter().all(|seen| !seen.query.contains(COLLECTOR_TOKEN)));
    assert!(mock.requests().iter().all(|seen| !seen.path.contains(COLLECTOR_TOKEN)));
}

#[test]
fn connecting_needs_a_valid_config_and_the_token() {
    let mut context = live(config("http://192.0.2.10", SNAPSHOT_PATH), COLLECTOR_TOKEN);
    context.secrets = Default::default();
    let refused = PLUGIN.connect(context).err().expect("no token, no driver");
    assert!(refused.0.contains("token"), "{refused}");

    let refused = PLUGIN.connect(live(config("http://192.0.2.10", "stolen"), COLLECTOR_TOKEN)).err().unwrap();
    assert!(refused.0.contains("snapshot_path"), "{refused}");

    let refused = PLUGIN.connect(live(config("http://192.0.2.10", SNAPSHOT_PATH), "line\nbreak")).err().unwrap();
    assert!(!refused.0.contains("line"), "{refused}");
}

// ----- agents (from the hub's machines.rs)

#[tokio::test]
async fn pairing_goes_through_the_collectors_own_flow() {
    let mock = MockHttp::start(node_with_pairing).await;
    let driver = PLUGIN.connect(live(pairing_config(&mock.url, SNAPSHOT_PATH), COLLECTOR_TOKEN)).unwrap();
    let pairing = driver.agents().expect("a node pairs agents").pair_agent("Windows box").await.unwrap();
    assert_eq!(pairing["code"], json!("PAIR-1234"));
    let posts = mock.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].path, PAIRING_PATH);
    assert_eq!(posts[0].body, json!({ "name": "Windows box" }).to_string());
    assert_eq!(posts[0].header("authorization"), format!("Bearer {COLLECTOR_TOKEN}"));
    assert_eq!(posts[0].header("content-type"), "application/json");
}

#[tokio::test]
async fn pairing_without_a_configured_path_says_so() {
    let mock = MockHttp::start(node_with_pairing).await;
    let driver = PLUGIN.connect(live(config(&mock.url, SNAPSHOT_PATH), COLLECTOR_TOKEN)).unwrap();
    let error = driver.agents().unwrap().pair_agent("x").await.unwrap_err();
    assert!(matches!(&error, DeviceError::Disabled(text) if text.contains("pairing_path")), "{error:?}");
    assert!(mock.posts().is_empty());
}

#[tokio::test]
async fn a_refused_pairing_is_a_refusal() {
    let mock = MockHttp::start(node_with_pairing).await;
    let driver = PLUGIN.connect(live(pairing_config(&mock.url, SNAPSHOT_PATH), "wrong-token")).unwrap();
    let error = driver.agents().unwrap().pair_agent("x").await.unwrap_err();
    assert!(matches!(&error, DeviceError::Rejected(text) if text.contains("token")), "{error:?}");
    assert_eq!(mock.posts().len(), 1, "sent once, never again");
}

#[tokio::test]
async fn agents_stay_monitoring_only() {
    assert!(MANIFEST.actions.is_empty(), "pairing is the hub's agent.pair, and nothing else can be asked of a node");
    let mock = MockHttp::start(node_with_pairing).await;
    let config = pairing_config(&mock.url, SNAPSHOT_PATH);
    let driver = PLUGIN.connect(live(config.clone(), COLLECTOR_TOKEN)).unwrap();
    driver.read_state("collector").await.unwrap();
    assert!(mock.posts().is_empty(), "a reading sends no command");
    let present = with_token();
    for op in [Op::Play, Op::HostReboot, Op::VmShutdown, Op::LightPower { on: true }] {
        assert!(PLUGIN.refusal(&view(&config, &present), "collector", &op).is_some(), "{op:?}");
        assert!(matches!(driver.command("collector", &op).await, Err(DeviceError::Disabled(_))), "{op:?}");
    }
    assert!(mock.posts().is_empty());
}
