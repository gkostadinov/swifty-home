//! The SQLite store for people, credentials, sessions, in-progress ceremonies,
//! prepared actions, action runs, the audit log, rooms, plugin instances and their
//! devices, scenes, notes, stored telemetry samples, the electricity tariff with the
//! energy cost ledger, and what Swifty remembers and has learned.
//!
//! The CLI writes the same file while the hub runs. Every write that spans
//! statements therefore runs in [`Store::tx`], which takes the write lock when it
//! begins (`BEGIN IMMEDIATE`), and every connection waits for the other process
//! instead of failing at once.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::json;
use swifty_plugin_api::{Capability, DeviceClass, DeviceDecl, MIN_LIGHT_DIMMING};

use crate::clock::{Clock, SystemClock};

/// The version [`Store::open`] brings every database to.
pub const SCHEMA_VERSION: i64 = 5;
/// The audit log has had its present shape since version 2, so a database from
/// then on can be inspected, including one that has not been upgraded yet.
const OLDEST_INSPECTABLE: i64 = 2;
/// How long a statement waits for another process's lock before it fails.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The triggers that make the audit log append-only. `audit::verify` compares
/// these against what the database really holds.
pub const AUDIT_TRIGGERS: [(&str, &str); 2] = [
    (
        "audit_log_no_update",
        "CREATE TRIGGER audit_log_no_update BEFORE UPDATE ON audit_log \
         BEGIN SELECT RAISE(ABORT, 'audit_log is append-only'); END",
    ),
    (
        "audit_log_no_delete",
        "CREATE TRIGGER audit_log_no_delete BEFORE DELETE ON audit_log \
         BEGIN SELECT RAISE(ABORT, 'audit_log is append-only'); END",
    ),
];

const SCHEMA: &str = r#"
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE credentials (
    id           TEXT PRIMARY KEY,
    cred_id      BLOB NOT NULL UNIQUE,
    name         TEXT NOT NULL,
    recovery     INTEGER NOT NULL DEFAULT 0,
    passkey      TEXT NOT NULL,
    sign_count   INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER,
    revoked_at   INTEGER
);

CREATE TABLE enroll_tokens (
    token_hash TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    used_at    INTEGER
);

CREATE TABLE sessions (
    id_hash       TEXT PRIMARY KEY,
    ref           TEXT NOT NULL UNIQUE,
    credential_id TEXT NOT NULL REFERENCES credentials(id),
    device        TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    last_seen_at  INTEGER NOT NULL,
    expires_at    INTEGER NOT NULL,
    locked_at     INTEGER,
    ended_at      INTEGER,
    end_reason    TEXT
);
CREATE INDEX sessions_by_credential ON sessions(credential_id);

CREATE TABLE ceremonies (
    id          TEXT PRIMARY KEY,
    kind        TEXT NOT NULL CHECK (kind IN ('enroll', 'login', 'register', 'stepup')),
    state       TEXT NOT NULL,
    session_ref TEXT,
    prepared_id TEXT,
    nonce       TEXT,
    meta        TEXT,
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    used_at     INTEGER
);
CREATE UNIQUE INDEX one_stepup_per_prepared ON ceremonies(prepared_id) WHERE kind = 'stepup';

CREATE TABLE prepared_actions (
    id          TEXT PRIMARY KEY,
    token_hash  TEXT,
    action      TEXT NOT NULL,
    target      TEXT NOT NULL,
    params      TEXT NOT NULL,
    binding     TEXT NOT NULL,
    tier        TEXT NOT NULL,
    session_ref TEXT NOT NULL,
    channel     TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    consumed_at INTEGER,
    run_id      TEXT
);

CREATE TABLE action_runs (
    id            TEXT PRIMARY KEY,
    prepared_id   TEXT,
    request_id    TEXT,
    action        TEXT NOT NULL,
    target        TEXT NOT NULL,
    params        TEXT NOT NULL,
    dedupe_key    TEXT NOT NULL,
    session_ref   TEXT NOT NULL,
    credential_id TEXT NOT NULL,
    device        TEXT NOT NULL,
    channel       TEXT NOT NULL,
    step_up       TEXT NOT NULL,
    phase         TEXT NOT NULL
        CHECK (phase IN ('commanded', 'confirmed', 'no_response', 'failed', 'simulated')),
    detail        TEXT,
    steps         TEXT,
    started_at    INTEGER NOT NULL,
    finished_at   INTEGER,
    rtt_ms        INTEGER
);
CREATE UNIQUE INDEX one_inflight ON action_runs(dedupe_key) WHERE phase = 'commanded';
CREATE UNIQUE INDEX one_per_request ON action_runs(session_ref, request_id)
    WHERE request_id IS NOT NULL;
CREATE INDEX runs_recent ON action_runs(action, target, started_at);

CREATE TABLE audit_log (
    seq            INTEGER PRIMARY KEY AUTOINCREMENT,
    ts             INTEGER NOT NULL,
    action         TEXT NOT NULL,
    target         TEXT NOT NULL,
    params         TEXT NOT NULL,
    credential_ref TEXT NOT NULL,
    session_ref    TEXT NOT NULL,
    device         TEXT NOT NULL,
    channel        TEXT NOT NULL,
    step_up        TEXT NOT NULL,
    result         TEXT NOT NULL,
    rtt_ms         INTEGER,
    run_id         TEXT,
    prev_hash      TEXT NOT NULL,
    hash           TEXT NOT NULL
);
CREATE INDEX audit_anonymous ON audit_log(ts) WHERE session_ref = '-';
"#;

/// People, rooms, machines and stored samples. A new database is built by running
/// the first schema and then this, so the migration is exercised on every install.
///
/// Until version 4 this also seeded four machines. A database made before then has
/// them already, and a new one starts with none.
const MIGRATE_V2: &str = r#"
CREATE TABLE users (
    id         TEXT PRIMARY KEY,
    handle     TEXT NOT NULL UNIQUE,
    name       TEXT NOT NULL,
    role       TEXT NOT NULL CHECK (role IN ('owner', 'family')),
    created_at INTEGER NOT NULL,
    removed_at INTEGER
);
CREATE UNIQUE INDEX one_owner ON users(role) WHERE role = 'owner';

ALTER TABLE credentials ADD COLUMN user_id TEXT REFERENCES users(id);
CREATE INDEX credentials_by_user ON credentials(user_id);

-- Rows from before this version stay empty, and their hash leaves it out.
ALTER TABLE audit_log ADD COLUMN user_ref TEXT NOT NULL DEFAULT '';

CREATE TABLE ceremonies_v2 (
    id          TEXT PRIMARY KEY,
    kind        TEXT NOT NULL CHECK (kind IN ('enroll', 'login', 'register', 'stepup', 'invite')),
    state       TEXT NOT NULL,
    session_ref TEXT,
    prepared_id TEXT,
    nonce       TEXT,
    meta        TEXT,
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    used_at     INTEGER
);
INSERT INTO ceremonies_v2 SELECT id, kind, state, session_ref, prepared_id, nonce, meta,
                                 created_at, expires_at, used_at FROM ceremonies;
DROP TABLE ceremonies;
ALTER TABLE ceremonies_v2 RENAME TO ceremonies;
CREATE UNIQUE INDEX one_stepup_per_prepared ON ceremonies(prepared_id) WHERE kind = 'stepup';

CREATE TABLE invitations (
    id           TEXT PRIMARY KEY,
    token_hash   TEXT NOT NULL UNIQUE,
    name         TEXT NOT NULL,
    created_by   TEXT NOT NULL REFERENCES users(id),
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL,
    used_at      INTEGER,
    cancelled_at INTEGER,
    user_id      TEXT
);

