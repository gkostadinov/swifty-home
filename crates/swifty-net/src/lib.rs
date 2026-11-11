//! The only code in Swifty Home that can open a socket to a device.
//!
//! Every function and client here needs a [`LiveGrant`], and the hub makes one
//! only from a configuration that says `live = true`. A dry-run process can
//! therefore not construct a real driver at all, on top of the check in the hub's
//! registry that returns before any driver is called.
//!
//! Each helper applies a connect and a read timeout. No helper retries, because
//! only the engine knows whether a command may be sent twice. Error text never
//! comes from the HTTP client, whose messages can include a URL, and the Hue key
//! travels in the URL path.

mod error;

pub use error::DeviceError;

/// The address types a plugin needs, so that it never names `std::net`. They
/// are plain values: nothing here opens a socket.
pub use std::net::{AddrParseError, IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use reqwest::header::{HeaderValue, AUTHORIZATION, RETRY_AFTER};
use reqwest::{RequestBuilder, StatusCode, Url};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{lookup_host, TcpStream, UdpSocket};
use tokio::time::{timeout, timeout_at, Instant};
use tokio_rustls::rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use tokio_rustls::rustls::crypto::{ring, verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio_rustls::rustls::{CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use tokio_rustls::TlsConnector;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

const DEFAULT_MAX_BODY: usize = 1024 * 1024;
const MAX_WS_MESSAGE: usize = 256 * 1024;
const MAX_TCP_REPLY: usize = 8 * 1024;
const MAX_UDP_REPLY: usize = 2 * 1024;
const WS_CLOSE_WAIT: Duration = Duration::from_millis(200);
/// How much of one framebuffer update the console relay will carry at once.
const MAX_CONSOLE_FRAME: usize = 8 * 1024 * 1024;
const MAX_DISCOVERY_REPLIES: usize = 128;
const MAX_DISCOVERY_ERRORS: usize = 8;

/// Proof that the configuration says `live = true`. Its field is private, so
/// [`LiveGrant::when_live`] is the only way to obtain one.
#[derive(Clone, Copy)]
pub struct LiveGrant(());

impl LiveGrant {
    /// A grant when `live` is true, and nothing otherwise. The hub calls this with
    /// its configuration's `live` flag, in one place.
    pub fn when_live(live: bool) -> Option<Self> {
        live.then_some(Self(()))
    }
}

/// How long a helper waits to connect, and then for each reply.
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    /// The longest wait for a connection.
    pub connect: Duration,
    /// The longest wait for a reply, once connected.
    pub read: Duration,
}

/// Device-supplied text made safe to show and store: no control characters, and
/// a bounded length.
pub fn device_text(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(160).collect()
}

/// The SHA-256 of `data`, in lowercase hex.
fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(data))
}

fn broken() -> DeviceError {
    DeviceError::Unreachable("the connection failed".to_string())
}

fn http_error(error: reqwest::Error) -> DeviceError {
    if error.is_timeout() {
        DeviceError::Timeout
    } else if error.is_builder() {
        // A request that could not be built (a header value with a line break in it, from a
        // credential that has one) never left this machine. That is not silence the engine may
        // try again; it is a request this driver cannot make, as a path that is not valid is.
        DeviceError::Disabled("the request is not valid".to_string())
    } else {
        broken()
    }
}

/// An HTTP reply, already read to the end under the size limit.
#[derive(Debug)]
pub struct Reply {
    pub status: StatusCode,
    pub body: Vec<u8>,
    /// How long the server asked to be left alone, when its `Retry-After` gives seconds.
    pub retry_after: Option<Duration>,
}

/// The seconds at the start of a `Retry-After`. Toshiba's cloud writes "60 seconds", so
/// anything after the number is ignored. A date, the other form, gives nothing.
pub fn retry_after_seconds(text: &str) -> Option<Duration> {
    let digits: String = text.trim().chars().take_while(char::is_ascii_digit).take(6).collect();
    digits.parse().ok().map(Duration::from_secs)
}

impl Reply {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json(&self) -> Result<Value, DeviceError> {
        serde_json::from_slice(&self.body)
            .map_err(|_| DeviceError::Rejected("the device sent an unreadable reply".to_string()))
    }
}

