//! Enrollment, joining from an invitation, signing in and out, step-up, the
//! sessions, and the passkeys.

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use webauthn_rs::prelude::{
    CreationChallengeResponse, PublicKeyCredential, RegisterPublicKeyCredential, RequestChallengeResponse,
};

use super::guards::{AnySession, Authed, Body, Owner, PriorSession};
use super::{actor, plain_event, RouterState};
use crate::actions::{self, StepUpOptions, StepUpRequest};
use crate::app::App;
use crate::error::ApiError;
use crate::passkeys::{self, CredentialView};
use crate::policy::Role;
use crate::sessions::{self, Person, Session};

pub(crate) fn routes() -> OpenApiRouter<RouterState> {
    OpenApiRouter::new()
        .routes(routes!(enroll_start))
        .routes(routes!(enroll_finish))
        .routes(routes!(login_start))
        .routes(routes!(login_finish))
        .routes(routes!(logout))
        .routes(routes!(step_up_start))
        .routes(routes!(invite_start))
        .routes(routes!(invite_finish))
        .routes(routes!(session_info))
        .routes(routes!(session_lock))
        .routes(routes!(sessions_list))
        .routes(routes!(session_end))
        .routes(routes!(passkeys_list))
        .routes(routes!(passkey_register_finish))
        .routes(routes!(passkey_rename))
}

// ----- what these routes answer

/// A registration ceremony: hand `options` to `navigator.credentials.create`, then
/// send what it gives back with `ceremony_id`.
#[derive(Serialize, ToSchema)]
pub struct RegistrationStarted {
    pub ceremony_id: String,
    /// WebAuthn creation options.
    #[schema(value_type = Object)]
    pub options: CreationChallengeResponse,
}

/// A sign-in ceremony: hand `options` to `navigator.credentials.get`, then send the
/// assertion with `ceremony_id`.
#[derive(Serialize, ToSchema)]
pub struct SignInStarted {
    pub ceremony_id: String,
    /// WebAuthn request options.
    #[schema(value_type = Object)]
    pub options: RequestChallengeResponse,
}

/// A passkey that was just stored.
#[derive(Serialize, ToSchema)]
pub struct CredentialAdded {
    pub credential: CredentialView,
}

/// Someone who joined from an invitation, and their passkey.
#[derive(Serialize, ToSchema)]
pub struct Joined {
    pub credential: CredentialView,
    pub user: Person,
}

/// A session that was just started. Its cookie is in `Set-Cookie`.
#[derive(Serialize, ToSchema)]
pub struct SignedIn {
    pub session: Session,
}

/// This request's session, and whether the hub is live.
#[derive(Serialize, ToSchema)]
pub struct SessionInfo {
    pub session: Session,
    /// Whether commands reach real devices.
    pub live: bool,
}

/// An active session.
#[derive(Serialize, ToSchema)]
pub struct SessionRow {
    #[serde(flatten)]
    pub session: Session,
    /// Whether it is the session of this request.
    pub current: bool,
}

/// The active sessions.
#[derive(Serialize, ToSchema)]
pub struct SessionList {
    pub sessions: Vec<SessionRow>,
}

/// A passkey.
#[derive(Serialize, ToSchema)]
pub struct PasskeyRow {
    #[serde(flatten)]
    pub credential: CredentialView,
    /// Whether this request's session was signed in with it.
    pub in_use: bool,
}

/// Every passkey, revoked ones included.
#[derive(Serialize, ToSchema)]
pub struct PasskeyList {
    pub passkeys: Vec<PasskeyRow>,
}

/// `{"ok": true}`.
#[derive(Serialize, ToSchema)]
pub struct SignedOut {
    pub ok: bool,
}

/// `{"locked": true}`.
#[derive(Serialize, ToSchema)]
pub struct Locked {
    pub locked: bool,
}

/// `{"ended": true}`.
#[derive(Serialize, ToSchema)]
pub struct Ended {
    pub ended: bool,
}

/// `{"renamed": true}`.
#[derive(Serialize, ToSchema)]
pub struct Renamed {
    pub renamed: bool,
}

// ----- enrollment and sign-in

