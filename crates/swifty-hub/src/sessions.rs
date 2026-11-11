//! Server-side sessions. The cookie holds a random opaque id, and only its hash
//! is stored. A separate random `ref` identifies the session in listings and in
//! the audit log, so those never contain anything that could be replayed.

use axum::http::{header, HeaderMap, HeaderValue};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::crypto::{random_id, random_token, sha256_hex};
use crate::policy::Role;

/// The `__Host-` prefix makes browsers enforce Secure, Path=/ and no Domain.
pub const COOKIE_NAME: &str = "__Host-swifty-session";

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub absolute_ms: i64,
    pub idle_ms: i64,
}

/// Who a session belongs to.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Person {
    pub user_id: String,
    pub user_name: String,
    pub role: Role,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Session {
    pub session_ref: String,
    pub credential_id: String,
    pub device: String,
    #[serde(flatten)]
    pub person: Person,
    pub created_at: i64,
    pub last_seen_at: i64,
    pub expires_at: i64,
    pub idle_expires_at: i64,
    pub locked: bool,
}

#[derive(Debug, Clone)]
pub enum Lookup {
    Active(Session),
    Locked(Session),
    /// No session, or one that has ended. The text is safe to return.
    Absent(&'static str),
}

pub fn create(
    conn: &Connection,
    now: i64,
    limits: Limits,
    credential_id: &str,
    device: &str,
    person: &Person,
) -> rusqlite::Result<(String, Session)> {
    let token = random_token();
    let session_ref = random_id();
    let expires_at = now + limits.absolute_ms;
    conn.execute(
        "INSERT INTO sessions (id_hash, ref, credential_id, device, created_at, last_seen_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)",
        params![sha256_hex(token.as_bytes()), session_ref, credential_id, device, now, expires_at],
    )?;
    let session = Session {
        session_ref,
        credential_id: credential_id.to_string(),
        device: device.to_string(),
        person: person.clone(),
        created_at: now,
        last_seen_at: now,
        expires_at,
        idle_expires_at: (now + limits.idle_ms).min(expires_at),
        locked: false,
    };
    Ok((token, session))
}

fn mark_ended(conn: &Connection, id_hash: &str, now: i64, reason: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sessions SET ended_at = ?1, end_reason = ?2 WHERE id_hash = ?3 AND ended_at IS NULL",
        params![now, reason, id_hash],
    )?;
    Ok(())
}

/// Resolves a cookie value. Enforces both limits, ending the session if either
/// has passed. `touch` is false for passive polling, which must not keep an
/// unattended session alive.
pub fn lookup(conn: &Connection, now: i64, limits: Limits, token: &str, touch: bool) -> rusqlite::Result<Lookup> {
    let id_hash = sha256_hex(token.as_bytes());
    let row = conn
        .query_row(
            "SELECT s.ref, s.credential_id, s.device, s.created_at, s.last_seen_at, s.expires_at,
                    s.locked_at, s.ended_at, c.revoked_at, u.id, u.name, u.role, u.removed_at
             FROM sessions s
             JOIN credentials c ON c.id = s.credential_id
             JOIN users u ON u.id = c.user_id
             WHERE s.id_hash = ?1",
            params![id_hash],
            |row| {
                Ok((
                    Session {
                        session_ref: row.get(0)?,
                        credential_id: row.get(1)?,
                        device: row.get(2)?,
                        person: Person {
                            user_id: row.get(9)?,
                            user_name: row.get(10)?,
                            role: Role::from_stored(&row.get::<_, String>(11)?),
                        },
                        created_at: row.get(3)?,
                        last_seen_at: row.get(4)?,
                        expires_at: row.get(5)?,
                        idle_expires_at: 0,
                        locked: row.get::<_, Option<i64>>(6)?.is_some(),
                    },
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<i64>>(8)?.or(row.get::<_, Option<i64>>(12)?),
                ))
            },
        )
        .optional()?;
    let Some((mut session, ended_at, revoked_at)) = row else {
        return Ok(Lookup::Absent("not_signed_in"));
    };
    if ended_at.is_some() {
        return Ok(Lookup::Absent("session_ended"));
    }
    // Revoking ends a credential's sessions in its own transaction. This is the
    // second line: nothing can use a session whose passkey is revoked.
    if revoked_at.is_some() {
        mark_ended(conn, &id_hash, now, "credential_revoked")?;
        return Ok(Lookup::Absent("session_ended"));
    }
    if now >= session.expires_at {
        mark_ended(conn, &id_hash, now, "absolute_limit")?;
        return Ok(Lookup::Absent("session_expired"));
    }
    if now - session.last_seen_at >= limits.idle_ms {
        mark_ended(conn, &id_hash, now, "idle_limit")?;
        return Ok(Lookup::Absent("session_idle"));
    }
    if touch && !session.locked {
        conn.execute("UPDATE sessions SET last_seen_at = ?1 WHERE id_hash = ?2", params![now, id_hash])?;
        session.last_seen_at = now;
    }
    session.idle_expires_at = (session.last_seen_at + limits.idle_ms).min(session.expires_at);
    Ok(if session.locked { Lookup::Locked(session) } else { Lookup::Active(session) })
}

