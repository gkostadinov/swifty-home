//! The conformance suite, and what the pure calls promise: they reach nothing and
//! refuse exactly what the driver refuses.

mod common;

use std::collections::BTreeMap;

use common::*;
use swifty_plugin_api::{DeviceError, Op, Plugin, Scalar, SetupInput};
use swifty_plugin_moonraker::PLUGIN;
use swifty_plugin_testkit::conformance::{run_every_check, Fixture};
use swifty_plugin_testkit::devices::{MockMoonraker, MOONRAKER_KEY};
use swifty_plugin_testkit::trap::Trap;

#[tokio::test]
async fn conformance() {
    let mock = MockMoonraker::start().await;
    let url = mock.http.url.clone();
    let values: BTreeMap<String, Scalar> =
        [("url".to_string(), Scalar::Text(url.clone())), ("api_key".to_string(), Scalar::Text(MOONRAKER_KEY.into()))]
            .into();
    let fixture = Fixture {
        config: config(&url),
        secrets: vec![("api_key".to_string(), MOONRAKER_KEY.to_string())],
        adopted: Vec::new(),
        device: Box::new(mock),
        ops: vec![
            ("printer".to_string(), Op::PrintStart),
            ("printer".to_string(), Op::PrintPause),
            ("printer".to_string(), Op::PrintResume),
            ("printer".to_string(), Op::PrintEmergencyStop),
        ],
        setup: vec![SetupInput::Values { values }],
    };
    run_every_check(&PLUGIN, fixture).await;
}

#[tokio::test]
async fn the_pure_calls_reach_nothing() {
    let trap = Trap::start().await;
    let config = config(&format!("http://{}", trap.addr));
    PLUGIN.validate(&config).unwrap();
    with_view(&config, |view| {
        PLUGIN.readiness(view);
        assert_eq!(PLUGIN.devices(view).len(), 1);
        for op in [Op::PrintStart, Op::PrintEmergencyStop, Op::VmStart] {
            PLUGIN.refusal(view, "printer", &op);
        }
    });
    assert_eq!(trap.traffic(), 0);
}

#[tokio::test]
async fn refusals_agree_with_the_driver_and_a_refused_op_sends_nothing() {
    let mock = MockMoonraker::start().await;
    let config = config(&mock.http.url);
    let driver = connect(&mock.http.url);
    let ops = [
        Op::PrintStart,
        Op::PrintPause,
        Op::PrintResume,
        Op::PrintEmergencyStop,
        Op::VmStart,
        Op::HostReboot,
        Op::LightPower { on: false },
        Op::Volume { level: 5 },
    ];
    let mut refused = 0;
    for device in ["printer", "printer2", "host", ""] {
        for op in &ops {
            let Some(reason) = with_view(&config, |view| PLUGIN.refusal(view, device, op)) else { continue };
            refused += 1;
            match driver.command(device, op).await {
                Err(DeviceError::Disabled(text)) => assert_eq!(text, reason, "{device} {op:?}"),
                other => panic!("{device} {op:?}: refused in dry-run, but live gave {other:?}"),
            }
        }
    }
    assert_eq!(refused, 28);
    assert_eq!(mock.http.traffic(), 0, "a refused op sends nothing");
}
