//! The append-only audit log.
//!
//! What this module does and does not guarantee:
//!
//! - Each row carries the hash of the row before it, so an edit, deletion or
//!   reordering of rows that leaves the later rows in place breaks the chain.
//! - SQL triggers refuse `UPDATE` and `DELETE`, and verification fails if they are
//!   missing or altered.
//! - The chain has no secret in it. On its own it protects against accidents and
//!   against writes that do not bother to recompute it. Anyone who can write the
//!   database can cut rows off the end and fix up the recorded head, or rebuild
//!   the whole chain.
//! - With `[audit] key_file` set, the head is also signed with a key the database
//!   does not hold, and a signature is only ever extended from a head that still
//!   verifies. Rewriting the log then needs the key. Someone who can read the key
//!   file, or who restores an older copy of the database along with its older
//!   signature, is not caught by this.
//! - Only a checkpoint held off this machine closes those last gaps. Each report
//!   gives a `checkpoint` to export, and `audit-verify --expect-head SEQ:HASH`
//!   checks the log still contains it.
//!
//! The log holds catalog ids, validated parameters and enumerated results.
//! Secrets, audio and transcripts never reach it.

use std::path::Path;
use std::str::FromStr;

use hmac::{Hmac, Mac};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::Sha256;
use swifty_plugin_api::Secret;

use crate::config::{read_secret_file, ConfigError};
use crate::crypto::{canonical_json, sha256_hex};
use crate::store::AUDIT_TRIGGERS;

pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const HEAD_KEY: &str = "audit_head";
const HEAD_SIGNATURE_KEY: &str = "audit_head_signature";
const MIN_KEY_LEN: usize = 32;

type HeadMac = Hmac<Sha256>;

/// The key that signs the audit head. It lives in its own file, never in the
/// database and never in the configuration.
pub struct AuditKey(Secret);

impl AuditKey {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let secret = read_secret_file(path)?;
        if secret.expose().len() < MIN_KEY_LEN {
            return Err(ConfigError::Invalid(format!(
                "the audit key in {} must be at least {MIN_KEY_LEN} characters",
                path.display()
            )));
        }
        Ok(Self(secret))
    }

    fn mac(&self, head: &str) -> HeadMac {
        let mut mac = HeadMac::new_from_slice(self.0.expose().as_bytes()).expect("HMAC accepts a key of any length");
        mac.update(crate::domains::AUDIT_HEAD);
        mac.update(head.as_bytes());
        mac
    }

    fn sign(&self, head: &str) -> String {
        hex::encode(self.mac(head).finalize().into_bytes())
    }

    fn signed(&self, head: &str, signature: &str) -> bool {
        hex::decode(signature).is_ok_and(|bytes| self.mac(head).verify_slice(&bytes).is_ok())
    }
}

/// A point in the log, exported as `seq:hash` and checked later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub seq: i64,
    pub hash: String,
}

impl FromStr for Checkpoint {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let invalid = || "a checkpoint is written seq:hash, with a 64 digit hex hash".to_string();
        let (seq, hash) = text.trim().split_once(':').ok_or_else(invalid)?;
        let seq: i64 = seq.parse().map_err(|_| invalid())?;
        if seq < 1 || hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid());
        }
        Ok(Self { seq, hash: hash.to_ascii_lowercase() })
    }
}