CREATE TABLE favourites (
    user_id  TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    target   TEXT NOT NULL,
    position INTEGER NOT NULL,
    PRIMARY KEY (user_id, target)
);

CREATE TABLE rooms (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    name_key   TEXT NOT NULL UNIQUE,
    position   INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);

-- A device is in one room at most.
CREATE TABLE room_devices (
    target   TEXT PRIMARY KEY,
    room_id  TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    position INTEGER NOT NULL
);
CREATE INDEX room_devices_by_room ON room_devices(room_id, position);

CREATE TABLE room_scenes (
    id       TEXT PRIMARY KEY,
    room_id  TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    name     TEXT NOT NULL,
    name_key TEXT NOT NULL,
    settings TEXT NOT NULL,
    UNIQUE (room_id, name_key)
);

CREATE TABLE pinned_rooms (
    user_id  TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    room_id  TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    PRIMARY KEY (user_id, room_id)
);

CREATE TABLE device_names (
    target TEXT PRIMARY KEY,
    name   TEXT NOT NULL
);

CREATE TABLE lights (
    target   TEXT PRIMARY KEY,
    mac      TEXT NOT NULL UNIQUE,
    address  TEXT NOT NULL,
    name     TEXT NOT NULL,
    added_at INTEGER NOT NULL,
    added_by TEXT NOT NULL
);

-- What a real broadcast found. A light can only be added from here, so nobody can
-- name an address for this service to send to.
CREATE TABLE light_candidates (
    mac     TEXT PRIMARY KEY,
    address TEXT NOT NULL,
    seen_at INTEGER NOT NULL
);

CREATE TABLE proxmox_hosts (
    id         TEXT PRIMARY KEY,
    address    TEXT NOT NULL UNIQUE,
    node       TEXT NOT NULL,
    token_file TEXT NOT NULL,
    ca_file    TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    created_by TEXT NOT NULL
);

CREATE TABLE machines (
    target      TEXT PRIMARY KEY,
    host        TEXT NOT NULL,
    vmid        INTEGER NOT NULL,
    name        TEXT NOT NULL,
    status      TEXT NOT NULL CHECK (status IN ('approved', 'pending')),
    first_seen  INTEGER NOT NULL,
    approved_at INTEGER,
    approved_by TEXT,
    UNIQUE (host, vmid)
);

CREATE TABLE collector_samples (
    ts     INTEGER NOT NULL,
    metric TEXT NOT NULL,
    value  REAL NOT NULL
);
CREATE INDEX samples_by_metric ON collector_samples(metric, ts);
CREATE INDEX samples_by_time ON collector_samples(ts);
"#;

/// What Swifty remembers, the phrases it has learned, and the commands it needed help
/// with. A memory with no person belongs to the house. See `memory.rs`.
const MIGRATE_V3: &str = r#"
CREATE TABLE memories (
    id         TEXT PRIMARY KEY,
    user_id    TEXT REFERENCES users(id),
    text       TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    created_by TEXT NOT NULL
);
CREATE INDEX memories_by_user ON memories(user_id);

CREATE TABLE learned_phrases (
    id         TEXT PRIMARY KEY,
    phrase     TEXT NOT NULL UNIQUE,
    action     TEXT NOT NULL,
    target     TEXT NOT NULL,
    params     TEXT NOT NULL,
    label      TEXT NOT NULL,
    learned_by TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    used_at    INTEGER,
    uses       INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE swifty_misses (
    at      INTEGER NOT NULL,
    user_id TEXT NOT NULL,
    phrase  TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('learned', 'worked_out', 'not_understood', 'corrected'))
);
CREATE INDEX swifty_misses_by_time ON swifty_misses(at);
"#;

/// Brings a first-version database up to this one. Credentials from before there
/// were people all belong to the owner, whose WebAuthn handle is already stored.
fn migrate_to_v2(tx: &Connection) -> rusqlite::Result<()> {
    tx.execute_batch(MIGRATE_V2)?;
    let credentials: i64 = tx.query_row("SELECT COUNT(*) FROM credentials", [], |row| row.get(0))?;
    if credentials == 0 {
        return Ok(());
    }
    let handle: String = tx.query_row("SELECT value FROM meta WHERE key = 'owner_id'", [], |row| row.get(0))?;
    let id = crate::crypto::random_id();
    tx.execute(
        "INSERT INTO users (id, handle, name, role, created_at) VALUES (?1, ?2, 'Owner', 'owner', 0)",
        rusqlite::params![id, handle],
    )?;
    tx.execute("UPDATE credentials SET user_id = ?1 WHERE user_id IS NULL", rusqlite::params![id])?;
    Ok(())
}

/// Plugin instances, the devices stored for them, what discovery found, the owner's
/// notes and the house scenes. The data of the old
/// `lights`, `light_candidates` and `machines` tables moves in, see [`migrate_to_v4`].
const MIGRATE_V4: &str = r#"
-- An instance of a plugin, with its config as JSON that passed the plugin's checks.
-- Credentials are never here, only in the secret store.
CREATE TABLE plugin_instances (
    id             TEXT PRIMARY KEY,
    plugin         TEXT NOT NULL,
    config         TEXT NOT NULL,
    config_version INTEGER NOT NULL,
    enabled        INTEGER NOT NULL DEFAULT 1,
    created_at     INTEGER NOT NULL,
    created_by     TEXT NOT NULL,
    updated_at     INTEGER NOT NULL
);

-- A device the hub stored for an instance. The target is given once and never
-- changes: from then on this row is the authority on which instance carries it.
CREATE TABLE devices (
    target      TEXT PRIMARY KEY,
    instance_id TEXT NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
    local_id    TEXT NOT NULL,
    class       TEXT NOT NULL,
    name        TEXT NOT NULL,
    attrs       TEXT NOT NULL DEFAULT '{}',
    origin      TEXT NOT NULL CHECK (origin IN ('config', 'adopted')),
    status      TEXT NOT NULL CHECK (status IN ('active', 'pending')),
    first_seen  INTEGER NOT NULL,
    adopted_at  INTEGER,
    adopted_by  TEXT,
    UNIQUE (instance_id, local_id)
);

-- What a real discovery found. A device can only be adopted from here, so nobody
-- can name an address for the hub to send to.
CREATE TABLE device_candidates (
    instance_id TEXT NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
    local_id    TEXT NOT NULL,
    decl        TEXT NOT NULL,
    seen_at     INTEGER NOT NULL,
    PRIMARY KEY (instance_id, local_id)
);

-- Free lines the owner writes about a device, as a JSON array.
CREATE TABLE device_notes (target TEXT PRIMARY KEY, notes TEXT NOT NULL);

-- One level for every light (percent 0 means off), the lights that differ as
-- [{target, percent, hue}], and what else the scene does, such as Movie's
-- {"tv_input": "HDMI 1", "volume_cap": 30}.
CREATE TABLE scenes (
    id        TEXT PRIMARY KEY,
    name      TEXT NOT NULL,
    position  INTEGER NOT NULL,
    percent   INTEGER NOT NULL,
    hue       INTEGER NOT NULL,
    overrides TEXT NOT NULL DEFAULT '[]',
    extras    TEXT NOT NULL DEFAULT '{}'
);
"#;

