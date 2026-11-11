//! The conformance suite every plugin runs.

mod common;

use common::*;
use swifty_plugin_plex::PLUGIN;
use swifty_plugin_testkit::conformance::{run_every_check, Fixture};
use swifty_plugin_testkit::devices::{plex_rigged, MockHttp, PLEX_TOKEN};

#[tokio::test]
async fn conformance() {
    let device = MockHttp::start_rigged(plex_rigged).await;
    let fixture = Fixture {
        config: config(&device.url),
        secrets: vec![("token".to_string(), PLEX_TOKEN.to_string())],
        setup: vec![values(&[("url", &device.url), ("token", PLEX_TOKEN)])],
        device: Box::new(device),
        ..Fixture::empty()
    };
    run_every_check(&PLUGIN, fixture).await;
}
