//! The devices, their state and names, and the lights: listing, discovery, adding
//! one, and pairing the Hue Bridge.
//!
//! Discovery asks every instance whose plugin has lights. Pairing the bridge runs
//! the Hue plugin's own setup flow as far as its key: the flow pairs, the hub
//! stores the key in the instance's secret slot and connects the instance again.
//!
//! Generic discovery and adoption, for any plugin: an instance's
//! discovery stores what it found as candidates, and a device can only be adopted
//! from a stored candidate, so nobody can name an address for the hub to send to.
//! A computer (a class that needs approval) is stored pending instead, and is
//! approved and removed with the critical machine actions, never here.

use axum::extract::{MatchedPath, Path, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::Json;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use swifty_plugin_api::{
    Audience, Capability, DeviceClass, DeviceDecl, DeviceError, Scalar, SetupInput, SetupStep, State as Reading,
};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::auth::Renamed;
use super::guards::{refuse_owner_only, Authed, Body, Owner};
use super::openapi::deprecation;
use super::{audit_arrangement, plain_event, unprocessable, NameBody, RouterState};
use crate::actions;
use crate::app::App;
use crate::catalog::validate_label;
use crate::error::ApiError;
use crate::inventory::{self, Light};
use crate::policy;
use crate::runtime::{Availability, StoredDevice};

/// The plugin whose bridge `POST /api/v1/lights/hue/pair` pairs, and the slot its key goes in.
const HUE_PLUGIN: &str = "hue";
const HUE_KEY_SLOT: &str = "app_key";

/// A light a Hue Bridge lists: `hue-` and its number on the bridge.
fn is_hue_light(id: &str) -> bool {
    id.strip_prefix("hue-")
        .is_some_and(|id| (1..=5).contains(&id.len()) && !id.starts_with('0') && id.bytes().all(|b| b.is_ascii_digit()))
}

/// What a light that a discovery found is asked for by: its MAC, twelve lower-case
/// hex digits, or the id its bridge lists it under.
fn is_light_id(id: &str) -> bool {
    let is_mac = id.len() == 12 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    is_mac || is_hue_light(id)
}

pub(crate) fn routes() -> OpenApiRouter<RouterState> {
    // The older light routes, kept for v1 and marked deprecated: each
    // answers with a `Deprecation` header, and the document lists them in
    // `openapi::DEPRECATED`.
    let lights = OpenApiRouter::new()
        .routes(routes!(lights_list, light_add))
        .routes(routes!(lights_discover))
        .routes(routes!(hue_pair))
        .routes(routes!(light_accept_address))
        .layer(middleware::from_fn(deprecation));
    OpenApiRouter::new()
        .routes(routes!(devices_list))
        .routes(routes!(state_all))
        .routes(routes!(state_one))
        .routes(routes!(device_rename))
        .routes(routes!(instance_discover))
        .routes(routes!(instance_candidates))
        .routes(routes!(device_adopt))
        .routes(routes!(device_remove))
        .merge(lights)
}

fn check_name(name: &str) -> Result<(), ApiError> {
    validate_label("name", name, 64).map_err(|e| unprocessable("invalid_name", e.0))
}

// ----- devices and state

/// The devices this person may see.
#[derive(Debug, Serialize, ToSchema)]
pub struct Devices {
    /// Whether commands reach real devices.
    pub live: bool,
    /// In target order.
    pub devices: Vec<Device>,
}

/// A device, as the index has it.
#[derive(Debug, Serialize, ToSchema)]
pub struct Device {
    /// Its public id. It never changes.
    pub target: String,
    pub name: String,
    pub class: DeviceClass,
    /// What it can do: which controls to draw, and the ranges it takes.
    pub capabilities: Vec<Capability>,
    /// The plugin that drives it.
    pub plugin: &'static str,
    /// The instance of that plugin it belongs to.
    pub instance: String,
    /// Who may see it.
    pub audience: Audience,
    /// Whether it can be used now.
    pub enabled: bool,
    /// Why not, when it cannot.
    pub reason: Option<String>,
}

/// Every active device this person may see, in target order, with what the index
/// says each one is.
#[utoipa::path(
    get,
    path = "/devices",
    tag = "devices",
    security(("session" = [])),
    responses((status = 200, body = Devices))
)]
async fn devices_list(State(app): State<App>, Authed(session): Authed) -> Result<Json<Devices>, ApiError> {
    let role = session.person.role;
    let index = app.registry.index();
    let devices = index
        .active()
        .filter(|device| policy::may_see(&index, role, &device.target))
        .map(|device| {
            let (enabled, reason) = match app.registry.availability(&device.target) {
                Availability::Disabled(reason) => (false, Some(reason)),
                Availability::Enabled => (true, None),
            };
            Device {
                target: device.target.clone(),
                name: device.name.clone(),
                class: device.class,
                capabilities: device.capabilities.clone(),
                plugin: device.plugin,
                instance: device.instance.clone(),
                audience: device.audience,
                enabled,
                reason,
            }
        })
        .collect();
    Ok(Json(Devices { live: app.registry.is_live(), devices }))
}

