//! The conformance suite every plugin runs.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde_json::json;
use swifty_net::{LiveGrant, Timeouts};
use swifty_plugin_api::{DeviceClass, DeviceRecord, Op, Plugin, Scalar, SetupContext, SetupInput, SetupStep};
use swifty_plugin_hue::PLUGIN;
use swifty_plugin_testkit::conformance::{run_every_check, Fixture};
use swifty_plugin_testkit::devices::{hue_bridge_rigged, HueLight, MockHttp, SharedBridge, HUE_KEY};

/// The fixture's setup script, run here once, so it is known to reach Done.
async fn assert_reaches_done(script: &[SetupInput]) {
    let ctx = SetupContext {
        instance_id: "hue".to_string(),
        grant: LiveGrant::when_live(true),
        timeouts: Timeouts { connect: Duration::from_millis(300), read: Duration::from_millis(300) },
        secrets_present: BTreeSet::new(),
    };
    let mut flow = PLUGIN.setup().unwrap();
    let mut step = flow.begin(&ctx, None).await;
    for input in script {
        step = flow.advance(&ctx, input.clone()).await;
    }
    let SetupStep::Done(result) = step else { panic!("the setup script ends at {step:?}") };
    PLUGIN.validate(&result.config).unwrap();
}

#[tokio::test]
async fn conformance() {
    let bridge = SharedBridge::default();
    bridge.lock().unwrap().lights.insert("1".to_string(), HueLight::colour("Lightstrip"));
    let lights = hue_bridge_rigged(bridge);
    // A bridge whose link button is already pressed, so the setup script reaches Done.
    let device = MockHttp::start_rigged(move |seen, switches| {
        if seen.method == "POST" && seen.path == "/api" {
            return (200, json!([{ "success": { "username": HUE_KEY } }]).to_string());
        }
        lights(seen, switches)
    })
    .await;
    let url = device.url.clone();
    let values: BTreeMap<String, Scalar> = [("url".to_string(), Scalar::Text(url.clone()))].into();
    let setup =
        vec![SetupInput::Values { values }, SetupInput::Poll, SetupInput::Chosen { ids: vec!["hue-1".to_string()] }];
    assert_reaches_done(&setup).await;
    let fixture = Fixture {
        config: json!({ "url": url }),
        secrets: vec![("app_key".to_string(), HUE_KEY.to_string())],
        adopted: vec![DeviceRecord {
            target: "hue-1".to_string(),
            local_id: "hue-1".to_string(),
            class: DeviceClass::Light,
            name: "Lightstrip".to_string(),
            attrs: json!({ "light": "1", "colour": true }),
        }],
        device: Box::new(device),
        ops: vec![
            ("hue-1".to_string(), Op::LightPower { on: true }),
            ("hue-1".to_string(), Op::LightBrightness { percent: 40 }),
            ("hue-1".to_string(), Op::LightHue { hue: 120 }),
            ("hue-1".to_string(), Op::LightSet { percent: 70, hue: 200 }),
        ],
        setup,
    };
    run_every_check(&PLUGIN, fixture).await;
}
