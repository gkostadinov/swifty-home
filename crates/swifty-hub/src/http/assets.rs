//! The embedded web app: the shell at `/`, its modules and styles under `/app/`, the
//! Content-Security-Policy that goes with them, and `/healthz`.
//!
//! Every file is compiled into the binary, so the origin serves nothing it was not built
//! with and nothing a person uploaded. That is why `'self'` is as strict here as a list of
//! hashes was when the page was one file with its script and style inline.

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

use super::RouterState;
use crate::crypto::sha256_hex;
use crate::error::ApiError;

const INDEX_HTML: &str = include_str!("../../../../web/index.html");

/// Embeds each named file of `web/app`, by its path there.
macro_rules! app_files {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_str!(concat!("../../../../web/app/", $path)))),*]
    };
}

/// Every file under `web/app`. A test checks this list against the directory, so a file
/// added there and not here fails the build's tests instead of going missing from the page.
const APP_FILES: &[(&str, &str)] = app_files![
    "main.js",
    "core/util.js",
    "core/api.js",
    "core/store.js",
    "core/session.js",
    "core/status.js",
    "core/actions.js",
    "core/catalog.js",
    "core/guards.js",
    "core/router.js",
    "core/events.js",
    "core/hostdata.js",
    "ui/roomlight.js",
    "ui/glass.js",
    "ui/toast.js",
    "ui/scenes.js",
    "ui/gestures.js",
    "ui/sheets.js",
    "ui/lock.js",
    "ui/dock.js",
    "ui/views.js",
    "cards/light.js",
    "cards/speaker.js",
    "cards/vm.js",
    "cards/feeder.js",
    "cards/camera.js",
    "cards/printer.js",
    "cards/plug.js",
    "cards/aircon.js",
    "cards/index.js",
    "cards/generic.js",
    "cards/tv.js",
    "cards/litter.js",
    "cards/host.js",
    "cards/hostusage.js",
    "cards/router.js",
    "cards/service.js",
    "cards/telemetry.js",
    "pages/home.js",
    "pages/rooms.js",
    "pages/monitor.js",
    "pages/audit.js",
    "pages/passkeys.js",
    "pages/memory.js",
    "pages/people.js",
    "pages/integrations.js",
    "pages/setup.js",
    "swifty/parser.js",
    "swifty/voice.js",
    "swifty/chat.js",
    "swifty/figure.js",
    "console/rfb.js",
    "console/des.js",
    "css/app.css",
    "css/tokens.css",
    "css/base.css",
    "css/components.css",
    "css/sheets.css",
    "css/pages.css",
];

/// Nothing inline and nothing from elsewhere: scripts and styles come only from this
/// origin's embedded files. Inline `style` attributes stay allowed, because the page sets
/// custom properties on elements; they cannot run code. The prototype's Google Fonts link
/// is gone, and the page uses its system font stack.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
     style-src-attr 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; font-src 'none'; \
     media-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Every embedded file by the path it is served at (`/` and `/app/...`), with its text.
pub fn embedded() -> Vec<(String, &'static str)> {
    std::iter::once(("/".to_string(), INDEX_HTML))
        .chain(APP_FILES.iter().map(|(path, body)| (format!("/app/{path}"), *body)))
        .collect()
}

pub(crate) fn routes() -> Router<RouterState> {
    Router::new().route("/", get(index)).route("/app/{*path}", get(app_file)).route("/healthz", get(|| async { "ok" }))
}

struct Asset {
    body: &'static str,
    content_type: HeaderValue,
    etag: HeaderValue,
}

/// The embedded files, each with its type and an `ETag` from its content, worked out once.
pub(super) struct Page {
    index: Asset,
    files: HashMap<&'static str, Asset>,
    csp: HeaderValue,
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        _ => "text/html; charset=utf-8",
    }
}

fn asset(path: &str, body: &'static str) -> Asset {
    let etag = format!("\"{}\"", &sha256_hex(body.as_bytes())[..32]);
    Asset {
        body,
        content_type: HeaderValue::from_static(content_type(path)),
        etag: HeaderValue::from_str(&etag).expect("hex is ASCII"),
    }
}

pub(super) fn normalized_page() -> Page {
    Page {
        index: asset("index.html", INDEX_HTML),
        files: APP_FILES.iter().map(|(path, body)| (*path, asset(path, body))).collect(),
        csp: HeaderValue::from_static(CONTENT_SECURITY_POLICY),
    }
}

/// True when the request already holds this version, so it gets a 304 with no body.
fn fresh(request: &HeaderMap, etag: &HeaderValue) -> bool {
    let Some(Ok(wanted)) = request.get(header::IF_NONE_MATCH).map(|v| v.to_str()) else { return false };
    let etag = etag.to_str().unwrap_or_default();
    wanted.split(',').map(str::trim).any(|tag| tag == "*" || tag.trim_start_matches("W/") == etag)
}

fn serve(asset: &Asset, csp: &HeaderValue, request: &HeaderMap) -> Response {
    let mut response =
        if fresh(request, &asset.etag) { StatusCode::NOT_MODIFIED.into_response() } else { asset.body.into_response() };
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, asset.content_type.clone());
    headers.insert(header::ETAG, asset.etag.clone());
    // Kept, but asked for again every time with the ETag. The
    // hardening layer leaves it; everything else is `no-store`.
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(header::CONTENT_SECURITY_POLICY, csp.clone());
    response
}

async fn index(State(state): State<RouterState>, request: HeaderMap) -> Response {
    serve(&state.page.index, &state.page.csp, &request)
}

async fn app_file(State(state): State<RouterState>, Path(path): Path<String>, request: HeaderMap) -> Response {
    match state.page.files.get(path.as_str()) {
        Some(asset) => serve(asset, &state.page.csp, &request),
        None => ApiError::new(StatusCode::NOT_FOUND, "not_found", "Not found.").into_response(),
    }
}
