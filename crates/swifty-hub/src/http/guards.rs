//! What stands in front of every handler: the origin check, the hardening
//! headers, the request body reader, and the extractors that say who is asking.
//!
//! The extractors here are the only code that reads the session cookie. A handler
//! gets a [`Principal`]: the person, the session they act through, and how the
//! request showed it. It never sees the cookie, so another way in, such as a bearer
//! token for a paired app, is added here and nowhere else.

use std::ops::Deref;

use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, FromRequestParts, MatchedPath, OriginalUri, Request, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::de::DeserializeOwned;

use super::{plain_event, RouterState, API_HEADER, API_MAJOR, API_PREFIX};
use crate::app::App;
use crate::error::ApiError;
use crate::policy::Role;
use crate::sessions::{self, Lookup, Person, Session};

/// Anything that is not a plain read must come from the configured origin.
pub(super) async fn origin_guard(State(app): State<App>, request: Request, next: Next) -> Response {
    let safe = matches!(*request.method(), Method::GET | Method::HEAD);
    if !safe {
        let origin = request.headers().get(header::ORIGIN).and_then(|v| v.to_str().ok());
        if origin != Some(app.config.webauthn.origin.as_str()) {
            tracing::warn!(path = %request.uri().path(), "refused a request with the wrong origin");
            return ApiError::new(StatusCode::FORBIDDEN, "bad_origin", "Wrong origin.").into_response();
        }
    }
    next.run(request).await
}

/// On every response, the page's files and every error included.
pub(super) async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    // Nothing is kept unless its handler says how: the page's own files are
    // `no-cache`, so the browser asks again each time with their `ETag`.
    if !headers.contains_key(header::CACHE_CONTROL) {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    headers.insert(header::STRICT_TRANSPORT_SECURITY, HeaderValue::from_static("max-age=31536000"));
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static(
            "microphone=(self), publickey-credentials-get=(self), publickey-credentials-create=(self), camera=(), geolocation=()",
        ),
    );
    headers.insert(API_HEADER, HeaderValue::from(API_MAJOR));
    response
}

/// JSON body extraction that answers in this API's error shape.
pub(crate) struct Body<T>(pub(crate) T);

impl<S, T> FromRequest<S> for Body<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
    Json<T>: FromRequest<S, Rejection = JsonRejection>,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, ApiError> {
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(Body(value)),
            Err(rejection) => {
                Err(ApiError::new(rejection.status(), "invalid_body", "The request body could not be read."))
            }
        }
    }
}

/// True for reads a page polls on a timer. They must not count as activity, or
/// an unattended open tab would never reach the idle limit.
fn is_passive(parts: &Parts) -> bool {
    // Inside the API's routes the URI has lost its prefix; the original still has it.
    let path = parts.extensions.get::<OriginalUri>().map_or(parts.uri.path(), |original| original.path());
    let Some(path) = path.strip_prefix(API_PREFIX) else {
        return false;
    };
    parts.method == Method::GET
        && (path == "/session"
            || path == "/health"
            || path.starts_with("/state")
            || path.starts_with("/actions/runs")
            || path.starts_with("/assistant/jobs/")
            || path.starts_with("/history/"))
}

/// How a request showed who it comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// The session cookie a passkey sign-in set.
    Cookie,
}

/// Who is asking: the person, the session they act through, and how the request
/// showed it. It reads as its session, so a handler writes `session.session_ref`.
#[derive(Debug, Clone)]
pub struct Principal {
    /// The same person as the session's.
    pub person: Person,
    pub session: Session,
    pub via: Via,
}

impl Principal {
    fn from_cookie(session: Session) -> Self {
        Self { person: session.person.clone(), session, via: Via::Cookie }
    }
}

impl Deref for Principal {
    type Target = Session;

    fn deref(&self) -> &Session {
        &self.session
    }
}

