//! The compiled-in plugins and their configured instances, for the
//! Integrations page. Owner only. Nothing here reaches a device: readiness is the
//! plugin's pure call, health is what the last poll said, and secrets are named by
//! their slots, never shown.
//!
//! - `GET /plugins`: every plugin in this build (what it reaches, whether another
//!   instance of it can be added and why not) and every stored instance;
//! - `GET /plugins/{plugin}`: one manifest in full, with each custom action's
//!   effective tier and the words the grammar did not take;
//! - `GET /instances/{id}`: one instance with its config, which of its plugin's
//!   secret slots hold a value, readiness, health and devices.
//!
//! An instance of a plugin this build does not have is listed too, disabled with
//! that reason, so it can be removed.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use swifty_plugin_api::{DeviceClass, Health, NetUse, Plugin, Reach, Readiness};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::guards::Owner;
use super::RouterState;
use crate::app::App;
use crate::catalog;
use crate::error::ApiError;
use crate::inventory;
use crate::plugin_actions::reach_text;
use crate::runtime::{contained, InstanceSpec, StoredDevice};

pub(crate) fn routes() -> OpenApiRouter<RouterState> {
    OpenApiRouter::new().routes(routes!(plugins_list)).routes(routes!(plugin_get)).routes(routes!(instance_get))
}

fn unknown_plugin() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "unknown_plugin", "This build of Swifty Home has no such plugin.")
}

fn unknown_instance() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "unknown_instance", "There is no such integration here.")
}

fn has_setup(plugin: &dyn Plugin) -> bool {
    contained(|| plugin.setup().is_some()).unwrap_or(false)
}

/// Why another instance of this plugin cannot be added now, if it cannot.
fn why_not(plugin: &dyn Plugin, instances: &[&str]) -> Option<String> {
    let manifest = plugin.manifest();
    if !has_setup(plugin) {
        return Some(format!("{} has no guided setup.", manifest.name));
    }
    match instances.first() {
        Some(existing) if !manifest.multi_instance => {
            Some(format!("{} is set up already as {existing}, and one is all it takes.", manifest.name))
        }
        _ => None,
    }
}

/// A plugin in this build.
#[derive(Debug, Serialize, ToSchema)]
pub struct PluginSummary {
    pub id: &'static str,
    pub name: &'static str,
    pub version: &'static str,
    /// One sentence on what it connects to.
    pub summary: &'static str,
    /// Whether it stays on the local network or goes through a vendor's servers.
    pub reach: Reach,
    /// The same, in sentences for a person: what it reaches and how.
    pub reach_text: Vec<String>,
    /// Everything it opens a socket to.
    pub network: Vec<NetUse>,
    /// The classes of the devices it offers.
    pub classes: Vec<DeviceClass>,
    /// Whether a second instance makes sense.
    pub multi_instance: bool,
    /// Whether it has a guided setup (`plugin.add`).
    pub setup: bool,
    /// The ids of its instances.
    pub instances: Vec<String>,
    /// Whether another instance can be added now.
    pub can_add: bool,
    /// Why not, when it cannot.
    pub why_not: Option<String>,
}

/// A plugin as the list shows it.
fn plugin_summary(plugin: &dyn Plugin, specs: &[InstanceSpec]) -> PluginSummary {
    let manifest = plugin.manifest();
    let instances: Vec<&str> =
        specs.iter().filter(|spec| spec.plugin == manifest.id).map(|spec| spec.id.as_str()).collect();
    let why_not = why_not(plugin, &instances);
    PluginSummary {
        id: manifest.id,
        name: manifest.name,
        version: manifest.version,
        summary: manifest.summary,
        reach: manifest.reach,
        reach_text: reach_text(plugin),
        network: manifest.network.to_vec(),
        classes: manifest.classes.to_vec(),
        multi_instance: manifest.multi_instance,
        setup: has_setup(plugin),
        instances: instances.iter().map(|id| id.to_string()).collect(),
        can_add: why_not.is_none(),
        why_not,
    }
}

/// A configured instance of a plugin.
#[derive(Debug, Serialize, ToSchema)]
pub struct InstanceSummary {
    pub id: String,
    /// The plugin's id.
    pub plugin: String,
    /// The plugin's name, or null when this build does not have the plugin.
    pub plugin_name: Option<&'static str>,
    /// Whether this build has the plugin. An instance of a plugin it lacks is
    /// disabled, and can be removed.
    pub in_build: bool,
    pub enabled: bool,
    /// The plugin's config version its config was written for.
    pub config_version: u32,
    pub readiness: Readiness,
    /// Why its devices cannot be used now, if they cannot.
    pub unavailable: Option<String>,
    /// What the last health check said.
    pub health: Health,
    /// The secret slots that hold a value. Never the values.
    pub secrets: Vec<String>,
    pub devices: Vec<InstanceDevice>,
    /// The id of the setup of this instance that is open, if one is.
    pub setup_open: Option<String>,
}

