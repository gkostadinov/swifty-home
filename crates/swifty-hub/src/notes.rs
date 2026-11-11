//! The owner's notes on each device: free lines, such as what goes offline while a
//! VM is down. They are shown where an action on the device is confirmed, and come
//! from the app or from the site file. Stored in `device_notes` by target, as a
//! JSON array of lines.
//!
//! Notes are kept by target, like names and rooms, so they need no device row of
//! their own and survive the device being removed and found again.

use std::collections::BTreeMap;

use rusqlite::{params, Connection, OptionalExtension};

/// The most lines one device can have.
pub const MAX_LINES: usize = 8;
/// The longest line, in characters.
pub const MAX_LINE: usize = 200;

/// What is wrong with these lines, if anything.
pub fn problem(lines: &[String]) -> Option<String> {
    if lines.len() > MAX_LINES {
        return Some(format!("a device has at most {MAX_LINES} notes"));
    }
    lines.iter().find_map(|line| {
        if line.trim().is_empty() || line.chars().count() > MAX_LINE {
            Some(format!("a note must be 1 to {MAX_LINE} characters"))
        } else if line.trim() != line || line.chars().any(char::is_control) {
            Some("a note must not have control characters or surrounding spaces".to_string())
        } else {
            None
        }
    })
}

fn parse(index: usize, text: &str) -> rusqlite::Result<Vec<String>> {
    serde_json::from_str(text)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(e)))
}

/// The notes on a device, oldest first. None is an empty list.
pub fn get(conn: &Connection, target: &str) -> rusqlite::Result<Vec<String>> {
    let text: Option<String> = conn
        .query_row("SELECT notes FROM device_notes WHERE target = ?1", params![target], |row| row.get(0))
        .optional()?;
    text.map_or_else(|| Ok(Vec::new()), |text| parse(0, &text))
}

/// Every device that has notes, by target.
pub fn all(conn: &Connection) -> rusqlite::Result<BTreeMap<String, Vec<String>>> {
    let mut statement = conn.prepare("SELECT target, notes FROM device_notes ORDER BY target ASC")?;
    let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
    rows.map(|row| {
        let (target, text) = row?;
        Ok((target, parse(1, &text)?))
    })
    .collect()
}

/// Replaces a device's notes. No lines removes them. Lines that fail [`problem`]
/// are refused before anything is written.
pub fn set(conn: &Connection, target: &str, lines: &[String]) -> rusqlite::Result<()> {
    if let Some(problem) = problem(lines) {
        return Err(rusqlite::Error::ToSqlConversionFailure(problem.into()));
    }
    if lines.is_empty() {
        remove(conn, target)?;
        return Ok(());
    }
    let text = serde_json::to_string(lines).map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
    conn.execute(
        "INSERT INTO device_notes (target, notes) VALUES (?1, ?2)
         ON CONFLICT(target) DO UPDATE SET notes = excluded.notes",
        params![target, text],
    )?;
    Ok(())
}

/// Removes a device's notes, and says whether it had any.
pub fn remove(conn: &Connection, target: &str) -> rusqlite::Result<bool> {
    Ok(conn.execute("DELETE FROM device_notes WHERE target = ?1", params![target])? == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("notes.db")).unwrap();
        (dir, store)
    }

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|line| line.to_string()).collect()
    }

    #[test]
    fn notes_are_kept_by_target_in_their_order() {
        let (_dir, store) = store();
        assert!(store.with(|conn| get(conn, "vm101")).unwrap().is_empty(), "no notes is an empty list");
        let written = lines(&["The shared folders go offline while this is down.", "Backups run at night."]);
        store.with(|conn| set(conn, "vm101", &written)).unwrap();
        store.with(|conn| set(conn, "host", &lines(&["Everything runs on this."]))).unwrap();
        assert_eq!(store.with(|conn| get(conn, "vm101")).unwrap(), written);
        let every = store.with(all).unwrap();
        assert_eq!(every.keys().collect::<Vec<_>>(), ["host", "vm101"]);

        store.with(|conn| set(conn, "vm101", &lines(&["Only this now."]))).unwrap();
        assert_eq!(store.with(|conn| get(conn, "vm101")).unwrap(), lines(&["Only this now."]));
    }

    #[test]
    fn no_lines_removes_the_notes() {
        let (_dir, store) = store();
        store.with(|conn| set(conn, "tv", &lines(&["Mounted on the wall."]))).unwrap();
        store.with(|conn| set(conn, "tv", &[])).unwrap();
        assert!(store.with(all).unwrap().is_empty());
        store.with(|conn| set(conn, "tv", &lines(&["Back."]))).unwrap();
        assert!(store.with(|conn| remove(conn, "tv")).unwrap());
        assert!(!store.with(|conn| remove(conn, "tv")).unwrap());
    }

    #[test]
    fn a_wrong_line_is_refused_and_nothing_is_written() {
        let (_dir, store) = store();
        store.with(|conn| set(conn, "vm101", &lines(&["Kept."]))).unwrap();
        let long = "x".repeat(MAX_LINE + 1);
        for wrong in [
            lines(&[""]),
            lines(&["   "]),
            lines(&[" padded"]),
            lines(&["two\nlines"]),
            lines(&["a bell\u{7}"]),
            lines(&[long.as_str()]),
            vec!["One more than allowed.".to_string(); MAX_LINES + 1],
        ] {
            assert!(problem(&wrong).is_some(), "{wrong:?}");
            assert!(store.with(|conn| set(conn, "vm101", &wrong)).is_err(), "{wrong:?}");
        }
        assert_eq!(store.with(|conn| get(conn, "vm101")).unwrap(), lines(&["Kept."]));
        let longest = vec!["x".repeat(MAX_LINE); MAX_LINES];
        assert!(problem(&longest).is_none(), "the limits themselves are allowed");
    }

    #[test]
    fn a_damaged_row_is_an_error_and_not_an_empty_list() {
        let (_dir, store) = store();
        store.with(|conn| conn.execute("INSERT INTO device_notes (target, notes) VALUES ('tv', '{')", [])).unwrap();
        assert!(store.with(|conn| get(conn, "tv")).is_err());
        assert!(store.with(all).is_err());
    }
}
