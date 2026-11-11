//! The voice route: a WAV in, the words or the result of acting on them out. It
//! takes a larger body than every other route.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::Json;
use serde::Deserialize;
use serde_json::Value;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::guards::Authed;
use super::{actor, RouterState};
use crate::app::App;
use crate::error::ApiError;
use crate::voice;

const VOICE_LIMIT: usize = 8 * 1024 * 1024;

pub(crate) fn routes() -> OpenApiRouter<RouterState> {
    OpenApiRouter::new().routes(routes!(voice_route)).layer(DefaultBodyLimit::max(VOICE_LIMIT))
}

/// The RIFF container header of a WAV file: `RIFF`, a length, then `WAVE`.
fn is_wav(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE"
}

fn check_wav(headers: &HeaderMap, wav: &Bytes) -> Result<(), ApiError> {
    let content_type =
        headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or_default().to_ascii_lowercase();
    let essence = content_type.split(';').next().unwrap_or_default().trim().to_string();
    if !matches!(essence.as_str(), "audio/wav" | "audio/x-wav" | "audio/wave") {
        return Err(ApiError::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type", "Send a WAV file."));
    }
    if wav.is_empty() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "empty_audio", "No audio was sent."));
    }
    if !is_wav(wav) {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "That is not a WAV file.",
        ));
    }
    Ok(())
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
struct VoiceQuery {
    /// Only the words, for showing them while the person is still talking.
    /// A query rather than a path, because the proxy allows long audio on this exact path.
    #[serde(default)]
    words: bool,
}

/// Speech in: the words, or the result of acting on them.
///
/// The body is a WAV file of at most 8 MB (`415 unsupported_media_type` otherwise,
/// `400 empty_audio` when empty). With `words=true` the answer is only the words, for
/// showing them while the person is still talking; without, the hub acts on them as
/// a spoken command; voice never runs a critical action.
#[utoipa::path(
    post,
    path = "/voice",
    tag = "assistant",
    security(("session" = [])),
    params(VoiceQuery),
    request_body(content = Vec<u8>, content_type = "audio/wav", description = "A WAV file."),
    responses((status = 200, description = "Not typed yet: see `docs/api/untyped.md`.", body = Object))
)]
async fn voice_route(
    State(app): State<App>,
    Authed(session): Authed,
    Query(query): Query<VoiceQuery>,
    headers: HeaderMap,
    wav: Bytes,
) -> Result<Json<Value>, ApiError> {
    check_wav(&headers, &wav)?;
    let reply = if query.words {
        voice::words(&app, wav.to_vec()).await?
    } else {
        voice::handle(&app, &actor(&session), wav.to_vec()).await?
    };
    Ok(Json(reply))
}