/// The enrollment token `swifty enroll-token` prints, and a name for the passkey.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct EnrollStart {
    token: String,
    name: String,
}

/// What `navigator.credentials.create` gave back for the ceremony.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct EnrollFinish {
    ceremony_id: String,
    #[schema(value_type = Object)]
    credential: RegisterPublicKeyCredential,
}

/// Starts enrolling the owner's first passkey, with the enrollment token.
///
/// Open to anyone who holds the token, which `swifty enroll-token` prints on the
/// hub. A refusal is audited.
#[utoipa::path(
    post,
    path = "/auth/enroll/start",
    tag = "auth",
    request_body = EnrollStart,
    responses((status = 200, body = RegistrationStarted))
)]
async fn enroll_start(
    State(app): State<App>,
    Body(body): Body<EnrollStart>,
) -> Result<Json<RegistrationStarted>, ApiError> {
    let result = app.passkeys.enroll_start(&app.store, app.now(), &body.token, &body.name);
    match result {
        Ok((ceremony_id, options)) => Ok(Json(RegistrationStarted { ceremony_id, options })),
        Err(error) => {
            app.audit_anonymous_refusal(plain_event(
                &app,
                "passkey.enroll",
                "new",
                None,
                &format!("refused: {}", error.code),
            ))?;
            Err(error)
        }
    }
}

/// Finishes enrolling: stores the passkey. Then sign in with it.
#[utoipa::path(
    post,
    path = "/auth/enroll/finish",
    tag = "auth",
    request_body = EnrollFinish,
    responses((status = 200, body = CredentialAdded))
)]
async fn enroll_finish(
    State(app): State<App>,
    Body(body): Body<EnrollFinish>,
) -> Result<Json<CredentialAdded>, ApiError> {
    let result = app.passkeys.enroll_finish(
        &app.store,
        app.now(),
        &body.ceremony_id,
        &body.credential,
        |tx, credential, owner| {
            let mut event = plain_event(&app, "passkey.enroll", &credential.id, None, "enrolled");
            event.credential_ref = credential.id.clone();
            event.user_ref = owner.user_id.clone();
            event.device = credential.name.clone();
            app.append_audit(tx, &event)?;
            Ok(())
        },
    );
    match result {
        Ok(credential) => Ok(Json(CredentialAdded { credential })),
        Err(error) => {
            app.audit_anonymous_refusal(plain_event(
                &app,
                "passkey.enroll",
                "new",
                None,
                &format!("refused: {}", error.code),
            ))?;
            Err(error)
        }
    }
}

/// Starts signing in with a passkey.
///
/// Open to anyone. `409` while no passkey is enrolled yet.
#[utoipa::path(post, path = "/auth/login/start", tag = "auth", responses((status = 200, body = SignInStarted)))]
async fn login_start(State(app): State<App>) -> Result<Json<SignInStarted>, ApiError> {
    let (ceremony_id, options) = app.passkeys.login_start(&app.store, app.now())?;
    Ok(Json(SignInStarted { ceremony_id, options }))
}

/// What `navigator.credentials.get` gave back for the ceremony.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct LoginFinish {
    ceremony_id: String,
    #[schema(value_type = Object)]
    assertion: PublicKeyCredential,
}

