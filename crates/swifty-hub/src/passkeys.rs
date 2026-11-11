//! Passkey ceremonies and credential management. There are no passwords
//! anywhere in this service.

use std::time::Duration;

use axum::http::StatusCode;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Serialize;
use serde_json::{json, Value};
use webauthn_rs::prelude::*;

use crate::catalog::validate_label;
use crate::config::WebauthnConfig;
use crate::crypto::{random_id, random_token, sha256, sha256_hex};
use crate::error::ApiError;
use crate::policy::{Role, STEP_UP_TTL_MS};
use crate::sessions::{self, Limits, Person};
use crate::store::Store;
use crate::users;

pub const ENROLL_TOKEN_TTL_MS: i64 = 15 * 60 * 1000;
pub const LOGIN_TTL_MS: i64 = 120_000;
pub const REGISTER_TTL_MS: i64 = 120_000;
pub const ENROLL_CEREMONY_TTL_MS: i64 = 300_000;
/// How many sign-ins can be open at once. Past this the oldest is dropped rather
/// than the new one refused, because sign-in is the only way in: to get in the
/// owner's way a flood has to outrun the time it takes to touch a passkey, and a
/// larger table means it has to be that much faster. At 256, an owner taking ten
/// seconds is only displaced by more than 25 new sign-ins every second.
pub const MAX_OPEN_LOGINS: i64 = 256;
const USER_NAME: &str = "owner";
const USER_DISPLAY: &str = "Swifty Home owner";
/// How long someone has to finish adding the passkey for an invitation.
pub const INVITE_CEREMONY_TTL_MS: i64 = 300_000;

pub struct Passkeys {
    webauthn: Webauthn,
    owner: Uuid,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct CredentialView {
    pub id: String,
    pub name: String,
    pub recovery: bool,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    pub revoked_at: Option<i64>,
    /// Whose passkey this is.
    pub user_id: Option<String>,
}

/// A credential that has just passed an assertion, and whose it is.
#[derive(Debug, Clone)]
pub struct Verified {
    pub credential_id: String,
    pub name: String,
    pub person: Person,
}

struct Ceremony {
    id: String,
    state: String,
    session_ref: Option<String>,
    nonce: Option<String>,
    meta: Option<String>,
}

fn rejected(code: &'static str, message: &str) -> ApiError {
    ApiError::new(StatusCode::UNAUTHORIZED, code, message)
}

/// Enumerated reasons from the library stay in the server log. The client only
/// learns that the assertion was refused.
fn webauthn_failure(error: WebauthnError) -> ApiError {
    tracing::warn!(?error, "passkey ceremony rejected");
    rejected("assertion_rejected", "The passkey response was not accepted.")
}

fn internal_webauthn(error: WebauthnError) -> ApiError {
    ApiError::internal(format!("webauthn: {error:?}"))
}

/// Takes a ceremony exactly once. Run it outside a transaction so that its use
/// is committed even when the rest of the request fails: a challenge that was
/// tried once can never be tried again.
fn take_ceremony(conn: &Connection, now: i64, column: &str, key: &str, kind: &str) -> Result<Ceremony, ApiError> {
    let sql = format!(
        "SELECT id, state, session_ref, nonce, meta, expires_at, used_at
         FROM ceremonies WHERE {column} = ?1 AND kind = ?2"
    );
    let row = conn
        .query_row(&sql, params![key, kind], |row| {
            Ok((
                Ceremony {
                    id: row.get(0)?,
                    state: row.get(1)?,
                    session_ref: row.get(2)?,
                    nonce: row.get(3)?,
                    meta: row.get(4)?,
                },
                row.get::<_, i64>(5)?,
                row.get::<_, Option<i64>>(6)?,
            ))
        })
        .optional()?;
    let Some((ceremony, expires_at, used_at)) = row else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_challenge", "No such challenge."));
    };
    if used_at.is_some() {
        return Err(ApiError::new(StatusCode::CONFLICT, "challenge_used", "This challenge was already used."));
    }
    conn.execute("UPDATE ceremonies SET used_at = ?1 WHERE id = ?2", params![now, ceremony.id])?;
    if now >= expires_at {
        return Err(ApiError::new(StatusCode::GONE, "challenge_expired", "This challenge has expired."));
    }
    Ok(ceremony)
}