/// What a certificate may chain to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Roots {
    /// The public roots and, if one is given, a private CA.
    PublicAndNamedCa,
    /// Only the CA that was named, for a host someone added by address.
    NamedCaOnly,
}

/// Why a client could not be built, before it is put in words.
enum Problem {
    Url,
    NotPem,
    Client,
}

impl Problem {
    /// The sentence, with `not_pem` for a CA that is no certificate.
    fn text(self, not_pem: &str) -> String {
        match self {
            Problem::Url => "the device URL is not valid".to_string(),
            Problem::NotPem => not_pem.to_string(),
            Problem::Client => "the HTTP client could not be built".to_string(),
        }
    }
}

/// An HTTP client for one device.
pub struct Http {
    client: reqwest::Client,
    base: Url,
    max_body: usize,
}

impl Http {
    /// `ca_file` adds a private certificate authority, as Proxmox uses. Invalid
    /// certificates are never accepted.
    pub fn new(grant: LiveGrant, base: &str, timeouts: Timeouts, ca_file: Option<&Path>) -> Result<Self, String> {
        Self::with_roots(grant, base, timeouts, ca_file, Roots::PublicAndNamedCa)
    }

    /// As [`Http::new`], saying what a certificate may chain to.
    pub fn with_roots(
        grant: LiveGrant,
        base: &str,
        timeouts: Timeouts,
        ca_file: Option<&Path>,
        roots: Roots,
    ) -> Result<Self, String> {
        let Some(path) = ca_file else {
            return Self::with_ca_pem(grant, base, timeouts, None, roots);
        };
        let pem =
            std::fs::read(path).map_err(|e| format!("cannot read the certificate file {}: {e}", path.display()))?;
        Self::build(grant, base, timeouts, Some(&pem), roots)
            .map_err(|problem| problem.text(&format!("{} is not a PEM certificate", path.display())))
    }

    /// As [`Http::with_roots`], with the private CA given as PEM text rather
    /// than a file, as a plugin holds it in a secret slot. Nothing is written
    /// anywhere. Invalid certificates are never accepted.
    pub fn with_ca_pem(
        grant: LiveGrant,
        base: &str,
        timeouts: Timeouts,
        ca_pem: Option<&[u8]>,
        roots: Roots,
    ) -> Result<Self, String> {
        Self::build(grant, base, timeouts, ca_pem, roots)
            .map_err(|problem| problem.text("the CA certificate is not a PEM certificate"))
    }

    fn build(
        _grant: LiveGrant,
        base: &str,
        timeouts: Timeouts,
        ca_pem: Option<&[u8]>,
        roots: Roots,
    ) -> Result<Self, Problem> {
        let base = Url::parse(base).map_err(|_| Problem::Url)?;
        let mut builder = reqwest::Client::builder()
            .connect_timeout(timeouts.connect)
            .read_timeout(timeouts.read)
            .timeout(timeouts.connect + timeouts.read)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .tls_built_in_root_certs(roots == Roots::PublicAndNamedCa)
            // A fresh connection per request: the client may resend on a stale
            // pooled one, and nothing here may be sent twice behind the engine.
            .pool_max_idle_per_host(0);
        if let Some(pem) = ca_pem {
            // The parser accepts text with no certificate in it, and every
            // connection would then fail with no sign of why.
            if !String::from_utf8_lossy(pem).contains("-----BEGIN CERTIFICATE-----") {
                return Err(Problem::NotPem);
            }
            let certificate = reqwest::Certificate::from_pem(pem).map_err(|_| Problem::NotPem)?;
            builder = builder.add_root_certificate(certificate);
        }
        let client = builder.build().map_err(|_| Problem::Client)?;
        Ok(Self { client, base, max_body: DEFAULT_MAX_BODY })
    }

    pub fn base(&self) -> &Url {
        &self.base
    }

    pub fn with_max_body(mut self, max_body: usize) -> Self {
        self.max_body = max_body;
        self
    }

