//! The driver, called directly, against a mock server on loopback. The tests
//! for Plex in the hub's former `tests/adapters.rs`, under the same names.

mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::*;
use swifty_plugin_api::{DeviceError, Op, Plugin, State};
use swifty_plugin_plex::PLUGIN;
use swifty_plugin_testkit::devices::{plex_handler, MockHttp, PLEX_TOKEN};

#[tokio::test]
async fn plex_sends_its_token_as_a_header_and_never_in_the_url() {
    let mock = MockHttp::start(plex_handler).await;
    let config = config(&mock.url);
    let driver = PLUGIN.connect(live(config.clone(), PLEX_TOKEN)).unwrap();
    assert_eq!(driver.read_state("plex").await.unwrap(), State::Service { reachable: true });
    let request = &mock.requests()[0];
    assert_eq!(request.header("x-plex-token"), PLEX_TOKEN);
    assert!(!request.query.contains(PLEX_TOKEN) && !request.path.contains(PLEX_TOKEN));
    let present = with_token();
    assert!(PLUGIN.refusal(&view(&config, &present), "plex", &Op::Play).is_some());
}

#[tokio::test]
async fn plex_refuses_a_bad_token_and_handles_silence() {
    let mock = MockHttp::start(plex_handler).await;
    let driver = PLUGIN.connect(live(config(&mock.url), "wrong-token")).unwrap();
    assert!(matches!(driver.read_state("plex").await, Err(DeviceError::Rejected(_))));

    let driver = PLUGIN.connect(live(config(NOTHING_LISTENS), PLEX_TOKEN)).unwrap();
    assert!(driver.read_state("plex").await.unwrap_err().is_silence());
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
async fn read_only_adapters_report_a_hung_device_as_silence_and_never_as_a_reading() {
    let plex = MockHttp::start(plex_handler).await;
    plex.hang.store(true, Ordering::SeqCst);
    let driver = PLUGIN.connect(live(config(&plex.url), PLEX_TOKEN)).unwrap();
    assert_gives_up("plex", driver.read_state("plex")).await;
    assert_eq!(plex.requests().len(), 1, "asked once");
}

#[tokio::test]
async fn plex_takes_no_command_and_sends_nothing_for_one() {
    let mock = MockHttp::start(plex_handler).await;
    let driver = PLUGIN.connect(live(config(&mock.url), PLEX_TOKEN)).unwrap();
    for op in [Op::Play, Op::Pause, Op::Volume { level: 5 }] {
        assert!(matches!(driver.command("plex", &op).await, Err(DeviceError::Disabled(_))), "{op:?}");
    }
    assert!(matches!(driver.read_state("tv").await, Err(DeviceError::Disabled(_))));
    assert_eq!(mock.traffic(), 0);
}

#[tokio::test]
async fn any_other_answer_is_a_refusal_that_never_carries_the_token() {
    let mock = MockHttp::start(|_| (503, String::new())).await;
    let driver = PLUGIN.connect(live(config(&mock.url), PLEX_TOKEN)).unwrap();
    let error = driver.read_state("plex").await.unwrap_err();
    assert!(matches!(&error, DeviceError::Rejected(text) if text.contains("503")), "{error:?}");
    assert!(!format!("{error} {error:?}").contains(PLEX_TOKEN));
}

#[test]
fn connecting_needs_a_valid_config_and_the_token() {
    let mut context = live(config("http://192.0.2.20:32400"), PLEX_TOKEN);
    context.secrets = Default::default();
    let refused = PLUGIN.connect(context).err().expect("no token, no driver");
    assert!(refused.0.contains("token"), "{refused}");

    let refused = PLUGIN.connect(live(config("http://192.0.2.20:32400/web"), PLEX_TOKEN)).err().unwrap();
    assert!(refused.0.contains("url"), "{refused}");

    let refused = PLUGIN.connect(live(config("http://192.0.2.20:32400"), "line\nbreak")).err().unwrap();
    assert!(!refused.0.contains("line"), "{refused}");
}
