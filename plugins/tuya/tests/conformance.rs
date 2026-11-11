//! The conformance suite every plugin runs, with this plugin's fixture.

mod common;

use common::*;
use serde_json::json;
use swifty_plugin_api::{Op, Plugin, Scalar, SetupInput, SetupStep};
use swifty_plugin_testkit::conformance::{run_every_check, Fixture};
use swifty_plugin_testkit::devices::{MockTuya, TUYA_KEY, TUYA_KEY_BASE64};
use swifty_plugin_tuya::PLUGIN;

const SOCKET: &str = "bf0123456789abcdef";

/// The inputs that set the fixture's socket up by hand ("I have the keys").
fn setup_script(plug: &MockTuya) -> Vec<SetupInput> {
    vec![
        chosen(&["manual"]),
        values(&[
            ("id", text(&plug.id)),
            ("address", text("127.0.0.1")),
            ("port", Scalar::Int(i64::from(plug.port))),
            ("local_key", text(std::str::from_utf8(TUYA_KEY).unwrap())),
            ("another", Scalar::Bool(false)),
        ]),
        chosen(&[plug.id.as_str()]),
    ]
}

async fn fixture() -> Fixture {
    let plug = MockTuya::start(SOCKET).await;
    plug.state.lock().unwrap().usb = Some(false);
    let target = plug.target();
    let mut config_row = row(&plug);
    config_row["usb_dp"] = json!(7);
    Fixture {
        config: json!({ "plugs": [config_row] }),
        secrets: vec![key(&plug, TUYA_KEY_BASE64)],
        adopted: Vec::new(),
        ops: vec![
            (target.clone(), Op::PlugPower { on: true }),
            (target.clone(), Op::PlugPower { on: false }),
            (format!("{target}-usb"), Op::PlugPower { on: true }),
        ],
        setup: setup_script(&plug),
        device: Box::new(plug),
    }
}

#[tokio::test]
async fn conformance() {
    run_every_check(&PLUGIN, fixture().await).await;
}

/// The fixture's setup script, run here until the suite's check 11 arrives: it
/// reaches `Done` against the mock, live, with the key only in `secrets`.
#[tokio::test]
async fn the_setup_script_reaches_done() {
    let plug = MockTuya::start(SOCKET).await;
    let ctx = context(true, &[]);
    let mut flow = PLUGIN.setup().unwrap();
    let mut step = flow.begin(&ctx, None).await;
    for input in setup_script(&plug) {
        assert!(!matches!(step, SetupStep::Done(_) | SetupStep::Failed { .. }), "{step:?}");
        step = flow.advance(&ctx, input).await;
    }
    let SetupStep::Done(result) = step else { panic!("{step:?}") };
    PLUGIN.validate(&result.config).unwrap();
    assert!(!result.config.to_string().contains(TUYA_KEY_BASE64));
    assert_eq!(result.secrets.len(), 1);
    assert_eq!(plug.traffic(), 1, "one read of the chosen socket");
}