    /// A URL on this device. A path can never change the destination, so a
    /// device's token cannot be sent anywhere else.
    pub fn url(&self, path: &str) -> Result<Url, DeviceError> {
        let invalid = || DeviceError::Disabled("the device path is not valid".to_string());
        let url = self.base.join(path).map_err(|_| invalid())?;
        let same_device = url.scheme() == self.base.scheme()
            && url.host() == self.base.host()
            && url.port_or_known_default() == self.base.port_or_known_default();
        if same_device {
            Ok(url)
        } else {
            Err(invalid())
        }
    }

    pub fn get(&self, url: Url) -> RequestBuilder {
        self.client.get(url)
    }

    pub fn post(&self, url: Url) -> RequestBuilder {
        self.client.post(url)
    }

    pub fn put(&self, url: Url) -> RequestBuilder {
        self.client.put(url)
    }

    /// Sends once and hands over the reply as it arrives, for an answer that takes minutes.
    pub async fn stream(
        &self,
        request: RequestBuilder,
        mut each: impl FnMut(&[u8]),
    ) -> Result<StatusCode, DeviceError> {
        let mut response = request.send().await.map_err(http_error)?;
        let status = response.status();
        while let Some(chunk) = response.chunk().await.map_err(http_error)? {
            each(&chunk);
        }
        Ok(status)
    }

