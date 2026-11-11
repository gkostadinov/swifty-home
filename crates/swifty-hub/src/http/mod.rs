//! The HTTP API.
//!
//! Every route of the API is under [`API_PREFIX`]. Only the page
//! (`GET /`, `GET /app/...`) and `GET /healthz` are outside it, and nothing else
//! answers: the old unversioned `/api/...` paths are gone, not aliased.
//!
//! Every state-changing request must carry the configured `Origin`. There is no
//! CORS layer, so no response ever grants another origin access.
//!
//! Each file here exposes `routes()`, with paths below the prefix, and [`router`]
//! mounts them under it. Every route is registered with `utoipa_axum`'s
//! `routes!`, so it comes with its entry in the OpenAPI document ([`openapi`]).
//! Everything that is owner-only takes the [`guards::Owner`] extractor, so the
//! rule is in the handler's signature and cannot be left out of its body.

pub mod actions;
pub mod assets;
pub mod assistant;
pub mod auth;
pub mod devices;
pub mod energy;
pub mod events;
pub mod guards;
pub mod machines;
pub mod memory;
pub mod meta;
pub mod notes;
pub mod openapi;
pub mod people;
pub mod plugins;
pub mod rooms;
pub mod scenes;
pub mod setup;
pub mod voice;

use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::middleware;
use axum::Router;
use serde::Deserialize;
use serde_json::Value;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;

use crate::actions::Actor;
use crate::app::App;
use crate::audit::Event;
use crate::error::ApiError;
use crate::sessions::Session;

const JSON_LIMIT: usize = 64 * 1024;

/// Where every route of the API lives. A breaking change would be `/api/v2` beside it.
pub const API_PREFIX: &str = "/api/v1";
/// The API's version. Within a major version nothing is removed or renamed; the
/// minor version moves when something is added.
pub const API_MAJOR: u32 = 1;
pub const API_MINOR: u32 = 0;
/// The header every response carries, with [`API_MAJOR`] as its value.
pub const API_HEADER: &str = "x-swifty-api";

#[derive(Clone)]
pub(crate) struct RouterState {
    pub(crate) app: App,
    page: Arc<assets::Page>,
}

impl axum::extract::FromRef<RouterState> for App {
    fn from_ref(state: &RouterState) -> App {
        state.app.clone()
    }
}

pub fn router(app: App) -> Router {
    let state = RouterState { app: app.clone(), page: Arc::new(assets::normalized_page()) };
    let (api, _document) = api().split_for_parts();
    Router::new()
        .merge(assets::routes())
        .merge(api)
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(JSON_LIMIT))
        .layer(middleware::from_fn_with_state(app, guards::origin_guard))
        .layer(middleware::from_fn(guards::security_headers))
        .with_state(state)
}

/// Every route of the API under [`API_PREFIX`], each with its entry in the document.
pub(crate) fn api() -> OpenApiRouter<RouterState> {
    OpenApiRouter::new().nest(API_PREFIX, api_routes())
}

/// Every route of the API, with its path below [`API_PREFIX`].
fn api_routes() -> OpenApiRouter<RouterState> {
    OpenApiRouter::new()
        .merge(meta::routes())
        .merge(auth::routes())
        .merge(people::routes())
        .merge(actions::routes())
        .merge(devices::routes())
        .merge(rooms::routes())
        .merge(machines::routes())
        .merge(energy::routes())
        .merge(memory::routes())
        .merge(assistant::routes())
        .merge(plugins::routes())
        .merge(setup::routes())
        .merge(scenes::routes())
        .merge(notes::routes())
        .merge(events::routes())
        .merge(openapi::routes())
        .merge(voice::routes())
}

async fn not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "not_found", "Not found.")
}

pub(crate) fn actor(session: &Session) -> Actor {
    Actor {
        session_ref: session.session_ref.clone(),
        credential_id: session.credential_id.clone(),
        device: session.device.clone(),
        user_id: session.person.user_id.clone(),
        role: session.person.role,
    }
}

pub(crate) fn plain_event(app: &App, action: &str, target: &str, session: Option<&Session>, result: &str) -> Event {
    Event {
        ts: app.now(),
        action: action.to_string(),
        target: target.to_string(),
        params: Value::Null,
        credential_ref: session.map_or("-".to_string(), |s| s.credential_id.clone()),
        session_ref: session.map_or("-".to_string(), |s| s.session_ref.clone()),
        user_ref: session.map_or("-".to_string(), |s| s.person.user_id.clone()),
        device: session.map_or("-".to_string(), |s| s.device.clone()),
        channel: "panel".to_string(),
        step_up: "none".to_string(),
        result: result.to_string(),
        rtt_ms: None,
        run_id: None,
    }
}

fn unprocessable(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, code, message)
}

fn audit_arrangement(
    app: &App,
    tx: &rusqlite::Connection,
    session: &Session,
    action: &str,
    target: &str,
    params: Value,
) -> Result<(), ApiError> {
    let mut event = plain_event(app, action, target, Some(session), "done");
    event.params = params;
    app.append_audit(tx, &event)?;
    Ok(())
}

// ----- request bodies shared by more than one file

/// Device targets, each once.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct TargetList {
    targets: Vec<String>,
}

/// Room ids, each once.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct RoomList {
    rooms: Vec<String>,
}

/// A name for a person to read: one line, 1 to 64 characters.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct NameBody {
    name: String,
}
