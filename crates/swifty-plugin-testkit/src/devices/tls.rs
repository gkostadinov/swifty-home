//! TLS servers whose certificate a private CA signed for a chosen name, for the
//! trust tests of a plugin that speaks HTTPS or a TLS websocket: the CA that
//! should be trusted, one that should not, and a certificate for another host.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::rustls::{crypto, ServerConfig};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Message;

use super::LOOPBACK;

/// A private certificate authority and a server certificate it signed.
pub struct PrivateCa {
    /// The CA's certificate, in PEM: what a client must trust.
    pub ca_pem: String,
    acceptor: TlsAcceptor,
}

impl PrivateCa {
    /// A new CA, and a server certificate it signed for `name`: a host name or
    /// an IP address, such as `127.0.0.1` for a server the client reaches by
    /// that address, or another name for a server that is not who it seems.
    pub fn new(name: &str) -> Self {
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut ca_params = rcgen::CertificateParams::new(Vec::new()).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let leaf_key = rcgen::KeyPair::generate().unwrap();
        let leaf =
            rcgen::CertificateParams::new(vec![name.to_string()]).unwrap().signed_by(&leaf_key, &ca, &ca_key).unwrap();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
        let chain: Vec<CertificateDer<'static>> = vec![leaf.der().clone()];
        let config = ServerConfig::builder_with_provider(Arc::new(crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .unwrap();
        Self { ca_pem: ca.pem(), acceptor: TlsAcceptor::from(Arc::new(config)) }
    }
}

/// An HTTPS server on loopback. It records what a client sends once the
/// handshake is done (the request head and what arrived with it) and answers
/// every request `200` with one JSON body. A client that does not trust its
/// certificate never gets as far as being recorded.
pub struct MockTlsHttp {
    /// `https://` and its address.
    pub url: String,
    /// Its address, `127.0.0.1:<port>`.
    pub address: String,
    /// The CA that signed its certificate.
    pub ca_pem: String,
    /// What each connection sent after the handshake, oldest first.
    pub requests: Arc<Mutex<Vec<String>>>,
    /// Every connection it accepted, handshake or not.
    pub connections: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl Drop for MockTlsHttp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockTlsHttp {
    /// On a certificate for `name`, answering every request with `body`.
    pub async fn start(name: &str, body: &str) -> Self {
        let PrivateCa { ca_pem, acceptor } = PrivateCa::new(name);
        let listener = TcpListener::bind(LOOPBACK).await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let (shared, counter, body) = (requests.clone(), connections.clone(), body.to_string());
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { continue };
                counter.fetch_add(1, Ordering::SeqCst);
                let (acceptor, shared, body) = (acceptor.clone(), shared.clone(), body.clone());
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(stream).await else { return };
                    let mut buffer = [0u8; 4096];
                    let Ok(length) = tls.read(&mut buffer).await else { return };
                    shared.lock().unwrap().push(String::from_utf8_lossy(&buffer[..length]).to_string());
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = tls.write_all(reply.as_bytes()).await;
                    let _ = tls.shutdown().await;
                });
            }
        });
        Self { url: format!("https://{address}"), address, ca_pem, requests, connections, task }
    }

    /// Every connection that has reached it.
    pub fn traffic(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

/// A websocket over TLS on loopback that echoes every binary frame, as a VM's
/// console relay does once it is open. For each handshake that got through
/// TLS it records the request's path and query, a space, and its
/// `Authorization` header, so a test can see what was presented and to whom.
pub struct MockTlsWs {
    /// Its address, `127.0.0.1:<port>`.
    pub address: String,
    /// The CA that signed its certificate.
    pub ca_pem: String,
    /// `<path?query> <authorization>` for each websocket handshake, oldest first.
    pub seen: Arc<Mutex<Vec<String>>>,
    /// Every connection it accepted, handshake or not.
    pub connections: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl Drop for MockTlsWs {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockTlsWs {
    /// On a certificate for `name`. It answers the `binary` subprotocol.
    pub async fn start(name: &str) -> Self {
        let PrivateCa { ca_pem, acceptor } = PrivateCa::new(name);
        let listener = TcpListener::bind(LOOPBACK).await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let (shared, counter) = (seen.clone(), connections.clone());
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { continue };
                counter.fetch_add(1, Ordering::SeqCst);
                let (acceptor, shared) = (acceptor.clone(), shared.clone());
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(stream).await else { return };
                    // The handshake callback's error type belongs to tungstenite.
                    #[allow(clippy::result_large_err)]
                    let callback = move |request: &Request, mut response: Response| {
                        let authorization =
                            request.headers().get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
                        shared.lock().unwrap().push(format!("{} {authorization}", request.uri()));
                        response.headers_mut().insert("sec-websocket-protocol", "binary".parse().unwrap());
                        Ok(response)
                    };
                    let Ok(mut socket) = tokio_tungstenite::accept_hdr_async(tls, callback).await else { return };
                    while let Some(Ok(Message::Binary(data))) = socket.next().await {
                        let _ = socket.send(Message::binary(data)).await;
                    }
                });
            }
        });
        Self { address, ca_pem, seen, connections, task }
    }

    /// Every connection that has reached it.
    pub fn traffic(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}
