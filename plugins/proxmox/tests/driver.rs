//! The driver against the mock Proxmox on loopback: the tests of the hub's
//! adapter, under the same names, and the machine extension.

mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::*;
use serde_json::json;
use swifty_plugin_api::{
    Capability, ConsoleGrant, DeviceClass, DeviceError, Driver, ExtraHost, HistoryRange, Op, Plugin, Secrets, State,
};
use swifty_plugin_proxmox::{NOT_A_CONSOLE, PLUGIN, PRIMARY_HOST};
use swifty_plugin_testkit::devices::{MockProxmox, MockTlsPve, PROXMOX_TOKEN, VNC_PORT, VNC_TICKET};

fn token() -> Secrets {
    Secrets::of([("token", PROXMOX_TOKEN)])
}

async fn proxmox() -> (MockProxmox, std::sync::Arc<dyn Driver>) {
    let mock = MockProxmox::start(NODE).await;
    let driver = connect(config(&mock.http.url), token(), four_vms());
    (mock, driver)
}

#[tokio::test]
async fn proxmox_power_uses_the_token_and_the_status_endpoints() {
    let (mock, driver) = proxmox().await;
    driver.command("vm101", &Op::VmShutdown).await.unwrap();
    driver.command("vm101", &Op::VmStart).await.unwrap();
    driver.command("vm101", &Op::VmReboot).await.unwrap();
    let posts = mock.http.posts();
    let paths: Vec<&str> = posts.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/api2/json/nodes/pve/qemu/101/status/shutdown",
            "/api2/json/nodes/pve/qemu/101/status/start",
            "/api2/json/nodes/pve/qemu/101/status/reboot",
        ]
    );
    assert!(posts.iter().all(|r| r.header("authorization") == format!("PVEAPIToken={PROXMOX_TOKEN}")));
    assert!(posts.iter().all(|r| r.query.is_empty() && r.body.is_empty()), "no parameter may be added");
}

#[tokio::test]
async fn proxmox_status_is_read_back_from_the_api() {
    let (mock, driver) = proxmox().await;
    assert_eq!(driver.read_state("vm100").await.unwrap(), State::Vm { state: "running".to_string(), uptime_s: 5_000 });
    mock.vms.lock().unwrap().get_mut(&100).unwrap().status = "stopped".to_string();
    assert!(matches!(driver.read_state("vm100").await.unwrap(), State::Vm { state, .. } if state == "stopped"));
    assert_eq!(driver.read_state("host").await.unwrap(), State::Host { up: true, uptime_s: 90_000 });
    // A token that cannot read the node still shows that the API answers.
    mock.node.lock().unwrap().audit_forbidden = true;
    assert_eq!(driver.read_state("host").await.unwrap(), State::Host { up: true, uptime_s: 0 });
}

#[tokio::test]
async fn proxmox_refuses_everything_outside_its_allow_list_without_a_request() {
    let (mock, driver) = proxmox().await;
    let config = config(&mock.http.url);
    let adopted = four_vms();
    let refused = [
        ("vm104", Op::VmStart),
        ("vm99", Op::VmShutdown),
        ("vm100", Op::VmForceStop),
        ("vm100", Op::HostShutdown),
        ("vm100", Op::HostReboot),
        ("vm100", Op::WireguardRestart),
    ];
    for (target, op) in refused {
        assert!(with_view(&config, &adopted, |view| PLUGIN.refusal(view, target, &op)).is_some(), "{target} {op:?}");
        assert!(matches!(driver.command(target, &op).await, Err(DeviceError::Disabled(_))), "{target} {op:?}");
    }
    assert!(matches!(driver.read_state("vm104").await, Err(DeviceError::Disabled(_))));
    assert_eq!(mock.http.requests().len(), 0);
}

#[tokio::test]
async fn proxmox_token_problems_are_refusals_and_never_show_the_token() {
    let mock = MockProxmox::start(NODE).await;
    let driver =
        connect(config(&mock.http.url), Secrets::of([("token", "swifty@pve!home=wrong-secret-value")]), four_vms());
    let error = driver.command("vm101", &Op::VmStart).await.unwrap_err();
    assert!(matches!(error, DeviceError::Rejected(_)));
    let text = error.to_string();
    assert!(text.contains("token"), "{text}");
    assert!(!text.contains("wrong-secret-value"), "{text}");
}

#[tokio::test]
async fn proxmox_needs_a_well_formed_token_file_and_a_real_ca_file() {
    let mock = MockProxmox::start(NODE).await;
    let bad_token = Secrets::of([("token", "just-a-secret")]);
    assert!(try_connect(config(&mock.http.url), bad_token, four_vms(), timeouts()).is_err());

    let bad_ca = Secrets::of([("token", PROXMOX_TOKEN), ("ca", "not a certificate")]);
    assert!(try_connect(config(&mock.http.url), bad_ca, four_vms(), timeouts()).is_err());
    assert_eq!(mock.http.traffic(), 0);
}

