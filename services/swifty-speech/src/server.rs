//! The HTTP service.
//!
//! The client address is checked when a connection is accepted, so a refused
//! client never takes a connection slot. A connection then has one deadline to
//! deliver complete headers, and one source can only have a few connections at
//! that stage. After that, requests are refused as early and as cheaply as
//! possible, in this order: token, route, model readiness, content type,
//! declared size, body read (size and time limits), WAV validation, queue
//! admission. Each connection carries one request, which keeps per-client state
//! to a minimum.

use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{
    HeaderMap, HeaderName, HeaderValue, ALLOW, AUTHORIZATION, CACHE_CONTROL, CONNECTION, CONTENT_LENGTH, CONTENT_TYPE,
    RETRY_AFTER, WWW_AUTHENTICATE,
};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, Notify, Semaphore};
use tokio::time::{interval, sleep, timeout};

use crate::logging::{ConnectionReport, Logger, RequestRecord};
use crate::recognizer::Recognize;
use crate::token::{bearer_credentials, Token};
use crate::wav::{self, WavError};
use crate::worker::{self, Job};

/// Largest accepted request body.
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
const QUEUE_DEPTH: usize = 2;
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(15);
/// From accept to the end of the request headers, however they arrive.
const HEADER_DEADLINE: Duration = Duration::from_secs(5);
const MAX_CONNECTIONS: usize = 32;
/// How many connections one address may have open before their headers are
/// complete, so that one source cannot hold every connection slot.
const MAX_PENDING_PER_SOURCE: usize = 8;
/// Refused connections being answered at once. Beyond this they are just closed.
const MAX_REFUSALS: usize = 64;
/// How often refused and timed-out connections are written to the log.
const CONNECTION_REPORT_INTERVAL: Duration = Duration::from_secs(60);
/// Backstop for anything else that could hold a connection open.
const CONNECTION_DEADLINE: Duration = Duration::from_secs(120);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// After answering early, unread request bytes are discarded for a moment.
/// Closing a socket with unread data resets the connection, which can destroy
/// the response before the client has read it.
const LINGER_TIME: Duration = Duration::from_secs(2);
const LINGER_BYTES: usize = 16 * 1024 * 1024;
const RETRY_AFTER_BUSY_SECONDS: u32 = 1;
const RETRY_AFTER_LOADING_SECONDS: u32 = 5;

const TOO_MANY_CONNECTIONS: &[u8] = b"HTTP/1.1 503 Service Unavailable\r\n\
    Content-Type: application/json\r\nRetry-After: 1\r\nConnection: close\r\nContent-Length: 16\r\n\r\n\
    {\"error\":\"busy\"}";

const ADDRESS_NOT_ALLOWED: &[u8] = b"HTTP/1.1 403 Forbidden\r\n\
    Content-Type: application/json\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: 21\r\n\r\n\
    {\"error\":\"forbidden\"}";

const WAV_MEDIA_TYPES: [&str; 4] = ["audio/wav", "audio/x-wav", "audio/wave", "audio/vnd.wave"];

pub struct Settings {
    pub bind: SocketAddr,
    pub allow: Vec<IpAddr>,
    pub token: Token,
    pub logger: Logger,
    /// How long to keep retrying if the address is not available yet. A service
    /// that starts at boot can run before a LAN address has been assigned.
    pub bind_wait: Duration,
}

struct Shared {
    token: Token,
    ready: Arc<AtomicBool>,
    jobs: mpsc::Sender<Job>,
    logger: Logger,
    turned_away: TurnedAway,
}

/// Connections that never became a request, counted since the last report.
#[derive(Default)]
struct TurnedAway {
    refused_address: AtomicU64,
    refused_busy: AtomicU64,
    header_timeouts: AtomicU64,
}

impl TurnedAway {
    /// Log the counts and start again from zero. Quiet when there is nothing to
    /// report, and at most one line per interval however much traffic there is.
    fn report(&self, logger: &Logger) {
        let report = ConnectionReport {
            refused_address: self.refused_address.swap(0, Ordering::Relaxed),
            refused_busy: self.refused_busy.swap(0, Ordering::Relaxed),
            header_timeouts: self.header_timeouts.swap(0, Ordering::Relaxed),
        };
        if report != ConnectionReport::default() {
            logger.connections(&report);
        }
    }
}

type PendingBySource = Arc<Mutex<HashMap<IpAddr, usize>>>;

/// Counts one connection against its source until its headers are complete.
/// Entries exist only while a connection slot is held, so the map stays small.
struct PendingHeaders {
    sources: PendingBySource,
    peer: IpAddr,
}

