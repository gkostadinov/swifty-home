//! WiZ bulbs on UDP, and the broadcast address that discovery reaches.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

use super::LOOPBACK;
use crate::conformance::Switches;

// ----- WiZ bulb

#[derive(Debug, Clone, PartialEq)]
pub struct BulbState {
    pub on: bool,
    pub dimming: u64,
    pub rgb: Option<(u64, u64, u64)>,
    pub temp: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulbMode {
    Normal,
    /// Hears everything and answers nothing.
    Silent,
    /// Says yes to setPilot and changes nothing.
    AcksOnly,
    Error,
    Garbage,
}

pub struct MockBulb {
    pub addr: SocketAddr,
    pub state: Arc<Mutex<BulbState>>,
    pub mode: Arc<Mutex<BulbMode>>,
    pub received: Arc<Mutex<Vec<Value>>>,
    pub mac: Arc<Mutex<String>>,
    /// The conformance suite's switches: a fault spoils each reply, and a name
    /// to report stands in for the MAC address a reading carries.
    pub switches: Switches,
    task: JoinHandle<()>,
}

impl Drop for MockBulb {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn pilot_json(state: &BulbState, mac: &str) -> Value {
    let mut result = json!({ "mac": mac, "rssi": -50, "state": state.on, "sceneId": 0 });
    if state.on {
        result["dimming"] = json!(state.dimming);
        if let Some((r, g, b)) = state.rgb {
            result["r"] = json!(r);
            result["g"] = json!(g);
            result["b"] = json!(b);
        } else if let Some(temp) = state.temp {
            result["temp"] = json!(temp);
        }
    }
    result
}

impl MockBulb {
    pub async fn start() -> Self {
        Self::start_at(LOOPBACK, "000000000000").await
    }

    pub async fn start_with_mac(mac: &str) -> Self {
        Self::start_at(LOOPBACK, mac).await
    }

    /// A bulb at a chosen loopback address.
    pub async fn start_at(address: &str, mac: &str) -> Self {
        let socket = UdpSocket::bind(address).await.unwrap();
        let addr = socket.local_addr().unwrap();
        let state = Arc::new(Mutex::new(BulbState { on: true, dimming: 60, rgb: None, temp: Some(2700) }));
        let mode = Arc::new(Mutex::new(BulbMode::Normal));
        let received = Arc::new(Mutex::new(Vec::new()));
        let mac = Arc::new(Mutex::new(mac.to_string()));
        let switches = Switches::default();
        let (s, m, r, own_mac, w) = (state.clone(), mode.clone(), received.clone(), mac.clone(), switches.clone());
        let task = tokio::spawn(async move {
            let mut buffer = [0u8; 2048];
            loop {
                // A closed client makes recv fail on Windows, so errors are skipped.
                let Ok((length, from)) = socket.recv_from(&mut buffer).await else { continue };
                let Ok(message) = serde_json::from_slice::<Value>(&buffer[..length]) else { continue };
                // Sent by the broadcast mock: answer as a bulb hearing the broadcast.
                if message["method"] == "__announce" {
                    let peer: SocketAddr = message["peer"].as_str().unwrap().parse().unwrap();
                    let mac = own_mac.lock().unwrap().clone();
                    let reply = json!({ "method": "getPilot", "env": "pro", "result": { "mac": mac, "rssi": -50, "state": true } });
                    let _ = socket.send_to(reply.to_string().as_bytes(), peer).await;
                    continue;
                }
                r.lock().unwrap().push(message.clone());
                let mode = *m.lock().unwrap();
                let reply = match mode {
                    BulbMode::Silent => continue,
                    BulbMode::Garbage => "this is not json".to_string(),
                    BulbMode::Error => {
                        json!({ "method": message["method"], "error": { "code": -32600, "message": "Invalid Request" } }).to_string()
                    }
                    BulbMode::Normal | BulbMode::AcksOnly => {
                        Self::answer(&message, &s, mode == BulbMode::Normal, &w.device_text("000000000000")).to_string()
                    }
                };
                let _ = socket.send_to(&w.garble(reply.into_bytes()), from).await;
            }
        });
        Self { addr, state, mode, received, mac, switches, task }
    }

