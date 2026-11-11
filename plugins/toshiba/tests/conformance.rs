//! The conformance suite, against the test kit's mock of Toshiba's cloud.

mod common;

use std::collections::BTreeMap;

use common::{config, login};
use swifty_plugin_api::{Op, Scalar, SetupInput};
use swifty_plugin_testkit::conformance::{run_every_check, Fixture};
use swifty_plugin_testkit::devices::{MockToshiba, AC_IDS, TOSHIBA_EMAIL, TOSHIBA_PASSWORD};
use swifty_plugin_toshiba::{LOGIN_SLOT, PLUGIN};

fn values(pairs: &[(&str, Scalar)]) -> SetupInput {
    let values: BTreeMap<String, Scalar> = pairs.iter().map(|(key, value)| (key.to_string(), value.clone())).collect();
    SetupInput::Values { values }
}

#[tokio::test]
async fn conformance() {
    let cloud = MockToshiba::start().await;
    let target = cloud.target(0);
    let fixture = Fixture {
        config: config(&cloud),
        secrets: vec![(LOGIN_SLOT.to_string(), login())],
        ops: vec![
            (target.clone(), Op::AirconPower { on: true }),
            (target.clone(), Op::AirconMode { mode: "heat".to_string() }),
            (target.clone(), Op::AirconTemp { celsius: 23 }),
            (target.clone(), Op::AirconFan { fan: "low".to_string() }),
            (target, Op::AirconPower { on: false }),
        ],
        setup: vec![
            values(&[("accept", Scalar::Bool(true))]),
            values(&[
                ("account", Scalar::Text(TOSHIBA_EMAIL.to_string())),
                ("sign_in", Scalar::Text(TOSHIBA_PASSWORD.to_string())),
            ]),
            SetupInput::Chosen { ids: AC_IDS.iter().map(|id| id.to_string()).collect() },
        ],
        device: Box::new(cloud),
        ..Fixture::empty()
    };
    run_every_check(&PLUGIN, fixture).await;
}
