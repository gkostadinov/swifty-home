//! What the hub has stored about its devices, over the `plugin_instances`,
//! `devices` and `device_candidates` tables: the configured instances, every device
//! by target (the names people gave them included), the devices an instance's
//! config names, machines waiting for approval, lights a discovery found, and
//! further Proxmox hosts.
//!
//! The store is the authority on targets: a device keeps the target it was stored
//! with. [`App::sync_directory`](crate::app::App::sync_directory) brings the devices
//! each instance's config names into the store, then loads the registry from it.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use axum::http::StatusCode;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use swifty_plugin_api::{DeviceClass, DeviceDecl, ExtraHost, SeenMachine};

use crate::config::IpNetwork;
use crate::error::ApiError;
use crate::runtime::index::assign_target;
use crate::runtime::{InstanceSpec, Origin, StoredDevice};
use crate::store::BRIDGE_ADDRESS;

/// The id a host plugin gives the host it was configured with, in a machine's `host`.
pub const PRIMARY_HOST: &str = "primary";
const PROXMOX_PORT: u16 = 8006;
const MAX_NAME: usize = 64;
/// The `meta` key that moves whenever an instance or its devices change, so that a
/// running hub reloads what another process (the CLI) wrote.
const INSTANCES_REV: &str = "instances_rev";

/// The rows of `devices` that are machines: a VM, with the host that listed it and
/// its number there.
const MACHINE_ROWS: &str =
    "class = 'vm' AND json_type(attrs, '$.host') = 'text' AND json_type(attrs, '$.vmid') = 'integer'";
/// The rows of `devices` that are lights added after a discovery.
const LIGHT_ROWS: &str = "class = 'light' AND origin = 'adopted'
    AND json_type(attrs, '$.mac') = 'text' AND json_type(attrs, '$.address') = 'text'";
/// The candidates that are lights a discovery found.
const LIGHT_CANDIDATES: &str = "json_extract(decl, '$.class') = 'light' AND json_type(decl, '$.attrs.mac') = 'text'";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineStatus {
    Approved,
    Pending,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Machine {
    pub target: String,
    pub host: String,
    pub vmid: u32,
    pub name: String,
    pub status: String,
    pub first_seen: i64,
    pub approved_at: Option<i64>,
    /// The host threads it is pinned to, where that is known. Nothing reports it
    /// yet, so it is always empty; the field stays so the machine list keeps its shape.
    pub cpus: Option<&'static str>,
}

fn conversion(error: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

// ----- instances and devices

/// Every configured instance, by id.
pub fn instance_specs(conn: &Connection) -> rusqlite::Result<Vec<InstanceSpec>> {
    let mut statement = conn
        .prepare("SELECT id, plugin, config, config_version, enabled, updated_at FROM plugin_instances ORDER BY id")?;
    let rows = statement.query_map([], |row| {
        let config: String = row.get(2)?;
        Ok(InstanceSpec {
            id: row.get(0)?,
            plugin: row.get(1)?,
            config: serde_json::from_str(&config).map_err(conversion)?,
            config_version: row.get(3)?,
            enabled: row.get::<_, i64>(4)? != 0,
            updated_at: row.get(5)?,
        })
    })?;
    rows.collect()
}

/// Whether an instance with this id is stored.
pub fn instance_exists(conn: &Connection, id: &str) -> rusqlite::Result<bool> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM plugin_instances WHERE id = ?1)", params![id], |row| row.get(0))
}

/// Marks a stored instance as changed in place, such as a secret replaced in the
/// same slot: its `updated_at` moves, always forward, so a running hub that loads
/// the instances again builds it anew. Move `instances_rev` in the same
/// transaction. Says whether there was such an instance.
pub fn touch_instance(conn: &Connection, id: &str, now: i64) -> rusqlite::Result<bool> {
    let changed = conn
        .execute("UPDATE plugin_instances SET updated_at = MAX(updated_at + 1, ?2) WHERE id = ?1", params![id, now])?;
    Ok(changed == 1)
}

fn class_of(text: &str) -> Option<DeviceClass> {
    serde_json::from_value(Value::String(text.to_string())).ok()
}

/// What a device's class is written as in the store.
fn class_text(class: DeviceClass) -> String {
    serde_json::to_value(class).ok().and_then(|value| value.as_str().map(str::to_string)).unwrap_or_default()
}

