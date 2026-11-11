//! The conformance suite every plugin runs.

mod common;

use common::*;
use swifty_plugin_api::Scalar;
use swifty_plugin_camera::PLUGIN;
use swifty_plugin_testkit::conformance::{run_every_check, Fixture};
use swifty_plugin_testkit::devices::{MockRtsp, CAMERA_LOGIN};

#[tokio::test]
async fn conformance() {
    let device = MockRtsp::start().await;
    let fixture = Fixture {
        config: camera_config(device.port),
        secrets: vec![("login".to_string(), CAMERA_LOGIN.to_string())],
        setup: vec![values(&[
            ("host", text("127.0.0.1")),
            ("port", Scalar::Int(i64::from(device.port))),
            ("login", text(CAMERA_LOGIN)),
        ])],
        device: Box::new(device),
        ..Fixture::empty()
    };
    run_every_check(&PLUGIN, fixture).await;
}