/// A device of an instance.
#[derive(Debug, Serialize, ToSchema)]
pub struct InstanceDevice {
    pub target: String,
    pub name: String,
    pub class: DeviceClass,
    /// Waiting for the owner's approval (`machine.approve`).
    pub pending: bool,
}

/// An instance as the list shows it: how it is doing, which slots hold a value, and
/// its devices.
fn instance_summary(app: &App, spec: &InstanceSpec, stored: &[StoredDevice]) -> InstanceSummary {
    let plugin = app.registry.plugin(&spec.plugin);
    let (readiness, unavailable, health) = match app.registry.instance(&spec.id) {
        Some(instance) => (instance.readiness(), instance.unavailable(app.registry.is_live()), instance.last_health()),
        None => {
            let reason = format!("This build of Swifty Home has no plugin {}.", spec.plugin);
            (Readiness::Disabled(reason.clone()), Some(reason), Health::unknown())
        }
    };
    let secrets: Vec<String> = app.registry.secret_store().slots(&spec.id).unwrap_or_default().into_iter().collect();
    let devices = stored
        .iter()
        .filter(|device| device.instance == spec.id)
        .map(|device| InstanceDevice {
            target: device.target.clone(),
            name: device.name.clone(),
            class: device.class,
            pending: device.pending,
        })
        .collect();
    InstanceSummary {
        id: spec.id.clone(),
        plugin: spec.plugin.clone(),
        plugin_name: plugin.map(|plugin| plugin.manifest().name),
        in_build: plugin.is_some(),
        enabled: spec.enabled,
        config_version: spec.config_version,
        readiness,
        unavailable,
        health,
        secrets,
        devices,
        setup_open: app.setup.open_for(app, &spec.id),
    }
}

/// The plugins of this build and the configured instances.
#[derive(Debug, Serialize, ToSchema)]
pub struct Plugins {
    /// Whether commands reach real devices.
    pub live: bool,
    pub plugins: Vec<PluginSummary>,
    pub instances: Vec<InstanceSummary>,
    /// How many setups are open across the hub (at most 4).
    pub setups_open: usize,
}

/// Every plugin in this build, and every configured instance.
///
/// Adding an instance is the critical action `plugin.add`; reconfiguring and
/// removing one are `plugin.reconfigure` and `plugin.remove`.
#[utoipa::path(
    get,
    path = "/plugins",
    tag = "plugins",
    security(("session" = ["owner"])),
    responses((status = 200, body = Plugins))
)]
async fn plugins_list(State(app): State<App>, Owner(_session): Owner) -> Result<Json<Plugins>, ApiError> {
    let (specs, stored) = app.store.with(|conn| -> rusqlite::Result<_> {
        Ok((inventory::instance_specs(conn)?, inventory::stored_devices(conn)?))
    })?;
    let plugins = app.registry.plugins().map(|plugin| plugin_summary(plugin, &specs)).collect();
    let instances = specs.iter().map(|spec| instance_summary(&app, spec, &stored)).collect();
    Ok(Json(Plugins { live: app.registry.is_live(), plugins, instances, setups_open: app.setup.open_count(&app) }))
}

