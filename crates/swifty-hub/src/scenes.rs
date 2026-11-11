//! House scenes: one level for every light, the lights that differ, and what else
//! a scene does. Stored in `scenes`, one row each, by id; the id is also the
//! scene's target.
//!
//! A new install has the four house scenes with uniform levels. Levels for single
//! lights arrive as overrides, from the app or from the site file. This module only
//! stores scenes: planning what a scene does is the catalog's.

use std::collections::BTreeSet;

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The highest hue, in degrees.
pub const MAX_HUE: u16 = 359;
/// The most lights that can have a level of their own in one scene.
pub const MAX_OVERRIDES: usize = 256;
const MAX_NAME: usize = 64;

/// One light's own level in a scene.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SceneOverride {
    pub target: String,
    /// Brightness in percent. 0 means off.
    pub percent: u8,
    pub hue: u16,
}

/// A scene as it is stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Scene {
    /// Its id, which is also its target.
    pub id: String,
    pub name: String,
    /// Where it is listed, lowest first.
    pub position: i64,
    /// Brightness in percent for every light without an override. 0 means off.
    pub percent: u8,
    pub hue: u16,
    pub overrides: Vec<SceneOverride>,
    /// What else it does, as an object. Movie: `{"tv_input": "HDMI 1", "volume_cap": 30}`.
    pub extras: Value,
}

impl Scene {
    /// The brightness and hue this scene sets on a light: its own override, or the
    /// scene's level.
    pub fn level_for(&self, target: &str) -> (u8, u16) {
        self.overrides
            .iter()
            .find(|level| level.target == target)
            .map_or((self.percent, self.hue), |level| (level.percent, level.hue))
    }

    /// What is wrong with it, if anything.
    pub fn problem(&self) -> Option<String> {
        if !is_scene_id(&self.id) {
            return Some("a scene id is 1 to 32 lower-case letters, digits and underscores".to_string());
        }
        if let Some(problem) = text_problem("name", &self.name, MAX_NAME) {
            return Some(problem);
        }
        if let Some(problem) = level_problem(self.percent, self.hue) {
            return Some(problem);
        }
        if self.overrides.len() > MAX_OVERRIDES {
            return Some(format!("a scene has at most {MAX_OVERRIDES} lights with a level of their own"));
        }
        let mut seen = BTreeSet::new();
        for level in &self.overrides {
            if !is_target(&level.target) {
                return Some(format!("{:?} is not a device id", level.target));
            }
            if !seen.insert(level.target.as_str()) {
                return Some(format!("{} has two levels", level.target));
            }
            if let Some(problem) = level_problem(level.percent, level.hue) {
                return Some(format!("{}: {problem}", level.target));
            }
        }
        if !self.extras.is_object() {
            return Some("extras must be an object".to_string());
        }
        None
    }
}

fn level_problem(percent: u8, hue: u16) -> Option<String> {
    if percent > 100 {
        return Some("percent is 0 to 100".to_string());
    }
    (hue > MAX_HUE).then(|| format!("hue is 0 to {MAX_HUE}"))
}

/// `[a-z0-9_]{1,32}`, as the house scenes' ids are.
fn is_scene_id(text: &str) -> bool {
    (1..=32).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// The shape every target has: `[a-z0-9][a-z0-9_-]{0,63}`.
fn is_target(text: &str) -> bool {
    let bytes = text.as_bytes();
    (1..=64).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

fn text_problem(field: &str, text: &str, max: usize) -> Option<String> {
    if text.trim().is_empty() || text.chars().count() > max {
        return Some(format!("{field} must be 1 to {max} characters"));
    }
    (text.trim() != text || text.chars().any(char::is_control))
        .then(|| format!("{field} must not have control characters or surrounding spaces"))
}

/// The four house scenes as a new install has them: every light at the same level.
/// Movie also turns the TV to its first input and caps the speaker's volume.
pub fn house_scenes() -> Vec<Scene> {
    let scene = |position: i64, id: &str, name: &str, percent: u8, hue: u16, extras: Value| Scene {
        id: id.to_string(),
        name: name.to_string(),
        position,
        percent,
        hue,
        overrides: Vec::new(),
        extras,
    };
    vec![
        scene(0, "evening", "Evening", 40, 34, json!({})),
        scene(1, "focus", "Focus", 90, 52, json!({})),
        scene(2, "all_off", "All off", 0, 0, json!({})),
        scene(3, "movie", "Movie", 10, 14, json!({ "tv_input": "HDMI 1", "volume_cap": 30 })),
    ]
}

/// Adds each house scene that is not there yet. A scene that exists keeps its levels.
pub fn insert_house_scenes(conn: &Connection) -> rusqlite::Result<()> {
    for scene in house_scenes() {
        conn.execute(
            "INSERT OR IGNORE INTO scenes (id, name, position, percent, hue, overrides, extras)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![scene.id, scene.name, scene.position, scene.percent, scene.hue, "[]", scene.extras.to_string()],
        )?;
    }
    Ok(())
}

const COLUMNS: &str = "id, name, position, percent, hue, overrides, extras";

fn json_column<T: serde::de::DeserializeOwned>(row: &Row<'_>, index: usize) -> rusqlite::Result<T> {
    let text: String = row.get(index)?;
    serde_json::from_str(&text)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(e)))
}