/// A device's state as a read of it found it. Every field but `available` may be
/// absent: an unknown device has only `available` and `error`.
#[derive(Serialize, ToSchema)]
pub struct DeviceReading {
    pub target: Option<String>,
    pub name: Option<String>,
    /// The plugin that drives it.
    pub plugin: Option<String>,
    /// The instance of that plugin it belongs to.
    pub instance: Option<String>,
    /// Whether the device answered with its state.
    pub available: bool,
    /// What it reported, when it answered.
    pub state: Option<Reading>,
    /// Dry-run: nothing was asked.
    pub simulated: Option<bool>,
    /// Why there is no state.
    pub error: Option<String>,
    /// The hub did not ask it this time, although nothing is known to be wrong
    /// with it: its controls can still be offered.
    pub not_contacted: Option<bool>,
}

/// The state of every device this person may see.
#[derive(Serialize, ToSchema)]
pub struct States {
    /// Whether commands reach real devices.
    pub live: bool,
    /// Dry-run: no device was asked.
    pub simulated: bool,
    /// When the hub read them.
    pub read_at: i64,
    /// By target.
    #[schema(value_type = std::collections::BTreeMap<String, DeviceReading>)]
    pub devices: Map<String, Value>,
}

/// The state of every active device this person may see, read now.
///
/// A page polls this; polling is not activity. A reading that differs from the last
/// one is also sent as a `state` event.
#[utoipa::path(
    get,
    path = "/state",
    tag = "devices",
    security(("session" = [])),
    responses((status = 200, body = States))
)]
async fn state_all(State(app): State<App>, Authed(session): Authed) -> Result<Json<States>, ApiError> {
    let role = session.person.role;
    let index = app.registry.index();
    let targets = index
        .active()
        .filter(|device| policy::may_see(&index, role, &device.target))
        .map(|device| device.target.clone())
        .collect();
    let devices = actions::read_many(&app, targets).await;
    Ok(Json(States { live: app.registry.is_live(), simulated: !app.registry.is_live(), read_at: app.now(), devices }))
}

/// The state of one device, read now.
///
/// A family member asking for a device they may not see gets `403 owner_only`,
/// whether it exists or not.
#[utoipa::path(
    get,
    path = "/state/{target}",
    tag = "devices",
    security(("session" = [])),
    params(("target" = String, Path, description = "The device's target.")),
    responses((status = 200, body = DeviceReading))
)]
async fn state_one(
    State(app): State<App>,
    Authed(session): Authed,
    route: MatchedPath,
    Path(target): Path<String>,
) -> Result<Json<Value>, ApiError> {
    // A family member is refused anything the index does not show them, whether it
    // exists or not, so a refusal says nothing about what exists.
    let index = app.registry.index();
    if !policy::may_see(&index, session.person.role, &target) {
        return Err(refuse_owner_only(&app, route.as_str(), &session));
    }
    if index.get(&target).is_none_or(|device| device.pending) {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_device", "No such device."));
    }
    Ok(Json(actions::read_one(&app, &target).await))
}

/// Gives a device another name.
#[utoipa::path(
    put,
    path = "/devices/{target}/name",
    tag = "devices",
    security(("session" = ["owner"])),
    params(("target" = String, Path, description = "The device's target.")),
    request_body = NameBody,
    responses((status = 200, body = Renamed))
)]
async fn device_rename(
    State(app): State<App>,
    Owner(session): Owner,
    Path(target): Path<String>,
    Body(body): Body<NameBody>,
) -> Result<Json<Renamed>, ApiError> {
    check_name(&body.name)?;
    if app.registry.index().get(&target).is_none_or(|device| device.pending) {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_device", "No such device."));
    }
    app.store.tx(|tx| -> Result<(), ApiError> {
        inventory::set_device_name(tx, &target, &body.name)?;
        audit_arrangement(&app, tx, &session, "device.rename", &target, json!({ "name": body.name }))
    })?;
    // The index holds names too.
    app.sync_directory()?;
    Ok(Json(Renamed { renamed: true }))
}

// ----- lights

/// A light to add by hand: its MAC (or its id on the Hue Bridge) and a name.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct AddLight {
    mac: String,
    name: String,
}

