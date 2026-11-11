//! A Proxmox VE host: its HTTP API, its VNC relay, and an HTTPS host with a private CA.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade};
use axum::extract::Query;
use axum::http::HeaderMap;
use axum::routing::get;
use axum::Router;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use super::http::MockHttp;
use super::LOOPBACK;

pub const PROXMOX_TOKEN: &str = "swifty@pve!home=11111111-2222-3333-4444-555555555555";

// ----- Proxmox

#[derive(Debug, Clone)]
pub struct VmState {
    pub status: String,
    pub uptime: u64,
}

/// The Proxmox node itself, and what has been asked of it.
#[derive(Debug, Clone)]
pub struct NodeState {
    pub uptime: u64,
    pub commands: Vec<String>,
    /// Stops answering once it has shut down.
    pub down: bool,
    /// As a token without `Sys.Audit` on the node sees it.
    pub audit_forbidden: bool,
    /// As a token without `Sys.PowerMgmt` on the node sees it.
    pub power_forbidden: bool,
    /// VMs that are there but not among the four it starts with: id, name, template.
    pub listed: Vec<(u32, String, bool)>,
    pub rrd_requests: Vec<String>,
    pub vncproxy_requests: Vec<String>,
}

/// What reached the VNC websocket.
#[derive(Debug, Clone, Default)]
pub struct ConsoleLog {
    pub uri: Vec<(String, String)>,
    pub authorization: Vec<String>,
    pub from_browser: Vec<Vec<u8>>,
    pub connections: usize,
}

pub struct MockProxmox {
    pub http: MockHttp,
    pub vms: Arc<Mutex<HashMap<u32, VmState>>>,
    /// False: commands are accepted and change nothing.
    pub apply: Arc<AtomicBool>,
    pub node: Arc<Mutex<NodeState>>,
    pub console: Arc<Mutex<ConsoleLog>>,
}

pub const VNC_TICKET: &str = "PVEVNC:6900ABCD::vnc-ticket-from-proxmox";
pub const VNC_PORT: u16 = 5901;

/// A VM's console as Proxmox offers it: the banner first, then an echo.
async fn serve_vnc(mut socket: WebSocket, log: Arc<Mutex<ConsoleLog>>) {
    if socket.send(AxumMessage::Binary(b"RFB 003.008\n".to_vec().into())).await.is_err() {
        return;
    }
    while let Some(Ok(message)) = socket.recv().await {
        if let AxumMessage::Binary(data) = message {
            // Lets a test end the session from the VM's side.
            if data.as_ref() == b"__vm_goes_away" {
                return;
            }
            log.lock().unwrap().from_browser.push(data.to_vec());
            let mut reply = b"vm:".to_vec();
            reply.extend_from_slice(&data);
            if socket.send(AxumMessage::Binary(reply.into())).await.is_err() {
                return;
            }
        }
    }
}