#[allow(clippy::too_many_arguments)]
fn insert_ceremony(
    conn: &Connection,
    now: i64,
    ttl_ms: i64,
    kind: &str,
    state: &str,
    session_ref: Option<&str>,
    prepared_id: Option<&str>,
    nonce: Option<&str>,
    meta: Option<&str>,
) -> rusqlite::Result<String> {
    let id = random_id();
    conn.execute(
        "INSERT INTO ceremonies (id, kind, state, session_ref, prepared_id, nonce, meta, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![id, kind, state, session_ref, prepared_id, nonce, meta, now, now + ttl_ms],
    )?;
    Ok(id)
}

fn prune(conn: &Connection, now: i64) -> rusqlite::Result<()> {
    let cutoff = now - 3_600_000;
    conn.execute("DELETE FROM ceremonies WHERE expires_at < ?1", params![cutoff])?;
    conn.execute("DELETE FROM prepared_actions WHERE expires_at < ?1", params![cutoff])?;
    conn.execute("DELETE FROM enroll_tokens WHERE expires_at < ?1", params![cutoff])?;
    Ok(())
}

fn active_passkeys(conn: &Connection) -> Result<Vec<Passkey>, ApiError> {
    passkeys_where(conn, "", "")
}

/// One person's passkeys. A step-up only ever accepts these, so nobody else's key
/// can approve what they do.
fn active_passkeys_of(conn: &Connection, user_id: &str) -> Result<Vec<Passkey>, ApiError> {
    passkeys_where(conn, "AND c.user_id = ?1", user_id)
}

fn passkeys_where(conn: &Connection, filter: &str, value: &str) -> Result<Vec<Passkey>, ApiError> {
    let mut statement = conn.prepare(&format!(
        "SELECT c.passkey FROM credentials c JOIN users u ON u.id = c.user_id
         WHERE c.revoked_at IS NULL AND u.removed_at IS NULL {filter}"
    ))?;
    let read = |row: &rusqlite::Row<'_>| row.get::<_, String>(0);
    let texts: Vec<rusqlite::Result<String>> = if filter.is_empty() {
        statement.query_map([], read)?.collect()
    } else {
        statement.query_map([value], read)?.collect()
    };
    let mut out = Vec::new();
    for text in texts {
        out.push(serde_json::from_str::<Passkey>(&text?)?);
    }
    Ok(out)
}

pub fn active_count(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM credentials WHERE revoked_at IS NULL", [], |row| row.get(0))
}

/// What a new passkey must not duplicate. A removed passkey stays in its device's keychain, so
/// listing it here would stop that device from ever adding a new one. cred_id is unique, so a
/// removed credential still cannot be registered again.
fn active_credential_ids(conn: &Connection) -> rusqlite::Result<Vec<CredentialID>> {
    let mut statement = conn.prepare("SELECT cred_id FROM credentials WHERE revoked_at IS NULL")?;
    let rows = statement.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
    rows.collect()
}

fn insert_credential(
    conn: &Connection,
    now: i64,
    passkey: &Passkey,
    name: &str,
    recovery: bool,
    user_id: &str,
) -> Result<CredentialView, ApiError> {
    let id = random_id();
    let raw_id: &[u8] = passkey.cred_id().as_slice();
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO credentials (id, cred_id, name, recovery, passkey, created_at, user_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, raw_id, name, recovery as i64, serde_json::to_string(passkey)?, now, user_id],
    )?;
    if inserted != 1 {
        return Err(ApiError::new(StatusCode::CONFLICT, "credential_exists", "This passkey is already registered."));
    }
    Ok(CredentialView {
        id,
        name: name.to_string(),
        recovery,
        created_at: now,
        last_used_at: None,
        revoked_at: None,
        user_id: Some(user_id.to_string()),
    })
}

