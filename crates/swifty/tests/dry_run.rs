//! Proof that dry-run sends nothing. A plugin whose driver really opens sockets
//! is pointed at loopback listeners that count every connection and datagram. A
//! live control run shows the counters do detect traffic.

mod common;

use std::net::{TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use common::*;
use serde_json::{json, Value};
use swifty_hub::runtime::Registry;
use swifty_hub::secrets::MemorySecretStore;
use swifty_plugin_api::{
    Capability, CommandAck, ConfigProblem, DeviceClass, DeviceDecl, DeviceError, Driver, InstanceView, LiveContext,
    Manifest, Op, Plugin, PluginError, Reach, Readiness, State, API_VERSION,
};

/// Loopback endpoints that record anything that reaches them.
struct Traps {
    tcp_port: u16,
    udp: UdpSocket,
    tcp_hits: Arc<AtomicUsize>,
}

impl Traps {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let tcp_port = listener.local_addr().unwrap().port();
        let tcp_hits = Arc::new(AtomicUsize::new(0));
        let hits = tcp_hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stream.is_ok() {
                    hits.fetch_add(1, Ordering::SeqCst);
                }
            }
        });
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        udp.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        Self { tcp_port, udp, tcp_hits }
    }

    fn plugin(&self) -> &'static SocketPlugin {
        let ports = (self.tcp_port, self.udp.local_addr().unwrap().port());
        Box::leak(Box::new(SocketPlugin { ports }))
    }

    fn tcp_count(&self) -> usize {
        std::thread::sleep(Duration::from_millis(100));
        self.tcp_hits.load(Ordering::SeqCst)
    }

    fn udp_received(&self) -> bool {
        let mut buffer = [0u8; 16];
        self.udp.recv_from(&mut buffer).is_ok()
    }
}

static SOCKETS: Manifest = Manifest {
    id: "sockets",
    name: "Sockets",
    version: "0",
    api: API_VERSION,
    summary: "A light whose every call opens real sockets.",
    reach: Reach::Local,
    network: &[],
    classes: &[DeviceClass::Light],
    config_version: 1,
    config: &[],
    secrets: &[],
    actions: &[],
    vocabulary: &[],
    nouns: &[],
    multi_instance: false,
};

/// One light, `bulb1`, that a real plugin would reach on the network.
struct SocketPlugin {
    ports: (u16, u16),
}

impl Plugin for SocketPlugin {
    fn manifest(&self) -> &'static Manifest {
        &SOCKETS
    }
    fn validate(&self, _config: &Value) -> Result<(), ConfigProblem> {
        Ok(())
    }
    fn readiness(&self, _instance: &InstanceView<'_>) -> Readiness {
        Readiness::Ready
    }
    fn devices(&self, _instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
        vec![DeviceDecl {
            local_id: "bulb1".to_string(),
            target_hint: Some("bulb1".to_string()),
            class: DeviceClass::Light,
            name: "Bulb 1".to_string(),
            capabilities: vec![Capability::Power, Capability::Brightness { min: 10 }, Capability::Color],
            attrs: json!({}),
        }]
    }
    fn refusal(&self, _instance: &InstanceView<'_>, _device: &str, _op: &Op) -> Option<String> {
        None
    }
    fn connect(&self, _ctx: LiveContext) -> Result<Arc<dyn Driver>, PluginError> {
        Ok(Arc::new(SocketDriver { ports: self.ports }))
    }
}

/// Behaves the way a real driver does: every call opens real sockets.
struct SocketDriver {
    ports: (u16, u16),
}

impl SocketDriver {
    fn touch(&self) {
        let (tcp, udp) = self.ports;
        let _ = TcpStream::connect(("127.0.0.1", tcp));
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let _ = socket.send_to(b"packet", ("127.0.0.1", udp));
    }
}

#[async_trait]
impl Driver for SocketDriver {
    async fn command(&self, _device: &str, _op: &Op) -> Result<CommandAck, DeviceError> {
        self.touch();
        Ok(CommandAck)
    }

    async fn read_state(&self, _device: &str) -> Result<State, DeviceError> {
        self.touch();
        Ok(State::Light { on: false, bri: 60, hue: 38, color: true })
    }
}

