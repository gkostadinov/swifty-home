//! The conformance suite every plugin runs.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde_json::json;
use swifty_net::{LiveGrant, Timeouts};
use swifty_plugin_api::{Op, Plugin, Scalar, SetupContext, SetupInput, SetupStep};
use swifty_plugin_testkit::conformance::{run_every_check, Fixture};
use swifty_plugin_testkit::devices::MockBulb;
use swifty_plugin_wiz::PLUGIN;

/// The fixture's setup script, run here once, so it is known to reach Done.
async fn assert_reaches_done(script: &[SetupInput]) {
    let ctx = SetupContext {
        instance_id: "wiz".to_string(),
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
    let bulb = MockBulb::start().await;
    let address = bulb.addr.to_string();
    let addresses: BTreeMap<String, Scalar> = [
        ("broadcast".to_string(), Scalar::Text(String::new())),
        ("addresses".to_string(), Scalar::Text(format!("bulb1={address}"))),
    ]
    .into();
    let setup = vec![SetupInput::Values { values: addresses }];
    assert_reaches_done(&setup).await;
    let fixture = Fixture {
        config: json!({ "bulbs": [{ "id": "bulb1", "address": address }] }),
        device: Box::new(bulb),
        ops: vec![
            ("bulb1".to_string(), Op::LightPower { on: false }),
            ("bulb1".to_string(), Op::LightBrightness { percent: 40 }),
            ("bulb1".to_string(), Op::LightHue { hue: 120 }),
            ("bulb1".to_string(), Op::LightSet { percent: 70, hue: 200 }),
        ],
        setup,
        ..Fixture::empty()
    };
    run_every_check(&PLUGIN, fixture).await;
}
