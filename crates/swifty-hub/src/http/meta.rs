//! `GET /api/v1/meta`: what this hub is and supports, for a client that has not
//! signed in yet. It names no device, person or setting.

use axum::extract::State;
use axum::Json;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::{RouterState, API_MAJOR, API_MINOR};
use crate::app::App;
use crate::error::ApiError;
use crate::passkeys;

/// What the product calls itself here.
pub const PRODUCT: &str = "Swifty Home";

pub(crate) fn routes() -> OpenApiRouter<RouterState> {
    OpenApiRouter::new().routes(routes!(meta))
}

/// What a hub is and supports, before anyone signs in.
#[derive(Debug, Serialize, ToSchema)]
pub struct Meta {
    /// Always `Swifty Home`.
    pub product: &'static str,
    /// The hub's version.
    pub version: &'static str,
    /// The API's version: a client of v1 works with any `minor`.
    pub api: ApiVersion,
    /// The version of the plugin contract this hub speaks.
    pub plugin_api: u32,
    /// Whether commands reach real devices. False in dry-run.
    pub live: bool,
    /// True until the first passkey is enrolled, which is what the enrollment token is for.
    pub needs_owner: bool,
    /// How a person signs in: `passkey`.
    pub auth: Vec<&'static str>,
    /// How a client hears of changes: `sse` (`GET /api/v1/events`).
    pub events: Vec<&'static str>,
}

/// The API's version.
#[derive(Debug, Serialize, ToSchema)]
pub struct ApiVersion {
    /// Nothing is removed or renamed within a major version.
    pub major: u32,
    /// Moves when something is added.
    pub minor: u32,
}

/// What this hub is and supports.
///
/// Open to anyone, so an app can see what a hub offers before it signs in. It names
/// no device, person or setting.
#[utoipa::path(get, path = "/meta", tag = "meta", responses((status = 200, body = Meta)))]
async fn meta(State(app): State<App>) -> Result<Json<Meta>, ApiError> {
    let needs_owner = app.store.with(passkeys::active_count)? == 0;
    Ok(Json(Meta {
        product: PRODUCT,
        version: env!("CARGO_PKG_VERSION"),
        api: ApiVersion { major: API_MAJOR, minor: API_MINOR },
        plugin_api: swifty_plugin_api::API_VERSION,
        live: app.registry.is_live(),
        needs_owner,
        auth: vec!["passkey"],
        events: vec!["sse"],
    }))
}