/// Finishes signing in, which sets the session cookie.
///
/// Signing in also unlocks: the session behind any cookie sent with the request is
/// ended and replaced. The cookie is `HttpOnly`, `Secure` and `SameSite=Strict`.
#[utoipa::path(
    post,
    path = "/auth/login/finish",
    tag = "auth",
    request_body = LoginFinish,
    responses((status = 200, description = "Signed in. The session cookie is in `Set-Cookie`.", body = SignedIn))
)]
async fn login_finish(
    State(app): State<App>,
    PriorSession(previous): PriorSession,
    Body(body): Body<LoginFinish>,
) -> Result<Response, ApiError> {
    let now = app.now();
    let limits = app.limits();
    let signed_in = app.passkeys.login_finish(&app.store, now, &body.ceremony_id, &body.assertion, |tx, verified| {
        if let Some(old) = &previous {
            sessions::end(tx, now, &old.session_ref, "replaced")?;
        }
        let (token, session) =
            sessions::create(tx, now, limits, &verified.credential_id, &verified.name, &verified.person)?;
        let mut event = plain_event(&app, "auth.login", "-", Some(&session), "signed_in");
        event.step_up = "passkey".to_string();
        app.append_audit(tx, &event)?;
        Ok((token, session))
    });
    let (token, session) = match signed_in {
        Ok(done) => done,
        Err(error) => {
            record_login_refusal(&app, &body.assertion, &error)?;
            return Err(error);
        }
    };
    if previous.is_some() {
        app.events.recheck_sessions();
    }

    let cookie = sessions::set_cookie_value(&token, app.config.session.absolute_secs as i64);
    let mut response = Json(SignedIn { session }).into_response();
    response.headers_mut().append(header::SET_COOKIE, cookie);
    Ok(response)
}

/// A refusal that followed a valid signature, such as a possible cloned key, is
/// always written: only someone holding the key can cause one, so a flood of
/// junk requests must not be able to keep it out of the log.
fn record_login_refusal(app: &App, assertion: &PublicKeyCredential, error: &ApiError) -> Result<(), ApiError> {
    let mut event = plain_event(app, "auth.login", "-", None, &format!("refused: {}", error.code));
    if !passkeys::follows_a_valid_signature(error.code) {
        return app.audit_anonymous_refusal(event);
    }
    let credential = app.store.with(|conn| passkeys::credential_by_raw_id(conn, assertion.get_credential_id()))?;
    if let Some((id, name, user)) = credential {
        event.credential_ref = id;
        event.device = name;
        event.user_ref = user;
    }
    app.audit(event)
}

/// Signs out: ends this session and clears its cookie. A locked session may sign out.
#[utoipa::path(
    post,
    path = "/auth/logout",
    tag = "auth",
    security(("session" = [])),
    responses((status = 200, body = SignedOut))
)]
async fn logout(State(app): State<App>, AnySession(session): AnySession) -> Result<Response, ApiError> {
    let now = app.now();
    app.store.tx(|tx| -> Result<(), ApiError> {
        sessions::end(tx, now, &session.session_ref, "signed_out")?;
        app.append_audit(tx, &plain_event(&app, "session.end", &session.session_ref, Some(&session), "signed_out"))?;
        Ok(())
    })?;
    app.events.recheck_sessions();
    let mut response = Json(SignedOut { ok: true }).into_response();
    response.headers_mut().append(header::SET_COOKIE, sessions::clear_cookie_value());
    Ok(response)
}

/// Starts the passkey step-up for a prepared critical action.
///
/// Send the action as it was prepared. The challenge in `options` is bound to that
/// action, so the assertion confirms nothing else. Then confirm with the assertion
/// (`POST /actions/confirm`).
#[utoipa::path(
    post,
    path = "/auth/step-up/start",
    tag = "auth",
    security(("session" = [])),
    request_body = StepUpRequest,
    responses((status = 200, body = StepUpOptions))
)]
async fn step_up_start(
    State(app): State<App>,
    Authed(session): Authed,
    Body(body): Body<StepUpRequest>,
) -> Result<Json<StepUpOptions>, ApiError> {
    Ok(Json(actions::begin_step_up(&app, &actor(&session), body)?))
}

// ----- joining from an invitation

/// The invitation's token, and a name for the passkey.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct InviteStart {
    token: String,
    name: String,
}

/// What `navigator.credentials.create` gave back for the ceremony.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct InviteFinish {
    ceremony_id: String,
    #[schema(value_type = Object)]
    credential: RegisterPublicKeyCredential,
}

