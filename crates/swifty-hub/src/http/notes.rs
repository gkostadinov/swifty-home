//! The owner's notes on each device: `GET` and `PUT /api/v1/devices/{target}/notes`,
//! for the owner only.
//!
//! Notes are free lines, such as what goes offline while a VM is down. They are
//! shown where an action on the device is confirmed (the catalog's description), and
//! [`crate::notes`] stores them by target. A `PUT` replaces every line; no lines
//! removes them. Each change is audited as `device.notes` with the number of lines,
//! not their text, and published as a `devices` event, so open pages fetch again.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::guards::{Body, Owner};
use super::{audit_arrangement, unprocessable, RouterState};
use crate::app::App;
use crate::error::ApiError;
use crate::events::Event;
use crate::notes;

pub(crate) fn routes() -> OpenApiRouter<RouterState> {
    OpenApiRouter::new().routes(routes!(notes_get, notes_set))
}

/// Every line of a device's notes: at most 8, each 1 to 200 characters, no control
/// characters or surrounding spaces. No lines removes them.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct NotesBody {
    notes: Vec<String>,
}

/// A device's notes.
#[derive(Serialize, ToSchema)]
pub struct Notes {
    pub target: String,
    /// Free lines, shown where an action on the device is confirmed. Empty when there are none.
    pub notes: Vec<String>,
}

/// Notes are for the devices the index lists, as names are.
fn known_device(app: &App, target: &str) -> Result<(), ApiError> {
    if app.registry.index().get(target).is_none_or(|device| device.pending) {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_device", "No such device."));
    }
    Ok(())
}

/// A device's notes.
#[utoipa::path(
    get,
    path = "/devices/{target}/notes",
    tag = "devices",
    security(("session" = ["owner"])),
    params(("target" = String, Path, description = "The device's target.")),
    responses((status = 200, body = Notes))
)]
async fn notes_get(
    State(app): State<App>,
    Owner(_session): Owner,
    Path(target): Path<String>,
) -> Result<Json<Notes>, ApiError> {
    known_device(&app, &target)?;
    let notes = app.store.with(|conn| notes::get(conn, &target))?;
    Ok(Json(Notes { target, notes }))
}

/// Replaces a device's notes.
///
/// Audited as `device.notes` with the number of lines, never their text. A change is
/// announced as a `devices` event.
#[utoipa::path(
    put,
    path = "/devices/{target}/notes",
    tag = "devices",
    security(("session" = ["owner"])),
    params(("target" = String, Path, description = "The device's target.")),
    request_body = NotesBody,
    responses((status = 200, body = Notes))
)]
async fn notes_set(
    State(app): State<App>,
    Owner(session): Owner,
    Path(target): Path<String>,
    Body(body): Body<NotesBody>,
) -> Result<Json<Notes>, ApiError> {
    known_device(&app, &target)?;
    if let Some(problem) = notes::problem(&body.notes) {
        return Err(unprocessable("invalid_notes", problem));
    }
    let changed = app.store.tx(|tx| -> Result<bool, ApiError> {
        let before = notes::get(tx, &target)?;
        notes::set(tx, &target, &body.notes)?;
        audit_arrangement(&app, tx, &session, "device.notes", &target, json!({ "lines": body.notes.len() }))?;
        Ok(before != body.notes)
    })?;
    if changed {
        app.events.publish(Event::Devices);
    }
    Ok(Json(Notes { target, notes: body.notes }))
}
