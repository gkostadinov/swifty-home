//! Rooms: which devices are where, in what order, what each room's scenes do, and
//! the plans for actions that touch one room and nothing else.

use std::collections::HashSet;

use axum::http::StatusCode;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use swifty_plugin_api::{DeviceClass, Op};

use crate::catalog::{movie_tv, validate_label, within_reach, Step};
use crate::crypto::random_id;
use crate::error::ApiError;
use crate::policy::{may_see, Role};
use crate::runtime::index::DeviceIndex;
use crate::voice;

const MAX_NAME: usize = 40;
const MAX_ROOMS: i64 = 64;
const MAX_DEVICES: usize = 128;
const MAX_SCENE_SETTINGS: usize = 64;

/// What a scene does to one light.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LightSetting {
    pub target: String,
    /// Absent or zero means off.
    #[serde(default)]
    pub percent: u8,
    #[serde(default)]
    pub hue: u16,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct SceneView {
    pub id: String,
    pub name: String,
    pub settings: Vec<LightSetting>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct RoomDevice {
    pub target: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct RoomView {
    pub id: String,
    pub name: String,
    pub position: i64,
    pub devices: Vec<RoomDevice>,
    pub scenes: Vec<SceneView>,
}

fn unprocessable(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, code, message)
}

fn unknown_room() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "unknown_room", "No such room.")
}

fn spoken_form(name: &str) -> Result<String, ApiError> {
    validate_label("name", name, MAX_NAME).map_err(|e| unprocessable("invalid_name", e.0))?;
    let key = voice::normalize(name);
    if key.is_empty() {
        return Err(unprocessable("invalid_name", "A name needs at least one letter or digit."));
    }
    Ok(key)
}

fn reserved_name() -> ApiError {
    unprocessable("reserved_name", "That word already means something to voice. Pick another.")
}

/// A room's spoken form, checked so that saying it can only ever mean this room.
fn name_key(name: &str) -> Result<String, ApiError> {
    let key = spoken_form(name)?;
    if voice::is_reserved_phrase(&key) {
        return Err(reserved_name());
    }
    Ok(key)
}

/// A scene's spoken form. "Evening" is a fine name for a scene: it only comes into
/// play when the room is named, and there the room's own scene is what is meant.
fn scene_key(name: &str) -> Result<String, ApiError> {
    let key = spoken_form(name)?;
    if voice::is_reserved_phrase(&key) && !voice::GLOBAL_SCENES.contains(&key.as_str()) {
        return Err(reserved_name());
    }
    Ok(key)
}

fn name_taken() -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "name_taken", "That name is already in use.")
}

pub fn exists(conn: &Connection, id: &str) -> rusqlite::Result<bool> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM rooms WHERE id = ?1)", params![id], |row| row.get(0))
}

fn room_targets(conn: &Connection, id: &str) -> rusqlite::Result<Vec<String>> {
    let mut statement = conn.prepare("SELECT target FROM room_devices WHERE room_id = ?1 ORDER BY position ASC")?;
    let rows = statement.query_map(params![id], |row| row.get(0))?;
    rows.collect()
}

fn scenes_of(conn: &Connection, id: &str) -> Result<Vec<SceneView>, ApiError> {
    let mut statement =
        conn.prepare("SELECT id, name, settings FROM room_scenes WHERE room_id = ?1 ORDER BY name ASC")?;
    let rows = statement.query_map(params![id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, name, settings) = row?;
        out.push(SceneView { id, name, settings: serde_json::from_str(&settings)? });
    }
    Ok(out)
}

/// Every room, showing only the devices this person may see.
pub fn list(conn: &Connection, index: &DeviceIndex, role: Role) -> Result<Vec<RoomView>, ApiError> {
    let mut statement = conn.prepare("SELECT id, name, position FROM rooms ORDER BY position ASC, name ASC")?;
    let rows: Vec<(String, String, i64)> =
        statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for (id, name, position) in rows {
        let members = room_targets(conn, &id)?
            .into_iter()
            .filter(|target| may_see(index, role, target))
            .map(|target| {
                let name = index.get(&target).map_or_else(|| target.clone(), |device| device.name.clone());
                RoomDevice { target, name }
            })
            .collect();
        let scenes = scenes_of(conn, &id)?;
        out.push(RoomView { id, name, position, devices: members, scenes });
    }
    Ok(out)
}

pub fn get(conn: &Connection, index: &DeviceIndex, role: Role, id: &str) -> Result<RoomView, ApiError> {
    list(conn, index, role)?.into_iter().find(|room| room.id == id).ok_or_else(unknown_room)
}