/// Applies a successful assertion: the credential must still be active and its
/// signature counter must have moved forward if it keeps one.
fn record_assertion(conn: &Connection, now: i64, result: &AuthenticationResult) -> Result<Verified, ApiError> {
    if !result.user_verified() {
        return Err(rejected("user_verification_required", "User verification is required."));
    }
    let raw_id: &[u8] = result.cred_id().as_slice();
    let row = conn
        .query_row(
            "SELECT c.id, c.name, c.passkey, c.sign_count, c.revoked_at, u.id, u.name, u.role, u.removed_at
             FROM credentials c JOIN users u ON u.id = c.user_id WHERE c.cred_id = ?1",
            params![raw_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    Person {
                        user_id: row.get(5)?,
                        user_name: row.get(6)?,
                        role: Role::from_stored(&row.get::<_, String>(7)?),
                    },
                    row.get::<_, Option<i64>>(8)?,
                ))
            },
        )
        .optional()?;
    let Some((id, name, passkey_text, stored_count, revoked_at, person, removed_at)) = row else {
        return Err(rejected("credential_not_active", "This passkey is not registered."));
    };
    if revoked_at.is_some() || removed_at.is_some() {
        return Err(rejected("credential_not_active", "This passkey has been revoked."));
    }
    // Synced passkeys report zero forever and are not checked. A credential
    // with a real counter must always move forward. The library compares
    // against the counter copied when the challenge was issued, which can be
    // stale when two ceremonies overlap, so the stored value is checked here.
    let count = result.counter() as i64;
    if (count > 0 || stored_count > 0) && count <= stored_count {
        tracing::warn!(credential = %id, "signature counter did not advance");
        return Err(rejected(
            "credential_counter_regressed",
            "This passkey's signature counter went backwards, which can mean it was cloned.",
        ));
    }
    let mut passkey: Passkey = serde_json::from_str(&passkey_text)?;
    passkey.update_credential(result);
    conn.execute(
        "UPDATE credentials SET passkey = ?1, sign_count = ?2, last_used_at = ?3 WHERE id = ?4",
        params![serde_json::to_string(&passkey)?, count.max(stored_count), now, id],
    )?;
    Ok(Verified { credential_id: id, name, person })
}