/// Every stored device, by target, under the name it is shown with: the one a
/// person gave it, or the stored one. A machine is shown with its number, as
/// `"<name> · VM <id>"`, unless a person named it.
pub fn stored_devices(conn: &Connection) -> rusqlite::Result<Vec<StoredDevice>> {
    let mut statement = conn.prepare(
        "SELECT d.target, d.instance_id, d.local_id, d.class,
                COALESCE(n.name, CASE WHEN d.class = 'vm' AND json_type(d.attrs, '$.vmid') = 'integer'
                                      THEN d.name || ' · VM ' || json_extract(d.attrs, '$.vmid')
                                      ELSE d.name END),
                d.attrs, d.origin, d.status
         FROM devices d LEFT JOIN device_names n ON n.target = d.target
         ORDER BY d.target",
    )?;
    let rows = statement.query_map([], |row| {
        let attrs: String = row.get(5)?;
        let origin: String = row.get(6)?;
        let status: String = row.get(7)?;
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            serde_json::from_str::<Value>(&attrs).map_err(conversion)?,
            origin,
            status,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (target, instance, local_id, class, name, attrs, origin, status) = row?;
        let Some(class) = class_of(&class) else {
            tracing::warn!(device = %target, %class, "a stored device has a class this build does not know");
            continue;
        };
        let origin = if origin == "config" { Origin::Config } else { Origin::Adopted };
        out.push(StoredDevice { target, instance, local_id, class, name, attrs, origin, pending: status == "pending" });
    }
    Ok(out)
}

/// Whether a device with this target is stored.
pub fn device_exists(conn: &Connection, target: &str) -> rusqlite::Result<bool> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM devices WHERE target = ?1)", params![target], |row| row.get(0))
}

/// A target no stored device has and the hub does not use itself.
fn free_target(conn: &Connection, instance: &str, decl: &DeviceDecl) -> rusqlite::Result<String> {
    let failed = std::cell::RefCell::new(None);
    let taken = |target: &str| match device_exists(conn, target) {
        Ok(exists) => exists,
        Err(error) => {
            failed.borrow_mut().get_or_insert(error);
            true
        }
    };
    let target = assign_target(&taken, instance, decl);
    match failed.into_inner() {
        Some(error) => Err(error),
        None => Ok(target),
    }
}

/// Brings the devices an instance's config names into the store: a new one is
/// stored under the target the hub gives it, one whose class, name or attributes
/// changed is updated, and one the config no longer names is removed and leaves its
/// room. Adopted and pending devices are never touched here. Returns the targets
/// that were added.
pub fn store_config_devices(
    conn: &Connection,
    now: i64,
    instance: &str,
    declared: &[DeviceDecl],
) -> rusqlite::Result<Vec<String>> {
    let stored: HashMap<String, (String, String)> = {
        let mut statement = conn.prepare("SELECT local_id, target, origin FROM devices WHERE instance_id = ?1")?;
        let rows = statement.query_map(params![instance], |row| Ok((row.get(0)?, (row.get(1)?, row.get(2)?))))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut added = Vec::new();
    for decl in declared {
        let attrs = decl.attrs.to_string();
        let name: String = decl.name.chars().filter(|c| !c.is_control()).take(MAX_NAME).collect();
        match stored.get(&decl.local_id) {
            // An adopted device the plugin recognises, declared back as it is.
            Some((_, origin)) if origin != "config" => {}
            Some((target, _)) => {
                conn.execute(
                    "UPDATE devices SET class = ?1, name = ?2, attrs = ?3
                     WHERE target = ?4 AND (class != ?1 OR name != ?2 OR attrs != ?3)",
                    params![class_text(decl.class), name, attrs, target],
                )?;
            }
            None => {
                let target = free_target(conn, instance, decl)?;
                conn.execute(
                    "INSERT INTO devices (target, instance_id, local_id, class, name, attrs, origin, status, first_seen)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'config', 'active', ?7)",
                    params![target, instance, decl.local_id, class_text(decl.class), name, attrs, now],
                )?;
                added.push(target);
            }
        }
    }
    for (local_id, (target, origin)) in &stored {
        if origin == "config" && !declared.iter().any(|decl| &decl.local_id == local_id) {
            conn.execute("DELETE FROM devices WHERE target = ?1", params![target])?;
            conn.execute("DELETE FROM room_devices WHERE target = ?1", params![target])?;
        }
    }
    Ok(added)
}

/// Where the instances and their devices stand: moves whenever another process
/// changed them, and a running hub then loads them again.
pub fn instances_rev(conn: &Connection) -> rusqlite::Result<i64> {
    let stored: Option<String> =
        conn.query_row("SELECT value FROM meta WHERE key = ?1", params![INSTANCES_REV], |row| row.get(0)).optional()?;
    Ok(stored.and_then(|text| text.parse().ok()).unwrap_or(0))
}

/// Tells a running hub that the instances or their devices changed. Whoever writes
/// them outside the hub (the CLI, a migration) calls it in the same transaction.
pub fn bump_instances_rev(conn: &Connection) -> rusqlite::Result<i64> {
    let next = instances_rev(conn)? + 1;
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![INSTANCES_REV, next.to_string()],
    )?;
    Ok(next)
}