/// One plugin's manifest in full.
///
/// The manifest as the plugin declares it (its config fields, secret slots, custom
/// actions, vocabulary and nouns), with `tier_effective` on each custom action (its
/// tier after the hub's floor), and the fields of the list's entry (`reach_text`,
/// `setup`, `instances`, `can_add`, `why_not`), and `dropped_words`: the words the
/// voice grammar did not take, with why. The manifest's own fields are described in
/// `docs/plugins/reference/manifest.md`.
#[utoipa::path(
    get,
    path = "/plugins/{plugin}",
    tag = "plugins",
    security(("session" = ["owner"])),
    params(("plugin" = String, Path, description = "The plugin's id, such as `hue`.")),
    responses((status = 200, description = "The manifest, with what the hub adds to it.", body = Object, example = json!({
        "id": "mock", "name": "Mock devices", "version": "0.1.0", "api": 1,
        "summary": "Pretend devices of every class that live in the hub's memory.",
        "reach": { "reach": "local" }, "network": [], "classes": ["light", "plug", "other"],
        "config_version": 1, "config": [], "secrets": [], "actions": [], "vocabulary": [], "nouns": [["gadget", "other"]],
        "multi_instance": true, "reach_text": ["It talks only to devices on your network."], "setup": true,
        "instances": ["demo"], "can_add": true, "why_not": null, "dropped_words": []
    })))
)]
async fn plugin_get(
    State(app): State<App>,
    Owner(_session): Owner,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let plugin = app.registry.plugin(&id).ok_or_else(unknown_plugin)?;
    let manifest = plugin.manifest();
    let specs = app.store.with(inventory::instance_specs)?;
    let mut full = serde_json::to_value(manifest)?;
    // The tier each custom action really has, with the hub's floor applied.
    if let Some(actions) = full.get_mut("actions").and_then(Value::as_array_mut) {
        for action in actions {
            let tier = action.get("id").and_then(Value::as_str).and_then(catalog::find).map(|def| def.tier.wire());
            action["tier_effective"] = tier.unwrap_or(Value::Null);
        }
    }
    let summary = serde_json::to_value(plugin_summary(plugin, &specs))?;
    for key in ["reach_text", "setup", "instances", "can_add", "why_not"] {
        full[key] = summary[key].clone();
    }
    full["dropped_words"] = app
        .dropped_words
        .iter()
        .filter(|dropped| dropped.plugin == manifest.id)
        .map(|dropped| json!({ "word": dropped.word, "why": dropped.why }))
        .collect();
    Ok(Json(full))
}

/// One instance in full.
#[derive(Debug, Serialize, ToSchema)]
pub struct Instance {
    #[serde(flatten)]
    pub summary: InstanceSummary,
    /// Its config, which never holds a secret (the slots hold those).
    pub config: Value,
    /// Each secret slot the plugin declares, and whether it holds a value.
    pub secret_slots: Vec<SecretSlotView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
}

/// A secret slot of an instance. Never its value.
#[derive(Debug, Serialize, ToSchema)]
pub struct SecretSlotView {
    /// The slot, such as `app_key`, or `base/{id}` for one value per row.
    pub slot: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub required: bool,
    /// Whether it holds a value (any row, for a `base/{id}` slot).
    pub set: bool,
    /// For a `base/{id}` slot: the rows that hold a value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<Vec<String>>,
}

/// One instance: its config (never a secret), which secret slots hold a value,
/// readiness, health and devices.
#[utoipa::path(
    get,
    path = "/instances/{id}",
    tag = "plugins",
    security(("session" = ["owner"])),
    params(("id" = String, Path, description = "The instance's id, such as `hue`.")),
    responses((status = 200, body = Instance))
)]
async fn instance_get(
    State(app): State<App>,
    Owner(_session): Owner,
    Path(id): Path<String>,
) -> Result<Json<Instance>, ApiError> {
    let (spec, stored, times) = app.store.with(|conn| -> rusqlite::Result<_> {
        let spec = inventory::instance_specs(conn)?.into_iter().find(|spec| spec.id == id);
        let times: Option<(i64, i64)> = conn
            .query_row("SELECT created_at, updated_at FROM plugin_instances WHERE id = ?1", params![id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?;
        Ok((spec, inventory::stored_devices(conn)?, times))
    })?;
    let spec = spec.ok_or_else(unknown_instance)?;
    let summary = instance_summary(&app, &spec, &stored);
    let filled = app.registry.secret_store().slots(&spec.id).unwrap_or_default();
    let secret_slots = app
        .registry
        .plugin(&spec.plugin)
        .map(|plugin| plugin.manifest().secrets)
        .unwrap_or_default()
        .iter()
        .map(|declared| {
            let (set, rows) = match declared.slot.split_once('/') {
                Some((base, _)) => {
                    let rows: Vec<String> = filled
                        .iter()
                        .filter_map(|slot| slot.strip_prefix(base)?.strip_prefix('/'))
                        .map(str::to_string)
                        .collect();
                    (!rows.is_empty(), Some(rows))
                }
                None => (filled.contains(declared.slot), None),
            };
            SecretSlotView {
                slot: declared.slot,
                label: declared.label,
                help: declared.help,
                required: declared.required,
                set,
                rows,
            }
        })
        .collect();
    Ok(Json(Instance {
        summary,
        // The config never holds a secret: a plugin's config and its credentials are kept apart.
        config: spec.config.clone(),
        secret_slots,
        created_at: times.map(|(created_at, _)| created_at),
        updated_at: times.map(|(_, updated_at)| updated_at),
    }))
}