/// Generous, but far below the 120 seconds a hung mock would take.
const OUTER_LIMIT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn a_device_that_never_answers_times_out_and_is_asked_once() {
    let pve = MockProxmox::start(NODE).await;
    pve.http.hang.store(true, Ordering::SeqCst);
    let driver = connect(config(&pve.http.url), token(), four_vms());
    let started = Instant::now();
    let result = tokio::time::timeout(OUTER_LIMIT, driver.command("vm100", &Op::VmStart))
        .await
        .unwrap_or_else(|_| panic!("proxmox never gave up"));
    assert!(started.elapsed() < OUTER_LIMIT, "proxmox");
    assert!(matches!(&result, Err(DeviceError::Timeout)), "proxmox: {result:?}");
    assert_eq!(pve.http.requests().len(), 1);
}

// ----- the machine extension

#[tokio::test]
async fn the_vm_list_leaves_out_templates_and_becomes_pending_candidates() {
    let (mock, driver) = proxmox().await;
    mock.node.lock().unwrap().listed = vec![(150, "golden image".to_string(), true), (151, "spare".to_string(), false)];
    let seen = machines(&driver).list_machines().await.unwrap();
    let mut ids: Vec<u32> = seen.iter().map(|machine| machine.vmid).collect();
    ids.sort_unstable();
    assert_eq!(ids, [100, 101, 102, 103, 151]);
    assert!(seen.iter().all(|machine| machine.host == PRIMARY_HOST));

    let candidates = driver.discover(Duration::from_secs(1)).await.unwrap();
    let spare = candidates.iter().find(|decl| decl.local_id == "vm151").unwrap();
    assert_eq!(spare.class, DeviceClass::Vm);
    assert!(spare.class.needs_approval(), "a VM waits for approval");
    assert_eq!(spare.target_hint.as_deref(), Some("vm151"));
    assert_eq!(spare.name, "spare");
    assert_eq!(spare.attrs, json!({ "host": PRIMARY_HOST, "vmid": 151 }));
    let with_console = candidates.iter().find(|decl| decl.local_id == "vm102").unwrap();
    assert!(with_console.capabilities.contains(&Capability::Console));
    assert!(!spare.capabilities.contains(&Capability::Console));
}

#[tokio::test]
async fn history_reads_the_node_and_approved_vms_only() {
    let (mock, driver) = proxmox().await;
    let host = machines(&driver).history("host", HistoryRange::Day).await.unwrap();
    assert_eq!(
        host,
        [
            json!({ "time": 1000, "cpu": 0.5, "maxcpu": 32, "memused": 1024 }),
            json!({ "time": 1060, "cpu": 0.25, "maxcpu": 32, "memused": 2048 })
        ]
    );
    let vm = machines(&driver).history("vm101", HistoryRange::Hour).await.unwrap();
    assert_eq!(vm, [json!({ "time": 2000, "cpu": 0.75, "netin": 12.5 })]);
    assert!(matches!(machines(&driver).history("vm104", HistoryRange::Week).await, Err(DeviceError::Disabled(_))));
    let requests = mock.node.lock().unwrap().rrd_requests.clone();
    assert_eq!(requests, ["node?timeframe=day&cf=AVERAGE", "101?timeframe=hour&cf=AVERAGE"]);
}

#[tokio::test]
async fn the_console_is_only_for_vms_on_the_console_list() {
    let (mock, driver) = proxmox().await;
    let host = machines(&driver);
    for (device, why) in [("vm101", NOT_A_CONSOLE), ("vm104", "vm104 is not on the approved list.")] {
        match host.open_console(device).await {
            Err(DeviceError::Disabled(text)) => assert_eq!(text, why),
            other => panic!("{device}: {other:?}"),
        }
        let stray = ConsoleGrant { target: device.to_string(), port: VNC_PORT, ticket: VNC_TICKET.to_string().into() };
        assert!(matches!(host.connect_console(&stray).await, Err(DeviceError::Disabled(_))));
    }
    assert!(mock.node.lock().unwrap().vncproxy_requests.is_empty());
    assert_eq!(mock.console.lock().unwrap().connections, 0);

    let grant = host.open_console("vm102").await.unwrap();
    assert_eq!((grant.target.as_str(), grant.port, grant.ticket.expose()), ("vm102", VNC_PORT, VNC_TICKET));
    assert_eq!(mock.node.lock().unwrap().vncproxy_requests, ["102:websocket=1"]);

    let mut link = host.connect_console(&grant).await.unwrap();
    assert_eq!(link.recv().await.unwrap(), b"RFB 003.008\n");
    link.send(b"hello".to_vec()).await.unwrap();
    assert_eq!(link.recv().await.unwrap(), b"vm:hello");
    link.close().await;
    let console = mock.console.lock().unwrap().clone();
    assert_eq!(console.connections, 1);
    assert!(console.uri.contains(&("port".to_string(), VNC_PORT.to_string())), "{:?}", console.uri);
    assert!(console.uri.contains(&("vncticket".to_string(), VNC_TICKET.to_string())), "{:?}", console.uri);
    assert_eq!(console.authorization, [format!("PVEAPIToken={PROXMOX_TOKEN}")]);
}