/// The lights the hub knows.
#[derive(Serialize, ToSchema)]
pub struct Lights {
    pub lights: Vec<Light>,
}

/// A light that was just added.
#[derive(Serialize, ToSchema)]
pub struct LightAdded {
    pub light: Light,
}

/// What a light discovery found.
#[derive(Serialize, ToSchema)]
pub struct LightsFound {
    /// Dry-run: nothing was asked, and `found` is empty.
    pub simulated: bool,
    /// Lights at an address of their own first, then those behind a bridge.
    pub found: Vec<FoundLight>,
    /// Why a bridge could not be asked, while other lights were found. Absent in dry-run.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub hue_error: Option<Option<String>>,
}

/// A light a discovery found.
#[derive(Serialize, ToSchema)]
pub struct FoundLight {
    pub mac: String,
    /// Whether the hub has it already.
    pub added: bool,
    /// Whether it was found at another address than the one stored.
    pub moved: bool,
    pub address: String,
    /// The plugin that found it.
    pub kind: &'static str,
    /// The instance that found it.
    pub instance: String,
    /// Its name on the bridge, for a light behind one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// The lights the hub knows. Deprecated: use `GET /devices`.
#[utoipa::path(
    get,
    path = "/lights",
    tag = "lights",
    security(("session" = ["owner"])),
    responses((status = 200, body = Lights))
)]
async fn lights_list(State(app): State<App>, Owner(_session): Owner) -> Result<Json<Lights>, ApiError> {
    let lights = app.store.with(inventory::lights)?;
    Ok(Json(Lights { lights }))
}

/// The instances discovery asks: every one whose plugin has lights and that can be
/// used now, in id order. One that is switched off or waits for its setup has
/// nothing to say.
fn light_instances(app: &App) -> Vec<String> {
    let live = app.registry.is_live();
    app.registry
        .instances()
        .iter()
        .filter(|instance| instance.plugin.manifest().classes.contains(&DeviceClass::Light))
        .filter(|instance| instance.unavailable(live).is_none())
        .map(|instance| instance.spec.id.clone())
        .collect()
}

/// Asks every instance with lights what it finds.
///
/// Refused, with the first reason, when nothing was found and one of them could not
/// look; one that cannot look does not spoil what the others found, and a failure
/// that is not about how it is set up is given beside them. In dry-run nothing is
/// sent. Deprecated: use `POST /instances/{id}/discover`.
#[utoipa::path(
    post,
    path = "/lights/discover",
    tag = "lights",
    security(("session" = ["owner"])),
    responses((status = 200, body = LightsFound))
)]
async fn lights_discover(State(app): State<App>, Owner(session): Owner) -> Result<Json<LightsFound>, ApiError> {
    if !app.registry.is_live() {
        app.audit(plain_event(&app, "light.discover", "-", Some(&session), "simulated"))?;
        return Ok(Json(LightsFound { simulated: true, found: Vec::new(), hue_error: None }));
    }
    app.audit(plain_event(&app, "light.discover", "-", Some(&session), "commanded"))?;
    let mut found: Vec<(String, DeviceDecl)> = Vec::new();
    let mut errors: Vec<DeviceError> = Vec::new();
    for instance in light_instances(&app) {
        match app.registry.discover(&instance).await {
            Ok(decls) => {
                let lights = decls.unwrap_or_default().into_iter().filter(|decl| decl.class == DeviceClass::Light);
                found.extend(lights.map(|decl| (instance.clone(), inventory::light_candidate(&decl))));
            }
            Err(error) => errors.push(error),
        }
    }
    if found.is_empty() && (!errors.is_empty() || light_instances(&app).is_empty()) {
        app.audit(plain_event(&app, "light.discover", "-", Some(&session), "refused: discovery_failed"))?;
        let error = errors
            .into_iter()
            .next()
            .unwrap_or_else(|| DeviceError::Disabled("No integration here can look for lights.".to_string()));
        let status =
            if matches!(error, DeviceError::Disabled(_)) { StatusCode::CONFLICT } else { StatusCode::BAD_GATEWAY };
        return Err(ApiError::new(status, "discovery_failed", error.to_string()));
    }
    // Lights at an address of their own first, then those behind a bridge.
    let address = |decl: &DeviceDecl| decl.attrs["address"].as_str().unwrap_or_default().to_string();
    found.sort_by_key(|(_, decl)| address(decl) == crate::store::BRIDGE_ADDRESS);
    let now = app.now();
    app.store.tx(|tx| -> Result<(), ApiError> {
        for (instance, decl) in &found {
            inventory::record_found(tx, now, instance, std::slice::from_ref(decl))?;
        }
        let mut event = plain_event(&app, "light.discover", "-", Some(&session), "done");
        event.params = json!({ "found": found.len() });
        app.append_audit(tx, &event)?;
        Ok(())
    })?;
    let known = app.store.with(inventory::lights)?;
    let list = found
        .iter()
        .map(|(instance, decl)| {
            let mac = decl.attrs["mac"].as_str().unwrap_or_default();
            let address = address(decl);
            let light = known.iter().find(|light| light.mac == mac);
            FoundLight {
                mac: mac.to_string(),
                added: light.is_some(),
                moved: light.is_some_and(|light| light.address != address),
                kind: app.registry.instance(instance).map_or("", |instance| instance.plugin.manifest().id),
                instance: instance.clone(),
                name: (address == crate::store::BRIDGE_ADDRESS).then(|| decl.name.clone()),
                address,
            }
        })
        .collect();
    let error = errors.first().filter(|error| !matches!(error, DeviceError::Disabled(_))).map(ToString::to_string);
    Ok(Json(LightsFound { simulated: false, found: list, hue_error: Some(error) }))
}