/// The action ids renamed before version 4, old then new. A phrase Swifty learned
/// under an old id is rewritten to the new one. Frozen: this is what version 4 did,
/// whatever the catalog says later.
pub const RENAMED_ACTIONS: [(&str, &str); 8] = [
    ("bulb.power", "light.power"),
    ("bulb.brightness", "light.brightness"),
    ("bulb.hue", "light.hue"),
    ("sonos.volume", "speaker.volume"),
    ("sonos.mute", "speaker.mute"),
    ("sonos.night", "speaker.night"),
    ("sonos.play", "speaker.play"),
    ("sonos.pause", "speaker.pause"),
];

/// The address a light on the Hue Bridge was stored with. Nothing is ever sent to
/// it. Frozen: it is how version 3 told the bridge's lights apart.
pub(crate) const BRIDGE_ADDRESS: &str = "hue-bridge";
/// The instances version 3's lights and machines are moved under.
pub(crate) const WIZ: &str = "wiz";
pub(crate) const HUE: &str = "hue";
pub(crate) const PROXMOX: &str = "proxmox";
/// A light on the Hue Bridge dims all the way down to 1 percent.
const HUE_MIN_BRIGHTNESS: u8 = 1;
/// Who created the instance rows the upgrade needed.
const UPGRADE: &str = "schema-v4";

/// A light that a legacy discovery found, as a candidate of the instance that
/// carries it. A light on the Hue Bridge is kept at the bridge's address and known
/// by the id the bridge listed it under; any other is a WiZ bulb, known by its MAC.
/// `name` is what the bridge calls it, where that is known. Adopting it stores
/// these `attrs`, which is what the `wiz` and `hue` plugins read.
pub(crate) fn legacy_light_candidate(mac: &str, address: &str, name: Option<&str>) -> (&'static str, DeviceDecl) {
    let light = |local_id: String, name: String, min: u8, attrs| DeviceDecl {
        target_hint: Some(local_id.clone()),
        local_id,
        class: DeviceClass::Light,
        name,
        capabilities: vec![Capability::Power, Capability::Brightness { min }, Capability::Color],
        attrs,
    };
    if address == BRIDGE_ADDRESS {
        let number = mac.strip_prefix("hue-").unwrap_or(mac);
        let name = name.map_or_else(|| format!("Hue light {number}"), str::to_string);
        let attrs = json!({ "mac": mac, "address": address, "light": number });
        (HUE, light(mac.to_string(), name, HUE_MIN_BRIGHTNESS, attrs))
    } else {
        let tail = mac.get(mac.len().saturating_sub(4)..).unwrap_or(mac);
        let name = name.map_or_else(|| format!("WiZ light {tail}"), str::to_string);
        let attrs = json!({ "mac": mac, "address": address });
        (WIZ, light(format!("wiz-{mac}"), name, MIN_LIGHT_DIMMING, attrs))
    }
}

/// Version 4, in the transaction that runs it. Every target stays exactly as it
/// was, so rooms, favourites, names, notes and the audit log still name the same
/// devices, and the audit log itself is not touched.
fn migrate_to_v4(tx: &Connection, now: i64) -> rusqlite::Result<()> {
    tx.execute_batch(MIGRATE_V4)?;

    // An instance for every legacy kind that has a light, a candidate or a machine,
    // before anything that refers to it.
    tx.execute(
        "INSERT INTO plugin_instances
             (id, plugin, config, config_version, enabled, created_at, created_by, updated_at)
         SELECT kind, kind, '{}', 1, 1, ?1, ?2, ?1 FROM (
             SELECT CASE WHEN address = ?3 THEN ?4 ELSE ?5 END AS kind FROM lights
             UNION SELECT CASE WHEN address = ?3 THEN ?4 ELSE ?5 END FROM light_candidates
             UNION SELECT ?6 FROM machines
         ) ORDER BY kind",
        params![now, UPGRADE, BRIDGE_ADDRESS, HUE, WIZ, PROXMOX],
    )?;

    // Added lights: adopted, active, under their own target, with the attrs that
    // adopting the same light from a discovery stores today.
    let lights: Vec<(String, String, String, String, i64, String)> = {
        let mut statement =
            tx.prepare("SELECT target, mac, address, name, added_at, added_by FROM lights ORDER BY added_at, target")?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (target, mac, address, name, added_at, added_by) in lights {
        let (instance, decl) = legacy_light_candidate(&mac, &address, None);
        tx.execute(
            "INSERT INTO devices (target, instance_id, local_id, class, name, attrs, origin, status,
                                  first_seen, adopted_at, adopted_by)
             VALUES (?1, ?2, ?1, 'light', ?3, ?4, 'adopted', 'active', ?5, ?5, ?6)",
            params![target, instance, name, decl.attrs.to_string(), added_at, added_by],
        )?;
    }

    // What discoveries found, as the candidates the plugins would report.
    let candidates: Vec<(String, String, i64)> = {
        let mut statement = tx.prepare("SELECT mac, address, seen_at FROM light_candidates ORDER BY mac")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (mac, address, seen_at) in candidates {
        let (instance, decl) = legacy_light_candidate(&mac, &address, None);
        let text = serde_json::to_string(&decl).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        tx.execute(
            "INSERT INTO device_candidates (instance_id, local_id, decl, seen_at) VALUES (?1, ?2, ?3, ?4)",
            params![instance, decl.local_id, text, seen_at],
        )?;
    }

    // Machines: a VM on the host it was listed by. Approved is active; a pending
    // one stays listed and inert until it is approved.
    tx.execute(
        "INSERT INTO devices (target, instance_id, local_id, class, name, attrs, origin, status,
                              first_seen, adopted_at, adopted_by)
         SELECT target, ?1, target, 'vm', name, json_object('host', host, 'vmid', vmid), 'adopted',
                CASE status WHEN 'approved' THEN 'active' ELSE 'pending' END,
                first_seen, approved_at, approved_by
         FROM machines ORDER BY host, vmid",
        params![PROXMOX],
    )?;

    tx.execute_batch(
        "DROP TABLE lights;
         DROP TABLE light_candidates;
         DROP TABLE machines;",
    )?;

    for (old, new) in RENAMED_ACTIONS {
        tx.execute("UPDATE learned_phrases SET action = ?1 WHERE action = ?2", params![new, old])?;
    }

    crate::scenes::insert_house_scenes(tx)
}

/// The electricity tariff and the energy cost ledger. Version 5 only adds these two
/// tables: nothing that exists is changed, so an install at version 4 keeps every row.
const MIGRATE_V5: &str = r#"
-- The tariff the owner set: at most one row. A day price per kWh and, for a two-rate
-- meter, a night price with the hours night runs (minutes after local midnight; the end
-- may come before the start, across midnight), other hours while daylight saving time is
-- in effect when they differ, and the IANA time zone the hours are read in. Money here is
-- an estimate, so it is kept as a REAL and rounded only for display.
CREATE TABLE energy_tariff (
    id                 INTEGER PRIMARY KEY CHECK (id = 1),
    currency           TEXT    NOT NULL,
    day_price          REAL    NOT NULL CHECK (day_price > 0),
    night_price        REAL             CHECK (night_price IS NULL OR night_price > 0),
    night_start        INTEGER,
    night_end          INTEGER,
    summer_night_start INTEGER,
    summer_night_end   INTEGER,
    time_zone          TEXT,
    set_at             INTEGER NOT NULL,
    set_by             TEXT    NOT NULL
);