    /// Sends once and reads the whole reply.
    pub async fn send(&self, request: RequestBuilder) -> Result<Reply, DeviceError> {
        let mut response = request.send().await.map_err(http_error)?;
        let status = response.status();
        let retry_after =
            response.headers().get(RETRY_AFTER).and_then(|value| value.to_str().ok()).and_then(retry_after_seconds);
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(http_error)? {
            if body.len() + chunk.len() > self.max_body {
                return Err(DeviceError::Rejected("the device sent too much data".to_string()));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Reply { status, body, retry_after })
    }
}

/// The addresses a device's configured name stands for right now.
pub async fn resolve(_grant: LiveGrant, host: &str, port: u16, wait: Duration) -> Result<Vec<IpAddr>, DeviceError> {
    match timeout(wait, lookup_host((host, port))).await {
        Err(_) => Err(DeviceError::Timeout),
        Ok(Err(_)) => Err(broken()),
        Ok(Ok(addresses)) => Ok(addresses.map(|address| address.ip()).collect()),
    }
}

async fn udp_socket(peer: SocketAddr) -> Result<UdpSocket, DeviceError> {
    let local: SocketAddr =
        if peer.is_ipv4() { (Ipv4Addr::UNSPECIFIED, 0).into() } else { (Ipv6Addr::UNSPECIFIED, 0).into() };
    UdpSocket::bind(local).await.map_err(|_| broken())
}

/// Sends one datagram and waits for one reply from that peer. A new socket per
/// call means a late answer to an earlier request cannot be taken for this one.
pub async fn udp_exchange(
    _grant: LiveGrant,
    peer: SocketAddr,
    payload: &[u8],
    wait: Duration,
) -> Result<Vec<u8>, DeviceError> {
    let socket = udp_socket(peer).await?;
    socket.connect(peer).await.map_err(|_| broken())?;
    socket.send(payload).await.map_err(|_| broken())?;
    let mut buffer = vec![0u8; MAX_UDP_REPLY];
    match timeout(wait, socket.recv(&mut buffer)).await {
        Err(_) => Err(DeviceError::Timeout),
        Ok(Err(_)) => Err(broken()),
        Ok(Ok(length)) => {
            buffer.truncate(length);
            Ok(buffer)
        }
    }
}

/// Sends one broadcast datagram. Nothing answers these.
pub async fn udp_broadcast(_grant: LiveGrant, peer: SocketAddr, payload: &[u8]) -> Result<(), DeviceError> {
    let socket = udp_socket(peer).await?;
    socket.set_broadcast(true).map_err(|_| broken())?;
    socket.send_to(payload, peer).await.map_err(|_| broken())?;
    Ok(())
}

/// Broadcasts one datagram and gathers whatever answers within the window. Each
/// reply comes with the address it came from.
pub async fn udp_discover(
    _grant: LiveGrant,
    peer: SocketAddr,
    payload: &[u8],
    window: Duration,
) -> Result<Vec<(SocketAddr, Vec<u8>)>, DeviceError> {
    let socket = udp_socket(peer).await?;
    socket.set_broadcast(true).map_err(|_| broken())?;
    socket.send_to(payload, peer).await.map_err(|_| broken())?;
    let deadline = Instant::now() + window;
    let mut buffer = vec![0u8; MAX_UDP_REPLY];
    let mut replies = Vec::new();
    let mut errors = 0;
    while replies.len() < MAX_DISCOVERY_REPLIES && errors < MAX_DISCOVERY_ERRORS {
        match timeout_at(deadline, socket.recv_from(&mut buffer)).await {
            Err(_) => break,
            // Windows reports an earlier send to a closed port on the next read.
            Ok(Err(_)) => errors += 1,
            Ok(Ok((length, from))) => replies.push((from, buffer[..length].to_vec())),
        }
    }
    Ok(replies)
}

async fn tcp_connect(host: &str, port: u16, wait: Duration) -> Result<TcpStream, DeviceError> {
    match timeout(wait, TcpStream::connect((host, port))).await {
        Err(_) => Err(DeviceError::Timeout),
        Ok(Err(_)) => Err(broken()),
        Ok(Ok(stream)) => Ok(stream),
    }
}

/// Connects, writes one request and reads up to the end of a header block.
pub async fn tcp_exchange(
    _grant: LiveGrant,
    host: &str,
    port: u16,
    request: &[u8],
    timeouts: Timeouts,
) -> Result<Vec<u8>, DeviceError> {
    let mut stream = tcp_connect(host, port, timeouts.connect).await?;
    match timeout(timeouts.read, stream.write_all(request)).await {
        Err(_) => return Err(DeviceError::Timeout),
        Ok(Err(_)) => return Err(broken()),
        Ok(Ok(())) => {}
    }
    let mut reply = Vec::new();
    let mut buffer = [0u8; 1024];
    while reply.len() < MAX_TCP_REPLY && !reply.windows(4).any(|w| w == b"\r\n\r\n") {
        match timeout(timeouts.read, stream.read(&mut buffer)).await {
            Err(_) => return Err(DeviceError::Timeout),
            Ok(Err(_)) => return Err(broken()),
            Ok(Ok(0)) => break,
            Ok(Ok(length)) => reply.extend_from_slice(&buffer[..length]),
        }
    }
    Ok(reply)
}

/// One open TCP connection to a device with a framed protocol of its own.
pub struct TcpLink {
    stream: TcpStream,
    read: Duration,
}

pub async fn tcp_open(_grant: LiveGrant, peer: SocketAddr, timeouts: Timeouts) -> Result<TcpLink, DeviceError> {
    let stream = tcp_connect(&peer.ip().to_string(), peer.port(), timeouts.connect).await?;
    Ok(TcpLink { stream, read: timeouts.read })
}

impl TcpLink {
    pub async fn send(&mut self, data: &[u8]) -> Result<(), DeviceError> {
        match timeout(self.read, self.stream.write_all(data)).await {
            Err(_) => Err(DeviceError::Timeout),
            Ok(Err(_)) => Err(broken()),
            Ok(Ok(())) => Ok(()),
        }
    }