// ----- machines

/// The host and VM id in a machine target: `vm101` is on the primary host and
/// `nas2-vm200` on a further one. Anything else is not a machine. Only the machine
/// list and the configuration read targets this way: what a device is comes from
/// the device index.
pub fn parse_machine(target: &str) -> Option<(Option<&str>, u32)> {
    let (host, rest) = match target.split_once('-') {
        Some((host, rest)) if is_host_id(host) => (Some(host), rest),
        Some(_) => return None,
        None => (None, target),
    };
    let digits = rest.strip_prefix("vm")?;
    if digits.is_empty() || digits.len() > 9 || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((host, digits.parse().ok()?))
}

/// A name for a further Proxmox host.
pub fn is_host_id(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 16
        && text.as_bytes()[0].is_ascii_lowercase()
        && text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// A machine's local id on the instance that lists it: `vm<id>` on its own host,
/// `<host>-vm<id>` on a further one.
pub fn machine_target(host: &str, vmid: u32) -> String {
    if host == PRIMARY_HOST {
        format!("vm{vmid}")
    } else {
        format!("{host}-vm{vmid}")
    }
}

fn machine_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Machine> {
    let target: String = row.get(0)?;
    Ok(Machine {
        cpus: None,
        target,
        host: row.get(1)?,
        vmid: row.get(2)?,
        name: row.get(3)?,
        status: row.get(4)?,
        first_seen: row.get(5)?,
        approved_at: row.get(6)?,
    })
}

/// The machine row as it always read: `approved` or `pending`, and when it was approved.
const MACHINE_COLUMNS: &str = "target, json_extract(attrs, '$.host'), json_extract(attrs, '$.vmid'), name,
    CASE status WHEN 'active' THEN 'approved' ELSE 'pending' END, first_seen, adopted_at";
const MACHINE_ORDER: &str = "json_extract(attrs, '$.host') ASC, json_extract(attrs, '$.vmid') ASC";

pub fn machines(conn: &Connection) -> rusqlite::Result<Vec<Machine>> {
    let mut statement =
        conn.prepare(&format!("SELECT {MACHINE_COLUMNS} FROM devices WHERE {MACHINE_ROWS} ORDER BY {MACHINE_ORDER}"))?;
    let rows = statement.query_map([], machine_from_row)?;
    rows.collect()
}

pub fn machine(conn: &Connection, target: &str) -> rusqlite::Result<Option<Machine>> {
    conn.query_row(
        &format!("SELECT {MACHINE_COLUMNS} FROM devices WHERE target = ?1 AND {MACHINE_ROWS}"),
        params![target],
        machine_from_row,
    )
    .optional()
}

pub fn machine_status(conn: &Connection, target: &str) -> rusqlite::Result<Option<MachineStatus>> {
    let status: Option<String> = conn
        .query_row(
            &format!("SELECT status FROM devices WHERE target = ?1 AND {MACHINE_ROWS}"),
            params![target],
            |row| row.get(0),
        )
        .optional()?;
    Ok(status.map(|status| if status == "active" { MachineStatus::Approved } else { MachineStatus::Pending }))
}

pub fn approved_targets(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut statement = conn.prepare(&format!(
        "SELECT target FROM devices WHERE {MACHINE_ROWS} AND status = 'active' ORDER BY {MACHINE_ORDER}"
    ))?;
    let rows = statement.query_map([], |row| row.get(0))?;
    rows.collect()
}

/// Lists anything new that an instance reported as pending, under that instance.
/// Nothing a scan does can approve a machine. Returns the targets it added.
pub fn record_seen(conn: &Connection, now: i64, instance: &str, seen: &[SeenMachine]) -> rusqlite::Result<Vec<String>> {
    let mut added = Vec::new();
    for machine in seen {
        let local_id = machine_target(&machine.host, machine.vmid);
        let name: String = machine.name.chars().filter(|c| !c.is_control()).take(MAX_NAME).collect();
        let name = if name.trim().is_empty() { format!("VM {}", machine.vmid) } else { name.trim().to_string() };
        let stored: Option<String> = conn
            .query_row(
                "SELECT target FROM devices WHERE instance_id = ?1 AND local_id = ?2",
                params![instance, local_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(target) = stored {
            conn.execute(
                &format!("UPDATE devices SET name = ?1 WHERE target = ?2 AND status = 'pending' AND {MACHINE_ROWS}"),
                params![name, target],
            )?;
            continue;
        }
        let decl = DeviceDecl {
            local_id: local_id.clone(),
            target_hint: Some(local_id.clone()),
            class: DeviceClass::Vm,
            name: name.clone(),
            capabilities: Vec::new(),
            attrs: json!({ "host": machine.host, "vmid": machine.vmid }),
        };
        let target = free_target(conn, instance, &decl)?;
        conn.execute(
            "INSERT INTO devices (target, instance_id, local_id, class, name, attrs, origin, status, first_seen)
             VALUES (?1, ?2, ?3, 'vm', ?4, ?5, 'adopted', 'pending', ?6)",
            params![target, instance, local_id, name, decl.attrs.to_string(), now],
        )?;
        added.push(target);
    }
    Ok(added)
}

pub fn approve(conn: &Connection, now: i64, target: &str, by: &str) -> rusqlite::Result<bool> {
    let changed = conn.execute(
        &format!(
            "UPDATE devices SET status = 'active', adopted_at = ?1, adopted_by = ?2
             WHERE target = ?3 AND status = 'pending' AND {MACHINE_ROWS}"
        ),
        params![now, by, target],
    )?;
    Ok(changed == 1)
}

pub fn remove_machine(conn: &Connection, target: &str) -> rusqlite::Result<bool> {
    let changed =
        conn.execute(&format!("DELETE FROM devices WHERE target = ?1 AND {MACHINE_ROWS}"), params![target])?;
    conn.execute("DELETE FROM room_devices WHERE target = ?1", params![target])?;
    Ok(changed == 1)
}

// ----- lights

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Light {
    pub target: String,
    pub mac: String,
    pub address: String,
    pub name: String,
    pub added_at: i64,
}

pub fn lights(conn: &Connection) -> rusqlite::Result<Vec<Light>> {
    let mut statement = conn.prepare(&format!(
        "SELECT target, json_extract(attrs, '$.mac'), json_extract(attrs, '$.address'), name,
                COALESCE(adopted_at, first_seen)
         FROM devices WHERE {LIGHT_ROWS} ORDER BY COALESCE(adopted_at, first_seen) ASC, target ASC"
    ))?;
    let rows = statement.query_map([], |row| {
        Ok(Light {
            target: row.get(0)?,
            mac: row.get(1)?,
            address: row.get(2)?,
            name: row.get(3)?,
            added_at: row.get(4)?,
        })
    })?;
    rows.collect()
}

/// Every light added after discovery, oldest first.
pub fn stored_light_targets(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    Ok(lights(conn)?.into_iter().map(|light| light.target).collect())
}

/// A light a discovery found, as it is kept: what the plugin reported, with the MAC
/// the light is asked for by (its own, or the id its bridge lists it under) and the
/// address it is at (its own, or its bridge's).
pub fn light_candidate(decl: &DeviceDecl) -> DeviceDecl {
    let mut decl = decl.clone();
    let mut attrs = match decl.attrs {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    if !attrs.get("mac").is_some_and(Value::is_string) {
        attrs.insert("mac".to_string(), json!(decl.local_id));
    }
    if !attrs.get("address").is_some_and(Value::is_string) {
        attrs.insert("address".to_string(), json!(BRIDGE_ADDRESS));
    }
    decl.attrs = Value::Object(attrs);
    decl
}

/// Remembers the lights an instance's discovery found, so that only a light that
/// is really there can be added. A light already added stays where it is, because
/// anyone on the network can answer a broadcast with its MAC; a new address is only
/// a candidate until the owner accepts it with [`accept_found_address`].
pub fn record_found(conn: &Connection, now: i64, instance: &str, found: &[DeviceDecl]) -> rusqlite::Result<()> {
    for decl in found.iter().filter(|decl| decl.class == DeviceClass::Light) {
        let decl = light_candidate(decl);
        let text = serde_json::to_string(&decl).map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        conn.execute(
            "INSERT INTO device_candidates (instance_id, local_id, decl, seen_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(instance_id, local_id) DO UPDATE SET decl = excluded.decl, seen_at = excluded.seen_at",
            params![instance, decl.local_id, text, now],
        )?;
    }
    Ok(())
}

/// The light a discovery last found with this MAC (or, behind a bridge, this id):
/// the instance it belongs to and what was found.
fn found_light(conn: &Connection, mac: &str) -> rusqlite::Result<Option<(String, DeviceDecl)>> {
    let row: Option<(String, String)> = conn
        .query_row(
            &format!(
                "SELECT instance_id, decl FROM device_candidates
                 WHERE {LIGHT_CANDIDATES} AND json_extract(decl, '$.attrs.mac') = ?1
                 ORDER BY seen_at DESC LIMIT 1"
            ),
            params![mac],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((instance, text)) = row else {
        return Ok(None);
    };
    let decl: DeviceDecl = serde_json::from_str(&text)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e)))?;
    Ok(decl.attrs.get("address").and_then(Value::as_str).is_some().then_some((instance, decl)))
}

fn address_of(decl: &DeviceDecl) -> String {
    decl.attrs.get("address").and_then(Value::as_str).unwrap_or_default().to_string()
}

/// Moves a light to the address the last discovery found for it, and returns that
/// address. It comes from the discovery and never from the request.
pub fn accept_found_address(conn: &Connection, target: &str) -> Result<String, ApiError> {
    let mac: Option<String> = conn
        .query_row(
            &format!("SELECT json_extract(attrs, '$.mac') FROM devices WHERE target = ?1 AND {LIGHT_ROWS}"),
            params![target],
            |row| row.get(0),
        )
        .optional()?;
    let Some(mac) = mac else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_device", "No such device."));
    };
    let Some((_, found)) = found_light(conn, &mac)? else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "light_not_discovered",
            "A discovery has not found that light. Run discovery first.",
        ));
    };
    let address = address_of(&found);
    conn.execute(
        "UPDATE devices SET attrs = json_set(attrs, '$.address', ?1) WHERE target = ?2",
        params![address, target],
    )?;
    Ok(address)
}