/// Starts joining the house from an invitation.
///
/// Open to anyone who holds the invitation's token, which the owner made with
/// `user.invite`. A refusal is audited.
#[utoipa::path(
    post,
    path = "/auth/invite/start",
    tag = "auth",
    request_body = InviteStart,
    responses((status = 200, body = RegistrationStarted))
)]
async fn invite_start(
    State(app): State<App>,
    Body(body): Body<InviteStart>,
) -> Result<Json<RegistrationStarted>, ApiError> {
    match app.passkeys.invite_start(&app.store, app.now(), &body.token, &body.name) {
        Ok((ceremony_id, options)) => Ok(Json(RegistrationStarted { ceremony_id, options })),
        Err(error) => {
            app.audit_anonymous_refusal(plain_event(
                &app,
                "user.join",
                "new",
                None,
                &format!("refused: {}", error.code),
            ))?;
            Err(error)
        }
    }
}

/// Finishes joining: the person and their passkey are stored. Then they sign in.
#[utoipa::path(
    post,
    path = "/auth/invite/finish",
    tag = "auth",
    request_body = InviteFinish,
    responses((status = 200, body = Joined))
)]
async fn invite_finish(State(app): State<App>, Body(body): Body<InviteFinish>) -> Result<Json<Joined>, ApiError> {
    let result = app.passkeys.invite_finish(
        &app.store,
        app.now(),
        &body.ceremony_id,
        &body.credential,
        |tx, credential, person| {
            let mut event = plain_event(&app, "user.join", &person.user_id, None, "joined");
            event.credential_ref = credential.id.clone();
            event.user_ref = person.user_id.clone();
            event.device = credential.name.clone();
            app.append_audit(tx, &event)?;
            Ok(())
        },
    );
    match result {
        Ok((credential, user)) => Ok(Json(Joined { credential, user })),
        Err(error) => {
            app.audit_anonymous_refusal(plain_event(
                &app,
                "user.join",
                "new",
                None,
                &format!("refused: {}", error.code),
            ))?;
            Err(error)
        }
    }
}

// ----- sessions

/// This request's session, locked or not.
///
/// Polling it is not activity: an open page does not keep its session alive.
#[utoipa::path(
    get,
    path = "/session",
    tag = "sessions",
    security(("session" = [])),
    responses((status = 200, body = SessionInfo))
)]
async fn session_info(
    State(app): State<App>,
    AnySession(principal): AnySession,
) -> Result<Json<SessionInfo>, ApiError> {
    Ok(Json(SessionInfo { session: principal.session, live: app.registry.is_live() }))
}

/// Locks this session.
///
/// Until a passkey sign-in unlocks it, every route that needs a session answers
/// `423 session_locked`, and its event streams end.
#[utoipa::path(
    post,
    path = "/session/lock",
    tag = "sessions",
    security(("session" = [])),
    responses((status = 200, body = Locked))
)]
async fn session_lock(State(app): State<App>, AnySession(session): AnySession) -> Result<Json<Locked>, ApiError> {
    let now = app.now();
    app.store.tx(|tx| -> Result<(), ApiError> {
        sessions::lock(tx, now, &session.session_ref)?;
        app.append_audit(tx, &plain_event(&app, "session.lock", &session.session_ref, Some(&session), "locked"))?;
        Ok(())
    })?;
    app.events.recheck_sessions();
    Ok(Json(Locked { locked: true }))
}

/// The active sessions: everyone's to the owner, a person's own to anyone else.
#[utoipa::path(
    get,
    path = "/sessions",
    tag = "sessions",
    security(("session" = [])),
    responses((status = 200, body = SessionList))
)]
async fn sessions_list(State(app): State<App>, Authed(session): Authed) -> Result<Json<SessionList>, ApiError> {
    let now = app.now();
    let limits = app.limits();
    let list = app.store.with(|conn| sessions::list_active(conn, now, limits))?;
    let sessions = list
        .into_iter()
        .filter(|s| session.person.role == Role::Owner || s.person.user_id == session.person.user_id)
        .map(|s| SessionRow { current: s.session_ref == session.session_ref, session: s })
        .collect();
    Ok(Json(SessionList { sessions }))
}