-- What the hub has counted of each telemetry node's energy counter, and what it
-- costs so far. `since_s` and `last_kwh` are the counter as the node last reported
-- it (unix seconds, kWh); `counted_kwh` is everything the hub has taken into account,
-- across counter restarts. `kwh_day`, `kwh_night`, `cost_day` and `cost_night` are the
-- energy charged and its cost at each price. `priced_since` is when the first charge was
-- made (null while no tariff is set) and `backfilled_kwh` the energy that had been
-- counted before it, charged once at the first tariff's day price.
CREATE TABLE energy_ledger (
    target         TEXT    PRIMARY KEY,
    since_s        INTEGER,
    last_kwh       REAL    NOT NULL,
    counted_kwh    REAL    NOT NULL,
    kwh_day        REAL    NOT NULL DEFAULT 0,
    kwh_night      REAL    NOT NULL DEFAULT 0,
    cost_day       REAL    NOT NULL DEFAULT 0,
    cost_night     REAL    NOT NULL DEFAULT 0,
    priced_since   INTEGER,
    backfilled_kwh REAL,
    first_seen     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
);
"#;

/// Runs every migration after `from` up to `to`, in the transaction given.
fn upgrade(tx: &Connection, from: i64, to: i64) -> rusqlite::Result<()> {
    if from >= to {
        return Ok(());
    }
    if from < 1 {
        tx.execute_batch(SCHEMA)?;
        for (_, trigger) in AUDIT_TRIGGERS {
            tx.execute_batch(trigger)?;
        }
    }
    if from < 2 && to >= 2 {
        migrate_to_v2(tx)?;
    }
    if from < 3 && to >= 3 {
        tx.execute_batch(MIGRATE_V3)?;
    }
    if from < 4 && to >= 4 {
        migrate_to_v4(tx, SystemClock.now_ms())?;
    }
    if from < 5 && to >= 5 {
        tx.execute_batch(MIGRATE_V5)?;
    }
    tx.execute_batch(&format!("PRAGMA user_version = {to};"))
}

fn user_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
}

/// Puts the file in write-ahead mode, which it keeps. Switching needs every other
/// connection to stand aside, and SQLite does not wait for that on its own, so a
/// busy answer is tried again until the usual timeout. A file already in this mode
/// answers at once.
fn use_wal(conn: &Connection) -> rusqlite::Result<()> {
    let deadline = std::time::Instant::now() + BUSY_TIMEOUT;
    loop {
        match conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get::<_, String>(0)) {
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::DatabaseBusy && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            outcome => return outcome.map(drop),
        }
    }
}

/// Creates a database at `path` with the tables an earlier version of this service
/// made, and no rows, for tests of the upgrade: [`Store::open`] brings it up to date.
pub fn create_version(path: &Path, version: i64) -> rusqlite::Result<()> {
    if !(1..=SCHEMA_VERSION).contains(&version) {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let conn = Connection::open(path)?;
    let tx = Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)?;
    upgrade(&tx, user_version(&tx)?, version)?;
    tx.commit()
}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// Opens the database, creating it if needed, and brings it up to
    /// [`SCHEMA_VERSION`] in one transaction.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        restrict_permissions(path);
        // First, so that the statements after it wait for another process.
        conn.busy_timeout(BUSY_TIMEOUT)?;
        use_wal(&conn)?;
        conn.execute_batch(
            "PRAGMA synchronous = FULL;
             PRAGMA foreign_keys = ON;",
        )?;
        if user_version(&conn)? < SCHEMA_VERSION {
            // The version is read again under the write lock: another process
            // opening the same file may have upgraded it in the meantime.
            let tx = Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)?;
            upgrade(&tx, user_version(&tx)?, SCHEMA_VERSION)?;
            tx.commit()?;
        }
        Ok(Self { conn: Mutex::new(conn) })
    }

    /// Opens a database only to inspect it. Nothing is created or written, and a
    /// file this service has never run on is refused, so an emptied or replaced
    /// file cannot pass for a clean log. A database an earlier version left is read
    /// as it is and not upgraded, so its audit chain can be checked before
    /// `swifty migrate` copies it.
    pub fn open_for_inspection(path: &Path) -> Result<Self, String> {
        let name = path.display();
        let not_ours = format!("{name} is not a database this service has run on");
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("cannot open {name}: {e}"))?;
        conn.busy_timeout(BUSY_TIMEOUT).map_err(|e| format!("cannot open {name}: {e}"))?;
        let version = user_version(&conn).map_err(|e| format!("cannot read {name}: {e}"))?;
        if !(OLDEST_INSPECTABLE..=SCHEMA_VERSION).contains(&version) {
            return Err(not_ours);
        }
        let owner: Option<String> = conn
            .query_row("SELECT value FROM meta WHERE key = 'owner_id'", [], |row| row.get(0))
            .optional()
            .map_err(|_| not_ours.clone())?;
        if owner.is_none() {
            return Err(not_ours);
        }
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn guard(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock leaves SQLite in a consistent state
        // because every multi-statement write is a transaction.
        self.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn with<R, E>(&self, f: impl FnOnce(&Connection) -> Result<R, E>) -> Result<R, E> {
        let conn = self.guard();
        f(&conn)
    }

    /// Runs `f` in a transaction that holds the write lock from its first statement,
    /// so nothing it reads can be changed by another process before it commits. It
    /// commits if `f` succeeds and rolls back otherwise.
    pub fn tx<R, E>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<R, E>) -> Result<R, E>
    where
        E: From<rusqlite::Error>,
    {
        let mut conn = self.guard();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(error) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        tracing::warn!(%error, "could not restrict database file permissions");
    }
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit;

    /// A database as the first version of the service left it.
    fn first_version_database(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        for (_, trigger) in AUDIT_TRIGGERS {
            conn.execute_batch(trigger).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO meta (key, value) VALUES ('owner_id', '11111111-2222-3333-4444-555555555555');
             INSERT INTO credentials (id, cred_id, name, passkey, created_at) VALUES ('old', x'01', 'Old', '{}', 1);
             INSERT INTO sessions (id_hash, ref, credential_id, device, created_at, last_seen_at, expires_at)
                 VALUES ('h', 'r', 'old', 'Old', 1, 1, 9);
             PRAGMA user_version = 1;",
        )
        .unwrap();
    }

    #[test]
    fn a_first_version_database_gains_its_owner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        first_version_database(&path);
        let store = Store::open(&path).unwrap();
        store
            .with(|conn| -> rusqlite::Result<()> {
                let (role, handle): (String, String) =
                    conn.query_row("SELECT role, handle FROM users", [], |row| Ok((row.get(0)?, row.get(1)?)))?;
                assert_eq!(role, "owner");
                assert_eq!(handle, "11111111-2222-3333-4444-555555555555");
                let linked: i64 =
                    conn.query_row("SELECT COUNT(*) FROM credentials c JOIN users u ON u.id = c.user_id", [], |row| {
                        row.get(0)
                    })?;
                assert_eq!(linked, 1);
                // The schema no longer brings machines of its own.
                let machines: i64 = conn.query_row("SELECT COUNT(*) FROM devices", [], |row| row.get(0))?;
                assert_eq!(machines, 0);
                let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
                assert_eq!(version, SCHEMA_VERSION);
                Ok(())
            })
            .unwrap();
        // Opening it again changes nothing.
        drop(store);
        Store::open(&path).unwrap();
    }

    #[test]
    fn an_invite_ceremony_fits_after_the_migration_and_nothing_else_changed() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("new.db")).unwrap();
        store
            .with(|conn| -> rusqlite::Result<()> {
                conn.execute(
                    "INSERT INTO ceremonies (id, kind, state, created_at, expires_at) VALUES ('a', 'invite', '{}', 1, 2)",
                    [],
                )?;
                let refused = conn.execute(
                    "INSERT INTO ceremonies (id, kind, state, created_at, expires_at) VALUES ('b', 'other', '{}', 1, 2)",
                    [],
                );
                assert!(refused.is_err());
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn there_can_only_be_one_owner() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("one.db")).unwrap();
        store
            .with(|conn| -> rusqlite::Result<()> {
                conn.execute(
                    "INSERT INTO users (id, handle, name, role, created_at) VALUES ('a', 'h1', 'A', 'owner', 1)",
                    [],
                )?;
                let second = conn.execute(
                    "INSERT INTO users (id, handle, name, role, created_at) VALUES ('b', 'h2', 'B', 'owner', 1)",
                    [],
                );
                assert!(second.is_err());
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn rows_from_before_people_still_verify_and_new_rows_commit_to_the_user() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.db");
        first_version_database(&path);
        // A row written by the first version, with its hash over the old fields.
        {
            let conn = Connection::open(&path).unwrap();
            let body = serde_json::json!([
                audit::GENESIS,
                1,
                5,
                "vm.start",
                "vm100",
                "null",
                "c",
                "s",
                "d",
                "panel",
                "none",
                "done",
                null,
                null
            ]);
            let hash = crate::crypto::sha256_hex(crate::crypto::canonical_json(&body).as_bytes());
            conn.execute(
                "INSERT INTO audit_log (seq, ts, action, target, params, credential_ref, session_ref, device, channel,
                                        step_up, result, prev_hash, hash)
                 VALUES (1, 5, 'vm.start', 'vm100', 'null', 'c', 's', 'd', 'panel', 'none', 'done', ?1, ?2)",
                [audit::GENESIS, hash.as_str()],
            )
            .unwrap();
            conn.execute("INSERT INTO meta (key, value) VALUES ('audit_head', ?1)", [format!("1:{hash}")]).unwrap();
        }
        let store = Store::open(&path).unwrap();
        store
            .with(|conn| -> rusqlite::Result<()> {
                assert!(audit::verify(conn)?.ok, "{:?}", audit::verify(conn)?);
                audit::append(
                    conn,
                    &audit::Event {
                        ts: 6,
                        action: "a".to_string(),
                        target: "t".to_string(),
                        params: serde_json::Value::Null,
                        credential_ref: "c".to_string(),
                        session_ref: "s".to_string(),
                        user_ref: "person".to_string(),
                        device: "d".to_string(),
                        channel: "panel".to_string(),
                        step_up: "none".to_string(),
                        result: "done".to_string(),
                        rtt_ms: None,
                        run_id: None,
                    },
                    None,
                )?;
                assert!(audit::verify(conn)?.ok);
                conn.execute_batch(
                    "DROP TRIGGER audit_log_no_update;
                     UPDATE audit_log SET user_ref = 'someone-else' WHERE seq = 2;",
                )?;
                let report = audit::verify(conn)?;
                assert!(!report.ok);
                assert_eq!(report.first_bad_seq, Some(2));
                Ok(())
            })
            .unwrap();
    }

    // ----- version 4: the device store

    use crate::audit::{AuditKey, VerifyOptions};
    use crate::inventory::{self, MachineStatus};
    use rusqlite::types::Value as Cell;
    use serde_json::Value;
    use std::collections::BTreeMap;

    const DROPPED: [&str; 3] = ["lights", "light_candidates", "machines"];
    const ADDED: [&str; 5] = ["plugin_instances", "devices", "device_candidates", "device_notes", "scenes"];
    /// What version 5 adds on top, when the same open takes a version 3 database all the way up.
    const ADDED_BY_V5: [&str; 2] = ["energy_tariff", "energy_ledger"];

    fn audit_key(dir: &Path) -> AuditKey {
        let path = dir.join("audit.key");
        std::fs::write(&path, "audit-signing-key-for-tests-0123456789").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        AuditKey::load(&path).unwrap()
    }

    fn keyed(key: &AuditKey) -> VerifyOptions<'_> {
        VerifyOptions { key: Some(key), ..Default::default() }
    }

    fn event(n: i64, action: &str, target: &str) -> audit::Event {
        audit::Event {
            ts: 1_000 + n,
            action: action.to_string(),
            target: target.to_string(),
            params: json!({ "n": n }),
            credential_ref: "c-owner".to_string(),
            session_ref: "s-owner".to_string(),
            user_ref: "u-owner".to_string(),
            device: "Phone".to_string(),
            channel: "panel".to_string(),
            step_up: "none".to_string(),
            result: "confirmed".to_string(),
            rtt_ms: Some(n),
            run_id: None,
        }
    }

    /// What a version 3 install that has been used holds: people with passkeys, the
    /// four machines the old schema seeded, one machine waiting for approval and one
    /// on a further host, lights and what discovery found on both instances, rooms,
    /// favourites, names, memories, and phrases learned under old and new ids.
    const V3_HOUSE: &str = r#"
