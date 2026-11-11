//! The one error shape the HTTP API returns.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;

use crate::http::openapi::{ErrorDetail, ErrorEnvelope};

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub extra: Option<Value>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self { status, code, message: message.into(), extra: None }
    }

    pub fn with_extra(mut self, extra: Value) -> Self {
        self.extra = Some(extra);
        self
    }

    /// Logs the cause on the server and returns a response that reveals nothing
    /// about it.
    pub fn internal(cause: impl std::fmt::Display) -> Self {
        tracing::error!(%cause, "internal error");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "Internal error. Nothing was sent to any device.")
    }
}

impl IntoResponse for ApiError {
    /// The envelope the OpenAPI document describes: `{"error": {"code", "message", "detail"?}}`.
    fn into_response(self) -> Response {
        let body = ErrorEnvelope {
            error: ErrorDetail { code: self.code.to_string(), message: self.message, detail: self.extra },
        };
        (self.status, Json(body)).into_response()
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(error: rusqlite::Error) -> Self {
        Self::internal(error)
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(error: serde_json::Error) -> Self {
        Self::internal(error)
    }
}
