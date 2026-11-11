//! An LG webOS TV on its websocket, and the Wake-on-LAN listener that powers it on.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, UdpSocket};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::handshake::server::{Request as WsRequest, Response as WsResponse};
use tokio_tungstenite::tungstenite::Message;

use super::LOOPBACK;
use crate::conformance::Switches;

pub const TV_KEY: &str = "tv-client-key-0123456789";

// ----- LG TV

/// Where a TV's pointer socket is, under its own address.
pub const POINTER_PATH: &str = "/resources/abc123/netinput.pointer.sock";

#[derive(Debug, Clone)]
pub struct TvState {
    pub power: String,
    pub app: String,
    pub accepted_key: String,
    /// False: commands are accepted and change nothing.
    pub apply: bool,
    /// Requests for this URI are answered with an error.
    pub refuse_uri: Option<String>,
    /// Accepts the next power off and then stops answering anything.
    pub silent_after_off: bool,
    pub silent: bool,
    /// What the TV says when asked where its pointer socket is.
    pub socket_path: String,
    /// The power state `turnOff` leaves it in: `Off` (the default) has no
    /// websocket at all, as a TV that powers its network down; `Active Standby`
    /// keeps answering and says it is off, as a TV with quick start does.
    pub powered_off_as: String,
}

pub struct MockTv {
    pub url: String,
    pub state: Arc<Mutex<TvState>>,
    pub connections: Arc<AtomicUsize>,
    pub received: Arc<Mutex<Vec<Value>>>,
    /// How many on-screen pairing prompts have been raised.
    pub prompts: Arc<AtomicUsize>,
    /// While set, the TV finishes the handshake and then says nothing.
    pub hang: Arc<AtomicBool>,
    /// Every text frame that reached the pointer socket.
    pub frames: Arc<Mutex<Vec<String>>>,
    pub pointer_connections: Arc<AtomicUsize>,
    /// The SHA-256 of its certificate, when it is on TLS.
    pub pin: Option<String>,
    /// While set, a registration without a key raises the prompt and the person
    /// accepts it at once: the TV says the prompt is up, then `registered` with
    /// its key. Unset, the prompt stays up and nobody accepts it.
    pub accepts_pairing: Arc<AtomicBool>,
    /// The conformance suite's switches: a fault spoils each reply frame, and a
    /// name to report stands in for the app in the foreground.
    pub switches: Switches,
    task: JoinHandle<()>,
}

/// What each connection to a mock TV shares.
#[derive(Clone)]
struct TvLink {
    state: Arc<Mutex<TvState>>,
    received: Arc<Mutex<Vec<Value>>>,
    prompts: Arc<AtomicUsize>,
    hang: Arc<AtomicBool>,
    frames: Arc<Mutex<Vec<String>>>,
    pointer_connections: Arc<AtomicUsize>,
    accepts_pairing: Arc<AtomicBool>,
    switches: Switches,
}

impl TvLink {
    // The handshake callback's error type belongs to tungstenite.
    #[allow(clippy::result_large_err)]
    async fn serve<S: AsyncRead + AsyncWrite + Unpin>(self, stream: S) {
        // A TV that is off has no websocket to offer.
        if self.state.lock().unwrap().power == "Off" {
            return;
        }
        let path = Arc::new(Mutex::new(String::new()));
        let seen = path.clone();
        let handshake =
            tokio_tungstenite::accept_hdr_async(stream, move |request: &WsRequest, response: WsResponse| {
                *seen.lock().unwrap() = request.uri().path().to_string();
                Ok(response)
            });
        let Ok(mut ws) = handshake.await else { return };
        if *path.lock().unwrap() == POINTER_PATH {
            self.pointer_connections.fetch_add(1, Ordering::SeqCst);
            while let Some(Ok(message)) = ws.next().await {
                if let Message::Text(text) = message {
                    self.frames.lock().unwrap().push(text.to_string());
                }
            }
            return;
        }
        while let Some(Ok(message)) = ws.next().await {
            let Message::Text(text) = message else { continue };
            let value: Value = serde_json::from_str(text.as_str()).unwrap();
            self.received.lock().unwrap().push(value.clone());
            if self.hang.load(Ordering::SeqCst) || self.state.lock().unwrap().silent {
                continue;
            }
            let (reply, close) = MockTv::answer(&value, &self.state, &self.prompts, &self.switches);
            let keyless = value["type"] == "register" && value["payload"].get("client-key").is_none();
            let accepted = keyless && self.accepts_pairing.load(Ordering::SeqCst);
            let _ = ws.send(Message::text(self.switches.garble_text(reply.to_string()))).await;
            if accepted {
                let key = self.state.lock().unwrap().accepted_key.clone();
                let registered = json!({ "type": "registered", "id": value["id"], "payload": { "client-key": key } });
                let _ = ws.send(Message::text(self.switches.garble_text(registered.to_string()))).await;
            }
            if close {
                let _ = ws.close(None).await;
                return;
            }
        }
    }
}

impl Drop for MockTv {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockTv {
    pub async fn start() -> Self {
        Self::listen(None, None).await
    }