impl PendingHeaders {
    fn admit(sources: &PendingBySource, peer: IpAddr) -> Option<Self> {
        let mut counts = sources.lock().unwrap_or_else(PoisonError::into_inner);
        let count = counts.entry(peer).or_insert(0);
        if *count >= MAX_PENDING_PER_SOURCE {
            return None;
        }
        *count += 1;
        drop(counts);
        Some(Self { sources: sources.clone(), peer })
    }
}

impl Drop for PendingHeaders {
    fn drop(&mut self) {
        let mut counts = self.sources.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = counts.get_mut(&self.peer) {
            *count -= 1;
            if *count == 0 {
                counts.remove(&self.peer);
            }
        }
    }
}

/// Run until `shutdown` completes. Returns an error if the service cannot start
/// or the model cannot be loaded.
pub fn run<R, L, S, B>(settings: Settings, load: L, shutdown: S, on_listening: B) -> Result<(), String>
where
    R: Recognize,
    L: FnOnce() -> Result<R, String> + Send + 'static,
    S: Future<Output = ()> + Send,
    B: FnOnce(SocketAddr),
{
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start the async runtime: {error}"))?;
    let result = runtime.block_on(serve(settings, load, shutdown, on_listening));
    runtime.shutdown_timeout(Duration::from_secs(1));
    result
}

async fn serve<R, L, S, B>(settings: Settings, load: L, shutdown: S, on_listening: B) -> Result<(), String>
where
    R: Recognize,
    L: FnOnce() -> Result<R, String> + Send + 'static,
    S: Future<Output = ()> + Send,
    B: FnOnce(SocketAddr),
{
    tokio::pin!(shutdown);

    let Some(listener) = bind(settings.bind, settings.bind_wait, shutdown.as_mut()).await? else {
        return Ok(());
    };

    let (jobs, job_queue) = mpsc::channel(QUEUE_DEPTH);
    let ready = Arc::new(AtomicBool::new(false));
    let (fatal_sender, mut fatal) = oneshot::channel();
    worker::spawn(load, job_queue, ready.clone(), fatal_sender, settings.logger.clone())
        .map_err(|error| format!("cannot start the decode worker: {error}"))?;

    let address = listener.local_addr().map_err(|error| format!("cannot read the listening address: {error}"))?;
    on_listening(address);

    let allow: Vec<IpAddr> = settings.allow.iter().map(|ip| ip.to_canonical()).collect();
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let refusals = Arc::new(Semaphore::new(MAX_REFUSALS));
    let pending = PendingBySource::default();
    let shared = Arc::new(Shared {
        token: settings.token,
        ready,
        jobs,
        logger: settings.logger,
        turned_away: TurnedAway::default(),
    });

    let mut report_timer = interval(CONNECTION_REPORT_INTERVAL);
    let mut loading = true;
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            // Resolves once: with the reason if loading failed, or with an
            // error when the sender is dropped after a successful load.
            outcome = &mut fatal, if loading => {
                loading = false;
                if let Ok(message) = outcome {
                    return Err(message);
                }
            }
            _ = report_timer.tick() => shared.turned_away.report(&shared.logger),
            accepted = listener.accept() => {
                let Ok((stream, peer)) = accepted else {
                    // Usually a client that vanished. Back off in case it is not.
                    sleep(Duration::from_millis(50)).await;
                    continue;
                };

                let peer = peer.ip().to_canonical();
                if !allow.is_empty() && !allow.contains(&peer) {
                    shared.turned_away.refused_address.fetch_add(1, Ordering::Relaxed);
                    refuse(stream, ADDRESS_NOT_ALLOWED, &refusals);
                    continue;
                }

                let admitted = PendingHeaders::admit(&pending, peer).and_then(|headers| {
                    connections.clone().try_acquire_owned().ok().map(|slot| (headers, slot))
                });
                let Some((headers, slot)) = admitted else {
                    shared.turned_away.refused_busy.fetch_add(1, Ordering::Relaxed);
                    refuse(stream, TOO_MANY_CONNECTIONS, &refusals);
                    continue;
                };

                let shared = shared.clone();
                tokio::spawn(async move {
                    let _slot = slot;
                    let _ = timeout(CONNECTION_DEADLINE, serve_connection(stream, headers, shared)).await;
                });
            }
        }
    }

    shared.turned_away.report(&shared.logger);
    drop(listener);
    let _ = timeout(SHUTDOWN_GRACE, connections.acquire_many(MAX_CONNECTIONS as u32)).await;
    Ok(())
}

async fn bind<S>(address: SocketAddr, wait: Duration, mut shutdown: Pin<&mut S>) -> Result<Option<TcpListener>, String>
where
    S: Future<Output = ()>,
{
    let deadline = Instant::now() + wait;
    loop {
        match TcpListener::bind(address).await {
            Ok(listener) => return Ok(Some(listener)),
            Err(error) if error.kind() == ErrorKind::AddrNotAvailable && Instant::now() < deadline => {
                tokio::select! {
                    () = &mut shutdown => return Ok(None),
                    () = sleep(Duration::from_secs(2)) => {}
                }
            }
            Err(error) => return Err(format!("cannot listen on {address}: {error}")),
        }
    }
}

