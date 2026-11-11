//! A generic HTTP device: records every request and answers through a handler.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::serve::ListenerExt;
use axum::Router;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use super::LOOPBACK;
use crate::conformance::Switches;

// ----- generic HTTP device

#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: HeaderMap,
    pub body: String,
}

impl Seen {
    pub fn header(&self, name: &str) -> &str {
        self.headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
    }
}

/// A handler that also reads the conformance suite's [`Switches`]: the name to
/// report while the suite asks for hostile text.
pub type RiggedHandler = dyn Fn(&Seen, &Switches) -> (u16, String) + Send + Sync + 'static;

/// Which requests ask the device to do something, as against reading it or signing in.
pub type CommandRule = dyn Fn(&Seen) -> bool + Send + Sync + 'static;

/// The usual rule: a request that is not `GET`, `HEAD` or `OPTIONS` asks the
/// device to do something.
pub fn is_command_method(seen: &Seen) -> bool {
    !matches!(seen.method.as_str(), "GET" | "HEAD" | "OPTIONS")
}

pub struct MockHttp {
    pub url: String,
    pub seen: Arc<Mutex<Vec<Seen>>>,
    pub connections: Arc<AtomicUsize>,
    /// While set, requests are recorded and never answered.
    pub hang: Arc<AtomicBool>,
    /// The same, for POST requests only.
    pub hang_posts: Arc<AtomicBool>,
    /// The conformance suite's switches. A fault in force spoils the body of
    /// every reply; a handler started with [`MockHttp::start_rigged`] reads the
    /// name to report from them.
    pub switches: Switches,
    commands: Arc<Mutex<Arc<CommandRule>>>,
    reports_text: bool,
    task: JoinHandle<()>,
}

impl Drop for MockHttp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockHttp {
    pub async fn start(handler: impl Fn(&Seen) -> (u16, String) + Send + Sync + 'static) -> Self {
        Self::start_with(Router::new(), handler).await
    }

    /// `extra` answers its own routes, and everything else goes to `handler`.
    pub async fn start_with(extra: Router, handler: impl Fn(&Seen) -> (u16, String) + Send + Sync + 'static) -> Self {
        Self::serve(extra, Arc::new(move |seen: &Seen, _: &Switches| handler(seen)), false).await
    }

    /// A device that reports text of its own (names, labels): `handler` takes
    /// the name to report from [`Switches::device_text`], so the conformance
    /// suite can make it report hostile text (check 10).
    pub async fn start_rigged(handler: impl Fn(&Seen, &Switches) -> (u16, String) + Send + Sync + 'static) -> Self {
        Self::serve(Router::new(), Arc::new(handler), true).await
    }

    /// [`MockHttp::start_rigged`] with routes of its own, as [`MockHttp::start_with`].
    pub async fn start_with_rigged(
        extra: Router,
        handler: impl Fn(&Seen, &Switches) -> (u16, String) + Send + Sync + 'static,
    ) -> Self {
        Self::serve(extra, Arc::new(handler), true).await
    }

    async fn serve(extra: Router, handler: Arc<RiggedHandler>, reports_text: bool) -> Self {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let hang = Arc::new(AtomicBool::new(false));
        let hang_posts = Arc::new(AtomicBool::new(false));
        let switches = Switches::default();
        let (seen_for_route, hang_for_route, posts_for_route, switches_for_route) =
            (seen.clone(), hang.clone(), hang_posts.clone(), switches.clone());
        let router = extra.fallback(move |request: Request| {
            let (seen, hang, hang_posts, handler, switches) = (
                seen_for_route.clone(),
                hang_for_route.clone(),
                posts_for_route.clone(),
                handler.clone(),
                switches_for_route.clone(),
            );
            async move {
                let (parts, body) = request.into_parts();
                let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
                let record = Seen {
                    method: parts.method.to_string(),
                    path: parts.uri.path().to_string(),
                    query: parts.uri.query().unwrap_or("").to_string(),
                    headers: parts.headers,
                    body: String::from_utf8_lossy(&bytes).to_string(),
                };
                seen.lock().unwrap().push(record.clone());
                if hang.load(Ordering::SeqCst) || (record.method == "POST" && hang_posts.load(Ordering::SeqCst)) {
                    tokio::time::sleep(Duration::from_secs(120)).await;
                }
                let (status, text) = handler(&record, &switches);
                let body = switches.garble(text.into_bytes());
                Response::builder().status(StatusCode::from_u16(status).unwrap()).body(Body::from(body)).unwrap()
            }
        });
        let listener = TcpListener::bind(LOOPBACK).await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let counter = connections.clone();
        let listener = listener.tap_io(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let commands: Arc<Mutex<Arc<CommandRule>>> = Arc::new(Mutex::new(Arc::new(is_command_method)));
        Self { url, seen, connections, hang, hang_posts, switches, commands, reports_text, task }
    }

    pub fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    pub fn posts(&self) -> Vec<Seen> {
        self.requests().into_iter().filter(|r| r.method == "POST").collect()
    }

    pub fn traffic(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    /// Says which requests ask this device to do something. [`is_command_method`]
    /// until it is changed.
    pub fn count_commands(&self, rule: impl Fn(&Seen) -> bool + Send + Sync + 'static) {
        *self.commands.lock().unwrap() = Arc::new(rule);
    }

    /// The requests so far that asked the device to do something.
    pub fn commands(&self) -> usize {
        let rule = self.commands.lock().unwrap().clone();
        self.seen.lock().unwrap().iter().filter(|seen| rule(seen)).count()
    }

    /// Whether its handler reports a name the suite can set.
    pub fn reports_text(&self) -> bool {
        self.reports_text
    }
}