    /// Fills `buffer` or fails. The caller decides how much it will accept, so
    /// a device cannot make this read more than that.
    pub async fn read_exact(&mut self, buffer: &mut [u8]) -> Result<(), DeviceError> {
        match timeout(self.read, self.stream.read_exact(buffer)).await {
            Err(_) => Err(DeviceError::Timeout),
            Ok(Err(_)) => Err(broken()),
            Ok(Ok(_)) => Ok(()),
        }
    }
}

/// One open websocket to a device.
pub struct Ws {
    stream: WebSocketStream<Box<dyn Duplex>>,
    read: Duration,
}

/// `wss` needs `pin`, the SHA-256 of the one certificate to accept.
pub async fn ws_connect(
    _grant: LiveGrant,
    url: &Url,
    pin: Option<&str>,
    timeouts: Timeouts,
) -> Result<Ws, DeviceError> {
    let disabled = || DeviceError::Disabled("the websocket URL is not valid".to_string());
    let host = url.host_str().ok_or_else(disabled)?.trim_matches(['[', ']']).to_string();
    let port = url.port_or_known_default().ok_or_else(disabled)?;
    let tcp = tcp_connect(&host, port, timeouts.connect).await?;
    let stream: Box<dyn Duplex> = match (url.scheme(), pin) {
        ("ws", _) => Box::new(tcp),
        ("wss", Some(pin)) => {
            let connector = pinned_connector(pin)?;
            let name = ServerName::try_from(host).map_err(|_| disabled())?;
            match timeout(timeouts.read, connector.connect(name, tcp)).await {
                Err(_) => return Err(DeviceError::Timeout),
                Ok(Err(_)) => return Err(broken()),
                Ok(Ok(tls)) => Box::new(tls),
            }
        }
        _ => return Err(disabled()),
    };
    let request = url.as_str().into_client_request().map_err(|_| disabled())?;
    let config = WebSocketConfig::default().max_message_size(Some(MAX_WS_MESSAGE)).max_frame_size(Some(MAX_WS_MESSAGE));
    let handshake = tokio_tungstenite::client_async_with_config(request, stream, Some(config));
    match timeout(timeouts.read, handshake).await {
        Err(_) => Err(DeviceError::Timeout),
        Ok(Err(_)) => Err(broken()),
        Ok(Ok((stream, _response))) => Ok(Ws { stream, read: timeouts.read }),
    }
}

impl Ws {
    pub async fn send(&mut self, text: String) -> Result<(), DeviceError> {
        match timeout(self.read, self.stream.send(Message::text(text))).await {
            Err(_) => Err(DeviceError::Timeout),
            Ok(Err(_)) => Err(broken()),
            Ok(Ok(())) => Ok(()),
        }
    }

    /// The next text message.
    pub async fn recv(&mut self) -> Result<String, DeviceError> {
        loop {
            match timeout(self.read, self.stream.next()).await {
                Err(_) => return Err(DeviceError::Timeout),
                Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(Message::Close(_)))) => return Err(broken()),
                Ok(Some(Ok(Message::Text(text)))) => return Ok(text.to_string()),
                Ok(Some(Ok(_))) => {}
            }
        }
    }

    pub async fn close(mut self) {
        let _ = timeout(WS_CLOSE_WAIT, self.stream.close(None)).await;
    }
}

trait Duplex: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> Duplex for T {}

fn bad_ca() -> DeviceError {
    DeviceError::Disabled("the CA certificate could not be used".to_string())
}

/// The same trust as the HTTP client for the same [`Roots`].
fn root_store(ca_pem: Option<&[u8]>, roots: Roots) -> Result<RootCertStore, DeviceError> {
    let mut store = RootCertStore::empty();
    if roots == Roots::PublicAndNamedCa {
        store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }
    if let Some(pem) = ca_pem {
        for certificate in CertificateDer::pem_slice_iter(pem) {
            store.add(certificate.map_err(|_| bad_ca())?).map_err(|_| bad_ca())?;
        }
    }
    Ok(store)
}

/// An invalid certificate is never accepted.
fn tls_connector(ca_pem: Option<&[u8]>, roots: Roots) -> Result<TlsConnector, DeviceError> {
    let config = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|_| bad_ca())?
        .with_root_certificates(root_store(ca_pem, roots)?)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