/// Adds a light that a discovery found, under the instance that found it and the
/// target the hub gives it. The address comes from that discovery and never from
/// the request, and the light keeps everything the discovery stored about it.
pub fn add_light(conn: &Connection, now: i64, mac: &str, name: &str, by: &str) -> Result<Light, ApiError> {
    let Some((instance, found)) = found_light(conn, mac)? else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "light_not_discovered",
            "That light was not found by a discovery. Run discovery first.",
        ));
    };
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM devices WHERE instance_id = ?1 AND local_id = ?2)",
        params![instance, found.local_id],
        |row| row.get(0),
    )?;
    if exists {
        return Err(ApiError::new(StatusCode::CONFLICT, "light_exists", "That light has already been added."));
    }
    let address = address_of(&found);
    let target = free_target(conn, &instance, &found)?;
    conn.execute(
        "INSERT INTO devices (target, instance_id, local_id, class, name, attrs, origin, status,
                              first_seen, adopted_at, adopted_by)
         VALUES (?1, ?2, ?3, 'light', ?4, ?5, 'adopted', 'active', ?6, ?6, ?7)",
        params![target, instance, found.local_id, name, found.attrs.to_string(), now, by],
    )?;
    Ok(Light { target, mac: mac.to_string(), address, name: name.to_string(), added_at: now })
}

