//! People: the one owner, the family, single-use invitations, and what each person
//! keeps for themselves. An invitation token is shown once and only its hash is
//! stored.

use axum::http::StatusCode;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};

use crate::crypto::{random_id, random_token, sha256_hex};
use crate::error::ApiError;
use crate::policy::Role;
use crate::sessions::Person;

/// How long an invitation can wait to be used.
pub const INVITATION_TTL_MS: i64 = 24 * 3_600_000;
pub const MAX_FAVOURITES: usize = 64;
pub const MAX_PINNED_ROOMS: usize = 32;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct UserView {
    pub id: String,
    pub name: String,
    pub role: Role,
    pub created_at: i64,
    pub passkeys: i64,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct InvitationView {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub expires_at: i64,
}

/// An invitation that can still be used.
pub struct OpenInvitation {
    pub id: String,
    pub name: String,
    pub expires_at: i64,
}

fn invalid_invitation() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "invalid_invitation", "That invitation is not valid.")
}

/// The owner, created the first time it is needed. There can only be one: a
/// unique index holds that, whatever calls this.
pub fn ensure_owner(conn: &Connection, now: i64, handle: &str) -> rusqlite::Result<Person> {
    let existing = conn
        .query_row("SELECT id, name FROM users WHERE role = 'owner'", [], |row| {
            Ok(Person { user_id: row.get(0)?, user_name: row.get(1)?, role: Role::Owner })
        })
        .optional()?;
    if let Some(owner) = existing {
        return Ok(owner);
    }
    let id = random_id();
    conn.execute(
        "INSERT INTO users (id, handle, name, role, created_at) VALUES (?1, ?2, 'Owner', 'owner', ?3)",
        params![id, handle, now],
    )?;
    Ok(Person { user_id: id, user_name: "Owner".to_string(), role: Role::Owner })
}

pub fn list_users(conn: &Connection) -> rusqlite::Result<Vec<UserView>> {
    let mut statement = conn.prepare(
        "SELECT u.id, u.name, u.role, u.created_at,
                (SELECT COUNT(*) FROM credentials c WHERE c.user_id = u.id AND c.revoked_at IS NULL)
         FROM users u WHERE u.removed_at IS NULL ORDER BY u.created_at ASC, u.rowid ASC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(UserView {
            id: row.get(0)?,
            name: row.get(1)?,
            role: Role::from_stored(&row.get::<_, String>(2)?),
            created_at: row.get(3)?,
            passkeys: row.get(4)?,
        })
    })?;
    rows.collect()
}

pub fn is_active_family(conn: &Connection, id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM users WHERE id = ?1 AND role = 'family' AND removed_at IS NULL)",
        params![id],
        |row| row.get(0),
    )
}

/// Takes a family member out: their passkeys and sessions end in this transaction.
pub fn remove_family(conn: &Connection, now: i64, id: &str) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE users SET removed_at = ?1 WHERE id = ?2 AND role = 'family' AND removed_at IS NULL",
        params![now, id],
    )?;
    conn.execute("UPDATE credentials SET revoked_at = ?1 WHERE user_id = ?2 AND revoked_at IS NULL", params![now, id])?;
    conn.execute(
        "UPDATE sessions SET ended_at = ?1, end_reason = 'user_removed'
         WHERE ended_at IS NULL AND credential_id IN (SELECT id FROM credentials WHERE user_id = ?2)",
        params![now, id],
    )
}

// ----- invitations

/// Creates an invitation and returns its token, which exists only in this reply.
pub fn create_invitation(
    conn: &Connection,
    now: i64,
    name: &str,
    created_by: &str,
) -> rusqlite::Result<(String, InvitationView)> {
    let id = random_id();
    let token = random_token();
    let expires_at = now + INVITATION_TTL_MS;
    conn.execute(
        "INSERT INTO invitations (id, token_hash, name, created_by, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, sha256_hex(token.as_bytes()), name, created_by, now, expires_at],
    )?;
    Ok((token, InvitationView { id, name: name.to_string(), created_at: now, expires_at }))
}