fn validate_name(name: &str) -> Result<(), ApiError> {
    validate_label("name", name, 64).map_err(|e| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_name", e.0))
}

/// The challenge for a step-up: it commits to the action, target, parameters,
/// session and prepared id through `binding`, so an assertion over it means
/// that exact action and nothing else.
fn derive_step_up_challenge(nonce: &str, binding: &str) -> [u8; 32] {
    sha256(format!("{}\0{nonce}\0{binding}", crate::domains::STEP_UP).as_bytes())
}

/// Replaces the library's random challenge with the derived one, in the options
/// sent to the browser and in the state kept here. webauthn-rs has no API for a
/// caller-chosen challenge. Failure refuses the step-up rather than falling back
/// to an unbound challenge.
fn bind_challenge(
    options: &mut RequestChallengeResponse,
    state: &PasskeyAuthentication,
    challenge: &[u8; 32],
) -> Result<String, ApiError> {
    options.public_key.challenge = challenge.to_vec();
    let mut value = serde_json::to_value(state)?;
    let slot = value
        .get_mut("ast")
        .and_then(|ast| ast.get_mut("challenge"))
        .filter(|slot| slot.is_string())
        .ok_or_else(|| ApiError::internal("step-up state has an unexpected layout"))?;
    *slot = Value::String(URL_SAFE_NO_PAD.encode(challenge));
    Ok(value.to_string())
}

fn state_challenge(state: &str) -> Option<String> {
    let value: Value = serde_json::from_str(state).ok()?;
    value.get("ast")?.get("challenge")?.as_str().map(str::to_string)
}

impl Passkeys {
    pub fn new(config: &WebauthnConfig, owner: Uuid) -> Result<Self, WebauthnError> {
        let origin = Url::parse(&config.origin).map_err(|_| WebauthnError::Configuration)?;
        let webauthn = WebauthnBuilder::new(&config.rp_id, &origin)?
            .rp_name(&config.rp_name)
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self { webauthn, owner })
    }

    // ----- first enrollment

    /// Run by the local CLI. Returns the token once. Only its hash is stored.
    /// Replaces any earlier token and cancels any enrollment opened with one, so
    /// issuing a new token after a leak really does shut the old one out.
    pub fn issue_enroll_token(store: &Store, now: i64) -> Result<(String, i64), ApiError> {
        store.tx(|tx| {
            if active_count(tx)? > 0 {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "already_enrolled",
                    "A passkey is already registered. Further passkeys are added from a signed-in session with a fresh step-up.",
                ));
            }
            tx.execute("DELETE FROM enroll_tokens", [])?;
            tx.execute(
                "UPDATE ceremonies SET used_at = ?1 WHERE kind = 'enroll' AND used_at IS NULL",
                params![now],
            )?;
            let token = random_token();
            let expires_at = now + ENROLL_TOKEN_TTL_MS;
            tx.execute(
                "INSERT INTO enroll_tokens (token_hash, created_at, expires_at) VALUES (?1, ?2, ?3)",
                params![sha256_hex(token.as_bytes()), now, expires_at],
            )?;
            Ok((token, expires_at))
        })
    }

    pub fn enroll_start(
        &self,
        store: &Store,
        now: i64,
        token: &str,
        name: &str,
    ) -> Result<(String, CreationChallengeResponse), ApiError> {
        validate_name(name)?;
        store.tx(|tx| {
            if active_count(tx)? > 0 {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "already_enrolled",
                    "First enrollment is already complete.",
                ));
            }
            let token_expires_at: Option<i64> = tx
                .query_row(
                    "UPDATE enroll_tokens SET used_at = ?1
                     WHERE token_hash = ?2 AND used_at IS NULL AND expires_at > ?1
                     RETURNING expires_at",
                    params![now, sha256_hex(token.as_bytes())],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(token_expires_at) = token_expires_at else {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "invalid_enrollment_token",
                    "That enrollment token is not valid.",
                ));
            };
            let (options, state) = self
                .webauthn
                .start_passkey_registration(self.owner, USER_NAME, USER_DISPLAY, None)
                .map_err(internal_webauthn)?;
            let meta = json!({ "name": name, "recovery": false }).to_string();
            // The ceremony cannot outlive the token that opened it.
            let id = insert_ceremony(
                tx,
                now,
                ENROLL_CEREMONY_TTL_MS.min(token_expires_at - now),
                "enroll",
                &serde_json::to_string(&state)?,
                None,
                None,
                None,
                Some(&meta),
            )?;
            Ok((id, options))
        })
    }

    /// Adds the first passkey. `record` runs in the same transaction, so the
    /// credential and its audit row commit or roll back together.
    pub fn enroll_finish(
        &self,
        store: &Store,
        now: i64,
        ceremony_id: &str,
        credential: &RegisterPublicKeyCredential,
        record: impl FnOnce(&Transaction<'_>, &CredentialView, &Person) -> Result<(), ApiError>,
    ) -> Result<CredentialView, ApiError> {
        let ceremony = store.with(|conn| take_ceremony(conn, now, "id", ceremony_id, "enroll"))?;
        store.tx(|tx| {
            if active_count(tx)? > 0 {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "already_enrolled",
                    "First enrollment is already complete.",
                ));
            }
            let owner = users::ensure_owner(tx, now, &self.owner.to_string())?;
            let added = self.complete_registration(tx, now, &ceremony, credential, &owner.user_id)?;
            record(tx, &added, &owner)?;
            Ok(added)
        })
    }

    // ----- joining from an invitation

    /// Opens the ceremony for an invitation. The invitation is only spent when it
    /// finishes, so a cancelled prompt can be tried again, and it can only ever
    /// finish once.
    pub fn invite_start(
        &self,
        store: &Store,
        now: i64,
        token: &str,
        passkey_name: &str,
    ) -> Result<(String, CreationChallengeResponse), ApiError> {
        validate_name(passkey_name)?;
        store.tx(|tx| {
            let invitation = users::open_invitation(tx, now, token)?;
            let handle = Uuid::from_bytes(crate::crypto::random_bytes::<16>());
            let (options, state) = self
                .webauthn
                .start_passkey_registration(handle, &invitation.name, &invitation.name, None)
                .map_err(internal_webauthn)?;
            let meta = json!({
                "name": passkey_name,
                "recovery": false,
                "invitation": invitation.id,
                "handle": handle.to_string(),
            })
            .to_string();
            let id = insert_ceremony(
                tx,
                now,
                INVITE_CEREMONY_TTL_MS.min(invitation.expires_at - now),
                "invite",
                &serde_json::to_string(&state)?,
                None,
                None,
                None,
                Some(&meta),
            )?;
            Ok((id, options))
        })
    }

    /// Spends the invitation, and creates the person and their passkey, in one
    /// transaction. `record` runs inside it, so all of it commits or none does.
    pub fn invite_finish(
        &self,
        store: &Store,
        now: i64,
        ceremony_id: &str,
        credential: &RegisterPublicKeyCredential,
        record: impl FnOnce(&Transaction<'_>, &CredentialView, &Person) -> Result<(), ApiError>,
    ) -> Result<(CredentialView, Person), ApiError> {
        let ceremony = store.with(|conn| take_ceremony(conn, now, "id", ceremony_id, "invite"))?;
        let meta: Value = serde_json::from_str(ceremony.meta.as_deref().unwrap_or("{}"))?;
        let invitation = meta.get("invitation").and_then(Value::as_str).unwrap_or_default();
        let handle = meta.get("handle").and_then(Value::as_str).unwrap_or_default();
        store.tx(|tx| {
            let person = users::redeem_invitation(tx, now, invitation, handle)?;
            let added = self.complete_registration(tx, now, &ceremony, credential, &person.user_id)?;
            record(tx, &added, &person)?;
            Ok((added, person))
        })
    }

    fn complete_registration(
        &self,
        conn: &Connection,
        now: i64,
        ceremony: &Ceremony,
        credential: &RegisterPublicKeyCredential,
        user_id: &str,
    ) -> Result<CredentialView, ApiError> {
        let state: PasskeyRegistration = serde_json::from_str(&ceremony.state)?;
        let passkey = self.webauthn.finish_passkey_registration(credential, &state).map_err(webauthn_failure)?;
        let meta: Value = serde_json::from_str(ceremony.meta.as_deref().unwrap_or("{}"))?;
        let name = meta.get("name").and_then(Value::as_str).unwrap_or("Passkey");
        let recovery = meta.get("recovery").and_then(Value::as_bool).unwrap_or(false);
        insert_credential(conn, now, &passkey, name, recovery, user_id)
    }

    // ----- login

    pub fn login_start(&self, store: &Store, now: i64) -> Result<(String, RequestChallengeResponse), ApiError> {
        store.tx(|tx| {
            prune(tx, now)?;
            let open: i64 = tx.query_row(
                "SELECT COUNT(*) FROM ceremonies WHERE kind = 'login' AND used_at IS NULL AND expires_at > ?1",
                params![now],
                |row| row.get(0),
            )?;
            if open >= MAX_OPEN_LOGINS {
                tx.execute(
                    "DELETE FROM ceremonies WHERE id IN (
                         SELECT id FROM ceremonies
                         WHERE kind = 'login' AND used_at IS NULL AND expires_at > ?1
                         ORDER BY created_at ASC, rowid ASC LIMIT ?2)",
                    params![now, open - MAX_OPEN_LOGINS + 1],
                )?;
            }
            let passkeys = active_passkeys(tx)?;
            if passkeys.is_empty() {
                return Err(ApiError::new(StatusCode::CONFLICT, "no_credentials", "No passkey is enrolled yet."));
            }
            let (options, state) = self.webauthn.start_passkey_authentication(&passkeys).map_err(internal_webauthn)?;
            let id = insert_ceremony(
                tx,
                now,
                LOGIN_TTL_MS,
                "login",
                &serde_json::to_string(&state)?,
                None,
                None,
                None,
                None,
            )?;
            Ok((id, options))
        })
    }

    /// Checks a sign-in and runs `complete` for it, in the transaction that checks
    /// the credential is still active. A revocation is a transaction too, so it
    /// either comes first and this is refused, or comes after and ends whatever
    /// `complete` created. No session can slip between the two.
    pub fn login_finish<R>(
        &self,
        store: &Store,
        now: i64,
        ceremony_id: &str,
        assertion: &PublicKeyCredential,
        complete: impl FnOnce(&Transaction<'_>, &Verified) -> Result<R, ApiError>,
    ) -> Result<R, ApiError> {
        let ceremony = store.with(|conn| take_ceremony(conn, now, "id", ceremony_id, "login"))?;
        let state: PasskeyAuthentication = serde_json::from_str(&ceremony.state)?;
        let result = self.webauthn.finish_passkey_authentication(assertion, &state).map_err(webauthn_failure)?;
        store.tx(|tx| {
            let verified = record_assertion(tx, now, &result)?;
            complete(tx, &verified)
        })
    }

    // ----- step-up

    /// Issues the challenge for one prepared critical action. Only one is ever
    /// issued for a prepared action, and only this person's passkeys can answer.
    pub fn step_up_start(
        &self,
        conn: &Connection,
        now: i64,
        session_ref: &str,
        prepared_id: &str,
        binding: &str,
        user_id: &str,
    ) -> Result<(RequestChallengeResponse, i64), ApiError> {
        let passkeys = active_passkeys_of(conn, user_id)?;
        let (mut options, state) = self.webauthn.start_passkey_authentication(&passkeys).map_err(internal_webauthn)?;
        let nonce = random_token();
        let challenge = derive_step_up_challenge(&nonce, binding);
        let state = bind_challenge(&mut options, &state, &challenge)?;
        let inserted = insert_ceremony(
            conn,
            now,
            STEP_UP_TTL_MS,
            "stepup",
            &state,
            Some(session_ref),
            Some(prepared_id),
            Some(&nonce),
            None,
        );
        match inserted {
            Ok(_) => Ok((options, now + STEP_UP_TTL_MS)),
            Err(rusqlite::Error::SqliteFailure(error, _)) if error.code == rusqlite::ErrorCode::ConstraintViolation => {
                Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "challenge_used",
                    "A step-up was already issued for this action. Prepare it again.",
                ))
            }
            Err(other) => Err(other.into()),
        }
    }

    /// Spends the step-up, then verifies the assertion against it. The spend
    /// commits first, so a failed attempt cannot be tried again.
    #[allow(clippy::too_many_arguments)]
    pub fn step_up_finish(
        &self,
        store: &Store,
        now: i64,
        session_ref: &str,
        prepared_id: &str,
        binding: &str,
        assertion: &PublicKeyCredential,
        user_id: &str,
    ) -> Result<Verified, ApiError> {
        let ceremony =
            store.with(|conn| take_ceremony(conn, now, "prepared_id", prepared_id, "stepup")).map_err(|error| {
                if error.code == "unknown_challenge" {
                    ApiError::new(
                        StatusCode::PRECONDITION_REQUIRED,
                        "step_up_required",
                        "This action needs a passkey step-up first.",
                    )
                } else {
                    error
                }
            })?;
        if ceremony.session_ref.as_deref() != Some(session_ref) {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "wrong_session",
                "This challenge belongs to another session.",
            ));
        }
        let nonce = ceremony.nonce.as_deref().unwrap_or_default();
        let expected = URL_SAFE_NO_PAD.encode(derive_step_up_challenge(nonce, binding));
        if state_challenge(&ceremony.state).as_deref() != Some(expected.as_str()) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "binding_mismatch",
                "This challenge was issued for a different action.",
            ));
        }
        let state: PasskeyAuthentication = serde_json::from_str(&ceremony.state)?;
        let result = self.webauthn.finish_passkey_authentication(assertion, &state).map_err(webauthn_failure)?;
        store.tx(|tx| {
            let verified = record_assertion(tx, now, &result)?;
            if verified.person.user_id != user_id {
                return Err(rejected("credential_not_active", "This passkey belongs to someone else."));
            }
            Ok(verified)
        })
    }

    // ----- further passkeys, after a step-up

    pub fn register_start(
        &self,
        conn: &Connection,
        now: i64,
        session_ref: &str,
        name: &str,
        recovery: bool,
    ) -> Result<(String, CreationChallengeResponse, i64), ApiError> {
        validate_name(name)?;
        // Only the owner reaches this, and the owner's handle is this one.
        let exclude = active_credential_ids(conn)?;
        let (options, state) = self
            .webauthn
            .start_passkey_registration(self.owner, USER_NAME, USER_DISPLAY, Some(exclude))
            .map_err(internal_webauthn)?;
        let meta = json!({ "name": name, "recovery": recovery }).to_string();
        let id = insert_ceremony(
            conn,
            now,
            REGISTER_TTL_MS,
            "register",
            &serde_json::to_string(&state)?,
            Some(session_ref),
            None,
            None,
            Some(&meta),
        )?;
        Ok((id, options, now + REGISTER_TTL_MS))
    }

    /// Adds a passkey to a signed-in account. The session is checked again in the
    /// transaction that adds it, because a revocation can end the session after
    /// this request was authenticated. `record` runs in the same transaction.
    #[allow(clippy::too_many_arguments)]
    pub fn register_finish(
        &self,
        store: &Store,
        now: i64,
        limits: Limits,
        session_ref: &str,
        ceremony_id: &str,
        credential: &RegisterPublicKeyCredential,
        record: impl FnOnce(&Transaction<'_>, &CredentialView) -> Result<(), ApiError>,
    ) -> Result<CredentialView, ApiError> {
        let ceremony = store.with(|conn| take_ceremony(conn, now, "id", ceremony_id, "register"))?;
        if ceremony.session_ref.as_deref() != Some(session_ref) {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "wrong_session",
                "This registration belongs to another session.",
            ));
        }
        store.tx(|tx| {
            if !sessions::is_live(tx, now, limits, session_ref)? {
                return Err(ApiError::new(
                    StatusCode::UNAUTHORIZED,
                    "session_ended",
                    "This session has ended. Sign in with a passkey.",
                ));
            }
            // The new passkey goes to whoever this session belongs to.
            let user_id = sessions::owner_of(tx, session_ref)?
                .ok_or_else(|| ApiError::internal("a live session has no person"))?;
            let added = self.complete_registration(tx, now, &ceremony, credential, &user_id)?;
            record(tx, &added)?;
            Ok(added)
        })
    }
}