INSERT INTO meta (key, value) VALUES ('owner_id', '11111111-2222-3333-4444-555555555555');
INSERT INTO users (id, handle, name, role, created_at) VALUES
    ('u-owner', '11111111-2222-3333-4444-555555555555', 'Owner', 'owner', 1),
    ('u-family', 'aaaaaaaa-2222-3333-4444-555555555555', 'Family', 'family', 2);
INSERT INTO credentials (id, cred_id, name, recovery, passkey, sign_count, created_at, user_id) VALUES
    ('c-owner', x'0102', 'Phone', 0, '{"cred":"owner"}', 7, 1, 'u-owner'),
    ('c-recovery', x'0304', 'Key', 1, '{"cred":"recovery"}', 0, 1, 'u-owner'),
    ('c-family', x'0506', 'Tablet', 0, '{"cred":"family"}', 2, 2, 'u-family');
INSERT INTO sessions (id_hash, ref, credential_id, device, created_at, last_seen_at, expires_at)
    VALUES ('h1', 's-owner', 'c-owner', 'Phone', 1, 5, 99);
INSERT INTO machines (target, host, vmid, name, status, first_seen, approved_at, approved_by) VALUES
    ('vm100', 'primary', 100, 'alpha', 'approved', 0, 0, 'seed'),
    ('vm101', 'primary', 101, 'bravo', 'approved', 0, 0, 'seed'),
    ('vm102', 'primary', 102, 'charlie', 'approved', 0, 0, 'seed'),
    ('vm103', 'primary', 103, 'delta', 'approved', 0, 0, 'seed'),
    ('vm150', 'primary', 150, 'new-vm', 'pending', 40, NULL, NULL),
    ('nas2-vm200', 'nas2', 200, 'store', 'approved', 41, 42, 'u-owner');
INSERT INTO proxmox_hosts (id, address, node, token_file, ca_file, created_at, created_by)
    VALUES ('nas2', '192.0.2.40:8006', 'pve2', '/s/pve2.token', '/s/pve2-ca.pem', 30, 'u-owner');
INSERT INTO lights (target, mac, address, name, added_at, added_by) VALUES
    ('wiz-a8bb50aaaaaa', 'a8bb50aaaaaa', '192.0.2.21:38899', 'Hall lamp', 10, 'u-owner'),
    ('hue-3', 'hue-3', 'hue-bridge', 'Strip', 11, 'u-owner');
INSERT INTO light_candidates (mac, address, seen_at) VALUES
    ('a8bb50aaaaaa', '192.0.2.21:38899', 20),
    ('a8bb50bbbbbb', '192.0.2.22:38899', 21),
    ('hue-3', 'hue-bridge', 22),
    ('hue-4', 'hue-bridge', 23);