// ----- device names

pub fn set_device_name(conn: &Connection, target: &str, name: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO device_names (target, name) VALUES (?1, ?2)
         ON CONFLICT(target) DO UPDATE SET name = excluded.name",
        params![target, name],
    )?;
    Ok(())
}

// ----- further Proxmox hosts

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct HostView {
    pub id: String,
    pub address: String,
    pub node: String,
    pub created_at: i64,
}

/// A further host's address: an IP literal and an optional port, inside a network
/// the configuration allows. There is no scheme, path, name or credential to get
/// wrong, and no name to resolve to something else later.
pub fn parse_host_address(networks: &[String], text: &str) -> Option<SocketAddr> {
    let parsed: SocketAddr =
        text.parse().ok().or_else(|| text.parse::<IpAddr>().ok().map(|ip| SocketAddr::new(ip, PROXMOX_PORT)))?;
    let ip = parsed.ip().to_canonical();
    let special = match ip {
        IpAddr::V4(v4) => v4.is_unspecified() || v4.is_multicast() || v4.is_broadcast() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_unspecified() || v6.is_multicast() || v6.is_unicast_link_local(),
    };
    let allowed = networks.iter().filter_map(|network| IpNetwork::parse(network)).any(|network| network.contains(ip));
    (parsed.port() != 0 && !special && allowed).then_some(SocketAddr::new(ip, parsed.port()))
}