fn traps_harness(live: bool, traps: &Traps) -> Harness {
    let plugin = traps.plugin();
    Harness::with_registry(
        |site| {
            site.live = live;
            site.add("wiz", "sockets", json!({}));
        },
        |config| Registry::new(live, vec![plugin], Arc::new(MemorySecretStore::default()), (&config.network).into()),
    )
}

#[tokio::test]
async fn dry_run_opens_no_socket_and_says_it_was_simulated() {
    let traps = Traps::new();
    let harness = traps_harness(false, &traps);
    let mut auth = authenticator();
    harness.enroll(&mut auth, "iPhone").await;
    let cookie = harness.login(&mut auth).await;

    let done = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    done.assert_ok();
    assert_eq!(done.phase(), "simulated");
    assert_eq!(done.body["simulated"], json!(true));
    assert!(done.body["detail"].as_str().unwrap().contains("no device was contacted"));

    let scene = harness.execute(&cookie, "scene.apply", "focus", json!({})).await;
    assert_eq!(scene.phase(), "simulated");

    let read = harness.execute(&cookie, "state.read", "bulb1", json!({})).await;
    assert_eq!(read.phase(), "simulated");
    let state = harness.get("/api/v1/state/bulb1", &cookie).await;
    assert_eq!(state.body["simulated"], json!(true));

    assert_eq!(traps.tcp_count(), 0, "dry-run must not open a TCP connection");
    assert!(!traps.udp_received(), "dry-run must not send a datagram");
    assert!(harness.audit_has("simulated"));
    assert!(!harness.audit_has("confirmed"));
}

#[tokio::test]
async fn the_same_adapter_does_reach_the_network_when_live() {
    let traps = Traps::new();
    let harness = traps_harness(true, &traps);
    let mut auth = authenticator();
    harness.enroll(&mut auth, "iPhone").await;
    let cookie = harness.login(&mut auth).await;

    let done = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    assert_eq!(done.phase(), "confirmed");
    assert!(traps.tcp_count() >= 2, "a command and a read-back each connect");
    assert!(traps.udp_received());
}

#[tokio::test]
async fn dry_run_never_calls_any_driver() {
    let harness = Harness::with(|c| c.live = false);
    let devices = harness.devices.clone();
    let mut auth = authenticator();
    harness.enroll(&mut auth, "iPhone").await;
    let cookie = harness.login(&mut auth).await;

    harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await.assert_ok();
    let prepared = harness.prepare(&cookie, "pets.feed", "feeder", json!({ "grams": 10 })).await;
    assert_eq!(harness.confirm_tap(&cookie, &prepared).await.phase(), "simulated");
    let critical = harness.critical(&cookie, &mut auth, "vm.stop", "vm101", json!({})).await;
    assert_eq!(critical.phase(), "simulated");
    harness.get("/api/v1/state", &cookie).await.assert_ok();

    assert_eq!(devices.total_commands(), 0);
    assert_eq!(devices.total_reads(), 0);
    assert_eq!(devices.state("bulb1"), Some(State::Light { on: true, bri: 60, hue: 38, color: true }));
}

#[tokio::test]
async fn the_policy_still_applies_in_dry_run() {
    let (_rig, harness, _auth, cookie) = rig_harness(false, |_| {}).await;

    harness
        .execute(&cookie, "vm.stop", "vm101", json!({}))
        .await
        .assert_refused(axum::http::StatusCode::FORBIDDEN, "step_up_required");
    harness
        .execute(&cookie, "light.brightness", "bulb1", json!({ "percent": 500 }))
        .await
        .assert_refused(axum::http::StatusCode::UNPROCESSABLE_ENTITY, "invalid_params");
    let simulated = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": true })).await;
    assert_eq!(simulated.phase(), "simulated");

    let devices = harness.get("/api/v1/devices", &cookie).await;
    assert_eq!(devices.body["live"], json!(false));
}

#[tokio::test]
async fn the_shipped_defaults_are_dry_run() {
    let config = swifty_hub::config::Config::parse(&config_text(std::path::Path::new("x.db"), "")).unwrap();
    assert!(!config.live);
    let example = include_str!("../../../packaging/swifty.example.toml");
    let parsed = swifty_hub::config::Config::parse(example).unwrap();
    assert!(!parsed.live, "the example configuration must not enable live mode");
}