fn scene_from_row(row: &Row<'_>) -> rusqlite::Result<Scene> {
    Ok(Scene {
        id: row.get(0)?,
        name: row.get(1)?,
        position: row.get(2)?,
        percent: row.get(3)?,
        hue: row.get(4)?,
        overrides: json_column(row, 5)?,
        extras: json_column(row, 6)?,
    })
}

/// A scene that fails [`Scene::problem`] is refused before anything is written.
fn refuse_if_wrong(scene: &Scene) -> rusqlite::Result<()> {
    match scene.problem() {
        Some(problem) => Err(rusqlite::Error::ToSqlConversionFailure(problem.into())),
        None => Ok(()),
    }
}

/// Every scene, in the order they are listed.
pub fn all(conn: &Connection) -> rusqlite::Result<Vec<Scene>> {
    let mut statement = conn.prepare(&format!("SELECT {COLUMNS} FROM scenes ORDER BY position ASC, id ASC"))?;
    let rows = statement.query_map([], scene_from_row)?;
    rows.collect()
}

pub fn get(conn: &Connection, id: &str) -> rusqlite::Result<Option<Scene>> {
    conn.query_row(&format!("SELECT {COLUMNS} FROM scenes WHERE id = ?1"), params![id], scene_from_row).optional()
}

/// Stores a scene, replacing the one with the same id.
pub fn save(conn: &Connection, scene: &Scene) -> rusqlite::Result<()> {
    refuse_if_wrong(scene)?;
    let overrides =
        serde_json::to_string(&scene.overrides).map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
    conn.execute(
        "INSERT INTO scenes (id, name, position, percent, hue, overrides, extras) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, position = excluded.position, percent = excluded.percent,
             hue = excluded.hue, overrides = excluded.overrides, extras = excluded.extras",
        params![scene.id, scene.name, scene.position, scene.percent, scene.hue, overrides, scene.extras.to_string()],
    )?;
    Ok(())
}