// ----- credential management

fn credential_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CredentialView> {
    Ok(CredentialView {
        id: row.get(0)?,
        name: row.get(1)?,
        recovery: row.get::<_, i64>(2)? != 0,
        created_at: row.get(3)?,
        last_used_at: row.get(4)?,
        revoked_at: row.get(5)?,
        user_id: row.get(6)?,
    })
}

const CREDENTIAL_COLUMNS: &str = "id, name, recovery, created_at, last_used_at, revoked_at, user_id";

pub fn list_credentials(conn: &Connection) -> rusqlite::Result<Vec<CredentialView>> {
    let mut statement =
        conn.prepare(&format!("SELECT {CREDENTIAL_COLUMNS} FROM credentials ORDER BY created_at ASC"))?;
    let rows = statement.query_map([], credential_from_row)?;
    rows.collect()
}

pub fn get_credential(conn: &Connection, id: &str) -> rusqlite::Result<Option<CredentialView>> {
    conn.query_row(
        &format!("SELECT {CREDENTIAL_COLUMNS} FROM credentials WHERE id = ?1"),
        params![id],
        credential_from_row,
    )
    .optional()
}

pub fn credential_name(conn: &Connection, id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT name FROM credentials WHERE id = ?1", params![id], |row| row.get(0)).optional()
}