/// Where a light was moved to.
#[derive(Serialize, ToSchema)]
pub struct LightMoved {
    pub address: String,
}

/// Moves a light to where the last discovery found it.
///
/// The address is never taken from the request, and nothing is sent to the bulb.
/// Deprecated.
#[utoipa::path(
    put,
    path = "/lights/{target}/address",
    tag = "lights",
    security(("session" = ["owner"])),
    params(("target" = String, Path, description = "The light's target.")),
    responses((status = 200, body = LightMoved))
)]
async fn light_accept_address(
    State(app): State<App>,
    Owner(session): Owner,
    Path(target): Path<String>,
) -> Result<Json<LightMoved>, ApiError> {
    let address = app.store.tx(|tx| -> Result<String, ApiError> {
        let address = inventory::accept_found_address(tx, &target)?;
        audit_arrangement(&app, tx, &session, "light.accept_address", &target, json!({ "address": address }))?;
        Ok(address)
    })?;
    app.sync_directory()?;
    Ok(Json(LightMoved { address }))
}

/// Adds a light by its MAC. Deprecated: use discovery and `POST /devices/adopt`.
#[utoipa::path(
    post,
    path = "/lights",
    tag = "lights",
    security(("session" = ["owner"])),
    request_body = AddLight,
    responses((status = 200, body = LightAdded))
)]
async fn light_add(
    State(app): State<App>,
    Owner(session): Owner,
    Body(body): Body<AddLight>,
) -> Result<Json<LightAdded>, ApiError> {
    check_name(&body.name)?;
    let mac = body.mac.to_ascii_lowercase();
    if !is_light_id(&mac) {
        return Err(unprocessable("invalid_mac", "mac must be twelve hex digits, or a light on the Hue Bridge."));
    }
    let now = app.now();
    let light = app.store.tx(|tx| -> Result<_, ApiError> {
        let light = inventory::add_light(tx, now, &mac, &body.name, &session.person.user_id)?;
        audit_arrangement(&app, tx, &session, "light.add", &light.target, json!({ "name": body.name }))?;
        Ok(light)
    })?;
    app.sync_directory()?;
    Ok(Json(LightAdded { light }))
}

/// How pairing the bridge ended.
enum Paired {
    /// The flow handed back a key, which is stored now.
    Key,
    /// The link button has not been pressed.
    Waiting,
    Failed(String),
}

/// Runs the Hue plugin's setup flow on the stored bridge address as far as its key:
/// the form with the address it has, one poll of the bridge, and no light chosen.
async fn pair_through_the_flow(app: &App, instance: &crate::runtime::Instance) -> Result<Paired, ApiError> {
    let Some(mut flow) = instance.plugin.setup() else {
        return Ok(Paired::Failed("The Hue plugin has no setup.".to_string()));
    };
    let ctx = app.registry.setup_context(instance);
    let url = instance.spec.config.get("url").and_then(Value::as_str).unwrap_or_default().to_string();
    flow.begin(&ctx, Some(&instance.spec.config)).await;
    let values = [("url".to_string(), Scalar::Text(url))].into_iter().collect();
    let mut step = flow.advance(&ctx, SetupInput::Values { values }).await;
    if matches!(step, SetupStep::Wait { .. }) {
        step = flow.advance(&ctx, SetupInput::Poll).await;
    }
    if matches!(step, SetupStep::Choose { .. }) {
        step = flow.advance(&ctx, SetupInput::Chosen { ids: Vec::new() }).await;
    }
    let result = match step {
        SetupStep::Done(result) => result,
        SetupStep::Wait { .. } => return Ok(Paired::Waiting),
        SetupStep::Failed { reason, .. } => return Ok(Paired::Failed(reason)),
        _ => return Ok(Paired::Failed("The bridge's setup asked for something this page cannot give.".to_string())),
    };
    let Some((_, key)) = result.secrets.iter().find(|(slot, _)| slot == HUE_KEY_SLOT) else {
        return Ok(Paired::Failed("The bridge gave no key.".to_string()));
    };
    app.registry
        .secret_store()
        .write(&instance.spec.id, HUE_KEY_SLOT, key)
        .map_err(|error| ApiError::internal(format!("the key could not be stored ({:?})", error.kind())))?;
    // Connects the instance again, now with its key.
    app.sync_directory()?;
    Ok(Paired::Key)
}