/// Ends a session: the owner any, anyone else their own.
///
/// To a family member, someone else's session is `404 unknown_session`. Its open
/// event streams end at once.
#[utoipa::path(
    delete,
    path = "/sessions/{session_ref}",
    tag = "sessions",
    security(("session" = [])),
    params(("session_ref" = String, Path, description = "The session's `session_ref`.")),
    responses((status = 200, body = Ended))
)]
async fn session_end(
    State(app): State<App>,
    Authed(session): Authed,
    Path(session_ref): Path<String>,
) -> Result<Json<Ended>, ApiError> {
    let now = app.now();
    let ended = app.store.tx(|tx| -> Result<bool, ApiError> {
        let own = sessions::owner_of(tx, &session_ref)?.as_deref() == Some(session.person.user_id.as_str());
        // Family can only end their own. Someone else's looks like no session at all.
        if session.person.role != Role::Owner && !own {
            return Ok(false);
        }
        let reason = if own { "ended_by_user" } else { "ended_by_owner" };
        let ended = sessions::end(tx, now, &session_ref, reason)?;
        if ended {
            app.append_audit(tx, &plain_event(&app, "session.end", &session_ref, Some(&session), "ended"))?;
        }
        Ok(ended)
    })?;
    if !ended {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_session", "No such active session."));
    }
    app.events.recheck_sessions();
    Ok(Json(Ended { ended: true }))
}

// ----- passkeys

/// Every passkey, revoked ones included.
#[utoipa::path(
    get,
    path = "/passkeys",
    tag = "passkeys",
    security(("session" = ["owner"])),
    responses((status = 200, body = PasskeyList))
)]
async fn passkeys_list(State(app): State<App>, Owner(session): Owner) -> Result<Json<PasskeyList>, ApiError> {
    let list = app.store.with(passkeys::list_credentials)?;
    let passkeys = list
        .into_iter()
        .map(|credential| PasskeyRow { in_use: credential.id == session.credential_id, credential })
        .collect();
    Ok(Json(PasskeyList { passkeys }))
}

/// A new name for a passkey.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct RenameBody {
    name: String,
}

/// Renames a passkey.
#[utoipa::path(
    patch,
    path = "/passkeys/{id}",
    tag = "passkeys",
    security(("session" = ["owner"])),
    params(("id" = String, Path, description = "The passkey's id.")),
    request_body = RenameBody,
    responses((status = 200, body = Renamed))
)]
async fn passkey_rename(
    State(app): State<App>,
    Owner(session): Owner,
    Path(id): Path<String>,
    Body(body): Body<RenameBody>,
) -> Result<Json<Renamed>, ApiError> {
    app.store.tx(|tx| -> Result<(), ApiError> {
        passkeys::rename_credential(tx, &id, &body.name)?;
        let mut event = plain_event(&app, "passkey.rename", &id, Some(&session), "renamed");
        event.params = json!({ "name": body.name });
        app.append_audit(tx, &event)?;
        Ok(())
    })?;
    Ok(Json(Renamed { renamed: true }))
}

/// What `navigator.credentials.create` gave back for the ceremony.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct RegisterFinish {
    ceremony_id: String,
    #[schema(value_type = Object)]
    credential: RegisterPublicKeyCredential,
}

/// Finishes registering another passkey.
///
/// The ceremony is started by the critical action `passkey.register`, whose run
/// carries the creation options in `registration`.
#[utoipa::path(
    post,
    path = "/passkeys/register/finish",
    tag = "passkeys",
    security(("session" = ["owner"])),
    request_body = RegisterFinish,
    responses((status = 200, body = CredentialAdded))
)]
async fn passkey_register_finish(
    State(app): State<App>,
    Owner(session): Owner,
    Body(body): Body<RegisterFinish>,
) -> Result<Json<CredentialAdded>, ApiError> {
    let result = app.passkeys.register_finish(
        &app.store,
        app.now(),
        app.limits(),
        &session.session_ref,
        &body.ceremony_id,
        &body.credential,
        |tx, credential| {
            app.append_audit(tx, &plain_event(&app, "passkey.register", &credential.id, Some(&session), "registered"))?;
            Ok(())
        },
    );
    match result {
        Ok(credential) => Ok(Json(CredentialAdded { credential })),
        Err(error) => {
            app.audit(plain_event(
                &app,
                "passkey.register",
                "new",
                Some(&session),
                &format!("refused: {}", error.code),
            ))?;
            Err(error)
        }
    }
}
