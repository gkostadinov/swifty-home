//! What Swifty remembers and has learned.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::guards::{Authed, Body, Owner};
use super::{audit_arrangement, RouterState};
use crate::app::App;
use crate::error::ApiError;
use crate::memory::{self, Learned, Memory, Review};
use crate::policy::Role;

pub(crate) fn routes() -> OpenApiRouter<RouterState> {
    OpenApiRouter::new()
        .routes(routes!(memory_read, memory_add))
        .routes(routes!(memory_forget))
        .routes(routes!(learned_forget))
}

/// Something for Swifty to remember: the person's own, or the house's (owner only).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct MemoryBody {
    text: String,
    /// The house's memory rather than the person's own. Only the owner may write it.
    #[serde(default)]
    house: bool,
}

/// What Swifty remembers for this person, and has learned.
#[derive(Serialize, ToSchema)]
pub struct Memories {
    /// The person's own.
    pub mine: Vec<Memory>,
    /// The house's, which everyone's Swifty knows.
    pub house: Vec<Memory>,
    /// Whether this person may change the house's: the owner.
    pub house_editable: bool,
    /// The phrases Swifty has learned. Empty for anyone but the owner.
    pub learned: Vec<Learned>,
    /// The last week's review. Only for the owner.
    pub review: Option<Review>,
}

/// A memory that was just kept.
#[derive(Serialize, ToSchema)]
pub struct MemoryKept {
    pub memory: Memory,
}

/// A memory that was just forgotten.
#[derive(Serialize, ToSchema)]
pub struct MemoryForgotten {
    pub forgotten: Memory,
}

/// `{"forgotten": true}`.
#[derive(Serialize, ToSchema)]
pub struct Unlearned {
    pub forgotten: bool,
}

fn memory_whose(house: bool) -> &'static str {
    if house {
        "house"
    } else {
        "-"
    }
}

/// The person's memories and the house's.
///
/// The owner also gets the learned phrases and the last week's review.
#[utoipa::path(
    get,
    path = "/memory",
    tag = "memory",
    security(("session" = [])),
    responses((status = 200, body = Memories))
)]
async fn memory_read(State(app): State<App>, Authed(session): Authed) -> Result<Json<Memories>, ApiError> {
    let person = &session.person;
    let owner = person.role == Role::Owner;
    let now = app.now();
    let (mine, house, learned, review) = app.store.with(|conn| -> rusqlite::Result<_> {
        let (mine, house) = memory::list(conn, &person.user_id)?;
        let learned = if owner { memory::learned_list(conn)? } else { Vec::new() };
        let review = if owner { Some(memory::review(conn, now)?) } else { None };
        Ok((mine, house, learned, review))
    })?;
    Ok(Json(Memories { mine, house, house_editable: owner, learned, review }))
}

/// Keeps something for Swifty to remember.
#[utoipa::path(
    post,
    path = "/memory",
    tag = "memory",
    security(("session" = [])),
    request_body = MemoryBody,
    responses((status = 200, body = MemoryKept))
)]
async fn memory_add(
    State(app): State<App>,
    Authed(session): Authed,
    Body(body): Body<MemoryBody>,
) -> Result<Json<MemoryKept>, ApiError> {
    let now = app.now();
    let memory = app.store.tx(|tx| -> Result<_, ApiError> {
        let kept = memory::add(tx, now, &session.person.user_id, session.person.role, &body.text, body.house)?;
        audit_arrangement(&app, tx, &session, "memory.add", memory_whose(body.house), Value::Null)?;
        Ok(kept)
    })?;
    Ok(Json(MemoryKept { memory }))
}

/// Forgets a memory: the person's own, or the house's (owner only).
#[utoipa::path(
    delete,
    path = "/memory/{id}",
    tag = "memory",
    security(("session" = [])),
    params(("id" = String, Path, description = "The memory's id.")),
    responses((status = 200, body = MemoryForgotten))
)]
async fn memory_forget(
    State(app): State<App>,
    Authed(session): Authed,
    Path(id): Path<String>,
) -> Result<Json<MemoryForgotten>, ApiError> {
    let forgotten = app.store.tx(|tx| -> Result<_, ApiError> {
        let gone = memory::forget(tx, &session.person.user_id, session.person.role, &id)?;
        audit_arrangement(&app, tx, &session, "memory.forget", memory_whose(gone.house), Value::Null)?;
        Ok(gone)
    })?;
    Ok(Json(MemoryForgotten { forgotten }))
}

/// Forgets a learned phrase.
#[utoipa::path(
    delete,
    path = "/learned/{id}",
    tag = "memory",
    security(("session" = ["owner"])),
    params(("id" = String, Path, description = "The learned phrase's id.")),
    responses((status = 200, body = Unlearned))
)]
async fn learned_forget(
    State(app): State<App>,
    Owner(session): Owner,
    Path(id): Path<String>,
) -> Result<Json<Unlearned>, ApiError> {
    app.store.tx(|tx| -> Result<(), ApiError> {
        if !memory::unlearn(tx, &id)? {
            return Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_phrase", "That phrase is not learned."));
        }
        audit_arrangement(&app, tx, &session, "learned.forget", "-", Value::Null)
    })?;
    Ok(Json(Unlearned { forgotten: true }))
}