/// Sets a stored scene's level and the lights that differ from it, leaving its
/// name, place and extras as they are. Says whether there was such a scene.
pub fn set_levels(
    conn: &Connection,
    id: &str,
    percent: u8,
    hue: u16,
    overrides: &[SceneOverride],
) -> rusqlite::Result<bool> {
    let Some(mut scene) = get(conn, id)? else {
        return Ok(false);
    };
    scene.percent = percent;
    scene.hue = hue;
    scene.overrides = overrides.to_vec();
    save(conn, &scene)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("scenes.db")).unwrap();
        (dir, store)
    }

    #[test]
    fn a_new_house_has_the_four_scenes_with_one_level_each() {
        let (_dir, store) = store();
        let scenes = store.with(all).unwrap();
        assert_eq!(scenes, house_scenes());
        let ids: Vec<&str> = scenes.iter().map(|scene| scene.id.as_str()).collect();
        assert_eq!(ids, ["evening", "focus", "all_off", "movie"]);
        assert!(scenes.iter().all(|scene| scene.overrides.is_empty() && scene.problem().is_none()));
        let movie = store.with(|conn| get(conn, "movie")).unwrap().unwrap();
        assert_eq!(movie.extras, json!({ "tv_input": "HDMI 1", "volume_cap": 30 }));
        assert_eq!(movie.level_for("bulb3"), (10, 14));
        // The house scenes' ids are words the hub keeps for itself.
        for scene in &scenes {
            assert!(crate::policy::SCENE_FAVOURITES.contains(&scene.id.as_str()), "{}", scene.id);
        }
    }

    #[test]
    fn adding_the_house_scenes_again_keeps_what_was_set() {
        let (_dir, store) = store();
        let levels = [SceneOverride { target: "bulb3".to_string(), percent: 0, hue: 0 }];
        assert!(store.with(|conn| set_levels(conn, "evening", 40, 34, &levels)).unwrap());
        store.with(insert_house_scenes).unwrap();
        let evening = store.with(|conn| get(conn, "evening")).unwrap().unwrap();
        assert_eq!(evening.overrides, levels);
        assert_eq!(evening.level_for("bulb3"), (0, 0));
        assert_eq!(evening.level_for("bulb1"), (40, 34));
    }

    #[test]
    fn levels_are_set_and_the_rest_of_the_scene_stays() {
        let (_dir, store) = store();
        let overrides = [
            SceneOverride { target: "bulb2".to_string(), percent: 35, hue: 30 },
            SceneOverride { target: "wiz-a8bb50aabbcc".to_string(), percent: 25, hue: 14 },
        ];
        assert!(store.with(|conn| set_levels(conn, "movie", 12, 20, &overrides)).unwrap());
        let movie = store.with(|conn| get(conn, "movie")).unwrap().unwrap();
        assert_eq!((movie.percent, movie.hue, movie.position, movie.name.as_str()), (12, 20, 3, "Movie"));
        assert_eq!(movie.extras["volume_cap"], json!(30));
        assert_eq!(movie.level_for("wiz-a8bb50aabbcc"), (25, 14));
        assert!(!store.with(|conn| set_levels(conn, "party", 1, 1, &[])).unwrap(), "there is no such scene");
    }

    #[test]
    fn a_scene_is_saved_in_its_place() {
        let (_dir, store) = store();
        let reading = Scene {
            id: "reading".to_string(),
            name: "Reading".to_string(),
            position: 1,
            percent: 70,
            hue: 40,
            overrides: Vec::new(),
            extras: json!({}),
        };
        store.with(|conn| save(conn, &reading)).unwrap();
        let ids: Vec<String> = store.with(all).unwrap().into_iter().map(|scene| scene.id).collect();
        assert_eq!(ids, ["evening", "focus", "reading", "all_off", "movie"]);
        store.with(|conn| save(conn, &Scene { name: "Read".to_string(), ..reading.clone() })).unwrap();
        assert_eq!(store.with(|conn| get(conn, "reading")).unwrap().unwrap().name, "Read");
    }

    #[test]
    fn a_wrong_scene_is_refused_and_nothing_is_written() {
        let (_dir, store) = store();
        let good = store.with(|conn| get(conn, "focus")).unwrap().unwrap();
        let level = |target: &str, percent, hue| SceneOverride { target: target.to_string(), percent, hue };
        let wrong = [
            Scene { id: "Focus".to_string(), ..good.clone() },
            Scene { id: String::new(), ..good.clone() },
            Scene { name: " Focus".to_string(), ..good.clone() },
            Scene { name: "Fo\ncus".to_string(), ..good.clone() },
            Scene { percent: 101, ..good.clone() },
            Scene { hue: 360, ..good.clone() },
            Scene { overrides: vec![level("bulb1", 101, 0)], ..good.clone() },
            Scene { overrides: vec![level("bulb1", 10, 400)], ..good.clone() },
            Scene { overrides: vec![level("Bulb 1", 10, 0)], ..good.clone() },
            Scene { overrides: vec![level("bulb1", 10, 0), level("bulb1", 20, 0)], ..good.clone() },
            Scene { overrides: vec![level("bulb1", 10, 0); MAX_OVERRIDES + 1], ..good.clone() },
            Scene { extras: json!([1]), ..good.clone() },
        ];
        for scene in wrong {
            assert!(scene.problem().is_some(), "{scene:?}");
            assert!(store.with(|conn| save(conn, &scene)).is_err(), "{scene:?}");
        }
        assert_eq!(store.with(all).unwrap(), house_scenes());
    }

    #[test]
    fn a_damaged_row_is_an_error_and_not_an_empty_scene() {
        let (_dir, store) = store();
        store.with(|conn| conn.execute("UPDATE scenes SET overrides = 'not json' WHERE id = 'focus'", [])).unwrap();
        assert!(store.with(|conn| get(conn, "focus")).is_err());
        assert!(store.with(|conn| get(conn, "evening")).unwrap().is_some());
    }
}