/// True for the refusals that only someone holding the key can cause, because
/// they come after the signature has been checked.
pub fn follows_a_valid_signature(code: &str) -> bool {
    matches!(code, "credential_counter_regressed" | "credential_not_active" | "user_verification_required")
}

/// The local id and name of the credential with this WebAuthn credential id.
pub fn credential_by_raw_id(conn: &Connection, raw_id: &[u8]) -> rusqlite::Result<Option<(String, String, String)>> {
    conn.query_row(
        "SELECT id, name, COALESCE(user_id, '-') FROM credentials WHERE cred_id = ?1",
        params![raw_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .optional()
}

pub fn rename_credential(conn: &Connection, id: &str, name: &str) -> Result<(), ApiError> {
    validate_name(name)?;
    let changed =
        conn.execute("UPDATE credentials SET name = ?1 WHERE id = ?2 AND revoked_at IS NULL", params![name, id])?;
    if changed != 1 {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_credential", "No such passkey."));
    }
    Ok(())
}

/// Why this credential may not be revoked, if there is a reason.
pub fn revocation_refusal(conn: &Connection, id: &str, acting_credential: &str) -> Result<Option<ApiError>, ApiError> {
    let refuse = |status, code, message: &str| Ok(Some(ApiError::new(status, code, message)));
    let Some(credential) = get_credential(conn, id)? else {
        return refuse(StatusCode::NOT_FOUND, "unknown_credential", "No such passkey.");
    };
    if credential.revoked_at.is_some() {
        return refuse(StatusCode::CONFLICT, "already_revoked", "That passkey is already revoked.");
    }
    if id == acting_credential {
        return refuse(
            StatusCode::CONFLICT,
            "credential_in_use",
            "You are signed in with this passkey. Sign in with another first.",
        );
    }
    // The rules about the last passkey protect the owner's way in. A family
    // member's passkey can always be taken away.
    let is_family: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM users WHERE id = ?1 AND role = 'family')",
        params![credential.user_id],
        |row| row.get(0),
    )?;
    if is_family {
        return Ok(None);
    }
    let others: i64 = conn.query_row(
        "SELECT COUNT(*) FROM credentials
         WHERE revoked_at IS NULL AND recovery = ?1 AND id != ?2 AND user_id = ?3",
        params![credential.recovery as i64, id, credential.user_id],
        |row| row.get(0),
    )?;
    if others == 0 {
        return if credential.recovery {
            refuse(
                StatusCode::CONFLICT,
                "last_recovery_credential",
                "This is the only recovery passkey and it must remain. Register another recovery passkey first.",
            )
        } else {
            refuse(
                StatusCode::CONFLICT,
                "last_usable_credential",
                "This is the last usable passkey. Register another first.",
            )
        };
    }
    Ok(None)
}

/// Revokes a credential and ends its sessions. Returns how many sessions ended.
pub fn revoke_credential(conn: &Connection, now: i64, id: &str) -> rusqlite::Result<usize> {
    conn.execute("UPDATE credentials SET revoked_at = ?1 WHERE id = ?2 AND revoked_at IS NULL", params![now, id])?;
    crate::sessions::end_for_credential(conn, now, id, "credential_revoked")
}
