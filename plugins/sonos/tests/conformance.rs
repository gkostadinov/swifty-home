//! The shared conformance suite, every check.

use std::collections::BTreeMap;

use serde_json::json;
use swifty_plugin_api::{Op, Scalar, SetupInput};
use swifty_plugin_testkit::conformance::{run_every_check, Fixture};
use swifty_plugin_testkit::devices::MockSonos;

#[tokio::test]
async fn conformance() {
    let device = MockSonos::start(true).await;
    let address = device.http.url.trim_start_matches("http://").trim_end_matches('/').to_string();
    let fixture = Fixture {
        config: json!({ "speakers": [{ "id": "sonos", "url": device.http.url }] }),
        ops: vec![
            ("sonos".to_string(), Op::Volume { level: 30 }),
            ("sonos".to_string(), Op::VolumeCap { max: 20 }),
            ("sonos".to_string(), Op::Mute { muted: true }),
            ("sonos".to_string(), Op::NightMode { on: true }),
            ("sonos".to_string(), Op::Pause),
            ("sonos".to_string(), Op::Play),
        ],
        setup: vec![SetupInput::Values { values: BTreeMap::from([("address".to_string(), Scalar::Text(address))]) }],
        device: Box::new(device),
        ..Fixture::empty()
    };
    run_every_check(&swifty_plugin_sonos::PLUGIN, fixture).await;
}
