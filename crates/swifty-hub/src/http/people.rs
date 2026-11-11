//! What each person keeps (favourites, pinned rooms), and the people and their
//! invitations.

use std::collections::HashSet;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::guards::{Authed, Body, Owner};
use super::{audit_arrangement, unprocessable, RoomList, RouterState, TargetList};
use crate::app::App;
use crate::error::ApiError;
use crate::policy::{self, may_see};
use crate::rooms;
use crate::sessions::Session;
use crate::users::{self, InvitationView, UserView};

pub(crate) fn routes() -> OpenApiRouter<RouterState> {
    OpenApiRouter::new()
        .routes(routes!(me))
        .routes(routes!(set_favourites))
        .routes(routes!(set_pinned_rooms))
        .routes(routes!(users_list))
        .routes(routes!(invitation_cancel))
}

fn all_different(list: &[String]) -> bool {
    list.iter().collect::<HashSet<_>>().len() == list.len()
}

/// What a person keeps for themselves, in their order.
#[derive(Serialize, ToSchema)]
pub struct Preferences {
    /// Device targets and scene ids.
    pub favourites: Vec<String>,
    /// Room ids.
    pub pinned_rooms: Vec<String>,
}

/// Who is signed in here, and what they keep.
#[derive(Serialize, ToSchema)]
pub struct Me {
    pub session: Session,
    #[schema(value_type = Preferences)]
    pub preferences: Value,
    /// Whether Swifty, the assistant, is set up on this hub.
    pub assistant: bool,
}

/// A person's preferences after a change.
#[derive(Serialize, ToSchema)]
pub struct PreferencesChanged {
    #[schema(value_type = Preferences)]
    pub preferences: Value,
}

/// The people of the house, and the invitations still open.
#[derive(Serialize, ToSchema)]
pub struct People {
    pub users: Vec<UserView>,
    pub invitations: Vec<InvitationView>,
}

/// `{"cancelled": true}`.
#[derive(Serialize, ToSchema)]
pub struct Cancelled {
    pub cancelled: bool,
}

/// Who is signed in, and what they keep.
#[utoipa::path(get, path = "/me", tag = "people", security(("session" = [])), responses((status = 200, body = Me)))]
async fn me(State(app): State<App>, Authed(principal): Authed) -> Result<Json<Me>, ApiError> {
    let preferences = app.store.with(|conn| users::preferences(conn, &principal.person.user_id))?;
    Ok(Json(Me { session: principal.session, preferences, assistant: app.assistant.is_some() }))
}

/// Sets this person's favourites, in order: devices they may see, and scenes.
#[utoipa::path(
    put,
    path = "/me/favourites",
    tag = "people",
    security(("session" = [])),
    request_body = TargetList,
    responses((status = 200, body = PreferencesChanged))
)]
async fn set_favourites(
    State(app): State<App>,
    Authed(session): Authed,
    Body(body): Body<TargetList>,
) -> Result<Json<PreferencesChanged>, ApiError> {
    if body.targets.len() > users::MAX_FAVOURITES || !all_different(&body.targets) {
        return Err(unprocessable("invalid_favourites", "List each favourite once, and not too many."));
    }
    let person = &session.person;
    let index = app.registry.index();
    let allowed = |target: &String| {
        policy::SCENE_FAVOURITES.contains(&target.as_str())
            || (may_see(&index, person.role, target) && index.get(target).is_some_and(|device| !device.pending))
    };
    if !body.targets.iter().all(allowed) {
        return Err(unprocessable("invalid_favourites", "One of those cannot be a favourite."));
    }
    let preferences = app.store.tx(|tx| -> Result<Value, ApiError> {
        users::set_favourites(tx, &person.user_id, &body.targets)?;
        Ok(users::preferences(tx, &person.user_id)?)
    })?;
    Ok(Json(PreferencesChanged { preferences }))
}

/// Sets the rooms this person pins, in order.
#[utoipa::path(
    put,
    path = "/me/pinned-rooms",
    tag = "people",
    security(("session" = [])),
    request_body = RoomList,
    responses((status = 200, body = PreferencesChanged))
)]
async fn set_pinned_rooms(
    State(app): State<App>,
    Authed(session): Authed,
    Body(body): Body<RoomList>,
) -> Result<Json<PreferencesChanged>, ApiError> {
    if body.rooms.len() > users::MAX_PINNED_ROOMS || !all_different(&body.rooms) {
        return Err(unprocessable("invalid_pins", "List each room once, and not too many."));
    }
    let person = &session.person;
    let preferences = app.store.tx(|tx| -> Result<Value, ApiError> {
        for room in &body.rooms {
            if !rooms::exists(tx, room)? {
                return Err(unprocessable("invalid_pins", "One of those is not a room."));
            }
        }
        users::set_pinned_rooms(tx, &person.user_id, &body.rooms)?;
        Ok(users::preferences(tx, &person.user_id)?)
    })?;
    Ok(Json(PreferencesChanged { preferences }))
}

/// The people of the house, and the invitations still open.
///
/// People are added with the critical action `user.invite` and removed with `user.remove`.
#[utoipa::path(
    get,
    path = "/users",
    tag = "people",
    security(("session" = ["owner"])),
    responses((status = 200, body = People))
)]
async fn users_list(State(app): State<App>, Owner(_session): Owner) -> Result<Json<People>, ApiError> {
    let now = app.now();
    let (users, invitations) = app.store.with(|conn| -> rusqlite::Result<_> {
        Ok((users::list_users(conn)?, users::pending_invitations(conn, now)?))
    })?;
    Ok(Json(People { users, invitations }))
}

/// Cancels an open invitation.
#[utoipa::path(
    delete,
    path = "/invitations/{id}",
    tag = "people",
    security(("session" = ["owner"])),
    params(("id" = String, Path, description = "The invitation's id.")),
    responses((status = 200, body = Cancelled))
)]
async fn invitation_cancel(
    State(app): State<App>,
    Owner(session): Owner,
    Path(id): Path<String>,
) -> Result<Json<Cancelled>, ApiError> {
    let now = app.now();
    app.store.tx(|tx| -> Result<(), ApiError> {
        if !users::cancel_invitation(tx, now, &id)? {
            return Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_invitation", "No such open invitation."));
        }
        audit_arrangement(&app, tx, &session, "user.invite_cancel", &id, Value::Null)
    })?;
    Ok(Json(Cancelled { cancelled: true }))
}