INSERT INTO rooms (id, name, name_key, position, created_at) VALUES ('r-living', 'Living room', 'living room', 0, 3);
INSERT INTO room_devices (target, room_id, position) VALUES
    ('bulb1', 'r-living', 0), ('wiz-a8bb50aaaaaa', 'r-living', 1), ('hue-3', 'r-living', 2), ('vm100', 'r-living', 3);
INSERT INTO room_scenes (id, room_id, name, name_key, settings) VALUES
    ('rs-1', 'r-living', 'Glow', 'glow', '[{"target":"hue-3","percent":80,"hue":120},{"target":"wiz-a8bb50aaaaaa"}]');
INSERT INTO pinned_rooms (user_id, room_id, position) VALUES ('u-family', 'r-living', 0);
INSERT INTO favourites (user_id, target, position) VALUES
    ('u-owner', 'hue-3', 0), ('u-owner', 'nas2-vm200', 1), ('u-family', 'wiz-a8bb50aaaaaa', 0);
INSERT INTO device_names (target, name) VALUES ('vm100', 'Desktop'), ('wiz-a8bb50aaaaaa', 'Reading lamp');
INSERT INTO collector_samples (ts, metric, value) VALUES (5, 'cpu', 0.25);
INSERT INTO memories (id, user_id, text, created_at, created_by) VALUES ('m-1', NULL, 'The cat eats at seven.', 6, 'u-owner');
INSERT INTO learned_phrases (id, phrase, action, target, params, label, learned_by, created_at, used_at, uses) VALUES
    ('p-volume', 'make it quieter', 'sonos.volume', 'sonos', '{"level":10}', 'Volume 10', 'u-owner', 7, 8, 3),
    ('p-lamp', 'lamp on', 'bulb.power', 'wiz-a8bb50aaaaaa', '{"on":true}', 'Lamp on', 'u-family', 7, NULL, 0),
    ('p-start', 'start the desktop', 'vm.start', 'vm100', '{}', 'Start', 'u-owner', 7, NULL, 0);