/// Accepts exactly one certificate, by its SHA-256. For a device whose certificate names neither its
/// address nor a CA anyone could check. The signature is still verified, so the peer has to hold that
/// certificate's private key.
#[derive(Debug)]
struct PinnedCertificate {
    sha256: String,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedCertificate {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, tokio_rustls::rustls::Error> {
        if sha256_hex(end_entity.as_ref()) == self.sha256 {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(tokio_rustls::rustls::Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
        verify_tls12_signature(message, cert, signature, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
        verify_tls13_signature(message, cert, signature, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn pinned_connector(sha256: &str) -> Result<TlsConnector, DeviceError> {
    let provider = Arc::new(ring::default_provider());
    let verifier = PinnedCertificate { sha256: sha256.to_ascii_lowercase(), provider: provider.clone() };
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| bad_ca())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

/// Takes note of the certificate a server presents, whatever it is, for trust on
/// first use. The handshake's signature is still verified, so the server holds
/// that certificate's private key.
#[derive(Debug)]
struct FirstUse {
    seen: Mutex<Option<String>>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for FirstUse {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, tokio_rustls::rustls::Error> {
        let pin = sha256_hex(end_entity.as_ref());
        *self.seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(pin);
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
        verify_tls12_signature(message, cert, signature, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
        verify_tls13_signature(message, cert, signature, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// Trust on first use: connects to a TLS server, finishes the handshake and
/// hands back the SHA-256 of the certificate it presented (lower-case hex), the
/// pin [`ws_connect`] takes. Nothing but the handshake is sent, and the
/// connection is closed at once.
///
/// Any certificate is accepted for this one connection, so the pin says only
/// what answered at that address. Show it to a person to confirm before it is
/// stored; from then on, pinned connections accept that certificate alone.
pub async fn first_use_pin(grant: LiveGrant, host: &str, port: u16, timeouts: Timeouts) -> Result<String, DeviceError> {
    let host = host.trim_matches(['[', ']']).to_string();
    let name = ServerName::try_from(host.clone())
        .map_err(|_| DeviceError::Disabled("the address is not valid".to_string()))?;
    let address = resolve(grant, &host, port, timeouts.connect).await?.into_iter().next().ok_or_else(broken)?;
    let tcp = tcp_open(grant, SocketAddr::new(address, port), timeouts).await?.stream;
    let provider = Arc::new(ring::default_provider());
    let verifier = Arc::new(FirstUse { seen: Mutex::new(None), provider: provider.clone() });
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| broken())?
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone())
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));
    let mut tls = match timeout(timeouts.read, connector.connect(name, tcp)).await {
        Err(_) => return Err(DeviceError::Timeout),
        Ok(Err(_)) => return Err(broken()),
        Ok(Ok(tls)) => tls,
    };
    // Says goodbye (a TLS alert, no data), and does not wait long for the reply.
    let _ = timeout(WS_CLOSE_WAIT, tls.shutdown()).await;
    let pin = verifier.seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
    pin.ok_or_else(broken)
}

/// An open VNC websocket to Proxmox. It carries bytes for one approved VM's console
/// and nothing else.
pub struct ConsoleLink {
    stream: WebSocketStream<Box<dyn Duplex>>,
}

pub async fn console_connect(
    _grant: LiveGrant,
    url: &Url,
    authorization: &HeaderValue,
    ca_pem: Option<&[u8]>,
    roots: Roots,
    timeouts: Timeouts,
) -> Result<ConsoleLink, DeviceError> {
    let disabled = || DeviceError::Disabled("the websocket URL is not valid".to_string());
    let host = url.host_str().ok_or_else(disabled)?.trim_matches(['[', ']']).to_string();
    let port = url.port_or_known_default().ok_or_else(disabled)?;
    let tcp = tcp_connect(&host, port, timeouts.connect).await?;
    let stream: Box<dyn Duplex> = match url.scheme() {
        "ws" => Box::new(tcp),
        "wss" => {
            let connector = tls_connector(ca_pem, roots)?;
            let name = ServerName::try_from(host).map_err(|_| disabled())?;
            match timeout(timeouts.read, connector.connect(name, tcp)).await {
                Err(_) => return Err(DeviceError::Timeout),
                Ok(Err(_)) => return Err(broken()),
                Ok(Ok(tls)) => Box::new(tls),
            }
        }
        _ => return Err(disabled()),
    };
    let mut request = url.as_str().into_client_request().map_err(|_| disabled())?;
    request.headers_mut().insert(AUTHORIZATION, authorization.clone());
    request.headers_mut().insert("sec-websocket-protocol", HeaderValue::from_static("binary"));
    let config =
        WebSocketConfig::default().max_message_size(Some(MAX_CONSOLE_FRAME)).max_frame_size(Some(MAX_CONSOLE_FRAME));
    let handshake = tokio_tungstenite::client_async_with_config(request, stream, Some(config));
    match timeout(timeouts.read, handshake).await {
        Err(_) => Err(DeviceError::Timeout),
        Ok(Err(_)) => Err(broken()),
        Ok(Ok((stream, _response))) => Ok(ConsoleLink { stream }),
    }
}

impl ConsoleLink {
    pub async fn send(&mut self, data: Vec<u8>) -> Result<(), DeviceError> {
        self.stream.send(Message::binary(data)).await.map_err(|_| broken())
    }

    /// The next bytes from the VM, or `None` once the console has closed.
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        loop {
            match self.stream.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return None,
                Some(Ok(Message::Binary(data))) => return Some(data.to_vec()),
                Some(Ok(_)) => {}
            }
        }
    }

    pub async fn close(mut self) {
        let _ = timeout(WS_CLOSE_WAIT, self.stream.close(None)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private_ca() -> String {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(Vec::new()).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.self_signed(&key).unwrap().pem()
    }

    #[test]
    fn a_named_ca_can_be_the_only_root() {
        let pem = private_ca();
        assert_eq!(root_store(Some(pem.as_bytes()), Roots::NamedCaOnly).unwrap().len(), 1);
        assert_eq!(root_store(None, Roots::NamedCaOnly).unwrap().len(), 0, "nothing named, nothing trusted");
        let public = webpki_roots::TLS_SERVER_ROOTS.len();
        assert_eq!(root_store(None, Roots::PublicAndNamedCa).unwrap().len(), public);
        assert_eq!(root_store(Some(pem.as_bytes()), Roots::PublicAndNamedCa).unwrap().len(), public + 1);
    }

    fn timeouts() -> Timeouts {
        Timeouts { connect: Duration::from_millis(800), read: Duration::from_millis(800) }
    }

    #[test]
    fn a_ca_is_taken_as_pem_text_and_checked() {
        let grant = LiveGrant::when_live(true).unwrap();
        let build = |pem: Option<&[u8]>| {
            Http::with_ca_pem(grant, "https://127.0.0.1:8006", timeouts(), pem, Roots::NamedCaOnly)
        };
        assert!(build(None).is_ok());
        assert!(build(Some(private_ca().as_bytes())).is_ok());
        assert_eq!(build(Some(b"not a certificate")).err().unwrap(), "the CA certificate is not a PEM certificate");
        let bad_url = Http::with_ca_pem(grant, "not a url", timeouts(), None, Roots::NamedCaOnly);
        assert_eq!(bad_url.err().unwrap(), "the device URL is not valid");
    }

    #[tokio::test]
    async fn a_bad_header_value_is_a_request_that_cannot_be_made_not_silence() {
        // Nothing listens here, and nothing is asked: the request is refused when it is built.
        let grant = LiveGrant::when_live(true).unwrap();
        let http = Http::new(grant, "http://127.0.0.1:9", timeouts(), None).unwrap();
        for value in ["line one\nline two", "bell\u{7}", "caf\u{e9}\r\n"] {
            let request = http.get(http.url("/").unwrap()).header("x-token", value);
            let error = http.send(request).await.unwrap_err();
            assert_eq!(error, DeviceError::Disabled("the request is not valid".to_string()), "{value:?}");
            assert!(!error.is_silence(), "the engine must not take it for a device that did not hear");
            assert!(!error.to_string().contains("line"), "the text names no value: {error}");
            let request = http.post(http.url("/").unwrap()).header("x-token", value);
            assert_eq!(http.stream(request, |_| {}).await.unwrap_err(), error);
        }
        // A good header goes out; with nothing listening that is the usual failure to connect.
        let request = http.get(http.url("/").unwrap()).header("x-token", "fine");
        assert!(matches!(http.send(request).await, Err(DeviceError::Unreachable(_) | DeviceError::Timeout)));
    }

    #[test]
    fn the_http_client_is_told_which_roots_to_use() {
        let source = include_str!("lib.rs");
        assert_eq!(
            source.matches(concat!(".tls_built_in_root_certs(", "roots == Roots::PublicAndNamedCa)")).count(),
            1
        );
    }
}
