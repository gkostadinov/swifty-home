//! The chat with Swifty, and the jobs its agent runs.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::Value;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::guards::{Authed, Body};
use super::{actor, RouterState};
use crate::app::App;
use crate::assistant;
use crate::error::ApiError;

pub(crate) fn routes() -> OpenApiRouter<RouterState> {
    OpenApiRouter::new()
        .routes(routes!(assistant_route))
        .routes(routes!(assistant_job))
        .routes(routes!(assistant_job_stop))
        .routes(routes!(assistant_job_answer))
        .routes(routes!(assistant_resume))
}

/// Asks Swifty.
///
/// The answer is a reply, an action for the person to run or confirm, or a job its
/// agent works on (follow it with `GET /assistant/jobs/{id}` or the `assistant`
/// event). `503` when Swifty is not set up here (`/me` says `assistant: false`).
#[utoipa::path(
    post,
    path = "/assistant",
    tag = "assistant",
    security(("session" = [])),
    request_body = assistant::AssistantRequest,
    responses((status = 200, description = "Not typed yet: see `docs/api/untyped.md`.", body = Object))
)]
async fn assistant_route(
    State(app): State<App>,
    Authed(session): Authed,
    Body(body): Body<assistant::AssistantRequest>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(assistant::answer(&app, &actor(&session), body).await?))
}

fn no_agent() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "unknown_job", "That answer is no longer here.")
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
struct JobQuery {
    /// The version of the job this client already has. The answer then waits until
    /// there is something newer, or a while has passed.
    seen: Option<u64>,
}

/// A job of Swifty's agent: what it has written so far.
#[utoipa::path(
    get,
    path = "/assistant/jobs/{id}",
    tag = "assistant",
    security(("session" = [])),
    params(("id" = String, Path, description = "The job's id."), JobQuery),
    responses((status = 200, description = "Not typed yet: see `docs/api/untyped.md`.", body = Object))
)]
async fn assistant_job(
    State(app): State<App>,
    Authed(session): Authed,
    Path(id): Path<String>,
    Query(query): Query<JobQuery>,
) -> Result<Json<Value>, ApiError> {
    let assistant = app.assistant.as_ref().ok_or_else(no_agent)?;
    Ok(Json(assistant.jobs().view(&session.person.user_id, &id, query.seen).await?))
}

/// Stops a job of Swifty's agent.
#[utoipa::path(
    post,
    path = "/assistant/jobs/{id}/stop",
    tag = "assistant",
    security(("session" = [])),
    params(("id" = String, Path, description = "The job's id.")),
    responses((status = 200, description = "Not typed yet: see `docs/api/untyped.md`.", body = Object))
)]
async fn assistant_job_stop(
    State(app): State<App>,
    Authed(session): Authed,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(crate::agent::stop(&app, &session.person.user_id, &id).await?))
}

/// Answers a question a job of Swifty's agent asked.
#[utoipa::path(
    post,
    path = "/assistant/jobs/{id}/answer",
    tag = "assistant",
    security(("session" = [])),
    params(("id" = String, Path, description = "The job's id.")),
    request_body = crate::agent::QuestionAnswer,
    responses((status = 200, description = "Not typed yet: see `docs/api/untyped.md`.", body = Object))
)]
async fn assistant_job_answer(
    State(app): State<App>,
    Authed(session): Authed,
    Path(id): Path<String>,
    Body(body): Body<crate::agent::QuestionAnswer>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(crate::agent::answer(&app, &actor(&session), &id, body).await?))
}

/// The answer still being written in a chat, after the page came back.
///
/// Starts nothing new.
#[utoipa::path(
    post,
    path = "/assistant/resume",
    tag = "assistant",
    security(("session" = [])),
    request_body = crate::agent::ResumeRequest,
    responses((status = 200, description = "Not typed yet: see `docs/api/untyped.md`.", body = Object))
)]
async fn assistant_resume(
    State(app): State<App>,
    Authed(session): Authed,
    Body(body): Body<crate::agent::ResumeRequest>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(crate::agent::resume(&app, &actor(&session), &body.chat).await?))
}
