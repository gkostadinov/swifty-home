//! Rooms: their devices, order and scenes.

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::guards::{Authed, Body, Owner};
use super::{audit_arrangement, NameBody, RoomList, RouterState, TargetList};
use crate::app::App;
use crate::error::ApiError;
use crate::policy::Role;
use crate::rooms::{self, LightSetting, RoomView};

pub(crate) fn routes() -> OpenApiRouter<RouterState> {
    OpenApiRouter::new()
        .routes(routes!(rooms_list, room_create))
        .routes(routes!(rooms_order))
        .routes(routes!(room_rename, room_delete))
        .routes(routes!(room_devices))
        .routes(routes!(room_scene_save))
        .routes(routes!(room_scene_delete))
}

/// A room's own scene: a name, and what it does to each light.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = RoomSceneBody)]
struct SceneBody {
    name: String,
    settings: Vec<LightSetting>,
}

/// The rooms, in their order.
#[derive(Serialize, ToSchema)]
pub struct Rooms {
    /// A family member sees only the devices they may see in each.
    pub rooms: Vec<RoomView>,
}

/// One room, after a change.
#[derive(Serialize, ToSchema)]
pub struct OneRoom {
    pub room: RoomView,
}

/// `{"deleted": true}`.
#[derive(Serialize, ToSchema)]
pub struct Deleted {
    pub deleted: bool,
}

/// The rooms, in their order, with their devices and scenes.
#[utoipa::path(
    get,
    path = "/rooms",
    tag = "rooms",
    security(("session" = [])),
    responses((status = 200, body = Rooms))
)]
async fn rooms_list(State(app): State<App>, Authed(session): Authed) -> Result<Json<Rooms>, ApiError> {
    let index = app.registry.index();
    let rooms = app.store.with(|conn| rooms::list(conn, &index, session.person.role))?;
    Ok(Json(Rooms { rooms }))
}

/// Makes a room, last in the order.
#[utoipa::path(
    post,
    path = "/rooms",
    tag = "rooms",
    security(("session" = ["owner"])),
    request_body = NameBody,
    responses((status = 200, body = OneRoom))
)]
async fn room_create(
    State(app): State<App>,
    Owner(session): Owner,
    Body(body): Body<NameBody>,
) -> Result<Json<OneRoom>, ApiError> {
    let now = app.now();
    let room = app.store.tx(|tx| -> Result<_, ApiError> {
        let id = rooms::create(tx, now, &body.name)?;
        audit_arrangement(&app, tx, &session, "room.create", &id, json!({ "name": body.name }))?;
        rooms::get(tx, &app.registry.index(), Role::Owner, &id)
    })?;
    Ok(Json(OneRoom { room }))
}

/// Renames a room.
#[utoipa::path(
    patch,
    path = "/rooms/{id}",
    tag = "rooms",
    security(("session" = ["owner"])),
    params(("id" = String, Path, description = "The room's id.")),
    request_body = NameBody,
    responses((status = 200, body = OneRoom))
)]
async fn room_rename(
    State(app): State<App>,
    Owner(session): Owner,
    Path(id): Path<String>,
    Body(body): Body<NameBody>,
) -> Result<Json<OneRoom>, ApiError> {
    let room = app.store.tx(|tx| -> Result<_, ApiError> {
        rooms::rename(tx, &id, &body.name)?;
        audit_arrangement(&app, tx, &session, "room.rename", &id, json!({ "name": body.name }))?;
        rooms::get(tx, &app.registry.index(), Role::Owner, &id)
    })?;
    Ok(Json(OneRoom { room }))
}

/// Deletes a room. Its devices stay; only their place in it goes.
#[utoipa::path(
    delete,
    path = "/rooms/{id}",
    tag = "rooms",
    security(("session" = ["owner"])),
    params(("id" = String, Path, description = "The room's id.")),
    responses((status = 200, body = Deleted))
)]
async fn room_delete(
    State(app): State<App>,
    Owner(session): Owner,
    Path(id): Path<String>,
) -> Result<Json<Deleted>, ApiError> {
    app.store.tx(|tx| -> Result<(), ApiError> {
        rooms::delete(tx, &id)?;
        audit_arrangement(&app, tx, &session, "room.delete", &id, Value::Null)
    })?;
    Ok(Json(Deleted { deleted: true }))
}