impl MockProxmox {
    pub async fn start(node: &'static str) -> Self {
        let vms: HashMap<u32, VmState> = [100, 101, 102, 103]
            .into_iter()
            .map(|id| (id, VmState { status: "running".to_string(), uptime: 5_000 }))
            .collect();
        let vms = Arc::new(Mutex::new(vms));
        let apply = Arc::new(AtomicBool::new(true));
        let node_state = Arc::new(Mutex::new(NodeState {
            uptime: 90_000,
            commands: Vec::new(),
            down: false,
            audit_forbidden: false,
            power_forbidden: false,
            listed: Vec::new(),
            rrd_requests: Vec::new(),
            vncproxy_requests: Vec::new(),
        }));
        let console = Arc::new(Mutex::new(ConsoleLog::default()));
        let console_for_route = console.clone();
        let vnc_route =
            get(move |upgrade: WebSocketUpgrade, headers: HeaderMap, Query(query): Query<Vec<(String, String)>>| {
                let log = console_for_route.clone();
                async move {
                    {
                        let mut log = log.lock().unwrap();
                        log.connections += 1;
                        log.uri = query;
                        log.authorization
                            .push(headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string());
                    }
                    upgrade.protocols(["binary"]).on_upgrade(move |socket| serve_vnc(socket, log))
                }
            });
        let extra = Router::new().route(&format!("/api2/json/nodes/{node}/qemu/{{id}}/vncwebsocket"), vnc_route);
        let (shared, apply_flag, node_shared) = (vms.clone(), apply.clone(), node_state.clone());
        let http = MockHttp::start_with_rigged(extra, move |seen, switches| {
            if node_shared.lock().unwrap().down {
                return (503, String::new());
            }
            if seen.header("authorization") != format!("PVEAPIToken={PROXMOX_TOKEN}") {
                return (401, String::new());
            }
            if seen.path == "/api2/json/version" {
                return (200, json!({ "data": { "version": "9.0" } }).to_string());
            }
            if seen.path == format!("/api2/json/nodes/{node}/status") {
                let mut state = node_shared.lock().unwrap();
                if seen.method == "POST" {
                    if state.power_forbidden {
                        return (403, String::new());
                    }
                    state.commands.push(seen.body.clone());
                    if apply_flag.load(Ordering::SeqCst) {
                        match seen.body.as_str() {
                            "command=reboot" => state.uptime = 3,
                            "command=shutdown" => state.down = true,
                            _ => return (400, String::new()),
                        }
                    }
                    return (200, json!({ "data": null }).to_string());
                }
                if state.audit_forbidden {
                    return (403, String::new());
                }
                return (200, json!({ "data": { "uptime": state.uptime } }).to_string());
            }
            if seen.path == format!("/api2/json/nodes/{node}/qemu") {
                let state = node_shared.lock().unwrap();
                let mut rows: Vec<Value> = shared
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(id, vm)| {
                        json!({ "vmid": id, "name": switches.device_text(&format!("vm-{id}")), "status": vm.status })
                    })
                    .collect();
                for (id, name, template) in &state.listed {
                    rows.push(
                        json!({ "vmid": id, "name": name, "status": "stopped", "template": u8::from(*template) }),
                    );
                }
                return (200, json!({ "data": rows }).to_string());
            }
            if seen.path == format!("/api2/json/nodes/{node}/rrddata") {
                node_shared.lock().unwrap().rrd_requests.push(format!("node?{}", seen.query));
                return (
                    200,
                    json!({ "data": [
                    { "time": 1000, "cpu": 0.5, "maxcpu": 32, "memused": 1024, "name": "not-a-number" },
                    { "time": 1060, "cpu": 0.25, "maxcpu": 32, "memused": 2048 },
                ] })
                    .to_string(),
                );
            }
            let prefix = format!("/api2/json/nodes/{node}/qemu/");
            let Some(rest) = seen.path.strip_prefix(&prefix) else { return (404, String::new()) };
            let parts: Vec<&str> = rest.split('/').collect();
            if let [id, "rrddata"] = parts[..] {
                node_shared.lock().unwrap().rrd_requests.push(format!("{id}?{}", seen.query));
                return (200, json!({ "data": [{ "time": 2000, "cpu": 0.75, "netin": 12.5 }] }).to_string());
            }
            if let [id, "vncproxy"] = parts[..] {
                let mut state = node_shared.lock().unwrap();
                state.vncproxy_requests.push(format!("{id}:{}", seen.body));
                return (
                    200,
                    json!({ "data": { "ticket": VNC_TICKET, "port": VNC_PORT, "upid": "UPID:x", "user": "x" } })
                        .to_string(),
                );
            }
            let [id, "status", leaf] = parts[..] else { return (404, String::new()) };
            let id: u32 = id.parse().unwrap();
            let mut vms = shared.lock().unwrap();
            let Some(vm) = vms.get_mut(&id) else { return (500, String::new()) };
            let apply = apply_flag.load(Ordering::SeqCst);
            match (seen.method.as_str(), leaf) {
                ("GET", "current") => {
                    let status = switches.device_text(&vm.status);
                    (200, json!({ "data": { "status": status, "qmpstatus": status, "uptime": vm.uptime } }).to_string())
                }
                ("POST", "start" | "shutdown" | "reboot") => {
                    if apply {
                        match leaf {
                            "start" => *vm = VmState { status: "running".to_string(), uptime: 1 },
                            "shutdown" => *vm = VmState { status: "stopped".to_string(), uptime: 0 },
                            _ => *vm = VmState { status: "running".to_string(), uptime: 2 },
                        }
                    }
                    (200, json!({ "data": "UPID:mock" }).to_string())
                }
                _ => (403, String::new()),
            }
        })
        .await;
        Self { http, vms, apply, node: node_state, console }
    }
}

/// A Proxmox API over https, with a private CA, for a further host. Records what
/// reaches it once the handshake is done.
pub struct MockTlsPve {
    pub address: String,
    pub ca_pem: String,
    pub requests: Arc<Mutex<Vec<String>>>,
    pub running: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl Drop for MockTlsPve {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockTlsPve {
    pub async fn start(node: &'static str, vmid: u32) -> Self {
        use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
        use tokio_rustls::rustls::{crypto, ServerConfig};
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut ca_params = rcgen::CertificateParams::new(Vec::new()).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let leaf_key = rcgen::KeyPair::generate().unwrap();
        let leaf_params = rcgen::CertificateParams::new(vec!["127.0.0.1".to_string()]).unwrap();
        let leaf = leaf_params.signed_by(&leaf_key, &ca, &ca_key).unwrap();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
        let chain: Vec<CertificateDer<'static>> = vec![leaf.der().clone()];
        let config = ServerConfig::builder_with_provider(Arc::new(crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind(LOOPBACK).await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let running = Arc::new(AtomicBool::new(false));
        let (shared, power) = (requests.clone(), running.clone());
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { continue };
                let (acceptor, shared, power) = (acceptor.clone(), shared.clone(), power.clone());
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(stream).await else { return };
                    let mut buffer = [0u8; 4096];
                    let Ok(length) = tls.read(&mut buffer).await else { return };
                    let text = String::from_utf8_lossy(&buffer[..length]).to_string();
                    let line = text.lines().next().unwrap_or("").to_string();
                    shared.lock().unwrap().push(text);
                    let base = format!("/api2/json/nodes/{node}/qemu");
                    let body = if line.starts_with(&format!("GET {base} ")) {
                        json!({ "data": [{ "vmid": vmid, "name": "further-vm", "status": "stopped" }] })
                    } else if line.contains("/status/current") {
                        let status = if power.load(Ordering::SeqCst) { "running" } else { "stopped" };
                        json!({ "data": { "status": status, "uptime": 5 } })
                    } else if line.contains("/status/start") {
                        power.store(true, Ordering::SeqCst);
                        json!({ "data": "UPID:mock" })
                    } else {
                        json!({ "data": null })
                    }
                    .to_string();
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = tls.write_all(reply.as_bytes()).await;
                    let _ = tls.shutdown().await;
                });
            }
        });
        Self { address, ca_pem: ca.pem(), requests, running, task }
    }
}