/// Whether a session can still act: unlocked, not ended, within both limits and
/// signed in with a passkey that has not been revoked. For use inside a write
/// transaction, where the answer cannot change before the write commits.
pub fn is_live(conn: &Connection, now: i64, limits: Limits, session_ref: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sessions s
             JOIN credentials c ON c.id = s.credential_id
             JOIN users u ON u.id = c.user_id
             WHERE s.ref = ?1 AND s.ended_at IS NULL AND s.locked_at IS NULL
               AND s.expires_at > ?2 AND s.last_seen_at > ?3 AND c.revoked_at IS NULL
               AND u.removed_at IS NULL)",
        params![session_ref, now, now - limits.idle_ms],
        |row| row.get(0),
    )
}

pub fn lock(conn: &Connection, now: i64, session_ref: &str) -> rusqlite::Result<bool> {
    let changed = conn.execute(
        "UPDATE sessions SET locked_at = ?1 WHERE ref = ?2 AND ended_at IS NULL AND locked_at IS NULL",
        params![now, session_ref],
    )?;
    Ok(changed == 1)
}

pub fn end(conn: &Connection, now: i64, session_ref: &str, reason: &str) -> rusqlite::Result<bool> {
    let changed = conn.execute(
        "UPDATE sessions SET ended_at = ?1, end_reason = ?2 WHERE ref = ?3 AND ended_at IS NULL",
        params![now, reason, session_ref],
    )?;
    Ok(changed == 1)
}

pub fn end_for_credential(conn: &Connection, now: i64, credential_id: &str, reason: &str) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE sessions SET ended_at = ?1, end_reason = ?2
         WHERE credential_id = ?3 AND ended_at IS NULL",
        params![now, reason, credential_id],
    )
}

/// Sessions that have not ended or passed either limit.
pub fn list_active(conn: &Connection, now: i64, limits: Limits) -> rusqlite::Result<Vec<Session>> {
    let mut statement = conn.prepare(
        "SELECT s.ref, s.credential_id, s.device, s.created_at, s.last_seen_at, s.expires_at, s.locked_at,
                u.id, u.name, u.role
         FROM sessions s
         JOIN credentials c ON c.id = s.credential_id
         JOIN users u ON u.id = c.user_id
         WHERE s.ended_at IS NULL AND s.expires_at > ?1 AND s.last_seen_at > ?2
           AND c.revoked_at IS NULL AND u.removed_at IS NULL
         ORDER BY s.created_at DESC",
    )?;
    let rows = statement.query_map(params![now, now - limits.idle_ms], |row| {
        let last_seen_at: i64 = row.get(4)?;
        let expires_at: i64 = row.get(5)?;
        Ok(Session {
            session_ref: row.get(0)?,
            credential_id: row.get(1)?,
            device: row.get(2)?,
            person: Person {
                user_id: row.get(7)?,
                user_name: row.get(8)?,
                role: Role::from_stored(&row.get::<_, String>(9)?),
            },
            created_at: row.get(3)?,
            last_seen_at,
            expires_at,
            idle_expires_at: (last_seen_at + limits.idle_ms).min(expires_at),
            locked: row.get::<_, Option<i64>>(6)?.is_some(),
        })
    })?;
    rows.collect()
}

/// Who a session reference belongs to, for checking a request about another one.
pub fn owner_of(conn: &Connection, session_ref: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT c.user_id FROM sessions s JOIN credentials c ON c.id = s.credential_id WHERE s.ref = ?1",
        params![session_ref],
        |row| row.get(0),
    )
    .optional()
}

/// Counts as activity, for something that is not an HTTP request. A locked or
/// ended session stays as it is.
pub fn touch(conn: &Connection, now: i64, session_ref: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sessions SET last_seen_at = ?1 WHERE ref = ?2 AND ended_at IS NULL AND locked_at IS NULL",
        params![now, session_ref],
    )?;
    Ok(())
}

pub fn set_cookie_value(token: &str, max_age_secs: i64) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{COOKIE_NAME}={token}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age={max_age_secs}"
    ))
    .expect("session tokens are URL-safe")
}

pub fn clear_cookie_value() -> HeaderValue {
    HeaderValue::from_static("__Host-swifty-session=; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=0")
}

pub fn token_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name == COOKIE_NAME && !value.is_empty()).then(|| value.to_string())
        })
}