    /// On a self-signed certificate, as a real TV is on its secure port.
    pub async fn start_tls() -> Self {
        use tokio_rustls::rustls::pki_types::PrivateKeyDer;
        use tokio_rustls::rustls::{crypto, ServerConfig};
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["tv.test".to_string()]).unwrap().self_signed(&key).unwrap();
        let pin = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(cert.der()));
        let private = PrivateKeyDer::try_from(key.serialize_der()).unwrap();
        let config = ServerConfig::builder_with_provider(Arc::new(crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert.der().clone()], private)
            .unwrap();
        Self::listen(Some(TlsAcceptor::from(Arc::new(config))), Some(pin)).await
    }

    async fn listen(acceptor: Option<TlsAcceptor>, pin: Option<String>) -> Self {
        let listener = TcpListener::bind(LOOPBACK).await.unwrap();
        let scheme = if acceptor.is_some() { "wss" } else { "ws" };
        let url = format!("{scheme}://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(TvState {
            power: "Active".to_string(),
            app: "com.webos.app.hdmi1".to_string(),
            accepted_key: TV_KEY.to_string(),
            apply: true,
            refuse_uri: None,
            silent_after_off: false,
            silent: false,
            socket_path: format!("{url}{POINTER_PATH}"),
            powered_off_as: "Off".to_string(),
        }));
        let connections = Arc::new(AtomicUsize::new(0));
        let received = Arc::new(Mutex::new(Vec::new()));
        let prompts = Arc::new(AtomicUsize::new(0));
        let hang = Arc::new(AtomicBool::new(false));
        let frames = Arc::new(Mutex::new(Vec::new()));
        let pointer_connections = Arc::new(AtomicUsize::new(0));
        let accepts_pairing = Arc::new(AtomicBool::new(false));
        let switches = Switches::default();
        let link = TvLink {
            state: state.clone(),
            received: received.clone(),
            prompts: prompts.clone(),
            hang: hang.clone(),
            frames: frames.clone(),
            pointer_connections: pointer_connections.clone(),
            accepts_pairing: accepts_pairing.clone(),
            switches: switches.clone(),
        };
        let counter = connections.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { continue };
                counter.fetch_add(1, Ordering::SeqCst);
                let (link, acceptor) = (link.clone(), acceptor.clone());
                tokio::spawn(async move {
                    match acceptor {
                        None => link.serve(stream).await,
                        Some(acceptor) => {
                            if let Ok(tls) = acceptor.accept(stream).await {
                                link.serve(tls).await;
                            }
                        }
                    }
                });
            }
        });
        Self {
            url,
            state,
            connections,
            received,
            prompts,
            hang,
            frames,
            pointer_connections,
            pin,
            accepts_pairing,
            switches,
            task,
        }
    }

    fn answer(message: &Value, state: &Mutex<TvState>, prompts: &AtomicUsize, switches: &Switches) -> (Value, bool) {
        let id = message["id"].clone();
        let mut tv = state.lock().unwrap();
        if message["type"] == "register" {
            if message["payload"]["client-key"].as_str() == Some(tv.accepted_key.as_str()) {
                return (
                    json!({ "type": "registered", "id": id, "payload": { "client-key": tv.accepted_key } }),
                    false,
                );
            }
            prompts.fetch_add(1, Ordering::SeqCst);
            return (
                json!({ "type": "response", "id": id, "payload": { "pairingType": "PROMPT", "returnValue": true } }),
                false,
            );
        }
        let ok = json!({ "type": "response", "id": id, "payload": { "returnValue": true } });
        if message["uri"].as_str() == tv.refuse_uri.as_deref() {
            return (
                json!({ "type": "error", "id": id, "error": "401 insufficient permissions", "payload": {} }),
                false,
            );
        }
        match message["uri"].as_str().unwrap_or("") {
            "ssap://com.webos.service.tvpower/power/getPowerState" => {
                (json!({ "type": "response", "id": id, "payload": { "returnValue": true, "state": tv.power } }), false)
            }
            "ssap://com.webos.applicationManager/getForegroundAppInfo" => {
                let app = switches.device_text(&tv.app);
                (json!({ "type": "response", "id": id, "payload": { "returnValue": true, "appId": app } }), false)
            }
            "ssap://com.webos.service.networkinput/getPointerInputSocket" => (
                json!({ "type": "response", "id": id, "payload": { "returnValue": true, "socketPath": tv.socket_path } }),
                false,
            ),
            "ssap://system/turnOff" => {
                let apply = tv.apply;
                if apply {
                    tv.power = tv.powered_off_as.clone();
                }
                if tv.silent_after_off {
                    tv.silent = true;
                }
                (ok, apply)
            }
            "ssap://system.launcher/launch" => {
                if tv.apply {
                    tv.app = message["payload"]["id"].as_str().unwrap_or("").to_string();
                }
                (ok, false)
            }
            _ => (json!({ "type": "error", "id": id, "error": "404 no such service or method", "payload": {} }), false),
        }
    }

    pub fn traffic(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    pub fn uris(&self) -> Vec<String> {
        self.received.lock().unwrap().iter().filter_map(|m| m["uri"].as_str().map(str::to_string)).collect()
    }
}

// ----- Wake-on-LAN listener

pub struct MockWol {
    pub addr: SocketAddr,
    pub packets: Arc<Mutex<Vec<Vec<u8>>>>,
    task: JoinHandle<()>,
}

impl Drop for MockWol {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockWol {
    /// A magic packet powers `tv` on, as the real thing would.
    pub async fn start(tv: Arc<Mutex<TvState>>) -> Self {
        let socket = UdpSocket::bind(LOOPBACK).await.unwrap();
        let addr = socket.local_addr().unwrap();
        let packets = Arc::new(Mutex::new(Vec::new()));
        let shared = packets.clone();
        let task = tokio::spawn(async move {
            let mut buffer = [0u8; 512];
            loop {
                let Ok((length, _)) = socket.recv_from(&mut buffer).await else { continue };
                shared.lock().unwrap().push(buffer[..length].to_vec());
                tv.lock().unwrap().power = "Active".to_string();
            }
        });
        Self { addr, packets, task }
    }

    pub fn traffic(&self) -> usize {
        self.packets.lock().unwrap().len()
    }
}