pub fn create(conn: &Connection, now: i64, name: &str) -> Result<String, ApiError> {
    let key = name_key(name)?;
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM rooms", [], |row| row.get(0))?;
    if count >= MAX_ROOMS {
        return Err(ApiError::new(StatusCode::CONFLICT, "too_many_rooms", "There are too many rooms."));
    }
    let position: i64 = conn.query_row("SELECT COALESCE(MAX(position) + 1, 0) FROM rooms", [], |row| row.get(0))?;
    let id = random_id();
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO rooms (id, name, name_key, position, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, name, key, position, now],
    )?;
    if inserted != 1 {
        return Err(name_taken());
    }
    Ok(id)
}

pub fn rename(conn: &Connection, id: &str, name: &str) -> Result<(), ApiError> {
    let key = name_key(name)?;
    if !exists(conn, id)? {
        return Err(unknown_room());
    }
    let taken: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM rooms WHERE name_key = ?1 AND id != ?2)",
        params![key, id],
        |row| row.get(0),
    )?;
    if taken {
        return Err(name_taken());
    }
    conn.execute("UPDATE rooms SET name = ?1, name_key = ?2 WHERE id = ?3", params![name, key, id])?;
    Ok(())
}

pub fn delete(conn: &Connection, id: &str) -> Result<(), ApiError> {
    if conn.execute("DELETE FROM rooms WHERE id = ?1", params![id])? != 1 {
        return Err(unknown_room());
    }
    Ok(())
}

/// Sets the order of every room at once. Anything but the complete list is refused.
pub fn reorder(conn: &Connection, ids: &[String]) -> Result<(), ApiError> {
    let unique: HashSet<&String> = ids.iter().collect();
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM rooms", [], |row| row.get(0))?;
    if unique.len() != ids.len() || ids.len() as i64 != count {
        return Err(unprocessable("invalid_order", "Give every room exactly once."));
    }
    for (position, id) in ids.iter().enumerate() {
        if conn.execute("UPDATE rooms SET position = ?1 WHERE id = ?2", params![position as i64, id])? != 1 {
            return Err(unprocessable("invalid_order", "Give every room exactly once."));
        }
    }
    Ok(())
}

/// Replaces what is in a room, in this order. A device that was in another room
/// moves here.
pub fn set_devices(conn: &Connection, index: &DeviceIndex, id: &str, targets: &[String]) -> Result<(), ApiError> {
    if !exists(conn, id)? {
        return Err(unknown_room());
    }
    let unique: HashSet<&String> = targets.iter().collect();
    if unique.len() != targets.len() || targets.len() > MAX_DEVICES {
        return Err(unprocessable("invalid_devices", "List each device once."));
    }
    if !targets.iter().all(|target| index.get(target).is_some_and(|device| !device.pending)) {
        return Err(unprocessable("invalid_devices", "One of those is not a device here."));
    }
    conn.execute("DELETE FROM room_devices WHERE room_id = ?1", params![id])?;
    for (position, target) in targets.iter().enumerate() {
        conn.execute(
            "INSERT INTO room_devices (target, room_id, position) VALUES (?1, ?2, ?3)
             ON CONFLICT(target) DO UPDATE SET room_id = excluded.room_id, position = excluded.position",
            params![target, id, position as i64],
        )?;
    }
    Ok(())
}

/// The lights in a room, going by what the index says each device is.
fn lights_in(conn: &Connection, index: &DeviceIndex, id: &str) -> rusqlite::Result<Vec<String>> {
    let is_light =
        |target: &String| index.get(target).is_some_and(|device| !device.pending && device.class == DeviceClass::Light);
    Ok(room_targets(conn, id)?.into_iter().filter(is_light).collect())
}

/// The lights Movie dims: those in the room holding the TV it turns on, or else in
/// the room called Living room. `None` means neither exists, so there is no room to
/// keep it to.
pub fn movie_lights(conn: &Connection, index: &DeviceIndex) -> rusqlite::Result<Option<Vec<String>>> {
    let holding_tv: Option<String> = match movie_tv(index) {
        Some(tv) => conn
            .query_row("SELECT room_id FROM room_devices WHERE target = ?1", params![tv.target], |row| row.get(0))
            .optional()?,
        None => None,
    };
    let room = match holding_tv {
        Some(id) => Some(id),
        None => conn
            .query_row("SELECT id FROM rooms WHERE name_key = ?1", params![voice::normalize("Living room")], |row| {
                row.get(0)
            })
            .optional()?,
    };
    room.map(|id: String| lights_in(conn, index, &id)).transpose()
}

