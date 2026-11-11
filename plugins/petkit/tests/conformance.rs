//! The conformance suite, against the test kit's mock of Petkit's cloud.

mod common;

use std::collections::BTreeMap;

use common::{login, petkit_config};
use serde_json::json;
use swifty_plugin_api::{Op, Scalar, SetupInput};
use swifty_plugin_petkit::{identity, LOGIN_SLOT, PLUGIN};
use swifty_plugin_testkit::conformance::{run_every_check, Fixture};
use swifty_plugin_testkit::devices::{MockPetkit, PETKIT_ACCOUNT};

fn values(pairs: &[(&str, Scalar)]) -> SetupInput {
    let values: BTreeMap<String, Scalar> = pairs.iter().map(|(key, value)| (key.to_string(), value.clone())).collect();
    SetupInput::Values { values }
}

#[tokio::test]
async fn conformance() {
    let mock = MockPetkit::start().await;
    let mut config = petkit_config(&mock, true);
    // Written out, because the suite reads a text default such as "16.1" as a number.
    config["os_version"] = json!(identity::OS_VERSION);
    let fixture = Fixture {
        config,
        secrets: vec![(LOGIN_SLOT.to_string(), login())],
        ops: vec![("feeder".to_string(), Op::PetFeed { grams: 10 }), ("litter".to_string(), Op::LitterClean)],
        setup: vec![
            values(&[("accept", Scalar::Bool(true))]),
            values(&[
                ("region", Scalar::Text("FR".to_string())),
                ("account", Scalar::Text(PETKIT_ACCOUNT.to_string())),
                ("sign_in", Scalar::Text("password".to_string())),
                ("timezone", Scalar::Text("Europe/Paris".to_string())),
                ("utc_offset_minutes", Scalar::Int(120)),
            ]),
            SetupInput::Chosen { ids: vec!["4001".to_string(), "4002".to_string()] },
        ],
        device: Box::new(mock),
        ..Fixture::empty()
    };
    run_every_check(&PLUGIN, fixture).await;
}