pub fn pending_invitations(conn: &Connection, now: i64) -> rusqlite::Result<Vec<InvitationView>> {
    let mut statement = conn.prepare(
        "SELECT id, name, created_at, expires_at FROM invitations
         WHERE used_at IS NULL AND cancelled_at IS NULL AND expires_at > ?1
         ORDER BY created_at ASC",
    )?;
    let rows = statement.query_map(params![now], |row| {
        Ok(InvitationView { id: row.get(0)?, name: row.get(1)?, created_at: row.get(2)?, expires_at: row.get(3)? })
    })?;
    rows.collect()
}

pub fn cancel_invitation(conn: &Connection, now: i64, id: &str) -> rusqlite::Result<bool> {
    let changed = conn.execute(
        "UPDATE invitations SET cancelled_at = ?1 WHERE id = ?2 AND used_at IS NULL AND cancelled_at IS NULL",
        params![now, id],
    )?;
    Ok(changed == 1)
}

/// Finds an invitation that can still be used, without spending it.
pub fn open_invitation(conn: &Connection, now: i64, token: &str) -> Result<OpenInvitation, ApiError> {
    conn.query_row(
        "SELECT id, name, expires_at FROM invitations
         WHERE token_hash = ?1 AND used_at IS NULL AND cancelled_at IS NULL AND expires_at > ?2",
        params![sha256_hex(token.as_bytes()), now],
        |row| Ok(OpenInvitation { id: row.get(0)?, name: row.get(1)?, expires_at: row.get(2)? }),
    )
    .optional()?
    .ok_or_else(invalid_invitation)
}

/// Spends the invitation and creates the family member. The update is the only
/// thing that can spend it, so of any number of ceremonies only one gets through.
pub fn redeem_invitation(conn: &Connection, now: i64, id: &str, handle: &str) -> Result<Person, ApiError> {
    let name: Option<String> = conn
        .query_row(
            "UPDATE invitations SET used_at = ?1
             WHERE id = ?2 AND used_at IS NULL AND cancelled_at IS NULL AND expires_at > ?1
             RETURNING name",
            params![now, id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(name) = name else {
        return Err(invalid_invitation());
    };
    let user_id = random_id();
    conn.execute(
        "INSERT INTO users (id, handle, name, role, created_at) VALUES (?1, ?2, ?3, 'family', ?4)",
        params![user_id, handle, name, now],
    )?;
    conn.execute("UPDATE invitations SET user_id = ?1 WHERE id = ?2", params![user_id, id])?;
    Ok(Person { user_id, user_name: name, role: Role::Family })
}

// ----- what each person keeps

fn targets_in_order(conn: &Connection, sql: &str, user_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut statement = conn.prepare(sql)?;
    let rows = statement.query_map(params![user_id], |row| row.get(0))?;
    rows.collect()
}

pub fn preferences(conn: &Connection, user_id: &str) -> rusqlite::Result<Value> {
    let favourites =
        targets_in_order(conn, "SELECT target FROM favourites WHERE user_id = ?1 ORDER BY position ASC", user_id)?;
    let pinned =
        targets_in_order(conn, "SELECT room_id FROM pinned_rooms WHERE user_id = ?1 ORDER BY position ASC", user_id)?;
    Ok(json!({ "favourites": favourites, "pinned_rooms": pinned }))
}

pub fn set_favourites(conn: &Connection, user_id: &str, targets: &[String]) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM favourites WHERE user_id = ?1", params![user_id])?;
    for (position, target) in targets.iter().enumerate() {
        conn.execute(
            "INSERT INTO favourites (user_id, target, position) VALUES (?1, ?2, ?3)",
            params![user_id, target, position as i64],
        )?;
    }
    Ok(())
}

pub fn set_pinned_rooms(conn: &Connection, user_id: &str, rooms: &[String]) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM pinned_rooms WHERE user_id = ?1", params![user_id])?;
    for (position, room) in rooms.iter().enumerate() {
        conn.execute(
            "INSERT INTO pinned_rooms (user_id, room_id, position) VALUES (?1, ?2, ?3)",
            params![user_id, room, position as i64],
        )?;
    }
    Ok(())
}