#[tokio::test]
async fn a_console_ticket_is_never_shown() {
    let (_mock, driver) = proxmox().await;
    let grant = machines(&driver).open_console("vm102").await.unwrap();
    assert!(!format!("{grant:?}").contains(VNC_TICKET));
    // The same grant against a host that has gone: the error says nothing of the ticket.
    let gone = connect(config("http://127.0.0.1:9"), token(), four_vms());
    let error = match machines(&gone).connect_console(&grant).await {
        Err(error) => error,
        Ok(_) => panic!("nothing listens there"),
    };
    assert!(!error.to_string().contains(VNC_TICKET), "{error}");
    assert!(!format!("{error:?}").contains(VNC_TICKET), "{error:?}");
}

#[tokio::test]
async fn a_further_host_is_reached_with_its_own_token_and_only_its_ca() {
    let primary = MockProxmox::start(NODE).await;
    let further = MockTlsPve::start("pve2", 200).await;
    let files = tempfile::tempdir().unwrap();
    let mut config = config(&primary.http.url);
    config["host_files_dir"] = json!(files.path());
    let mut adopted = four_vms();
    adopted.push(vm("two-vm200"));
    let driver = connect(config, token(), adopted);

    // Not loaded yet: its VM is there, its host is not.
    assert!(matches!(driver.command("two-vm200", &Op::VmStart).await, Err(DeviceError::Disabled(_))));
    assert!(further.requests.lock().unwrap().is_empty());

    let spec = ExtraHost {
        id: "two".to_string(),
        address: further.address.parse().unwrap(),
        node: "pve2".to_string(),
        token_file: write_file(files.path(), "two.credential", PROXMOX_TOKEN),
        ca_file: write_file(files.path(), "two-ca.crt", &further.ca_pem),
    };
    machines(&driver).add_host(&spec).unwrap();

    let seen = machines(&driver).list_machines().await.unwrap();
    assert!(seen.iter().any(|machine| machine.host == "two" && machine.vmid == 200), "{seen:?}");
    driver.command("two-vm200", &Op::VmStart).await.unwrap();
    assert_eq!(driver.read_state("two-vm200").await.unwrap(), State::Vm { state: "running".to_string(), uptime_s: 5 });
    {
        let requests = further.requests.lock().unwrap();
        assert!(requests.iter().any(|r| r.starts_with("POST /api2/json/nodes/pve2/qemu/200/status/start ")));
        assert!(requests.iter().all(|r| r.contains(&format!("PVEAPIToken={PROXMOX_TOKEN}"))));
    }
    // The configured host's VMs did not move.
    assert!(primary.http.posts().is_empty());

    machines(&driver).remove_host("two");
    assert!(matches!(driver.command("two-vm200", &Op::VmStart).await, Err(DeviceError::Disabled(_))));
}

#[tokio::test]
async fn a_further_host_with_another_cas_certificate_gets_no_token_and_hides_no_other_vm() {
    let primary = MockProxmox::start(NODE).await;
    let further = MockTlsPve::start("pve2", 200).await;
    let other = MockTlsPve::start("pve2", 200).await;
    let files = tempfile::tempdir().unwrap();
    let mut config = config(&primary.http.url);
    config["host_files_dir"] = json!(files.path());
    let driver = connect(config, token(), four_vms());
    let spec = ExtraHost {
        id: "two".to_string(),
        address: further.address.parse().unwrap(),
        node: "pve2".to_string(),
        token_file: write_file(files.path(), "two.credential", PROXMOX_TOKEN),
        ca_file: write_file(files.path(), "two-ca.crt", &other.ca_pem),
    };
    machines(&driver).add_host(&spec).unwrap();
    let seen = machines(&driver).list_machines().await.unwrap();
    assert_eq!(seen.len(), 4, "the configured host's VMs are still listed: {seen:?}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(further.requests.lock().unwrap().is_empty(), "the token reached an untrusted server");
}