/// Puts the rooms in this order. Every room must be listed once.
#[utoipa::path(
    put,
    path = "/rooms/order",
    tag = "rooms",
    security(("session" = ["owner"])),
    request_body = RoomList,
    responses((status = 200, body = Rooms))
)]
async fn rooms_order(
    State(app): State<App>,
    Owner(session): Owner,
    Body(body): Body<RoomList>,
) -> Result<Json<Rooms>, ApiError> {
    let rooms = app.store.tx(|tx| -> Result<_, ApiError> {
        rooms::reorder(tx, &body.rooms)?;
        audit_arrangement(&app, tx, &session, "room.order", "-", Value::Null)?;
        rooms::list(tx, &app.registry.index(), Role::Owner)
    })?;
    Ok(Json(Rooms { rooms }))
}

/// Sets which devices are in a room, in order. A device is in one room at most.
#[utoipa::path(
    put,
    path = "/rooms/{id}/devices",
    tag = "rooms",
    security(("session" = ["owner"])),
    params(("id" = String, Path, description = "The room's id.")),
    request_body = TargetList,
    responses((status = 200, body = OneRoom))
)]
async fn room_devices(
    State(app): State<App>,
    Owner(session): Owner,
    Path(id): Path<String>,
    Body(body): Body<TargetList>,
) -> Result<Json<OneRoom>, ApiError> {
    let room = app.store.tx(|tx| -> Result<_, ApiError> {
        rooms::set_devices(tx, &app.registry.index(), &id, &body.targets)?;
        audit_arrangement(&app, tx, &session, "room.devices", &id, json!({ "targets": body.targets }))?;
        rooms::get(tx, &app.registry.index(), Role::Owner, &id)
    })?;
    Ok(Json(OneRoom { room }))
}

/// Saves a scene of the room's own, run with `room.scene`.
#[utoipa::path(
    post,
    path = "/rooms/{id}/scenes",
    tag = "rooms",
    security(("session" = ["owner"])),
    params(("id" = String, Path, description = "The room's id.")),
    request_body = SceneBody,
    responses((status = 200, body = OneRoom))
)]
async fn room_scene_save(
    State(app): State<App>,
    Owner(session): Owner,
    Path(id): Path<String>,
    Body(body): Body<SceneBody>,
) -> Result<Json<OneRoom>, ApiError> {
    let room = app.store.tx(|tx| -> Result<_, ApiError> {
        rooms::save_scene(tx, &app.registry.index(), &id, &body.name, &body.settings)?;
        audit_arrangement(&app, tx, &session, "room.scene_save", &id, json!({ "name": body.name }))?;
        rooms::get(tx, &app.registry.index(), Role::Owner, &id)
    })?;
    Ok(Json(OneRoom { room }))
}

/// Deletes one of a room's scenes.
#[utoipa::path(
    delete,
    path = "/rooms/{id}/scenes/{scene}",
    tag = "rooms",
    security(("session" = ["owner"])),
    params(
        ("id" = String, Path, description = "The room's id."),
        ("scene" = String, Path, description = "The scene's id within the room."),
    ),
    responses((status = 200, body = Deleted))
)]
async fn room_scene_delete(
    State(app): State<App>,
    Owner(session): Owner,
    Path((id, scene)): Path<(String, String)>,
) -> Result<Json<Deleted>, ApiError> {
    app.store.tx(|tx| -> Result<(), ApiError> {
        rooms::delete_scene(tx, &id, &scene)?;
        audit_arrangement(&app, tx, &session, "room.scene_delete", &id, Value::Null)
    })?;
    Ok(Json(Deleted { deleted: true }))
}