    fn answer(message: &Value, state: &Mutex<BulbState>, apply: bool, mac: &str) -> Value {
        let method = message["method"].as_str().unwrap_or("");
        match method {
            "getPilot" => {
                json!({ "method": "getPilot", "env": "pro", "result": pilot_json(&state.lock().unwrap(), mac) })
            }
            "setPilot" => {
                let params = &message["params"];
                // The real bulbs refuse a brightness under 10.
                if params["dimming"].as_u64().is_some_and(|d| d < 10) {
                    return json!({ "method": "setPilot", "error": { "code": -32602, "message": "Invalid params" } });
                }
                if apply {
                    let mut bulb = state.lock().unwrap();
                    if let Some(on) = params["state"].as_bool() {
                        bulb.on = on;
                    }
                    if let Some(dimming) = params["dimming"].as_u64() {
                        bulb.dimming = dimming;
                    }
                    if let (Some(r), Some(g), Some(b)) =
                        (params["r"].as_u64(), params["g"].as_u64(), params["b"].as_u64())
                    {
                        bulb.rgb = Some((r, g, b));
                        bulb.temp = None;
                    }
                }
                json!({ "method": "setPilot", "env": "pro", "result": { "success": true } })
            }
            _ => json!({ "method": method, "error": { "code": -32601, "message": "Method not found" } }),
        }
    }

    pub fn set_mode(&self, mode: BulbMode) {
        *self.mode.lock().unwrap() = mode;
    }

    pub fn methods(&self) -> Vec<String> {
        self.received.lock().unwrap().iter().map(|m| m["method"].as_str().unwrap_or("").to_string()).collect()
    }

    pub fn count(&self, method: &str) -> usize {
        self.methods().iter().filter(|m| *m == method).count()
    }

    pub fn traffic(&self) -> usize {
        self.received.lock().unwrap().len()
    }
}

/// What a broadcast reaches. It tells each bulb behind it to answer from its own
/// socket, so a reply's address is the bulb's address, as on a real network.
pub struct MockBroadcast {
    pub addr: SocketAddr,
    pub datagrams: Arc<Mutex<Vec<Value>>>,
    answered_by: Arc<Mutex<Vec<SocketAddr>>>,
    task: JoinHandle<()>,
}

impl Drop for MockBroadcast {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockBroadcast {
    /// Takes the top address of 127.0.0.0/24, and starts one bulb per MAC inside that
    /// network on the same port, because real bulbs all listen on one port.
    pub async fn start(macs: &[&str]) -> (Self, Vec<MockBulb>) {
        let socket = UdpSocket::bind("127.0.0.255:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        let mut bulbs = Vec::new();
        for (index, mac) in macs.iter().enumerate() {
            bulbs.push(MockBulb::start_at(&format!("127.0.0.{}:{}", index + 2, addr.port()), mac).await);
        }
        let answered_by = Arc::new(Mutex::new(bulbs.iter().map(|bulb| bulb.addr).collect::<Vec<_>>()));
        let datagrams = Arc::new(Mutex::new(Vec::new()));
        let (shared, responders) = (datagrams.clone(), answered_by.clone());
        let task = tokio::spawn(async move {
            let mut buffer = [0u8; 2048];
            loop {
                let Ok((length, from)) = socket.recv_from(&mut buffer).await else { continue };
                let Ok(message) = serde_json::from_slice::<Value>(&buffer[..length]) else { continue };
                shared.lock().unwrap().push(message);
                let everyone = responders.lock().unwrap().clone();
                for bulb in &everyone {
                    let order = json!({ "method": "__announce", "peer": from.to_string() });
                    let _ = socket.send_to(order.to_string().as_bytes(), bulb).await;
                }
            }
        });
        (Self { addr, datagrams, answered_by, task }, bulbs)
    }

    /// From now on, only these answer.
    pub fn answered_only_by(&self, who: Vec<SocketAddr>) {
        *self.answered_by.lock().unwrap() = who;
    }

    pub fn also_answered_by(&self, who: SocketAddr) {
        self.answered_by.lock().unwrap().push(who);
    }

    pub fn traffic(&self) -> usize {
        self.datagrams.lock().unwrap().len()
    }
}
