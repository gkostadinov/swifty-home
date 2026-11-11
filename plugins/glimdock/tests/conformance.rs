//! The conformance suite every plugin runs.

mod common;

use common::*;
use swifty_plugin_glimdock::PLUGIN;
use swifty_plugin_testkit::conformance::{run_every_check, Fixture};
use swifty_plugin_testkit::devices::{collector_rigged, MockHttp, COLLECTOR_TOKEN, SNAPSHOT_PATH};

#[tokio::test]
async fn conformance() {
    let device = MockHttp::start_rigged(collector_rigged).await;
    let config = pairing_config(&device.url, SNAPSHOT_PATH);
    let setup = vec![values(&[
        ("url", &device.url),
        ("snapshot_path", SNAPSHOT_PATH),
        ("token", COLLECTOR_TOKEN),
        ("pairing_path", PAIRING_PATH),
    ])];
    let fixture = Fixture {
        config,
        secrets: vec![("token".to_string(), COLLECTOR_TOKEN.to_string())],
        device: Box::new(device),
        setup,
        ..Fixture::empty()
    };
    run_every_check(&PLUGIN, fixture).await;
}