/// One event to record.
#[derive(Debug, Clone)]
pub struct Event {
    pub ts: i64,
    pub action: String,
    pub target: String,
    pub params: Value,
    pub credential_ref: String,
    pub session_ref: String,
    /// The person, by user id, or `-` when nobody is signed in.
    pub user_ref: String,
    pub device: String,
    pub channel: String,
    pub step_up: String,
    pub result: String,
    pub rtt_ms: Option<i64>,
    pub run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Row {
    pub seq: i64,
    pub ts: i64,
    pub action: String,
    pub target: String,
    pub params: Value,
    pub credential_ref: String,
    pub session_ref: String,
    pub user_ref: String,
    pub device: String,
    pub channel: String,
    pub step_up: String,
    pub result: String,
    pub rtt_ms: Option<i64>,
    pub run_id: Option<String>,
    pub prev_hash: String,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct VerifyReport {
    pub ok: bool,
    pub rows: i64,
    pub head: String,
    /// `seq:hash` of the last row that verified, to export for a later check.
    pub checkpoint: String,
    pub first_bad_seq: Option<i64>,
    pub problem: Option<String>,
    /// The log has no rows and no recorded head: it says nothing either way.
    pub empty: bool,
    /// Whether the head's signature was checked.
    pub keyed: bool,
}

/// What to check beyond the chain itself.
#[derive(Clone, Copy, Default)]
pub struct VerifyOptions<'a> {
    pub key: Option<&'a AuditKey>,
    pub expected: Option<&'a Checkpoint>,
    /// Accept a log with no rows, as on a service that has never been used.
    pub allow_empty: bool,
}

#[allow(clippy::too_many_arguments)]
fn row_hash(
    prev_hash: &str,
    seq: i64,
    ts: i64,
    action: &str,
    target: &str,
    params: &str,
    credential_ref: &str,
    session_ref: &str,
    device: &str,
    channel: &str,
    step_up: &str,
    result: &str,
    rtt_ms: Option<i64>,
    run_id: Option<&str>,
    user_ref: &str,
) -> String {
    let mut body = json!([
        prev_hash,
        seq,
        ts,
        action,
        target,
        params,
        credential_ref,
        session_ref,
        device,
        channel,
        step_up,
        result,
        rtt_ms,
        run_id
    ]);
    // Empty only on rows written before people existed. Every row since has a
    // value here and commits to it, so changing who did something breaks the chain.
    if !user_ref.is_empty() {
        if let Some(fields) = body.as_array_mut() {
            fields.push(json!(user_ref));
        }
    }
    sha256_hex(canonical_json(&body).as_bytes())
}

fn meta(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", params![key], |row| row.get(0)).optional()
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// Appends one row. Call it inside the transaction that performs the change it
/// records ([`Store::tx`](crate::store::Store::tx)), so the change and its audit row
/// commit or roll back together.
///
/// Called outside a transaction, it opens its own, which takes the write lock
/// before the last row is read. The CLI appends to the same log while the hub runs,
/// and without the lock both could read the same head and write the same row
/// number. A transaction that was begun without the write lock gets no such
/// protection, which is why `Store::tx` begins `IMMEDIATE`.
///
/// A signature is only extended from a head that still verifies. If the log was
/// tampered with while the service runs, signing the next row would bless the
/// tampering, so the old signature stays and verification keeps failing.
pub fn append(conn: &Connection, event: &Event, key: Option<&AuditKey>) -> rusqlite::Result<i64> {
    if !conn.is_autocommit() {
        return append_in_transaction(conn, event, key);
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let seq = append_in_transaction(&tx, event, key)?;
    tx.commit()?;
    Ok(seq)
}

fn append_in_transaction(conn: &Connection, event: &Event, key: Option<&AuditKey>) -> rusqlite::Result<i64> {
    let last: Option<(i64, String)> = conn
        .query_row("SELECT seq, hash FROM audit_log ORDER BY seq DESC LIMIT 1", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    let (prev_seq, prev_hash) = last.unwrap_or((0, GENESIS.to_string()));
    let seq = prev_seq + 1;
    let params_text = canonical_json(&event.params);
    let hash = row_hash(
        &prev_hash,
        seq,
        event.ts,
        &event.action,
        &event.target,
        &params_text,
        &event.credential_ref,
        &event.session_ref,
        &event.device,
        &event.channel,
        &event.step_up,
        &event.result,
        event.rtt_ms,
        event.run_id.as_deref(),
        &event.user_ref,
    );
    conn.execute(
        "INSERT INTO audit_log (seq, ts, action, target, params, credential_ref, session_ref,
                                device, channel, step_up, result, rtt_ms, run_id, prev_hash, hash,
                                user_ref)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            seq,
            event.ts,
            event.action,
            event.target,
            params_text,
            event.credential_ref,
            event.session_ref,
            event.device,
            event.channel,
            event.step_up,
            event.result,
            event.rtt_ms,
            event.run_id,
            prev_hash,
            hash,
            event.user_ref
        ],
    )?;
    let head = format!("{seq}:{hash}");
    set_meta(conn, HEAD_KEY, &head)?;
    if let Some(key) = key {
        let signature = meta(conn, HEAD_SIGNATURE_KEY)?;
        let continues = match (prev_seq, signature) {
            (0, None) => true,
            (0, Some(_)) | (_, None) => false,
            (_, Some(signature)) => key.signed(&format!("{prev_seq}:{prev_hash}"), &signature),
        };
        if continues {
            set_meta(conn, HEAD_SIGNATURE_KEY, &key.sign(&head))?;
        }
    }
    Ok(seq)
}

/// Newest rows first. `before` continues from an earlier page.
pub fn read(conn: &Connection, limit: i64, before: Option<i64>) -> rusqlite::Result<Vec<Row>> {
    let mut statement = conn.prepare(
        "SELECT seq, ts, action, target, params, credential_ref, session_ref, device, channel,
                step_up, result, rtt_ms, run_id, prev_hash, hash, user_ref
         FROM audit_log WHERE seq < ?1 ORDER BY seq DESC LIMIT ?2",
    )?;
    let rows = statement.query_map(params![before.unwrap_or(i64::MAX), limit], |row| {
        let params_text: String = row.get(4)?;
        Ok(Row {
            seq: row.get(0)?,
            ts: row.get(1)?,
            action: row.get(2)?,
            target: row.get(3)?,
            params: serde_json::from_str(&params_text).unwrap_or(Value::Null),
            credential_ref: row.get(5)?,
            session_ref: row.get(6)?,
            user_ref: row.get(15)?,
            device: row.get(7)?,
            channel: row.get(8)?,
            step_up: row.get(9)?,
            result: row.get(10)?,
            rtt_ms: row.get(11)?,
            run_id: row.get(12)?,
            prev_hash: row.get(13)?,
            hash: row.get(14)?,
        })
    })?;
    rows.collect()
}

/// The current head as `seq:hash`, if the log has one.
pub fn checkpoint(conn: &Connection) -> rusqlite::Result<Option<String>> {
    meta(conn, HEAD_KEY)
}

fn squeeze(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The name of the first append-only trigger that is missing or was altered.
fn altered_trigger(conn: &Connection) -> rusqlite::Result<Option<&'static str>> {
    for (name, expected) in AUDIT_TRIGGERS {
        let stored: Option<String> = conn
            .query_row("SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = ?1", params![name], |row| {
                row.get(0)
            })
            .optional()?;
        if stored.map(|sql| squeeze(&sql)) != Some(squeeze(expected)) {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// Walks the whole chain with no key and no exported checkpoint.
pub fn verify(conn: &Connection) -> rusqlite::Result<VerifyReport> {
    verify_with(conn, VerifyOptions::default())
}

/// Walks the whole chain and reports the first thing that does not fit.
pub fn verify_with(conn: &Connection, options: VerifyOptions<'_>) -> rusqlite::Result<VerifyReport> {
    let keyed = options.key.is_some();
    let report = |ok, rows: i64, head: &str, first_bad_seq, problem, empty| VerifyReport {
        ok,
        rows,
        head: head.to_string(),
        checkpoint: format!("{rows}:{head}"),
        first_bad_seq,
        problem,
        empty,
        keyed,
    };
    let fail = |rows, head: &str, seq, problem: String| report(false, rows, head, seq, Some(problem), false);

    let mut statement = conn.prepare(
        "SELECT seq, ts, action, target, params, credential_ref, session_ref, device, channel,
                step_up, result, rtt_ms, run_id, prev_hash, hash, user_ref
         FROM audit_log ORDER BY seq ASC",
    )?;
    let mut cursor = statement.query([])?;
    let mut expected_seq = 1i64;
    let mut prev = GENESIS.to_string();
    let mut count = 0i64;

    while let Some(row) = cursor.next()? {
        let seq: i64 = row.get(0)?;
        let params_text: String = row.get(4)?;
        let run_id: Option<String> = row.get(12)?;
        let stored_prev: String = row.get(13)?;
        let stored_hash: String = row.get(14)?;
        if seq != expected_seq {
            return Ok(fail(count, &prev, Some(expected_seq), format!("row {expected_seq} is missing")));
        }
        if stored_prev != prev {
            return Ok(fail(count, &prev, Some(seq), format!("row {seq} does not link to the row before it")));
        }
        let computed = row_hash(
            &stored_prev,
            seq,
            row.get(1)?,
            &row.get::<_, String>(2)?,
            &row.get::<_, String>(3)?,
            &params_text,
            &row.get::<_, String>(5)?,
            &row.get::<_, String>(6)?,
            &row.get::<_, String>(7)?,
            &row.get::<_, String>(8)?,
            &row.get::<_, String>(9)?,
            &row.get::<_, String>(10)?,
            row.get(11)?,
            run_id.as_deref(),
            &row.get::<_, String>(15)?,
        );
        if computed != stored_hash {
            return Ok(fail(count, &prev, Some(seq), format!("row {seq} was changed after it was written")));
        }
        if options.expected.is_some_and(|expected| expected.seq == seq && expected.hash != stored_hash) {
            return Ok(fail(count, &prev, Some(seq), format!("row {seq} is not the row in the exported checkpoint")));
        }
        prev = stored_hash;
        expected_seq += 1;
        count += 1;
    }

    let recorded = meta(conn, HEAD_KEY)?;
    match &recorded {
        Some(recorded) if *recorded != format!("{count}:{prev}") => {
            let recorded_seq = recorded.split(':').next().and_then(|s| s.parse().ok());
            return Ok(fail(
                count,
                &prev,
                recorded_seq,
                "the recorded head does not match the last row: rows were removed from the end".to_string(),
            ));
        }
        None if count > 0 => {
            return Ok(fail(count, &prev, None, "the recorded head is missing".to_string()));
        }
        _ => {}
    }

    if let Some(key) = options.key {
        let signature = meta(conn, HEAD_SIGNATURE_KEY)?;
        let holds = match (&recorded, &signature) {
            (None, None) => true,
            (Some(head), Some(signature)) => key.signed(head, signature),
            _ => false,
        };
        if !holds {
            return Ok(fail(
                count,
                &prev,
                None,
                "the recorded head does not carry a valid signature from the audit key".to_string(),
            ));
        }
    }

    if let Some(expected) = options.expected {
        if expected.seq > count {
            return Ok(fail(
                count,
                &prev,
                Some(expected.seq),
                format!("the log ends before row {} of the exported checkpoint: rows were removed", expected.seq),
            ));
        }
    }

    if let Some(trigger) = altered_trigger(conn)? {
        return Ok(fail(count, &prev, None, format!("the append-only trigger {trigger} is missing or was altered")));
    }

    if count == 0 && recorded.is_none() {
        let problem = (!options.allow_empty)
            .then(|| "the log has no rows and no recorded head, so there is nothing to verify".to_string());
        return Ok(report(options.allow_empty, 0, &prev, None, problem, true));
    }

    Ok(report(true, count, &prev, None, None, false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    const ROWS: usize = 120;

    fn event(writer: usize, n: usize) -> Event {
        Event {
            ts: n as i64,
            action: "test.append".to_string(),
            target: "-".to_string(),
            params: json!({ "n": n }),
            credential_ref: format!("writer-{writer}"),
            session_ref: "-".to_string(),
            user_ref: "-".to_string(),
            device: "-".to_string(),
            channel: "panel".to_string(),
            step_up: "none".to_string(),
            result: "done".to_string(),
            rtt_ms: None,
            run_id: None,
        }
    }

    /// Two connections to one file, as the hub and the CLI have: one appends on its
    /// own, the other inside transactions that read before they write.
    #[test]
    fn two_connections_appending_at_once_both_succeed_and_the_chain_holds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.db");
        Store::open(&path).unwrap();
        let start = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            for writer in 0..2 {
                let (path, start) = (&path, &start);
                scope.spawn(move || {
                    let store = Store::open(path).unwrap();
                    start.wait();
                    for n in 0..ROWS {
                        if (n + writer).is_multiple_of(2) {
                            store.with(|conn| append(conn, &event(writer, n), None)).unwrap();
                        } else {
                            store
                                .tx(|tx| {
                                    tx.query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get::<_, i64>(0))?;
                                    append(tx, &event(writer, n), None)
                                })
                                .unwrap();
                        }
                    }
                });
            }
        });
        let store = Store::open(&path).unwrap();
        let report = store.with(verify).unwrap();
        assert!(report.ok, "{report:?}");
        assert_eq!(report.rows, 2 * ROWS as i64);
        for writer in 0..2 {
            let rows: i64 = store
                .with(|conn| {
                    conn.query_row(
                        "SELECT COUNT(DISTINCT params) FROM audit_log WHERE credential_ref = ?1",
                        [format!("writer-{writer}")],
                        |row| row.get(0),
                    )
                })
                .unwrap();
            assert_eq!(rows, ROWS as i64, "every row of writer {writer} is there once");
        }
    }

    #[test]
    fn an_append_of_its_own_that_fails_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("audit.db")).unwrap();
        store.with(|conn| append(conn, &event(0, 0), None)).unwrap();
        let head = store.with(checkpoint).unwrap();
        // The head is written after the row, so a refusal there must take the row back.
        store
            .with(|conn| {
                conn.execute_batch("CREATE TRIGGER no_head BEFORE UPDATE ON meta BEGIN SELECT RAISE(ABORT, 'no'); END;")
            })
            .unwrap();
        assert!(store.with(|conn| append(conn, &event(0, 1), None)).is_err());
        store.with(|conn| conn.execute_batch("DROP TRIGGER no_head;")).unwrap();
        assert_eq!(store.with(checkpoint).unwrap(), head);
        let report = store.with(verify).unwrap();
        assert!(report.ok, "{report:?}");
        assert_eq!(report.rows, 1);
    }
}