/// The shape of a Proxmox token's secret, which Proxmox shows on its own line. It would
/// pass for a name, so it has to be told apart before anything keeps it.
fn looks_like_token_secret(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, length)| group.len() == length && group.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A bare file name: it cannot climb out of the secrets directory.
fn is_file_name(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text.as_bytes()[0].is_ascii_alphanumeric()
        && !text.contains("..")
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !looks_like_token_secret(text)
}

fn is_node_name(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 63
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !looks_like_token_secret(text)
}

fn text<'a>(params: &'a Value, name: &str) -> &'a str {
    params.get(name).and_then(Value::as_str).unwrap_or_default()
}

fn host_spec(secrets_dir: &Path, networks: &[String], fields: [&str; 5]) -> Result<ExtraHost, String> {
    let [id, address, node, token_file, ca_file] = fields;
    if !is_host_id(id) || id == PRIMARY_HOST {
        return Err("id must be a lower-case letter, then lower-case letters and digits".to_string());
    }
    let Some(address) = parse_host_address(networks, address) else {
        return Err("address must be an IP address, with a port if needed, on an allowed network".to_string());
    };
    if !is_node_name(node) {
        return Err("node must be a plain node name".to_string());
    }
    if !is_file_name(token_file) || !is_file_name(ca_file) {
        return Err("token_file and ca_file must be bare file names".to_string());
    }
    Ok(ExtraHost {
        id: id.to_string(),
        address,
        node: node.to_string(),
        token_file: secrets_dir.join(token_file),
        ca_file: secrets_dir.join(ca_file),
    })
}

/// Everything that is checked before a further host can exist: the networks the
/// configuration allows, and the directory the host integration keeps further
/// hosts' files in (its `host_files_dir`).
pub fn validate_host(networks: &[String], files_dir: Option<&Path>, params: &Value) -> Result<ExtraHost, ApiError> {
    let Some(files_dir) = files_dir else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "hosts_not_configured",
            "The host integration has no host_files_dir, so a further host cannot be added.",
        ));
    };
    let fields = ["id", "address", "node", "token_file", "ca_file"].map(|name| text(params, name));
    host_spec(files_dir, networks, fields)
        .map_err(|why| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_params", why))
}

/// Why this host cannot be added next to the ones already stored, if it cannot.
pub fn host_conflict(conn: &Connection, spec: &ExtraHost) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM proxmox_hosts WHERE id = ?1 OR address = ?2)",
        params![spec.id, spec.address.to_string()],
        |row| row.get(0),
    )
}

pub fn store_host(conn: &Connection, now: i64, spec: &ExtraHost, by: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO proxmox_hosts (id, address, node, token_file, ca_file, created_at, created_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            spec.id,
            spec.address.to_string(),
            spec.node,
            spec.token_file.to_string_lossy(),
            spec.ca_file.to_string_lossy(),
            now,
            by
        ],
    )?;
    Ok(())
}

pub fn hosts(conn: &Connection) -> rusqlite::Result<Vec<HostView>> {
    let mut statement =
        conn.prepare("SELECT id, address, node, created_at FROM proxmox_hosts ORDER BY created_at ASC")?;
    let rows = statement.query_map([], |row| {
        Ok(HostView { id: row.get(0)?, address: row.get(1)?, node: row.get(2)?, created_at: row.get(3)? })
    })?;
    rows.collect()
}