INSERT INTO swifty_misses (at, user_id, phrase, outcome) VALUES (9, 'u-owner', 'make it quieter', 'learned');
"#;

    /// The audit rows of that house, each naming a device that moves.
    const V3_AUDIT: [(&str, &str); 6] = [
        ("light.add", "hue-3"),
        ("light.power", "wiz-a8bb50aaaaaa"),
        ("light.hue", "hue-3"),
        ("machine.approve", "nas2-vm200"),
        ("vm.start", "vm100"),
        ("sonos.volume", "sonos"),
    ];

    fn version_3_house(path: &Path, key: &AuditKey) {
        create_version(path, 3).unwrap();
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(V3_HOUSE).unwrap();
        for (n, (action, target)) in V3_AUDIT.into_iter().enumerate() {
            audit::append(&conn, &event(n as i64, action, target), Some(key)).unwrap();
        }
    }

    type Rows = Vec<Vec<Cell>>;
    /// Each table's rows by name, and each table, index and trigger with its definition.
    type Snapshot = (BTreeMap<String, Rows>, Vec<(String, Option<String>)>);
    /// (target, host, name, vmid, status, approved at)
    type MachineLine<'a> = (&'a str, &'a str, &'a str, u32, &'a str, Option<i64>);

    fn dump(conn: &Connection, table: &str) -> rusqlite::Result<Rows> {
        let mut statement = conn.prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))?;
        let columns = statement.column_count();
        let rows = statement.query_map([], |row| (0..columns).map(|i| row.get::<_, Cell>(i)).collect())?;
        rows.collect()
    }

    /// Every table's rows, and every table's, index's and trigger's definition.
    fn snapshot(conn: &Connection) -> rusqlite::Result<Snapshot> {
        let names: Vec<String> = {
            let mut statement = conn.prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )?;
            let rows = statement.query_map([], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut tables = BTreeMap::new();
        for name in names {
            let rows = dump(conn, &name)?;
            tables.insert(name, rows);
        }
        let mut statement =
            conn.prepare("SELECT name, sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY name")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok((tables, rows.collect::<rusqlite::Result<_>>()?))
    }

    fn text(cell: &Cell) -> &str {
        match cell {
            Cell::Text(text) => text,
            other => panic!("not text: {other:?}"),
        }
    }

    /// (target, instance, class, name, attrs, status, first seen, adopted at, adopted by)
    type DeviceRow = (String, String, String, String, Value, String, i64, Option<i64>, Option<String>);

    fn device_rows(conn: &Connection) -> rusqlite::Result<Vec<DeviceRow>> {
        let mut statement = conn.prepare(
            "SELECT target, instance_id, local_id, class, name, attrs, origin, status, first_seen, adopted_at, adopted_by
             FROM devices ORDER BY target",
        )?;
        let rows = statement.query_map([], |row| {
            let target: String = row.get(0)?;
            assert_eq!(row.get::<_, String>(2)?, target, "the local id of a legacy device is its target");
            assert_eq!(row.get::<_, String>(6)?, "adopted", "{target}");
            let attrs: String = row.get(5)?;
            Ok((
                target,
                row.get(1)?,
                row.get(3)?,
                row.get(4)?,
                serde_json::from_str(&attrs).unwrap(),
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
            ))
        })?;
        rows.collect()
    }

    fn device(
        target: &str,
        instance: &str,
        class: &str,
        name: &str,
        attrs: Value,
        status: &str,
        (first_seen, adopted_at, adopted_by): (i64, Option<i64>, Option<&str>),
    ) -> DeviceRow {
        (
            target.to_string(),
            instance.to_string(),
            class.to_string(),
            name.to_string(),
            attrs,
            status.to_string(),
            first_seen,
            adopted_at,
            adopted_by.map(str::to_string),
        )
    }

    #[test]
    fn a_version_3_house_moves_into_the_device_store_with_every_target_and_its_audit_chain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("home.db");
        let key = audit_key(dir.path());
        version_3_house(&path, &key);

        let (tables_before, schema_before, report_before) = {
            let conn = Connection::open(&path).unwrap();
            let (tables, schema) = snapshot(&conn).unwrap();
            (tables, schema, audit::verify_with(&conn, keyed(&key)).unwrap())
        };
        assert!(report_before.ok && report_before.keyed, "{report_before:?}");
        assert_eq!(report_before.rows, V3_AUDIT.len() as i64);

        // The old database can be checked as it is, before anything is upgraded.
        let inspected = Store::open_for_inspection(&path).unwrap();
        assert_eq!(inspected.with(|conn| audit::verify_with(conn, keyed(&key))).unwrap(), report_before);
        assert_eq!(inspected.with(user_version).unwrap(), 3, "inspecting changes nothing");
        drop(inspected);

        let store = Store::open(&path).unwrap();
        store
            .with(|conn| -> rusqlite::Result<()> {
                assert_eq!(user_version(conn)?, SCHEMA_VERSION);

                // The audit log is untouched: the same rows, head, checkpoint and signature.
                let report = audit::verify_with(conn, keyed(&key))?;
                assert_eq!(report, report_before);

                // Every table that stays keeps every row and its definition, except the
                // phrases learned under a renamed id.
                let (tables, schema) = snapshot(conn)?;
                for (name, rows) in &tables_before {
                    if DROPPED.contains(&name.as_str()) {
                        assert!(!tables.contains_key(name), "{name} is still there");
                    } else if name != "learned_phrases" {
                        assert_eq!(&tables[name], rows, "{name} changed");
                    }
                }
                let kept: Vec<_> = schema_before
                    .iter()
                    .filter(|(name, _)| !DROPPED.iter().any(|dropped| name.contains(dropped)))
                    .collect();
                let now: Vec<_> = schema
                    .iter()
                    .filter(|(name, _)| !ADDED.contains(&name.as_str()) && !ADDED_BY_V5.contains(&name.as_str()))
                    .collect();
                assert_eq!(now, kept, "only the device store's tables were added and removed");
                for table in ADDED_BY_V5 {
                    assert!(tables.contains_key(table), "{table} comes with version 5");
                }

                // A phrase learned under an old id now reads the new one, and nothing
                // else about any phrase changed.
                let renamed: Rows = tables_before["learned_phrases"]
                    .iter()
                    .map(|row| {
                        let mut row = row.clone();
                        if let Some((_, new)) = RENAMED_ACTIONS.iter().find(|(old, _)| *old == text(&row[2])) {
                            row[2] = Cell::Text(new.to_string());
                        }
                        row
                    })
                    .collect();
                assert_eq!(tables["learned_phrases"], renamed);
                let actions: Vec<(String, String)> = {
                    let mut statement = conn.prepare("SELECT id, action FROM learned_phrases ORDER BY id")?;
                    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
                    rows.collect::<rusqlite::Result<_>>()?
                };
                let expected = [("p-lamp", "light.power"), ("p-start", "vm.start"), ("p-volume", "speaker.volume")];
                assert_eq!(actions, expected.map(|(id, action)| (id.to_string(), action.to_string())));

                // Every light and machine is a device under the target it always had.
                let primary = |vmid: i64| json!({ "host": "primary", "vmid": vmid });
                let seed = (0, Some(0), Some("seed"));
                assert_eq!(
                    device_rows(conn)?,
                    [
                        device(
                            "hue-3",
                            HUE,
                            "light",
                            "Strip",
                            json!({ "mac": "hue-3", "address": "hue-bridge", "light": "3" }),
                            "active",
                            (11, Some(11), Some("u-owner")),
                        ),
                        device(
                            "nas2-vm200",
                            PROXMOX,
                            "vm",
                            "store",
                            json!({ "host": "nas2", "vmid": 200 }),
                            "active",
                            (41, Some(42), Some("u-owner")),
                        ),
                        device("vm100", PROXMOX, "vm", "alpha", primary(100), "active", seed),
                        device("vm101", PROXMOX, "vm", "bravo", primary(101), "active", seed),
                        device("vm102", PROXMOX, "vm", "charlie", primary(102), "active", seed),
                        device("vm103", PROXMOX, "vm", "delta", primary(103), "active", seed),
                        device("vm150", PROXMOX, "vm", "new-vm", primary(150), "pending", (40, None, None)),
                        device(
                            "wiz-a8bb50aaaaaa",
                            WIZ,
                            "light",
                            "Hall lamp",
                            json!({ "mac": "a8bb50aaaaaa", "address": "192.0.2.21:38899" }),
                            "active",
                            (10, Some(10), Some("u-owner")),
                        ),
                    ]
                );

                // What discovery found is a candidate of the instance that would find it now.
                let mut statement = conn.prepare(
                    "SELECT instance_id, local_id, decl, seen_at FROM device_candidates ORDER BY instance_id, local_id",
                )?;
                let candidates: Vec<(String, String, DeviceDecl, i64)> = statement
                    .query_map([], |row| {
                        let decl: String = row.get(2)?;
                        Ok((row.get(0)?, row.get(1)?, serde_json::from_str(&decl).unwrap(), row.get(3)?))
                    })?
                    .collect::<rusqlite::Result<_>>()?;
                let found: Vec<(&str, &str, &str, i64)> = candidates
                    .iter()
                    .map(|(instance, local_id, decl, seen_at)| {
                        assert_eq!(&decl.local_id, local_id);
                        assert_eq!(decl.target_hint.as_deref(), Some(local_id.as_str()));
                        assert_eq!(decl.class, DeviceClass::Light);
                        (instance.as_str(), local_id.as_str(), decl.attrs["address"].as_str().unwrap(), *seen_at)
                    })
                    .collect();
                assert_eq!(
                    found,
                    [
                        (HUE, "hue-3", BRIDGE_ADDRESS, 22),
                        (HUE, "hue-4", BRIDGE_ADDRESS, 23),
                        (WIZ, "wiz-a8bb50aaaaaa", "192.0.2.21:38899", 20),
                        (WIZ, "wiz-a8bb50bbbbbb", "192.0.2.22:38899", 21),
                    ]
                );
                assert_eq!(candidates[3].2.attrs, json!({ "mac": "a8bb50bbbbbb", "address": "192.0.2.22:38899" }));
                assert!(candidates[3].2.capabilities.contains(&Capability::Brightness { min: MIN_LIGHT_DIMMING }));

                // One placeholder instance for each legacy kind that has something, with no config yet.
                let mut statement = conn.prepare(
                    "SELECT id, plugin, config, config_version, enabled, created_by FROM plugin_instances ORDER BY id",
                )?;
                let instances: Vec<(String, String, String, i64, bool, String)> = statement
                    .query_map([], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))
                    })?
                    .collect::<rusqlite::Result<_>>()?;
                let placeholder =
                    |id: &str| (id.to_string(), id.to_string(), "{}".to_string(), 1, true, UPGRADE.to_string());
                assert_eq!(instances, [placeholder(HUE), placeholder(PROXMOX), placeholder(WIZ)]);

                assert_eq!(crate::scenes::all(conn)?, crate::scenes::house_scenes());
                assert!(crate::notes::all(conn)?.is_empty());
                Ok(())
            })
            .unwrap();

        // The hub reads the same machines and lights as before the upgrade.
        let machines = store.with(inventory::machines).unwrap();
        let listed: Vec<MachineLine<'_>> = machines
            .iter()
            .map(|m| (m.target.as_str(), m.host.as_str(), m.name.as_str(), m.vmid, m.status.as_str(), m.approved_at))
            .collect();
        assert_eq!(
            listed,
            [
                ("nas2-vm200", "nas2", "store", 200, "approved", Some(42)),
                ("vm100", "primary", "alpha", 100, "approved", Some(0)),
                ("vm101", "primary", "bravo", 101, "approved", Some(0)),
                ("vm102", "primary", "charlie", 102, "approved", Some(0)),
                ("vm103", "primary", "delta", 103, "approved", Some(0)),
                ("vm150", "primary", "new-vm", 150, "pending", None),
            ]
        );
        let lights = store.with(inventory::lights).unwrap();
        let listed: Vec<(&str, &str, &str, &str, i64)> = lights
            .iter()
            .map(|l| (l.target.as_str(), l.mac.as_str(), l.address.as_str(), l.name.as_str(), l.added_at))
            .collect();
        assert_eq!(
            listed,
            [
                ("wiz-a8bb50aaaaaa", "a8bb50aaaaaa", "192.0.2.21:38899", "Hall lamp", 10),
                ("hue-3", "hue-3", BRIDGE_ADDRESS, "Strip", 11),
            ]
        );
        let entries = store.with(inventory::stored_devices).unwrap();
        let entry = |target: &str| {
            let found = entries.iter().find(|entry| entry.target == target && !entry.pending);
            found.map(|e| (e.name.as_str(), e.instance.as_str()))
        };
        assert_eq!(entry("vm100"), Some(("Desktop", PROXMOX)), "a name someone gave stays");
        assert_eq!(entry("wiz-a8bb50aaaaaa"), Some(("Reading lamp", WIZ)));
        assert_eq!(entry("hue-3"), Some(("Strip", HUE)));
        assert_eq!(entry("nas2-vm200"), Some(("store · VM 200", PROXMOX)));
        assert_eq!(entry("vm150"), None, "a machine waiting for approval is not listed");

        // What was found before the upgrade can still be added and accepted, and a
        // waiting machine still approved.
        store
            .tx(|tx| -> Result<(), crate::error::ApiError> {
                let porch = inventory::add_light(tx, 50, "a8bb50bbbbbb", "Porch", "u-owner")?;
                assert_eq!((porch.target.as_str(), porch.address.as_str()), ("wiz-a8bb50bbbbbb", "192.0.2.22:38899"));
                let strip = inventory::add_light(tx, 51, "hue-4", "Shelf", "u-owner")?;
                assert_eq!((strip.target.as_str(), strip.address.as_str()), ("hue-4", BRIDGE_ADDRESS));
                assert_eq!(inventory::add_light(tx, 52, "hue-3", "Again", "u-owner").unwrap_err().code, "light_exists");
                assert_eq!(inventory::accept_found_address(tx, "wiz-a8bb50aaaaaa")?, "192.0.2.21:38899");
                assert!(inventory::approve(tx, 53, "vm150", "u-owner")?);
                assert_eq!(inventory::machine_status(tx, "vm150")?, Some(MachineStatus::Approved));
                Ok(())
            })
            .unwrap();

        // The chain goes on from the same head, and the signature with it.
        let checkpoint: audit::Checkpoint = report_before.checkpoint.parse().unwrap();
        store
            .with(|conn| -> rusqlite::Result<()> {
                audit::append(conn, &event(10, "light.add", "wiz-a8bb50bbbbbb"), Some(&key))?;
                audit::append(conn, &event(11, "machine.approve", "vm150"), Some(&key))?;
                let options = VerifyOptions { key: Some(&key), expected: Some(&checkpoint), allow_empty: false };
                let report = audit::verify_with(conn, options)?;
                assert!(report.ok, "{report:?}");
                assert_eq!(report.rows, report_before.rows + 2);
                Ok(())
            })
            .unwrap();

        // Opening it again changes nothing.
        let (tables, _) = store.with(snapshot).unwrap();
        drop(store);
        let again = Store::open(&path).unwrap();
        assert_eq!(again.with(snapshot).unwrap().0, tables);
    }

    #[test]
    fn a_new_database_has_no_machines_or_devices_and_has_the_house_scenes() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("new.db")).unwrap();
        store
            .with(|conn| -> rusqlite::Result<()> {
                for table in ["plugin_instances", "devices", "device_candidates", "device_notes"] {
                    let rows: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))?;
                    assert_eq!(rows, 0, "{table}");
                }
                assert!(inventory::machines(conn)?.is_empty());
                assert!(inventory::approved_targets(conn)?.is_empty());
                assert!(inventory::lights(conn)?.is_empty());
                assert_eq!(crate::scenes::all(conn)?, crate::scenes::house_scenes());
                Ok(())
            })
            .unwrap();
    }

    // ----- version 5: the electricity tariff and the energy ledger

    fn table_names(conn: &Connection) -> rusqlite::Result<Vec<String>> {
        let mut statement = conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect()
    }

    #[test]
    fn version_five_only_adds_the_tariff_and_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v4.db");
        create_version(&path, 4).unwrap();
        let before = {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "INSERT INTO meta (key, value) VALUES ('owner_id', 'owner-handle');
                 INSERT INTO device_notes (target, notes) VALUES ('node', '[\"keep me\"]');",
            )
            .unwrap();
            assert_eq!(user_version(&conn).unwrap(), 4);
            table_names(&conn).unwrap()
        };
        assert!(!before.iter().any(|name| name.starts_with("energy_")));

        let store = Store::open(&path).unwrap();
        store
            .with(|conn| -> rusqlite::Result<()> {
                assert_eq!(user_version(conn)?, 5);
                let after = table_names(conn)?;
                let added: Vec<&String> = after.iter().filter(|name| !before.contains(name)).collect();
                assert_eq!(added, ["energy_ledger", "energy_tariff"], "nothing else is added");
                assert!(before.iter().all(|name| after.contains(name)), "and nothing is dropped");
                let notes: String =
                    conn.query_row("SELECT notes FROM device_notes WHERE target = 'node'", [], |row| row.get(0))?;
                assert_eq!(notes, "[\"keep me\"]");
                for table in ["energy_tariff", "energy_ledger"] {
                    let rows: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))?;
                    assert_eq!(rows, 0, "{table} starts empty: no tariff, nothing counted");
                }
                Ok(())
            })
            .unwrap();
        // Opening it again changes nothing.
        drop(store);
        Store::open(&path).unwrap();
    }

    #[test]
    fn an_old_database_with_no_devices_gets_no_instance() {
        for version in 1..SCHEMA_VERSION {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("old.db");
            create_version(&path, version).unwrap();
            let store = Store::open(&path).unwrap();
            let instances: i64 = store
                .with(|conn| conn.query_row("SELECT COUNT(*) FROM plugin_instances", [], |row| row.get(0)))
                .unwrap();
            assert_eq!(instances, 0, "from version {version}");
            assert_eq!(store.with(crate::scenes::all).unwrap().len(), 4, "from version {version}");
        }
        assert!(create_version(Path::new("unused.db"), 0).is_err());
        assert!(create_version(Path::new("unused.db"), SCHEMA_VERSION + 1).is_err());
    }

    #[test]
    fn processes_opening_an_old_database_at_once_upgrade_it_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("home.db");
        let key = audit_key(dir.path());
        version_3_house(&path, &key);
        let start = std::sync::Barrier::new(4);
        std::thread::scope(|scope| {
            let openers: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        start.wait();
                        Store::open(&path).map(drop)
                    })
                })
                .collect();
            for opener in openers {
                opener.join().unwrap().unwrap();
            }
        });
        let store = Store::open(&path).unwrap();
        let count = |table: &str| -> i64 {
            store.with(|conn| conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))).unwrap()
        };
        assert_eq!(
            (count("devices"), count("device_candidates"), count("plugin_instances"), count("scenes")),
            (8, 4, 3, 4)
        );
        assert!(store.with(|conn| audit::verify_with(conn, keyed(&key))).unwrap().ok);
    }

    #[test]
    fn the_renamed_ids_are_the_ones_the_catalog_still_accepts() {
        for (old, new) in RENAMED_ACTIONS {
            let def = crate::catalog::find(old).unwrap_or_else(|| panic!("{old} is no longer accepted"));
            assert_eq!(def.id, new);
            assert_eq!(crate::catalog::find(new).map(|def| def.id), Some(new));
        }
    }

    #[test]
    fn the_bridge_address_is_frozen_and_marks_a_light_on_the_bridge() {
        // What version 3 stored; the hub still marks a light behind a bridge with it.
        assert_eq!(BRIDGE_ADDRESS, "hue-bridge");
        let (instance, decl) = legacy_light_candidate("hue-12", BRIDGE_ADDRESS, Some("Desk"));
        assert_eq!((instance, decl.local_id.as_str(), decl.name.as_str()), (HUE, "hue-12", "Desk"));
        assert_eq!(decl.attrs["light"], json!("12"));
        let (instance, decl) = legacy_light_candidate("a8bb50aabbcc", "192.0.2.21:38899", None);
        assert_eq!((instance, decl.local_id.as_str(), decl.name.as_str()), (WIZ, "wiz-a8bb50aabbcc", "WiZ light bbcc"));
    }

    #[test]
    fn a_database_from_before_the_audit_log_had_people_is_not_inspected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        first_version_database(&path);
        let refused = Store::open_for_inspection(&path).err().unwrap();
        assert!(refused.contains("not a database this service has run on"), "{refused}");
    }
}
