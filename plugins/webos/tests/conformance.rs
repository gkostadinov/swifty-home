//! The conformance suite every plugin runs.
//!
//! The fixture is the test kit's TV on its secure port, so that the config
//! carries a certificate pin as a real one does. It accepts the pairing prompt
//! at once, so the setup reaches `Done`, and it is a TV with quick start: off,
//! it still answers and says so, so a power off can be confirmed.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

use serde_json::json;
use swifty_plugin_api::{Op, Scalar, SetupInput};
use swifty_plugin_testkit::conformance::{run_every_check, Fixture};
use swifty_plugin_testkit::devices::{MockTv, TV_KEY};

#[tokio::test]
async fn conformance() {
    let device = MockTv::start_tls().await;
    device.accepts_pairing.store(true, Ordering::SeqCst);
    device.state.lock().unwrap().powered_off_as = "Active Standby".to_string();
    let pin = device.pin.clone().unwrap();
    let form = BTreeMap::from([("url".to_string(), Scalar::Text(device.url.clone()))]);
    let fixture = Fixture {
        config: json!({ "url": device.url, "cert_sha256": pin }),
        secrets: vec![("client_key".to_string(), TV_KEY.to_string())],
        ops: vec![
            ("tv".to_string(), Op::TvInput { input: "HDMI 2".to_string() }),
            ("tv".to_string(), Op::TvKey { key: "ok".to_string() }),
            ("tv".to_string(), Op::TvPower { on: false }),
        ],
        // The suite sets the instance up again: the TV still presents the
        // certificate stored for it, so there is nothing to confirm, and the
        // prompt is accepted at once.
        setup: vec![SetupInput::Values { values: form }, SetupInput::Poll, SetupInput::Poll],
        device: Box::new(device),
        ..Fixture::empty()
    };
    run_every_check(&swifty_plugin_webos::PLUGIN, fixture).await;
}