pub fn host_exists(conn: &Connection, id: &str) -> rusqlite::Result<bool> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM proxmox_hosts WHERE id = ?1)", params![id], |row| row.get(0))
}

/// Forgets a further host and every machine listed on it, and says how many there were.
pub fn remove_host(conn: &Connection, id: &str) -> rusqlite::Result<usize> {
    let targets: Vec<String> = {
        let mut statement = conn.prepare(&format!(
            "SELECT target FROM devices WHERE {MACHINE_ROWS} AND json_extract(attrs, '$.host') = ?1"
        ))?;
        let rows = statement.query_map(params![id], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for target in &targets {
        remove_machine(conn, target)?;
    }
    conn.execute("DELETE FROM proxmox_hosts WHERE id = ?1", params![id])?;
    Ok(targets.len())
}

/// A stored host, put through the same checks as a new one. Only the file's name is
/// taken from the store, so it is looked for in today's files directory.
fn host_from_store(networks: &[String], files_dir: Option<&Path>, fields: [&str; 5]) -> Result<ExtraHost, String> {
    let Some(files_dir) = files_dir else {
        return Err("the host integration has no host_files_dir".to_string());
    };
    let [id, address, node, token_file, ca_file] = fields;
    let name = |path: &str| Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or_default().to_string();
    host_spec(files_dir, networks, [id, address, node, &name(token_file), &name(ca_file)])
}

/// The stored hosts the configuration still allows, to load at startup. Narrowing
/// the networks has to take effect on hosts that were added before it was narrowed.
pub fn stored_hosts(
    conn: &Connection,
    networks: &[String],
    files_dir: Option<&Path>,
) -> rusqlite::Result<Vec<ExtraHost>> {
    let mut statement = conn.prepare("SELECT id, address, node, token_file, ca_file FROM proxmox_hosts")?;
    let rows = statement
        .query_map([], |row| Ok([row.get::<_, String>(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?]))?;
    let mut out = Vec::new();
    for row in rows {
        let [id, address, node, token_file, ca_file] = row?;
        match host_from_store(networks, files_dir, [&id, &address, &node, &token_file, &ca_file]) {
            Ok(host) => out.push(host),
            Err(reason) => {
                tracing::warn!(host = %id, %reason, "a stored Proxmox host is no longer allowed and was not loaded")
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn networks() -> Vec<String> {
        vec!["192.0.2.0/24".to_string(), "fc00::/7".to_string()]
    }

    #[test]
    fn a_host_address_is_only_an_ip_and_a_port_on_an_allowed_network() {
        let list = networks();
        assert_eq!(parse_host_address(&list, "192.0.2.5").unwrap().to_string(), "192.0.2.5:8006");
        assert_eq!(parse_host_address(&list, "192.0.2.5:8443").unwrap().to_string(), "192.0.2.5:8443");
        assert_eq!(parse_host_address(&list, "[fd00::5]:8006").unwrap().to_string(), "[fd00::5]:8006");
        for bad in [
            "",
            "pve.example",
            "localhost",
            "https://192.0.2.5",
            "http://192.0.2.5:8006",
            "192.0.2.5/api",
            "192.0.2.5:8006/api2",
            "user@192.0.2.5",
            "user:x@192.0.2.5:8006",
            "192.0.2.5?x=1",
            "192.0.2.5#x",
            "192.0.2.5:0",
            "192.0.2.5:99999",
            "8.8.8.8",
            "127.0.0.1",
            "::1",
            "[::1]:8006",
            "0.0.0.0",
            "169.254.169.254",
            "224.0.0.1",
            "255.255.255.255",
            "fe80::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:8.8.8.8",
            "0x7f.0.0.1",
            "192.000.002.005",
            "192.0.2",
            "2130706433",
        ] {
            assert_eq!(parse_host_address(&list, bad), None, "{bad}");
        }
    }

    #[test]
    fn a_mapped_address_is_judged_as_the_address_it_is() {
        let list = networks();
        assert_eq!(parse_host_address(&list, "[::ffff:192.0.2.5]:8006").unwrap().to_string(), "192.0.2.5:8006");
    }

    #[test]
    fn token_and_ca_references_are_bare_file_names() {
        for good in ["pve2.token", "pve-root-ca.pem", "a_b.c"] {
            assert!(is_file_name(good), "{good}");
        }
        for bad in ["", "../shadow", "..", "a/b", "a\\b", "/etc/shadow", "C:\\x", ".hidden", "a..b", "a b", "a\0b"] {
            assert!(!is_file_name(bad), "{bad}");
        }
    }

    #[test]
    fn a_token_secret_is_neither_a_file_nor_a_node() {
        let secret = "11111111-2222-3333-4444-555555555555";
        assert!(!is_file_name(secret));
        assert!(!is_node_name(secret));
        assert!(!is_file_name("ABCDEF01-abcd-ABCD-0123-abcdefABCDEF"));
        for name in
            ["11111111-2222-3333-4444", "1111111-2222-3333-4444-555555555555", "pve-2222-3333-4444-555555555555"]
        {
            assert!(is_file_name(name), "{name}");
        }
    }

    fn stored(id: &str, address: &str) -> [String; 5] {
        [id, address, "pve", "/old/place/t.token", "/old/place/c.pem"].map(str::to_string)
    }

    fn from_store(files_dir: Option<&Path>, row: &[String; 5]) -> Result<ExtraHost, String> {
        host_from_store(&networks(), files_dir, [&row[0], &row[1], &row[2], &row[3], &row[4]])
    }

    #[test]
    fn a_stored_host_is_judged_by_todays_configuration() {
        let dir = Some(Path::new("/s"));
        let host = from_store(dir, &stored("two", "192.0.2.5:8006")).unwrap();
        assert_eq!(host.token_file, Path::new("/s").join("t.token"), "only its name comes from the store");
        assert_eq!(host.ca_file, Path::new("/s").join("c.pem"));
        assert!(from_store(dir, &stored("two", "198.51.100.5:8006")).is_err(), "off the allowed networks");
        assert!(from_store(dir, &stored("primary", "192.0.2.5:8006")).is_err(), "the reserved name");
        assert!(from_store(None, &stored("two", "192.0.2.5:8006")).is_err(), "no files directory");
    }

    #[test]
    fn a_host_needs_a_files_directory() {
        let params = json!({ "id": "two", "address": "192.0.2.5", "node": "pve", "token_file": "t", "ca_file": "c" });
        assert_eq!(validate_host(&networks(), None, &params).unwrap_err().code, "hosts_not_configured");
    }

    #[test]
    fn the_primary_host_name_is_taken() {
        let fields = ["primary", "192.0.2.5", "pve", "t", "c"];
        assert!(host_spec(Path::new("/s"), &networks(), fields).is_err());
    }

    #[test]
    fn machine_targets_have_one_exact_shape() {
        assert_eq!(parse_machine("vm100"), Some((None, 100)));
        assert_eq!(parse_machine("nas2-vm200"), Some((Some("nas2"), 200)));
        for bad in [
            "vm",
            "vm0",
            "vm01",
            "vm1234567890",
            "100",
            "host",
            "vm100/../101",
            "vm-100",
            "-vm100",
            "NAS-vm100",
            "a-b-vm100",
            "nas2-vm",
            "vm100-",
            "vm 100",
            "1a-vm100",
        ] {
            assert_eq!(parse_machine(bad), None, "{bad}");
        }
    }

    fn found(local_id: &str, attrs: Value) -> DeviceDecl {
        DeviceDecl {
            local_id: local_id.to_string(),
            target_hint: Some(local_id.to_string()),
            class: DeviceClass::Light,
            name: "A light".to_string(),
            capabilities: Vec::new(),
            attrs,
        }
    }

    #[test]
    fn a_light_found_behind_a_bridge_is_asked_for_by_its_id_and_keeps_what_was_found() {
        let behind = light_candidate(&found("hue-3", json!({ "light": "3", "colour": false })));
        assert_eq!(behind.attrs, json!({ "light": "3", "colour": false, "mac": "hue-3", "address": BRIDGE_ADDRESS }));
        let own = json!({ "mac": "a8bb50aabbcc", "address": "192.0.2.10:38899" });
        assert_eq!(light_candidate(&found("wiz-a8bb50aabbcc", own.clone())).attrs, own, "a bulb's own are kept");
    }

    #[test]
    fn machine_targets_read_back() {
        assert_eq!(machine_target(PRIMARY_HOST, 104), "vm104");
        assert_eq!(machine_target("nas2", 200), "nas2-vm200");
        assert_eq!(parse_machine(&machine_target("nas2", 200)), Some((Some("nas2"), 200)));
    }
}
