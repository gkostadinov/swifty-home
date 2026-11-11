//! The conformance suite, and what the pure calls promise: they reach nothing, say
//! what is missing, and refuse exactly what the driver refuses.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::*;
use serde_json::json;
use swifty_plugin_api::{DeviceError, InstanceView, Op, Plugin, Readiness, Scalar, Secrets, SetupInput};
use swifty_plugin_proxmox::{CA_MISSING, PLUGIN, TOKEN_MISSING};
use swifty_plugin_testkit::conformance::{run_every_check, Fixture};
use swifty_plugin_testkit::devices::{MockProxmox, PROXMOX_TOKEN};
use swifty_plugin_testkit::trap::Trap;

fn form(url: &str) -> SetupInput {
    let values: BTreeMap<String, Scalar> = [
        ("url", Scalar::Text(url.to_string())),
        ("node", Scalar::Text(NODE.to_string())),
        ("token", Scalar::Text(PROXMOX_TOKEN.to_string())),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value))
    .collect();
    SetupInput::Values { values }
}

#[tokio::test]
async fn conformance() {
    let mock = MockProxmox::start(NODE).await;
    let files = tempfile::tempdir().unwrap();
    let url = mock.http.url.clone();
    let fixture = Fixture {
        config: json!({
            "url": url,
            "node": NODE,
            "public_ca": false,
            "console_targets": [{ "vm": "vm102" }],
            "host_files_dir": files.path(),
        }),
        secrets: vec![("token".to_string(), PROXMOX_TOKEN.to_string())],
        adopted: four_vms(),
        device: Box::new(mock),
        // The reboot comes first: the mock VM's uptime after a start (1 s) is lower
        // than after a reboot (2 s), and a reboot is confirmed by a lower uptime.
        ops: vec![
            ("vm101".to_string(), Op::VmReboot),
            ("vm101".to_string(), Op::VmShutdown),
            ("vm101".to_string(), Op::VmStart),
            ("host".to_string(), Op::HostReboot),
        ],
        setup: vec![
            SetupInput::Values { values: BTreeMap::new() },
            form(&url),
            SetupInput::Chosen { ids: vec!["vm101".to_string(), "vm102".to_string()] },
            SetupInput::Chosen { ids: vec!["vm102".to_string()] },
        ],
    };
    run_every_check(&PLUGIN, fixture).await;
}

#[tokio::test]
async fn the_pure_calls_reach_nothing() {
    let trap = Trap::start().await;
    let config =
        json!({ "url": format!("https://{}", trap.addr), "node": NODE, "console_targets": [{ "vm": "vm102" }] });
    let adopted = four_vms();
    PLUGIN.validate(&config).unwrap();
    with_view(&config, &adopted, |view| {
        PLUGIN.readiness(view);
        assert_eq!(PLUGIN.devices(view).len(), 5);
        for device in ["host", "vm100", "vm104"] {
            for op in [Op::VmStart, Op::HostReboot, Op::VmForceStop] {
                PLUGIN.refusal(view, device, &op);
            }
        }
    });
    assert_eq!(trap.traffic(), 0);
}

#[test]
fn readiness_says_what_is_missing() {
    let readiness = |config: serde_json::Value, slots: &[&str]| {
        let present: BTreeSet<String> = slots.iter().map(|slot| slot.to_string()).collect();
        PLUGIN.readiness(&InstanceView { id: "proxmox", config: &config, secrets_present: &present, adopted: &[] })
    };
    let https = json!({ "url": "https://192.0.2.10:8006", "node": NODE });
    assert_eq!(readiness(https.clone(), &[]), Readiness::NeedsSetup(TOKEN_MISSING.to_string()));
    assert_eq!(readiness(https.clone(), &["token"]), Readiness::NeedsSetup(CA_MISSING.to_string()));
    assert_eq!(readiness(https.clone(), &["token", "ca"]), Readiness::Ready);
    let public = json!({ "url": "https://192.0.2.10:8006", "node": NODE, "public_ca": true });
    assert_eq!(readiness(public, &["token"]), Readiness::Ready);
    let loopback = json!({ "url": "http://127.0.0.1:8006", "node": NODE });
    assert_eq!(readiness(loopback, &["token"]), Readiness::Ready);
    let broken = json!({ "url": "http://192.0.2.10:8006", "node": NODE });
    assert!(matches!(readiness(broken, &["token"]), Readiness::Disabled(why) if why.contains("url")));
}

#[tokio::test]
async fn refusals_agree_with_the_driver_and_a_refused_op_sends_nothing() {
    let mock = MockProxmox::start(NODE).await;
    let config = config(&mock.http.url);
    let adopted = four_vms();
    let driver = connect(config.clone(), Secrets::of([("token", PROXMOX_TOKEN)]), adopted.clone());
    let devices = ["host", "vm100", "vm103", "vm104", "two-vm200", "printer", ""];
    let ops = [
        Op::VmStart,
        Op::VmShutdown,
        Op::VmReboot,
        Op::VmForceStop,
        Op::HostShutdown,
        Op::HostReboot,
        Op::WireguardRestart,
        Op::LightPower { on: true },
        Op::PrintStart,
    ];
    let mut refused = 0;
    for device in devices {
        for op in &ops {
            let Some(reason) = with_view(&config, &adopted, |view| PLUGIN.refusal(view, device, op)) else { continue };
            refused += 1;
            match driver.command(device, op).await {
                Err(DeviceError::Disabled(text)) => assert_eq!(text, reason, "{device} {op:?}"),
                other => panic!("{device} {op:?}: refused in dry-run, but live gave {other:?}"),
            }
        }
    }
    assert!(refused > 40, "{refused}");
    assert_eq!(mock.http.traffic(), 0, "a refused op sends nothing");
}