/// How pairing the bridge went.
#[derive(Serialize, ToSchema)]
pub struct HuePaired {
    /// Dry-run: nothing was sent, and nothing was paired.
    pub simulated: bool,
    pub paired: bool,
}

/// Pairs the Hue Bridge: press its link button first.
///
/// Runs the Hue plugin's own setup flow as far as its key. The key goes straight
/// into the Hue instance's secret slot; it is not in this response, the log, or
/// the database. Deprecated: use `plugin.reconfigure`.
#[utoipa::path(
    post,
    path = "/lights/hue/pair",
    tag = "lights",
    security(("session" = ["owner"])),
    responses((status = 200, body = HuePaired))
)]
async fn hue_pair(State(app): State<App>, Owner(session): Owner) -> Result<Json<HuePaired>, ApiError> {
    let refused = |status, code: &'static str, message: String| {
        app.audit(plain_event(&app, "light.hue_pair", "hue", Some(&session), &format!("refused: {code}")))?;
        Err::<Json<HuePaired>, ApiError>(ApiError::new(status, code, message))
    };
    let Some(instance) = app.registry.instances().into_iter().find(|instance| instance.spec.plugin == HUE_PLUGIN)
    else {
        return refused(StatusCode::CONFLICT, "hue_not_configured", "No Hue Bridge is set up here.".to_string());
    };
    if app.registry.setup_context(&instance).secrets_present.contains(HUE_KEY_SLOT) {
        return refused(StatusCode::CONFLICT, "already_paired", "The bridge already has a key here.".to_string());
    }
    if !app.registry.is_live() {
        app.audit(plain_event(&app, "light.hue_pair", "hue", Some(&session), "simulated"))?;
        return Ok(Json(HuePaired { simulated: true, paired: false }));
    }
    app.audit(plain_event(&app, "light.hue_pair", "hue", Some(&session), "commanded"))?;
    match pair_through_the_flow(&app, &instance).await? {
        Paired::Key => {
            app.audit(plain_event(&app, "light.hue_pair", "hue", Some(&session), "paired"))?;
            Ok(Json(HuePaired { simulated: false, paired: true }))
        }
        Paired::Waiting => refused(
            StatusCode::CONFLICT,
            "link_button_not_pressed",
            "Press the link button on the bridge, then try again within 30 seconds.".to_string(),
        ),
        Paired::Failed(reason) => refused(StatusCode::BAD_GATEWAY, "hue_pair_failed", reason),
    }
}

// ----- generic discovery and adoption

fn unknown_instance() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "unknown_instance", "There is no such integration here.")
}

fn needs_approval() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "needs_approval",
        "A computer is listed as pending until the owner approves it with a passkey (machine.approve), \
         and is taken off the list with machine.remove.",
    )
}

/// A name a plugin reported, made safe to show and store.
fn shown_name(name: &str, local_id: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).take(64).collect();
    if name.trim().is_empty() {
        local_id.to_string()
    } else {
        name.trim().to_string()
    }
}

/// The plugin of a stored instance, if there is one.
fn instance_plugin(app: &App, id: &str) -> Result<Option<String>, ApiError> {
    Ok(app.store.with(|conn| {
        conn.query_row("SELECT plugin FROM plugin_instances WHERE id = ?1", rusqlite::params![id], |row| row.get(0))
            .optional()
    })?)
}

/// A device a discovery found.
#[derive(Debug, Serialize, ToSchema)]
pub struct FoundDevice {
    /// Its id within the instance: what `POST /devices/adopt` takes.
    pub local_id: String,
    pub name: String,
    pub class: DeviceClass,
    pub capabilities: Vec<Capability>,
    /// What the plugin needs to find it again, such as its address. Never a secret.
    pub attrs: Value,
    /// The device it already is, if it was stored.
    pub target: Option<String>,
    /// Whether it is adopted already.
    pub adopted: bool,
    /// Whether it waits for the owner's approval: a computer.
    pub pending: bool,
}