/// What the request's cookie names. `touch` says whether the request counts as activity.
fn lookup_cookie(app: &App, parts: &Parts, touch: bool) -> Result<Lookup, ApiError> {
    let Some(token) = sessions::token_from_headers(&parts.headers) else {
        return Ok(Lookup::Absent("not_signed_in"));
    };
    Ok(app.store.with(|conn| sessions::lookup(conn, app.now(), app.limits(), &token, touch))?)
}

fn unauthenticated(reason: &'static str) -> ApiError {
    ApiError::new(StatusCode::UNAUTHORIZED, reason, "Sign in with a passkey.")
}

/// An unlocked session, or the refusal that says why there is none.
fn unlocked(found: Lookup) -> Result<Principal, ApiError> {
    match found {
        Lookup::Active(session) => Ok(Principal::from_cookie(session)),
        Lookup::Locked(_) => Err(ApiError::new(
            StatusCode::LOCKED,
            "session_locked",
            "This session is locked. Sign in with a passkey to unlock it.",
        )),
        Lookup::Absent(reason) => Err(unauthenticated(reason)),
    }
}

/// A signed-in, unlocked session.
pub(crate) struct Authed(pub(crate) Principal);

impl FromRequestParts<RouterState> for Authed {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &RouterState) -> Result<Self, ApiError> {
        unlocked(lookup_cookie(&state.app, parts, !is_passive(parts))?).map(Authed)
    }
}

/// A signed-in, unlocked session, looked up without counting as activity: for a
/// stream a page holds open, which must not keep an unattended session alive.
pub(crate) struct Passive(pub(crate) Principal);

impl FromRequestParts<RouterState> for Passive {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &RouterState) -> Result<Self, ApiError> {
        unlocked(lookup_cookie(&state.app, parts, false)?).map(Passive)
    }
}

/// A signed-in, unlocked session belonging to the owner. A refusal is recorded,
/// and the audit row names the route from the router's own table, so nothing a
/// client typed reaches the log.
pub(crate) struct Owner(pub(crate) Principal);

impl FromRequestParts<RouterState> for Owner {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &RouterState) -> Result<Self, ApiError> {
        let Authed(principal) = Authed::from_request_parts(parts, state).await?;
        if principal.person.role == Role::Owner {
            return Ok(Owner(principal));
        }
        let route = parts.extensions.get::<MatchedPath>().map_or("-", MatchedPath::as_str);
        Err(refuse_owner_only(&state.app, route, &principal))
    }
}

/// Records that someone reached for what only the owner may have, and hands back
/// the refusal. `route` comes from the router's table.
pub(crate) fn refuse_owner_only(app: &App, route: &str, session: &Session) -> ApiError {
    match app.audit(plain_event(app, "access.owner_only", route, Some(session), "refused: owner_only")) {
        Ok(()) => ApiError::new(StatusCode::FORBIDDEN, "owner_only", "Only the owner can do this."),
        Err(error) => error,
    }
}

/// A signed-in session that may be locked.
pub(crate) struct AnySession(pub(crate) Principal);

impl FromRequestParts<RouterState> for AnySession {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &RouterState) -> Result<Self, ApiError> {
        match lookup_cookie(&state.app, parts, !is_passive(parts))? {
            Lookup::Active(session) | Lookup::Locked(session) => Ok(AnySession(Principal::from_cookie(session))),
            Lookup::Absent(reason) => Err(unauthenticated(reason)),
        }
    }
}

/// The session this browser already holds, active or locked, if it holds one.
/// Signing in ends it and starts another. Looking it up is not activity.
pub(crate) struct PriorSession(pub(crate) Option<Session>);

impl FromRequestParts<RouterState> for PriorSession {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &RouterState) -> Result<Self, ApiError> {
        match lookup_cookie(&state.app, parts, false)? {
            Lookup::Active(session) | Lookup::Locked(session) => Ok(PriorSession(Some(session))),
            Lookup::Absent(_) => Ok(PriorSession(None)),
        }
    }
}