/// Answer a connection that will not be served, without holding a connection
/// slot. The number of these running is capped, so this cannot grow without limit.
fn refuse(stream: TcpStream, response: &'static [u8], refusals: &Arc<Semaphore>) {
    if let Ok(permit) = refusals.clone().try_acquire_owned() {
        tokio::spawn(async move {
            let _permit = permit;
            refuse_connection(stream, response).await;
        });
    }
}

async fn refuse_connection(mut stream: TcpStream, response: &[u8]) {
    if let Ok(Ok(())) = timeout(Duration::from_secs(1), stream.write_all(response)).await {
        linger(stream).await;
    }
}

async fn serve_connection(stream: TcpStream, pending: PendingHeaders, shared: Arc<Shared>) {
    let headers_complete = Arc::new(Notify::new());
    let signal = headers_complete.clone();
    let handler_shared = shared.clone();

    type HandlerFuture = Pin<Box<dyn Future<Output = Result<Response<Full<Bytes>>, Infallible>> + Send>>;
    let service = service_fn(move |request| -> HandlerFuture {
        signal.notify_one();
        Box::pin(handle(request, handler_shared.clone()))
    });

    let connection = http1::Builder::new()
        // HEADER_DEADLINE below covers the whole wait, including the first byte.
        .header_read_timeout(None)
        // A client may close its sending side once the request is complete.
        .half_close(true)
        .keep_alive(false)
        .serve_connection(TokioIo::new(stream), service)
        .without_shutdown();
    tokio::pin!(connection);

    let early = tokio::select! {
        finished = &mut connection => Some(finished),
        () = headers_complete.notified() => None,
        () = sleep(HEADER_DEADLINE) => {
            shared.turned_away.header_timeouts.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };
    drop(pending);

    let finished = match early {
        Some(finished) => finished,
        None => connection.await,
    };
    if let Ok(parts) = finished {
        linger(parts.io.into_inner()).await;
    }
}

async fn linger(mut stream: TcpStream) {
    let _ = stream.shutdown().await;
    let mut sink = [0_u8; 16 * 1024];
    let mut discarded = 0;
    let _ = timeout(LINGER_TIME, async {
        while discarded < LINGER_BYTES {
            match stream.read(&mut sink).await {
                Ok(count) if count > 0 => discarded += count,
                _ => break,
            }
        }
    })
    .await;
}

/// Why a request was not answered with a transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    Unauthorized,
    NotFound,
    MethodNotAllowed(&'static str),
    BadRequest(&'static str),
    Timeout,
    TooLarge,
    Unsupported(&'static str),
    Unprocessable(&'static str),
    Busy,
    Loading,
    Internal,
}

impl Failure {
    fn into_response(self) -> Response<Full<Bytes>> {
        let (status, code, detail) = match self {
            Failure::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized", None),
            Failure::NotFound => (StatusCode::NOT_FOUND, "not_found", None),
            Failure::MethodNotAllowed(_) => (StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed", None),
            Failure::BadRequest(detail) => (StatusCode::BAD_REQUEST, "malformed_wav", Some(detail)),
            Failure::Timeout => (StatusCode::REQUEST_TIMEOUT, "body_read_timeout", None),
            Failure::TooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "body_too_large", None),
            Failure::Unsupported(detail) => (StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_format", Some(detail)),
            Failure::Unprocessable(detail) => (StatusCode::UNPROCESSABLE_ENTITY, "unprocessable_audio", Some(detail)),
            Failure::Busy => (StatusCode::SERVICE_UNAVAILABLE, "busy", None),
            Failure::Loading => (StatusCode::SERVICE_UNAVAILABLE, "model_loading", None),
            Failure::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error", None),
        };
        let body = match detail {
            Some(detail) => json!({ "error": code, "detail": detail }),
            None => json!({ "error": code }),
        };
        let mut response = json_response(status, body.to_string());
        let headers = response.headers_mut();
        match self {
            Failure::Unauthorized => set(headers, WWW_AUTHENTICATE, HeaderValue::from_static("Bearer")),
            Failure::MethodNotAllowed(allow) => set(headers, ALLOW, HeaderValue::from_static(allow)),
            Failure::Busy => set(headers, RETRY_AFTER, HeaderValue::from(RETRY_AFTER_BUSY_SECONDS)),
            Failure::Loading => set(headers, RETRY_AFTER, HeaderValue::from(RETRY_AFTER_LOADING_SECONDS)),
            _ => {}
        }
        response
    }
}

fn set(headers: &mut HeaderMap, name: HeaderName, value: HeaderValue) {
    headers.insert(name, value);
}

fn json_response(status: StatusCode, body: String) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(body)));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    set(headers, CONTENT_TYPE, HeaderValue::from_static("application/json"));
    set(headers, CACHE_CONTROL, HeaderValue::from_static("no-store"));
    set(headers, CONNECTION, HeaderValue::from_static("close"));
    response
}

async fn handle(request: Request<Incoming>, shared: Arc<Shared>) -> Result<Response<Full<Bytes>>, Infallible> {
    let started = Instant::now();
    let mut record = RequestRecord::default();

    let response = match route(request, &shared, &mut record).await {
        Ok(response) => response,
        Err(failure) => failure.into_response(),
    };

    record.status = response.status().as_u16();
    record.duration = started.elapsed();
    shared.logger.request(&record);
    Ok(response)
}

async fn route(
    request: Request<Incoming>,
    shared: &Shared,
    record: &mut RequestRecord,
) -> Result<Response<Full<Bytes>>, Failure> {
    let is_health = request.uri().path() == "/healthz";
    if is_health && request.method() == Method::GET {
        return health(shared);
    }

    if !authorized(request.headers(), &shared.token) {
        return Err(Failure::Unauthorized);
    }

    match (request.uri().path(), request.method()) {
        ("/v1/transcribe", &Method::POST) => transcribe(request, shared, record).await,
        ("/v1/transcribe", _) => Err(Failure::MethodNotAllowed("POST")),
        _ if is_health => Err(Failure::MethodNotAllowed("GET")),
        _ => Err(Failure::NotFound),
    }
}

fn authorized(headers: &HeaderMap, token: &Token) -> bool {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| bearer_credentials(value.as_bytes()))
        .is_some_and(|presented| token.matches(presented))
}

fn health(shared: &Shared) -> Result<Response<Full<Bytes>>, Failure> {
    if shared.ready.load(Ordering::Acquire) {
        Ok(json_response(StatusCode::OK, r#"{"status":"ok"}"#.to_string()))
    } else {
        Err(Failure::Loading)
    }
}

async fn transcribe(
    request: Request<Incoming>,
    shared: &Shared,
    record: &mut RequestRecord,
) -> Result<Response<Full<Bytes>>, Failure> {
    if !shared.ready.load(Ordering::Acquire) {
        return Err(Failure::Loading);
    }
    if !is_wav_content_type(request.headers()) {
        return Err(Failure::Unsupported("Content-Type must be audio/wav"));
    }

    let declared = declared_length(request.headers());
    if declared.is_some_and(|length| length > MAX_BODY_BYTES as u64) {
        record.body_bytes = declared.unwrap_or(0);
        return Err(Failure::TooLarge);
    }

    let hint = declared.unwrap_or(0) as usize;
    let body = match timeout(BODY_READ_TIMEOUT, read_body(request.into_body(), hint)).await {
        Ok(result) => result?,
        Err(_) => return Err(Failure::Timeout),
    };
    record.body_bytes = body.len() as u64;

    let audio = wav::parse(&body).map_err(|error| match error {
        WavError::Malformed(detail) => Failure::BadRequest(detail),
        WavError::Unsupported(detail) => Failure::Unsupported(detail),
        WavError::TooLong => Failure::Unprocessable("audio is longer than 30 seconds"),
        WavError::Empty => Failure::Unprocessable("audio is empty"),
    })?;
    drop(body);
    let audio_ms = audio.audio_ms;
    record.audio_ms = Some(audio_ms);

    let (reply, answer) = oneshot::channel();
    shared.jobs.try_send(Job { audio, reply }).map_err(|error| match error {
        mpsc::error::TrySendError::Full(_) => Failure::Busy,
        mpsc::error::TrySendError::Closed(_) => Failure::Internal,
    })?;

    let decoded = answer.await.map_err(|_| Failure::Internal)?.map_err(|_| Failure::Internal)?;
    record.decode_ms = Some(decoded.decode_ms);

    let body = json!({ "text": decoded.text, "audio_ms": audio_ms, "decode_ms": decoded.decode_ms });
    Ok(json_response(StatusCode::OK, body.to_string()))
}

fn is_wav_content_type(headers: &HeaderMap) -> bool {
    let Some(value) = headers.get(CONTENT_TYPE).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let essence = value.split(';').next().unwrap_or("").trim();
    WAV_MEDIA_TYPES.iter().any(|known| essence.eq_ignore_ascii_case(known))
}

fn declared_length(headers: &HeaderMap) -> Option<u64> {
    headers.get(CONTENT_LENGTH)?.to_str().ok()?.parse().ok()
}

async fn read_body(mut body: Incoming, hint: usize) -> Result<Vec<u8>, Failure> {
    let mut bytes = Vec::with_capacity(hint.min(MAX_BODY_BYTES));
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| Failure::BadRequest("the request body could not be read"))?;
        if let Ok(data) = frame.into_data() {
            if data.len() > MAX_BODY_BYTES - bytes.len() {
                return Err(Failure::TooLarge);
            }
            bytes.extend_from_slice(&data);
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream as BlockingStream;
    use std::path::Path;
    use std::sync::{mpsc as std_mpsc, Condvar, Mutex};
    use std::thread;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// A recognizer driven by the first sample of each clip.
    struct Fake {
        gate: Arc<Gate>,
    }

    #[derive(Default)]
    struct Gate {
        open: Mutex<bool>,
        changed: Condvar,
        entered: Mutex<usize>,
        arrived: Condvar,
    }

    impl Gate {
        fn release(&self) {
            *self.open.lock().unwrap() = true;
            self.changed.notify_all();
        }

        fn wait_until_entered(&self, count: usize) {
            let guard = self.entered.lock().unwrap();
            let (_guard, waited) =
                self.arrived.wait_timeout_while(guard, Duration::from_secs(10), |entered| *entered < count).unwrap();
            assert!(!waited.timed_out(), "the worker did not start the job");
        }
    }

    impl Recognize for Fake {
        fn recognize(&self, _rate: u32, samples: &[f32]) -> Result<String, &'static str> {
            let first = samples[0];
            if first < -0.5 {
                panic!("scripted panic");
            }
            if first > 0.5 {
                return Err("scripted failure");
            }
            if first > 0.1 {
                *self.gate.entered.lock().unwrap() += 1;
                self.gate.arrived.notify_all();
                let guard = self.gate.open.lock().unwrap();
                let _guard = self.gate.changed.wait_while(guard, |open| !*open).unwrap();
            }
            Ok("hello \"world\"".to_string())
        }
    }

    struct Harness {
        address: SocketAddr,
        gate: Arc<Gate>,
        stop: Option<oneshot::Sender<()>>,
        thread: Option<thread::JoinHandle<Result<(), String>>>,
    }

    impl Harness {
        fn start(allow: Vec<IpAddr>) -> Self {
            Self::start_with(allow, |_| {})
        }

        fn start_with(allow: Vec<IpAddr>, before_load: impl FnOnce(&Arc<Gate>) + Send + 'static) -> Self {
            let gate = Arc::new(Gate::default());
            let load_gate = gate.clone();
            let (stop, stopped) = oneshot::channel::<()>();
            let (bound, address) = std_mpsc::channel();
            let settings = Settings {
                bind: "127.0.0.1:0".parse().unwrap(),
                allow,
                token: test_token(),
                logger: Logger::discard(),
                bind_wait: Duration::ZERO,
            };
            let thread = thread::spawn(move || {
                run(
                    settings,
                    move || {
                        before_load(&load_gate);
                        Ok(Fake { gate: load_gate })
                    },
                    async move {
                        let _ = stopped.await;
                    },
                    move |address| bound.send(address).unwrap(),
                )
            });
            let address = address.recv_timeout(Duration::from_secs(10)).unwrap();
            Self { address, gate, stop: Some(stop), thread: Some(thread) }
        }

        fn wait_ready(&self) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while send(self.address, &get("/healthz", &[])).status != 200 {
                assert!(Instant::now() < deadline, "the service never became ready");
                thread::sleep(Duration::from_millis(5));
            }
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            self.gate.release();
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            if let Some(thread) = self.thread.take() {
                let result = thread.join();
                if !thread::panicking() {
                    result.unwrap().unwrap();
                }
            }
        }
    }

    fn test_token() -> Token {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let index = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("gs-server-{}-{index}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = Path::new(&dir).join("token");
        let readers = crate::token::Readers::CurrentUserAdministratorsAndSystem;
        crate::token::create_new(&path, TOKEN.as_bytes(), readers).unwrap();
        let token = crate::token::load_or_create(&path, readers).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        token
    }

    /// Opens connections that each send `head` and hold a slot, until every slot is held. They are opened
    /// one at a time and each is kept only once the service has taken it: a held one gets no answer (the
    /// service waits for its body), while one turned away, because this source already had as many
    /// unfinished headers as it may, gets a 503 at once and is tried again. Opening them all at once let a
    /// slow machine turn some away, so fewer slots were held than the test meant.
    fn hold_every_slot(address: SocketAddr, head: &[u8]) -> Vec<BlockingStream> {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut held = Vec::new();
        while held.len() < MAX_CONNECTIONS {
            assert!(Instant::now() < deadline, "only {} of {MAX_CONNECTIONS} slots could be held", held.len());
            let mut stream = BlockingStream::connect(address).unwrap();
            stream.write_all(head).unwrap();
            stream.set_read_timeout(Some(Duration::from_millis(150))).unwrap();
            let mut first = [0_u8; 1];
            match stream.read(&mut first) {
                Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => held.push(stream),
                _ => thread::sleep(Duration::from_millis(50)),
            }
        }
        held
    }

    /// Waits until the service refuses a new connection as busy (503), as it does once every slot is
    /// held. A service that never fills up fails here, so this only removes the guess of a fixed pause.
    fn wait_until_full(address: SocketAddr) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while send(address, &get("/healthz", &[])).status != 503 {
            assert!(Instant::now() < deadline, "the service never refused a connection as busy");
            thread::sleep(Duration::from_millis(20));
        }
    }

    struct Reply {
        status: u16,
        head: String,
        body: String,
    }

    fn send(address: SocketAddr, request: &[u8]) -> Reply {
        let mut stream = BlockingStream::connect(address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        stream.write_all(request).unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        let text = String::from_utf8_lossy(&raw).into_owned();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let status = head.split(' ').nth(1).unwrap_or("0").parse().unwrap_or(0);
        Reply { status, head: head.to_ascii_lowercase(), body: body.to_string() }
    }

    fn get(path: &str, extra: &[&str]) -> Vec<u8> {
        let headers: String = extra.iter().map(|h| format!("{h}\r\n")).collect();
        format!("GET {path} HTTP/1.1\r\nHost: t\r\n{headers}\r\n").into_bytes()
    }

    fn post(first_sample: i16, extra: &[&str]) -> Vec<u8> {
        let mut body = wav_bytes(first_sample);
        let headers: String = extra.iter().map(|h| format!("{h}\r\n")).collect();
        let mut request =
            format!("POST /v1/transcribe HTTP/1.1\r\nHost: t\r\nContent-Length: {}\r\n{headers}\r\n", body.len())
                .into_bytes();
        request.append(&mut body);
        request
    }

    fn auth() -> String {
        format!("Authorization: Bearer {TOKEN}")
    }

    fn post_ok(first_sample: i16) -> Vec<u8> {
        post(first_sample, &[&auth(), "Content-Type: audio/wav"])
    }

    fn wav_bytes(first_sample: i16) -> Vec<u8> {
        let mut file = Vec::new();
        file.extend_from_slice(b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0");
        file.extend_from_slice(&16_000_u32.to_le_bytes());
        file.extend_from_slice(&32_000_u32.to_le_bytes());
        file.extend_from_slice(&[2, 0, 16, 0]);
        file.extend_from_slice(b"data\x04\0\0\0");
        file.extend_from_slice(&first_sample.to_le_bytes());
        file.extend_from_slice(&[0, 0]);
        file
    }

    #[test]
    fn health_needs_no_token_and_is_exact() {
        let server = Harness::start(vec![]);
        server.wait_ready();
        let reply = send(server.address, &get("/healthz", &[]));
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body, r#"{"status":"ok"}"#);
    }

    #[test]
    fn health_is_503_with_retry_after_until_the_model_loads() {
        let hold = Arc::new(Gate::default());
        let held = hold.clone();
        let server = Harness::start_with(vec![], move |_| {
            let guard = held.open.lock().unwrap();
            let _guard = held.changed.wait_while(guard, |open| !*open).unwrap();
        });

        let before = send(server.address, &get("/healthz", &[]));
        assert_eq!(before.status, 503);
        assert!(before.head.contains("retry-after:"));
        let early_post = send(server.address, &post_ok(0));
        assert_eq!(early_post.status, 503);
        assert!(early_post.head.contains("retry-after:"));

        hold.release();
        server.wait_ready();
    }

    #[test]
    fn missing_and_wrong_tokens_are_refused() {
        let server = Harness::start(vec![]);
        server.wait_ready();
        let wrong = format!("Authorization: Bearer {}", "f".repeat(64));
        let basic = format!("Authorization: Basic {TOKEN}");
        for extra in [vec!["Content-Type: audio/wav"], vec![wrong.as_str()], vec![basic.as_str()]] {
            let reply = send(server.address, &post(0, &extra));
            assert_eq!(reply.status, 401, "{extra:?}");
            assert!(reply.head.contains("www-authenticate: bearer"));
        }
        assert_eq!(send(server.address, &get("/nothing", &[])).status, 401);
        assert_eq!(send(server.address, &get("/v1/transcribe", &[])).status, 401);
    }

    #[test]
    fn routes_and_methods() {
        let server = Harness::start(vec![]);
        server.wait_ready();
        let token = auth();
        assert_eq!(send(server.address, &get("/nothing", &[&token])).status, 404);
        let reply = send(server.address, &get("/v1/transcribe", &[&token]));
        assert_eq!(reply.status, 405);
        assert!(reply.head.contains("allow: post"));
        let delete = format!("DELETE /healthz HTTP/1.1\r\nHost: t\r\n{token}\r\n\r\n");
        assert_eq!(send(server.address, delete.as_bytes()).status, 405);
        let unauthenticated = "DELETE /healthz HTTP/1.1\r\nHost: t\r\n\r\n";
        assert_eq!(send(server.address, unauthenticated.as_bytes()).status, 401);
    }

    #[test]
    fn allow_list_refuses_everything_from_other_addresses() {
        let server = Harness::start(vec!["192.0.2.1".parse().unwrap()]);
        assert_eq!(send(server.address, &get("/healthz", &[])).status, 403);
        assert_eq!(send(server.address, &post_ok(0)).status, 403);
    }

    #[test]
    fn refused_addresses_never_take_a_connection_slot() {
        let server = Harness::start(vec!["192.0.2.1".parse().unwrap()]);
        let idle: Vec<_> = (0..MAX_CONNECTIONS + 8).map(|_| BlockingStream::connect(server.address).unwrap()).collect();
        thread::sleep(Duration::from_millis(200));
        let reply = send(server.address, &get("/healthz", &[]));
        assert_eq!(reply.status, 403);
        assert_eq!(reply.body, r#"{"error":"forbidden"}"#);
        drop(idle);
    }

    #[test]
    fn one_source_cannot_hold_every_connection_with_unfinished_headers() {
        let server = Harness::start(vec![]);
        server.wait_ready();
        let idle: Vec<_> =
            (0..MAX_PENDING_PER_SOURCE).map(|_| BlockingStream::connect(server.address).unwrap()).collect();
        wait_until_full(server.address);

        let refused = send(server.address, &get("/healthz", &[]));
        assert_eq!(refused.status, 503);
        assert!(refused.head.contains("retry-after: 1"));

        drop(idle);
        let deadline = Instant::now() + Duration::from_secs(5);
        while send(server.address, &get("/healthz", &[])).status != 200 {
            assert!(Instant::now() < deadline, "the source's connections were not released");
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_refusal_reaches_a_client_that_is_still_uploading() {
        let server = Harness::start(vec![]);
        server.wait_ready();
        // Complete headers and a body still to come: each of these holds a slot.
        let head = format!(
            "POST /v1/transcribe HTTP/1.1\r\nHost: t\r\n{}\r\nContent-Type: audio/wav\r\nContent-Length: 1000\r\n\r\n",
            auth()
        );
        let held = hold_every_slot(server.address, head.as_bytes());

        let mut upload = format!(
            "POST /v1/transcribe HTTP/1.1\r\nHost: t\r\n{}\r\nContent-Type: audio/wav\r\nContent-Length: 1048576\r\n\r\n",
            auth()
        )
        .into_bytes();
        upload.resize(upload.len() + 1024 * 1024, 0);
        for _ in 0..10 {
            let reply = send(server.address, &upload);
            assert_eq!(reply.status, 503);
            assert!(reply.head.contains("retry-after: 1"));
        }
        drop(held);
    }

    #[test]
    fn headers_have_one_deadline_from_accept() {
        let server = Harness::start(vec![]);
        server.wait_ready();
        let started = Instant::now();
        let mut stream = BlockingStream::connect(server.address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        thread::sleep(Duration::from_secs(3));
        stream.write_all(b"G").unwrap();
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw);
        assert!(raw.is_empty());
        let elapsed = started.elapsed();
        assert!(elapsed >= Duration::from_secs(4) && elapsed < Duration::from_secs(7), "{elapsed:?}");
    }

    #[test]
    fn a_client_that_half_closes_still_gets_its_answer() {
        let server = Harness::start(vec![]);
        server.wait_ready();
        for request in [post_ok(0), get("/healthz", &[])] {
            let mut stream = BlockingStream::connect(server.address).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
            stream.write_all(&request).unwrap();
            stream.shutdown(std::net::Shutdown::Write).unwrap();
            let mut raw = Vec::new();
            stream.read_to_end(&mut raw).unwrap();
            assert!(raw.starts_with(b"HTTP/1.1 200"), "{}", String::from_utf8_lossy(&raw));
        }
    }

    #[test]
    fn allow_list_admits_a_listed_address() {
        let server = Harness::start(vec!["192.0.2.1".parse().unwrap(), "127.0.0.1".parse().unwrap()]);
        server.wait_ready();
        assert_eq!(send(server.address, &get("/healthz", &[])).status, 200);
    }

    #[test]
    fn allow_list_matches_ipv4_mapped_addresses() {
        let mapped: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        let server = Harness::start(vec![mapped]);
        server.wait_ready();
        assert_eq!(send(server.address, &get("/healthz", &[])).status, 200);
    }

    #[test]
    fn transcribes_and_escapes_json() {
        let server = Harness::start(vec![]);
        server.wait_ready();
        let reply = send(server.address, &post_ok(0));
        assert_eq!(reply.status, 200, "{}", reply.body);
        let value: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(value["text"], "hello \"world\"");
        assert_eq!(value["audio_ms"], 0);
        assert!(value["decode_ms"].is_u64());
    }

    #[test]
    fn a_decoding_panic_is_a_500_and_the_service_survives() {
        let server = Harness::start(vec![]);
        server.wait_ready();
        for _ in 0..3 {
            let reply = send(server.address, &post_ok(-30_000));
            assert_eq!(reply.status, 500);
            assert!(reply.body.contains("internal_error"));
            assert_eq!(send(server.address, &post_ok(0)).status, 200);
        }
        assert_eq!(send(server.address, &get("/healthz", &[])).status, 200);
    }

    #[test]
    fn a_recognizer_error_is_a_500() {
        let server = Harness::start(vec![]);
        server.wait_ready();
        assert_eq!(send(server.address, &post_ok(30_000)).status, 500);
    }

    #[test]
    fn one_running_plus_two_waiting_then_503_then_recovery() {
        let server = Harness::start(vec![]);
        server.wait_ready();
        let address = server.address;

        // The first job holds the worker; the next two fill the queue.
        let first = thread::spawn(move || send(address, &post_ok(16_000)));
        server.gate.wait_until_entered(1);
        let queued: Vec<_> = (0..2).map(|_| thread::spawn(move || send(address, &post_ok(0)))).collect();
        // Nothing reports that a request has been queued, so give them a moment.
        thread::sleep(Duration::from_millis(500));

        let overflow = send(address, &post_ok(0));
        assert_eq!(overflow.status, 503);
        assert!(overflow.head.contains("retry-after: 1"));
        assert!(overflow.body.contains("busy"));
        assert_eq!(send(address, &get("/healthz", &[])).status, 200, "health stays up while busy");

        server.gate.release();
        assert_eq!(first.join().unwrap().status, 200);
        for waiter in queued {
            assert_eq!(waiter.join().unwrap().status, 200);
        }
        assert_eq!(send(address, &post_ok(0)).status, 200, "the service recovers");
    }

    #[test]
    fn content_type_is_checked_before_the_body_is_used() {
        let server = Harness::start(vec![]);
        server.wait_ready();
        let token = auth();
        assert_eq!(send(server.address, &post(0, &[&token])).status, 415);
        assert_eq!(send(server.address, &post(0, &[&token, "Content-Type: text/plain"])).status, 415);
        assert_eq!(send(server.address, &post(0, &[&token, "Content-Type: AUDIO/WAV; rate=16000"])).status, 200);
        assert_eq!(send(server.address, &post(0, &[&token, "Content-Type: audio/x-wav"])).status, 200);
    }

    #[test]
    fn a_declared_oversize_body_is_refused_without_reading_it() {
        let server = Harness::start(vec![]);
        server.wait_ready();
        let request = format!(
            "POST /v1/transcribe HTTP/1.1\r\nHost: t\r\n{}\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\n\r\n",
            auth(),
            MAX_BODY_BYTES + 1
        );
        assert_eq!(send(server.address, request.as_bytes()).status, 413);
    }

    #[test]
    fn wav_errors_map_to_statuses() {
        let server = Harness::start(vec![]);
        server.wait_ready();
        let with_body = |body: &[u8]| {
            let mut request = format!(
                "POST /v1/transcribe HTTP/1.1\r\nHost: t\r\n{}\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\n\r\n",
                auth(),
                body.len()
            )
            .into_bytes();
            request.extend_from_slice(body);
            send(server.address, &request).status
        };
        assert_eq!(with_body(b"not audio at all"), 400);
        let mut float = wav_bytes(0);
        float[20] = 3;
        assert_eq!(with_body(&float), 415);
        let mut empty = wav_bytes(0);
        empty.truncate(44);
        empty[40..].copy_from_slice(&0_u32.to_le_bytes());
        assert_eq!(with_body(&empty), 422);
    }

    #[test]
    fn a_failed_model_load_stops_the_server_with_the_reason() {
        let settings = Settings {
            bind: "127.0.0.1:0".parse().unwrap(),
            allow: vec![],
            token: test_token(),
            logger: Logger::discard(),
            bind_wait: Duration::ZERO,
        };
        let result =
            run(settings, || Err::<Fake, String>("the model is broken".to_string()), std::future::pending(), |_| {});
        assert_eq!(result.unwrap_err(), "the model is broken");
    }
}