/// A found device as an answer shows it, with the device it already is, if it was stored.
fn found_device(decl: &DeviceDecl, stored: Option<&StoredDevice>) -> FoundDevice {
    FoundDevice {
        local_id: decl.local_id.clone(),
        name: shown_name(&decl.name, &decl.local_id),
        class: decl.class,
        capabilities: decl.capabilities.clone(),
        attrs: decl.attrs.clone(),
        target: stored.map(|device| device.target.clone()),
        adopted: stored.is_some_and(|device| !device.pending),
        pending: stored.is_some_and(|device| device.pending),
    }
}

/// What a discovery of an instance found.
#[derive(Debug, Serialize, ToSchema)]
pub struct Discovered {
    /// Dry-run: nothing was asked, and `found` is empty.
    pub simulated: bool,
    pub found: Vec<FoundDevice>,
}

/// Asks one instance what it can find, and stores it.
///
/// A computer (a class that needs approval) is stored as a device waiting for the
/// owner's approval (`machine.approve`); anything else as a candidate, which
/// `POST /devices/adopt` adopts. Nothing is adopted here. In dry-run nothing is sent.
#[utoipa::path(
    post,
    path = "/instances/{id}/discover",
    tag = "devices",
    security(("session" = ["owner"])),
    params(("id" = String, Path, description = "The instance's id.")),
    responses((status = 200, body = Discovered))
)]
async fn instance_discover(
    State(app): State<App>,
    Owner(session): Owner,
    Path(id): Path<String>,
) -> Result<Json<Discovered>, ApiError> {
    let Some(plugin) = instance_plugin(&app, &id)? else {
        return Err(unknown_instance());
    };
    if !app.registry.is_live() {
        app.audit(plain_event(&app, "device.discover", &id, Some(&session), "simulated"))?;
        return Ok(Json(Discovered { simulated: true, found: Vec::new() }));
    }
    app.audit(plain_event(&app, "device.discover", &id, Some(&session), "commanded"))?;
    let found = match app.registry.discover(&id).await {
        Ok(found) => found.unwrap_or_default(),
        Err(error) => {
            app.audit(plain_event(&app, "device.discover", &id, Some(&session), "refused: discovery_failed"))?;
            let status =
                if matches!(error, DeviceError::Disabled(_)) { StatusCode::CONFLICT } else { StatusCode::BAD_GATEWAY };
            return Err(ApiError::new(status, "discovery_failed", error.to_string()));
        }
    };
    // Only what the plugin may offer: well-formed ids, of its own classes.
    let classes = app.registry.plugin(&plugin).map(|plugin| plugin.manifest().classes).unwrap_or_default();
    let found: Vec<DeviceDecl> = found
        .into_iter()
        .filter(|decl| crate::setup::is_local_id(&decl.local_id) && classes.contains(&decl.class))
        .collect();
    let now = app.now();
    app.store.tx(|tx| -> Result<(), ApiError> {
        for decl in &found {
            if decl.class.needs_approval() {
                let stored: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM devices WHERE instance_id = ?1 AND local_id = ?2)",
                    rusqlite::params![id, decl.local_id],
                    |row| row.get(0),
                )?;
                if !stored {
                    let target = crate::setup::free_target(tx, &id, decl)?;
                    let class = serde_json::to_value(decl.class)?;
                    tx.execute(
                        "INSERT INTO devices (target, instance_id, local_id, class, name, attrs, origin, status, first_seen)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'adopted', 'pending', ?7)",
                        rusqlite::params![
                            target,
                            id,
                            decl.local_id,
                            class.as_str().unwrap_or_default(),
                            shown_name(&decl.name, &decl.local_id),
                            decl.attrs.to_string(),
                            now
                        ],
                    )?;
                }
            } else if decl.class == DeviceClass::Light {
                inventory::record_found(tx, now, &id, std::slice::from_ref(decl))?;
            } else {
                tx.execute(
                    "INSERT INTO device_candidates (instance_id, local_id, decl, seen_at) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(instance_id, local_id) DO UPDATE SET decl = excluded.decl, seen_at = excluded.seen_at",
                    rusqlite::params![id, decl.local_id, serde_json::to_string(decl)?, now],
                )?;
            }
        }
        let mut event = plain_event(&app, "device.discover", &id, Some(&session), "done");
        event.params = json!({ "found": found.len() });
        app.append_audit(tx, &event)?;
        Ok(())
    })?;
    app.sync_directory()?;
    let stored = app.store.with(inventory::stored_devices)?;
    let list = found
        .iter()
        .map(|decl| {
            let device = stored.iter().find(|device| device.instance == id && device.local_id == decl.local_id);
            found_device(decl, device)
        })
        .collect();
    Ok(Json(Discovered { simulated: false, found: list }))
}

