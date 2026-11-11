//! The old database, read without changing a byte of it, and the copy in memory that
//! a migration is rehearsed on.
//!
//! Even a read-only SQLite connection to a database in write-ahead mode creates the
//! `-shm` and `-wal` files beside it when they are missing. The old directories must
//! stay exactly as they were, so the old file is opened as immutable: no lock, no
//! side file, nothing written. That is only right while nothing else writes it, so a
//! file with a write-ahead log or a rollback journal beside it is refused: the old
//! service is still running, or did not stop cleanly.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension};

use super::command::MigrateError;
use crate::store::SCHEMA_VERSION;

/// The oldest schema whose audit log can be checked (it has had its present shape since 2).
const OLDEST: i64 = 2;
/// The schema glimdock-home left. Fixed: a later schema is one of Swifty Home's own.
const NEWEST_OLD: i64 = 3;

/// A file SQLite keeps beside a database: `-wal`, `-shm` or `-journal`.
pub(super) fn beside(database: &Path, suffix: &str) -> PathBuf {
    let mut name = database.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// A `file:` URI for the path, with `immutable=1`.
fn immutable_uri(path: &Path) -> Result<String, MigrateError> {
    let absolute = std::path::absolute(path)
        .map_err(|error| MigrateError::new(format!("cannot find {}: {error}", path.display())))?;
    let text = absolute
        .to_str()
        .ok_or_else(|| MigrateError::new(format!("{} is not a path SQLite can be given", path.display())))?
        .replace('\\', "/");
    let text = if text.starts_with('/') { text } else { format!("/{text}") };
    let mut uri = String::from("file://");
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~' | b':') {
            uri.push(char::from(byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri.push_str("?mode=ro&immutable=1");
    Ok(uri)
}

/// Opens the old database for reading only, as immutable. Refuses one that another
/// process may still be writing.
pub(super) fn open_old(path: &Path) -> Result<Connection, MigrateError> {
    for (suffix, what) in [("-wal", "write-ahead log"), ("-journal", "rollback journal")] {
        let side = beside(path, suffix);
        if std::fs::metadata(&side).is_ok_and(|metadata| metadata.len() > 0) {
            return Err(MigrateError::new(format!(
                "the old database has a {what} beside it ({}): glimdock-home is still running, or did not stop cleanly",
                side.display()
            ))
            .hint(
                "Start glimdock-home and stop it again, so it finishes its writes, then run swifty migrate again:\n  \
                 sudo systemctl restart glimdock-home && sudo systemctl stop glimdock-home",
            ));
        }
    }
    let metadata = std::fs::metadata(path).map_err(|error| {
        MigrateError::new(format!(
            "cannot read the old database {}, which the old configuration names: {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(MigrateError::new(format!("{} is not a database file", path.display())));
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(immutable_uri(path)?, flags)
        .map_err(|error| MigrateError::new(format!("cannot open the old database {}: {error}", path.display())))?;
    conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .map_err(|error| MigrateError::new(format!("cannot read the old database {}: {error}", path.display())))?;
    Ok(conn)
}

/// Copies one database into another with SQLite's online backup.
pub(super) fn backup(source: &Connection, target: &mut Connection) -> rusqlite::Result<()> {
    let backup = rusqlite::backup::Backup::new(source, target)?;
    backup.run_to_completion(512, Duration::ZERO, None)
}

/// The old schema version, and the checks that it is a database glimdock-home ran on.
pub(super) fn old_version(conn: &Connection, path: &Path) -> Result<i64, MigrateError> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let shown = path.display();
    if version > NEWEST_OLD && version <= SCHEMA_VERSION {
        return Err(MigrateError::new(format!(
            "{shown} is already at schema {version}: it has been moved to Swifty Home before"
        )));
    }
    if version > SCHEMA_VERSION {
        return Err(MigrateError::new(format!("{shown} is at schema {version}, newer than this build knows")));
    }
    if !(OLDEST..=NEWEST_OLD).contains(&version) {
        return Err(MigrateError::new(format!(
            "{shown} is not a database glimdock-home has run on (schema {version})"
        )));
    }
    let owner: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = 'owner_id'", [], |row| row.get(0))
        .optional()
        .unwrap_or(None);
    if owner.is_none() {
        return Err(MigrateError::new(format!("{shown} is not a database glimdock-home has run on")));
    }
    Ok(version)
}

/// A copy of the old database in this process's memory. Its URI opens it from a
/// second connection, so the store can run on it; it lives until this is dropped.
pub(super) struct Rehearsal {
    holder: Connection,
    uri: String,
}

impl Rehearsal {
    pub(super) fn copy(source: &Connection) -> Result<Self, MigrateError> {
        let uri = format!("file:/swifty-migrate-{}?vfs=memdb", hex::encode(crate::crypto::random_bytes::<8>()));
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let holder = Connection::open_with_flags(&uri, flags)?;
        // Not the online backup: it copies the old file's header as it is, and a header
        // that says write-ahead log cannot be opened in memory. `VACUUM INTO` writes the
        // same tables, rows and schema version into a fresh database.
        source
            .execute("VACUUM INTO ?1", [&uri])
            .map_err(|error| MigrateError::new(format!("cannot copy the old database into memory: {error}")))?;
        Ok(Self { holder, uri })
    }

    pub(super) fn conn(&self) -> &Connection {
        &self.holder
    }

    /// What `Store::open` is given to work on the copy.
    pub(super) fn path(&self) -> &Path {
        Path::new(&self.uri)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> =
            std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn the_old_file_is_read_and_copied_without_a_byte_changing_beside_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old home.db");
        crate::store::create_version(&path, 3).unwrap();
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("PRAGMA journal_mode = WAL; INSERT INTO meta (key, value) VALUES ('owner_id', 'x');")
                .unwrap();
        }
        assert_eq!(names(dir.path()), ["old home.db"], "a clean close leaves no side file");
        let before = std::fs::read(&path).unwrap();

        let source = open_old(&path).unwrap();
        assert_eq!(old_version(&source, &path).unwrap(), 3);
        let rehearsal = Rehearsal::copy(&source).unwrap();
        let store = Store::open(rehearsal.path()).unwrap();
        let version: i64 = store.with(|conn| conn.query_row("PRAGMA user_version", [], |row| row.get(0))).unwrap();
        assert_eq!(version, SCHEMA_VERSION, "the copy is upgraded");
        drop(store);
        let again: i64 = rehearsal.conn().query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
        assert_eq!(again, SCHEMA_VERSION, "the store worked on the copy the holder keeps");
        let other = dir.path().join("copy.db");
        let mut target = Connection::open(&other).unwrap();
        backup(&source, &mut target).unwrap();
        drop((source, target));

        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(names(dir.path()), ["copy.db", "old home.db"]);
    }

    #[test]
    fn a_database_that_may_still_be_written_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("home.db");
        crate::store::create_version(&path, 3).unwrap();
        std::fs::write(beside(&path, "-wal"), b"frames").unwrap();
        let error = open_old(&path).unwrap_err();
        assert!(error.message.contains("still running"), "{error}");
        std::fs::write(beside(&path, "-wal"), b"").unwrap();
        open_old(&path).unwrap();
        assert!(open_old(&dir.path().join("missing.db")).unwrap_err().message.contains("cannot read"));
    }

    #[test]
    fn only_a_database_glimdock_home_left_is_taken() {
        let dir = tempfile::tempdir().unwrap();
        let new = dir.path().join("new.db");
        drop(Store::open(&new).unwrap());
        let conn = open_old(&new).unwrap();
        assert!(old_version(&conn, &new).unwrap_err().message.contains("moved to Swifty Home before"));
        // Nor is a database of an earlier Swifty Home version: its schema is above glimdock-home's.
        let earlier = dir.path().join("earlier.db");
        crate::store::create_version(&earlier, 4).unwrap();
        let conn = open_old(&earlier).unwrap();
        let refused = old_version(&conn, &earlier).unwrap_err().message;
        assert!(
            refused.contains("already at schema 4") && refused.contains("moved to Swifty Home before"),
            "{refused}"
        );
        let ownerless = dir.path().join("ownerless.db");
        crate::store::create_version(&ownerless, 3).unwrap();
        let conn = open_old(&ownerless).unwrap();
        assert!(old_version(&conn, &ownerless).unwrap_err().message.contains("not a database"));
    }
}