/// Saves a scene. Each setting has to be for a light in this room.
pub fn save_scene(
    conn: &Connection,
    index: &DeviceIndex,
    id: &str,
    name: &str,
    settings: &[LightSetting],
) -> Result<String, ApiError> {
    if !exists(conn, id)? {
        return Err(unknown_room());
    }
    let key = scene_key(name)?;
    if settings.is_empty() || settings.len() > MAX_SCENE_SETTINGS {
        return Err(unprocessable("invalid_scene", "A scene needs at least one light."));
    }
    let lights = lights_in(conn, index, id)?;
    let mut seen = HashSet::new();
    for setting in settings {
        if !lights.contains(&setting.target) {
            return Err(unprocessable("invalid_scene", "A scene can only set lights in its own room."));
        }
        if !seen.insert(&setting.target) {
            return Err(unprocessable("invalid_scene", "List each light once."));
        }
        if setting.percent > 100 || setting.hue > 359 {
            return Err(unprocessable("invalid_scene", "percent is 0 to 100 and hue is 0 to 359."));
        }
    }
    let scene = random_id();
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO room_scenes (id, room_id, name, name_key, settings) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![scene, id, name, key, serde_json::to_string(settings)?],
    )?;
    if inserted != 1 {
        return Err(name_taken());
    }
    Ok(scene)
}

pub fn delete_scene(conn: &Connection, id: &str, scene: &str) -> Result<(), ApiError> {
    if conn.execute("DELETE FROM room_scenes WHERE id = ?1 AND room_id = ?2", params![scene, id])? != 1 {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_scene", "No such scene."));
    }
    Ok(())
}

fn step_for(target: &str, op: Op) -> Step {
    Step { target: target.to_string(), op }
}

fn no_lights() -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "room_has_no_lights", "This room has no lights.")
}

/// The operations for a room-level action. Only lights that are in this room right
/// now are touched, so nothing outside the room can be affected.
pub fn plan(
    conn: &Connection,
    index: &DeviceIndex,
    action: &str,
    id: &str,
    params: &Value,
) -> Result<Vec<Step>, ApiError> {
    let lights = lights_in(conn, index, id)?;
    if lights.is_empty() {
        return Err(no_lights());
    }
    let invalid = |name: &str| unprocessable("invalid_params", format!("{name} is required"));
    match action {
        "room.power" => {
            let on = params.get("on").and_then(Value::as_bool).ok_or_else(|| invalid("on"))?;
            Ok(lights.iter().map(|target| step_for(target, Op::LightPower { on })).collect())
        }
        "room.brightness" => {
            let percent = params.get("percent").and_then(Value::as_u64).ok_or_else(|| invalid("percent"))? as u8;
            let at = |target: &String| index.get(target).map_or(percent, |device| within_reach(device, percent));
            Ok(lights.iter().map(|target| step_for(target, Op::LightBrightness { percent: at(target) })).collect())
        }
        "room.scene" => {
            let name = params.get("scene").and_then(Value::as_str).ok_or_else(|| invalid("scene"))?;
            let stored: Option<String> = conn
                .query_row(
                    "SELECT settings FROM room_scenes WHERE room_id = ?1 AND name_key = ?2",
                    rusqlite::params![id, voice::normalize(name)],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(stored) = stored else {
                return Err(ApiError::new(
                    StatusCode::NOT_FOUND,
                    "unknown_scene",
                    "This room has no scene by that name.",
                ));
            };
            let settings: Vec<LightSetting> = serde_json::from_str(&stored)?;
            // A light that has since left the room is left alone.
            let steps: Vec<Step> = settings
                .iter()
                .filter(|setting| lights.contains(&setting.target))
                .map(|setting| {
                    let percent = index
                        .get(&setting.target)
                        .map_or(setting.percent, |device| within_reach(device, setting.percent));
                    step_for(
                        &setting.target,
                        if percent == 0 {
                            Op::LightPower { on: false }
                        } else {
                            Op::LightSet { percent, hue: setting.hue }
                        },
                    )
                })
                .collect();
            if steps.is_empty() {
                return Err(no_lights());
            }
            Ok(steps)
        }
        other => Err(ApiError::internal(format!("{other} has no room plan"))),
    }
}

/// What voice needs to know about rooms.
pub fn voice_index(conn: &Connection) -> Result<Vec<voice::SpokenRoom>, ApiError> {
    let mut statement = conn.prepare("SELECT id, name, name_key FROM rooms ORDER BY position ASC")?;
    let rows: Vec<(String, String, String)> =
        statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for (id, name, key) in rows {
        let mut scenes_query = conn.prepare("SELECT name, name_key FROM room_scenes WHERE room_id = ?1")?;
        let scenes = scenes_query
            .query_map(params![id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<(String, String)>>>()?;
        out.push(voice::SpokenRoom { id, name, key, scenes });
    }
    Ok(out)
}