/// What an instance's discoveries found, and its devices that wait for approval.
#[derive(Debug, Serialize, ToSchema)]
pub struct Candidates {
    pub instance: String,
    /// In local id order.
    pub candidates: Vec<Candidate>,
    pub pending: Vec<PendingDevice>,
}

/// A device a discovery found and stored.
#[derive(Debug, Serialize, ToSchema)]
pub struct Candidate {
    #[serde(flatten)]
    pub found: FoundDevice,
    /// When a discovery last found it.
    pub seen_at: i64,
}

/// A device that waits for the owner's approval.
#[derive(Debug, Serialize, ToSchema)]
pub struct PendingDevice {
    pub target: String,
    pub local_id: String,
    pub name: String,
    pub class: DeviceClass,
}

/// What an instance's discoveries found, and its devices that wait for approval.
#[utoipa::path(
    get,
    path = "/instances/{id}/candidates",
    tag = "devices",
    security(("session" = ["owner"])),
    params(("id" = String, Path, description = "The instance's id.")),
    responses((status = 200, body = Candidates))
)]
async fn instance_candidates(
    State(app): State<App>,
    Owner(_session): Owner,
    Path(id): Path<String>,
) -> Result<Json<Candidates>, ApiError> {
    if instance_plugin(&app, &id)?.is_none() {
        return Err(unknown_instance());
    }
    let (rows, stored) = app.store.with(|conn| -> rusqlite::Result<_> {
        let mut statement =
            conn.prepare("SELECT decl, seen_at FROM device_candidates WHERE instance_id = ?1 ORDER BY local_id")?;
        let rows = statement
            .query_map(rusqlite::params![id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((rows, inventory::stored_devices(conn)?))
    })?;
    let mine: Vec<&StoredDevice> = stored.iter().filter(|device| device.instance == id).collect();
    let candidates = rows
        .iter()
        .filter_map(|(text, seen_at)| {
            let decl: DeviceDecl = serde_json::from_str(text).ok()?;
            let device = mine.iter().copied().find(|device| device.local_id == decl.local_id);
            Some(Candidate { found: found_device(&decl, device), seen_at: *seen_at })
        })
        .collect();
    let pending = mine
        .iter()
        .filter(|device| device.pending)
        .map(|device| PendingDevice {
            target: device.target.clone(),
            local_id: device.local_id.clone(),
            name: device.name.clone(),
            class: device.class,
        })
        .collect();
    Ok(Json(Candidates { instance: id, candidates, pending }))
}

/// A stored candidate to adopt, by its instance and local id, with a name for it
/// if it should not keep the one it was found with.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct AdoptBody {
    instance: String,
    local_id: String,
    #[serde(default)]
    name: Option<String>,
}

/// A device that was just adopted.
#[derive(Debug, Serialize, ToSchema)]
pub struct Adopted {
    pub device: AdoptedDevice,
}

/// What an adopted device is.
#[derive(Debug, Serialize, ToSchema)]
pub struct AdoptedDevice {
    pub target: String,
    pub name: String,
    pub class: DeviceClass,
    pub instance: String,
    pub local_id: String,
}

/// Adopts a device a discovery of its instance found.
///
/// What is stored about it (its address, say) comes from that discovery and never
/// from the request: `404 unknown_candidate` until a discovery found it. A computer
/// is `409 needs_approval` (it goes through `machine.approve`), a device added
/// already `409 device_exists`.
#[utoipa::path(
    post,
    path = "/devices/adopt",
    tag = "devices",
    security(("session" = ["owner"])),
    request_body = AdoptBody,
    responses((status = 200, body = Adopted))
)]
async fn device_adopt(
    State(app): State<App>,
    Owner(session): Owner,
    Body(body): Body<AdoptBody>,
) -> Result<Json<Adopted>, ApiError> {
    if let Some(name) = &body.name {
        check_name(name)?;
    }
    let candidate: Option<String> = app.store.with(|conn| {
        conn.query_row(
            "SELECT decl FROM device_candidates WHERE instance_id = ?1 AND local_id = ?2",
            rusqlite::params![body.instance, body.local_id],
            |row| row.get(0),
        )
        .optional()
    })?;
    let Some(mut decl) = candidate.and_then(|text| serde_json::from_str::<DeviceDecl>(&text).ok()) else {
        // A computer a discovery found is stored pending, never as a candidate.
        let pending: bool = app.store.with(|conn| {
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM devices WHERE instance_id = ?1 AND local_id = ?2 AND status = 'pending')",
                rusqlite::params![body.instance, body.local_id],
                |row| row.get(0),
            )
        })?;
        if pending {
            return Err(needs_approval());
        }
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_candidate",
            "A discovery of that integration has not found that device. Run discovery first.",
        ));
    };
    if decl.class.needs_approval() {
        return Err(needs_approval());
    }
    if let Some(name) = &body.name {
        decl.name = name.clone();
    }
    let name = shown_name(&decl.name, &decl.local_id);
    let now = app.now();
    let target = app.store.tx(|tx| -> Result<String, ApiError> {
        let stored: Option<String> = tx
            .query_row(
                "SELECT target FROM devices WHERE instance_id = ?1 AND local_id = ?2",
                rusqlite::params![body.instance, decl.local_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(target) = stored {
            return Err(ApiError::new(StatusCode::CONFLICT, "device_exists", "That device has been added already.")
                .with_extra(json!({ "target": target })));
        }
        let target = crate::setup::adopt_device(tx, now, &body.instance, &decl, &session.person.user_id)?;
        let params = json!({ "instance": body.instance, "local_id": decl.local_id, "name": name });
        audit_arrangement(&app, tx, &session, "device.adopt", &target, params)?;
        Ok(target)
    })?;
    app.sync_directory()?;
    Ok(Json(Adopted {
        device: AdoptedDevice { target, name, class: decl.class, instance: body.instance, local_id: decl.local_id },
    }))
}

/// `{"removed": true}`.
#[derive(Debug, Serialize, ToSchema)]
pub struct Removed {
    pub removed: bool,
}

/// Removes an adopted device.
///
/// With it go its places in rooms, the favourites that name it, its name and its
/// notes. It can be adopted again from its candidate. A device its instance's config
/// names goes by setting the instance up again (`409 declared_by_config`), and a
/// computer by `machine.remove` (`409 needs_approval`).
#[utoipa::path(
    delete,
    path = "/devices/{target}",
    tag = "devices",
    security(("session" = ["owner"])),
    params(("target" = String, Path, description = "The device's target.")),
    responses((status = 200, body = Removed))
)]
async fn device_remove(
    State(app): State<App>,
    Owner(session): Owner,
    Path(target): Path<String>,
) -> Result<Json<Removed>, ApiError> {
    let found: Option<(String, String, String, String)> = app.store.with(|conn| {
        conn.query_row(
            "SELECT instance_id, local_id, class, origin FROM devices WHERE target = ?1",
            rusqlite::params![target],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
    })?;
    let Some((instance, local_id, class, origin)) = found else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_device", "No such device."));
    };
    let class: Option<DeviceClass> = serde_json::from_value(Value::String(class)).ok();
    if class.is_none_or(DeviceClass::needs_approval) {
        return Err(needs_approval());
    }
    if origin == "config" {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "declared_by_config",
            "This device comes from its integration's settings. Set the integration up again to leave it out.",
        ));
    }
    app.store.tx(|tx| -> Result<(), ApiError> {
        for table in ["room_devices", "favourites", "device_names", "device_notes"] {
            tx.execute(&format!("DELETE FROM {table} WHERE target = ?1"), rusqlite::params![target])?;
        }
        tx.execute("DELETE FROM devices WHERE target = ?1", rusqlite::params![target])?;
        let params = json!({ "instance": instance, "local_id": local_id });
        audit_arrangement(&app, tx, &session, "device.remove", &target, params)
    })?;
    app.sync_directory()?;
    Ok(Json(Removed { removed: true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hue_light_target_is_exactly_a_decimal_id() {
        for good in ["hue-1", "hue-12", "hue-99999"] {
            assert!(is_hue_light(good), "{good}");
        }
        for bad in
            ["hue", "hue-", "hue-0", "hue-01", "hue-a", "hue-1a", "hue--1", "hue-+1", "hue-1 ", " hue-1", "hue-123456"]
        {
            assert!(!is_hue_light(bad), "{bad}");
        }
    }

    #[test]
    fn a_stored_light_target_is_exactly_a_lower_case_mac() {
        assert!(is_light_id("a8bb50aabbcc"));
        assert!(is_light_id("hue-1"), "a light on the bridge is added too");
        for bad in ["", "a8bb50aabbc", "a8bb50aabbccd", "g8bb50aabbcc", "bulb1", "wiz-a8bb50aabbcc"] {
            assert!(!is_light_id(bad), "{bad}");
        }
        for bad in ["hue", "hue-0", "hue-01", "hue-1a", "HUE-1"] {
            assert!(!is_light_id(bad), "{bad}");
        }
    }
}
