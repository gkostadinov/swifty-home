//! The action engine: prepare, confirm, step-up, execute, and the two-phase
//! result of every command.
//!
//! A command moves from `commanded` to `confirmed` only when the device's state
//! has been read back and matches. Silence becomes `no_response`, a refusal
//! becomes `failed`, and in dry-run the result is `simulated`. Non-idempotent
//! actions are sent exactly once.

use std::collections::HashSet;
use std::time::Duration;

use axum::http::StatusCode;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use swifty_plugin_api::{CapKind, DeviceError, Expect, Op, State};
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio::time::Instant;
use webauthn_rs::prelude::{PublicKeyCredential, RequestChallengeResponse};

use crate::app::App;
use crate::audit::Event;
use crate::catalog::{self, is_plain_id, ActionDef, Executor, Facts, Step, Targets, ACK_PARAM};
use crate::console;
use crate::crypto::{canonical_json, constant_time_eq, random_id, random_token, sha256_hex};
use crate::error::ApiError;
use crate::events;
use crate::inventory::{self, MachineStatus};
use crate::passkeys;
use crate::policy::{self, Channel, Flow, Role};
use crate::rooms;
use crate::runtime::index::DeviceIndex;
use crate::runtime::{Availability, Dispatched};
use crate::{notes, plugin_actions, scenes, setup, users};

const READ_TIMEOUT: Duration = Duration::from_secs(3);
/// The device whose own pairing `agent.pair` starts.
/// The audit result of a request that was joined to another person's run.
const JOINED_RUN: &str = "joined_existing_run";

/// Who is asking.
#[derive(Debug, Clone)]
pub struct Actor {
    pub session_ref: String,
    pub credential_id: String,
    pub device: String,
    pub user_id: String,
    pub role: Role,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ActionRequest {
    pub action: String,
    pub target: String,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub channel: Option<String>,
    /// Set by the client so a resubmitted request runs once.
    #[serde(default)]
    pub request_id: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StepUpRequest {
    pub prepared_id: String,
    pub action: String,
    pub target: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfirmRequest {
    pub prepared_id: String,
    pub action: String,
    pub target: String,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub confirmation: Option<String>,
    /// The passkey's assertion, for a critical action after `POST /auth/step-up/start`.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub assertion: Option<PublicKeyCredential>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Commanded,
    Confirmed,
    NoResponse,
    Failed,
    Simulated,
    /// A key press went to the TV. Nothing can read one back, so it is never `Confirmed`.
    /// A press leaves no run row, so this only appears in a response and the audit log.
    Sent,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Commanded => "commanded",
            Phase::Confirmed => "confirmed",
            Phase::NoResponse => "no_response",
            Phase::Failed => "failed",
            Phase::Simulated => "simulated",
            Phase::Sent => "sent",
        }
    }
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct RunView {
    pub run_id: Option<String>,
    pub action: String,
    pub target: String,
    pub params: Value,
    pub channel: String,
    pub step_up: String,
    pub phase: String,
    pub simulated: bool,
    pub detail: Option<String>,
    /// How each device the action touched answered.
    #[schema(value_type = Vec<crate::http::actions::RunStep>)]
    pub steps: Value,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub rtt_ms: Option<i64>,
    pub duplicate: bool,
    /// For `state.read`: each target's reading (as `DeviceReading`), and `metric` when one was asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<Value>,
    /// For `passkey.register`: the ceremony to finish with `POST /passkeys/register/finish`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<crate::http::actions::PasskeyRegistration>)]
    pub registration: Option<Value>,
    /// What the person must be told about this action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    /// How to reach a console that was just opened. Never stored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub console: Option<Value>,
    /// A new invitation, with its token. Shown once and never stored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invitation: Option<Value>,
    /// What the collector said about pairing an agent. Never stored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pairing: Option<Value>,
    /// The setup an integration action opened: its id and first step. Never stored.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<crate::http::setup::Setup>)]
    pub setup: Option<Value>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct PreparedView {
    pub prepared_id: String,
    pub action: String,
    pub target: String,
    pub params: Value,
    #[schema(value_type = crate::http::actions::TierWire)]
    pub tier: Value,
    pub tier_name: String,
    pub label: String,
    pub target_name: String,
    pub what: String,
    pub consequences: Vec<String>,
    pub affected: Vec<String>,
    pub warning: Option<String>,
    pub channel: String,
    /// "tap" for a single-use confirmation, "passkey" for a step-up.
    pub needs: String,
    pub confirmation: Option<String>,
    pub expires_at: i64,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct StepUpOptions {
    pub prepared_id: String,
    /// The WebAuthn request options to hand to `navigator.credentials.get`.
    #[schema(value_type = Object)]
    pub options: RequestChallengeResponse,
    pub expires_at: i64,
}

struct Validated {
    def: &'static ActionDef,
    target: String,
    params: Value,
}

/// One attempt at an action, as far as it is known.
struct Attempt<'a> {
    actor: &'a Actor,
    channel: Channel,
    action: &'a str,
    target: &'a str,
    params: &'a Value,
    step_up: &'a str,
}

/// The device this hub runs on, if it is one of its own: stopping it takes this
/// console offline, and it cannot be taken off the list. `runs_on` in the
/// configuration names it; there is no default.
pub fn runs_on(app: &App) -> Option<String> {
    app.config.runs_on.clone()
}

/// The device that pairs agents: the telemetry node that feeds the charts when it
/// can, or else the first device that pairs agents.
pub fn pairs_agents(app: &App) -> Option<String> {
    let index = app.registry.index();
    crate::history::telemetry_source(app)
        .filter(|target| index.has(target, CapKind::AgentPairing))
        .or_else(|| index.active().find(|device| device.has(CapKind::AgentPairing)).map(|device| device.target.clone()))
}

/// Why `agent.pair` has nothing to ask.
const NOTHING_PAIRS_AGENTS: &str = "No device here pairs agents.";

/// What a further host is checked against: the networks the configuration allows,
/// and the directory the host integration keeps further hosts' files in.
pub fn host_rules(app: &App) -> (Vec<String>, Option<std::path::PathBuf>) {
    let networks = app.config.network.allowed_host_networks.clone();
    let files = app
        .registry
        .host_instance()
        .and_then(|instance| instance.spec.config.get("host_files_dir").and_then(Value::as_str).map(Into::into));
    (networks, files)
}

fn validate_host(app: &App, params: &Value) -> Result<swifty_plugin_api::ExtraHost, ApiError> {
    let (networks, files) = host_rules(app);
    inventory::validate_host(&networks, files.as_deref(), params)
}

/// Whether these parameters might hold something pasted into the wrong field. The
/// fields of a further host name files that hold a token, so a token pasted into one
/// is schema-valid. Only that host's own checks can tell, and the log is permanent.
fn may_hold_a_pasted_secret(app: &App, def: &ActionDef, params: &Value) -> bool {
    def.executor == Executor::ProxmoxAddHost && validate_host(app, params).is_err()
}

/// Whether a refused attempt's target may be written down: a target the action
/// admits, a device the index knows for an action aimed at devices, or the id of
/// a passkey. Anything else is what a client made up.
fn loggable_target(index: &DeviceIndex, def: &ActionDef, target: &str) -> bool {
    def.admits(index, target).is_ok()
        || (def.targets.names_devices() && index.get(target).is_some())
        || (def.targets == Targets::Credential && is_plain_id(target))
}

/// Records a refused attempt and hands back its error. Only catalog ids,
/// allowed targets and schema-valid parameters are written, so nothing a client
/// made up can reach the log. Parameters that may hold a pasted secret are left out.
fn record_refusal(app: &App, attempt: &Attempt<'_>, error: ApiError) -> ApiError {
    let known = catalog::find(attempt.action);
    let index = app.registry.index();
    let target = match known {
        Some(def) if loggable_target(&index, def, attempt.target) => attempt.target,
        _ => "-",
    };
    let params = known
        .and_then(|def| {
            let params = def.validate_params(attempt.params).ok()?;
            (!may_hold_a_pasted_secret(app, def, &params)).then_some(params)
        })
        .unwrap_or(Value::Null);
    let event = Event {
        ts: app.now(),
        action: known.map_or("unknown", |def| def.id).to_string(),
        target: target.to_string(),
        params,
        credential_ref: attempt.actor.credential_id.clone(),
        session_ref: attempt.actor.session_ref.clone(),
        user_ref: attempt.actor.user_id.clone(),
        device: attempt.actor.device.clone(),
        channel: attempt.channel.name().to_string(),
        step_up: attempt.step_up.to_string(),
        result: format!("refused: {}", error.code),
        rtt_ms: None,
        run_id: None,
    };
    if let Err(audit_error) = app.store.with(|conn| app.append_audit(conn, &event)) {
        return ApiError::internal(audit_error);
    }
    error
}

fn unprocessable(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, code, message)
}

/// Why this target cannot be used: the index does not admit it, or it is a room,
/// person or further host the store does not have.
fn target_refusal(app: &App, index: &DeviceIndex, def: &ActionDef, target: &str) -> Result<Option<ApiError>, ApiError> {
    let invalid = |message: String| unprocessable("invalid_target", message);
    if let Err(not_admitted) = def.admits(index, target) {
        if def.waits_for_approval(index, target) {
            return Ok(Some(ApiError::new(
                StatusCode::FORBIDDEN,
                "machine_not_approved",
                "This machine is waiting for the owner to approve it.",
            )));
        }
        if def.targets == Targets::Pending
            && app.store.with(|conn| inventory::machine_status(conn, target))? == Some(MachineStatus::Approved)
        {
            return Ok(Some(ApiError::new(
                StatusCode::CONFLICT,
                "already_approved",
                "This machine is already approved.",
            )));
        }
        return Ok(Some(invalid(not_admitted.0)));
    }
    let missing = || invalid(format!("{target} is not a target of {}", def.id));
    let refusal = app.store.with(|conn| -> rusqlite::Result<Option<ApiError>> {
        Ok(match def.targets {
            Targets::Room => (!rooms::exists(conn, target)?).then(missing),
            Targets::User => (!users::is_active_family(conn, target)?).then(missing),
            Targets::FurtherHost => (!inventory::host_exists(conn, target)?).then(missing),
            Targets::Instance => (!inventory::instance_exists(conn, target)?).then(missing),
            _ => None,
        })
    })?;
    Ok(refusal)
}

fn unknown_scene() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "unknown_scene", "There is no such scene here.")
}

/// The stored scene a scene action names.
fn stored_scene(app: &App, def: &ActionDef, target: &str) -> Result<Option<scenes::Scene>, ApiError> {
    if !catalog::applies_a_scene(def) {
        return Ok(None);
    }
    Ok(app.store.with(|conn| scenes::get(conn, target))?)
}

fn plan_for(app: &App, def: &ActionDef, target: &str, params: &Value) -> Result<Vec<Step>, ApiError> {
    let index = app.registry.index();
    if matches!(def.targets, Targets::Room) {
        return app.store.with(|conn| rooms::plan(conn, &index, def.id, target, params));
    }
    let scene = stored_scene(app, def, target)?;
    if catalog::applies_a_scene(def) && scene.is_none() {
        return Err(unknown_scene());
    }
    if let (Some(scene), "scene.movie") = (&scene, def.id) {
        if let Some(lights) = app.store.with(|conn| rooms::movie_lights(conn, &index))? {
            return Ok(catalog::movie_in_room(scene, &index, &lights));
        }
    }
    let lights = if catalog::reaches_every_light(def) {
        catalog::every_light(&index, &app.store.with(inventory::stored_light_targets)?)
    } else {
        Vec::new()
    };
    let context = catalog::Context { index: &index, lights: &lights, scene: scene.as_ref() };
    catalog::plan(def, target, params, &context).map_err(|invalid| unprocessable("invalid_params", invalid.0))
}

/// Every check that does not depend on the flow: this person may do it, the action
/// exists, the target is allowed, the parameters fit, and the device can be used.
fn validate(app: &App, attempt: &Attempt<'_>) -> Result<Validated, ApiError> {
    let Some(def) = catalog::find(attempt.action) else {
        let error = ApiError::new(StatusCode::NOT_FOUND, "unknown_action", "That action does not exist.");
        return Err(record_refusal(app, attempt, error));
    };
    let index = app.registry.index();
    if !policy::may_run(&index, attempt.actor.role, def.id, attempt.target) {
        let error = ApiError::new(StatusCode::FORBIDDEN, "owner_only", "Only the owner can do this.");
        return Err(record_refusal(app, attempt, error));
    }
    // Voice and Swifty never offer an action a plugin added, and cannot run one.
    if def.is_custom() && matches!(attempt.channel, Channel::Voice | Channel::Assistant) {
        let error = ApiError::new(
            StatusCode::FORBIDDEN,
            "not_available_by_voice",
            "Voice cannot run an action a plugin added.",
        );
        return Err(record_refusal(app, attempt, error));
    }
    if let Some(error) = target_refusal(app, &index, def, attempt.target)? {
        return Err(record_refusal(app, attempt, error));
    }
    let params = match def.validate_params(attempt.params) {
        Ok(params) => params,
        Err(invalid) => {
            return Err(record_refusal(app, attempt, unprocessable("invalid_params", invalid.0)));
        }
    };
    let runs_on = runs_on(app);
    if catalog::takes_console_offline(def, attempt.target, runs_on.as_deref())
        && params.get(ACK_PARAM) != Some(&Value::Bool(true))
    {
        let warning = catalog::console_offline_warning(def);
        let error = unprocessable("acknowledgement_required", warning.clone())
            .with_extra(json!({ "field": ACK_PARAM, "warning": warning }));
        return Err(record_refusal(app, attempt, error));
    }
    match def.executor {
        Executor::Device => {
            let steps = match plan_for(app, def, attempt.target, &params) {
                Ok(steps) => steps,
                Err(error) => return Err(record_refusal(app, attempt, error)),
            };
            for step in &steps {
                let reason = match app.registry.availability(&step.target) {
                    Availability::Disabled(reason) => Some(reason),
                    Availability::Enabled => app.registry.refusal(&step.target, &step.op),
                };
                if let Some(reason) = reason {
                    let error = ApiError::new(StatusCode::CONFLICT, "device_unavailable", reason);
                    return Err(record_refusal(app, attempt, error));
                }
            }
        }
        Executor::PasskeyRevoke => {
            let refusal = app
                .store
                .with(|conn| passkeys::revocation_refusal(conn, attempt.target, &attempt.actor.credential_id))?;
            if let Some(error) = refusal {
                return Err(record_refusal(app, attempt, error));
            }
        }
        Executor::ConsoleOpen => {
            if let Some(error) = console::not_available(app, attempt.target) {
                return Err(record_refusal(app, attempt, error));
            }
            if let Availability::Disabled(reason) = app.registry.availability(attempt.target) {
                let error = ApiError::new(StatusCode::CONFLICT, "device_unavailable", reason);
                return Err(record_refusal(app, attempt, error));
            }
        }
        Executor::AgentPair => {
            let reason = match pairs_agents(app) {
                None => Some(NOTHING_PAIRS_AGENTS.to_string()),
                Some(target) => match app.registry.availability(&target) {
                    Availability::Disabled(reason) => Some(reason),
                    Availability::Enabled => None,
                },
            };
            if let Some(reason) = reason {
                let error = ApiError::new(StatusCode::CONFLICT, "device_unavailable", reason);
                return Err(record_refusal(app, attempt, error));
            }
        }
        Executor::MachineRemove => {
            if runs_on.as_deref() == Some(attempt.target) {
                let error = ApiError::new(
                    StatusCode::CONFLICT,
                    "console_machine",
                    "This console runs on that VM, so it cannot be taken off the list.",
                );
                return Err(record_refusal(app, attempt, error));
            }
        }
        Executor::ProxmoxAddHost => {
            let conflict = match validate_host(app, &params) {
                Err(error) => Some(error),
                Ok(spec) => app.store.with(|conn| inventory::host_conflict(conn, &spec))?.then(|| {
                    ApiError::new(
                        StatusCode::CONFLICT,
                        "host_exists",
                        "A host with that name or address already exists.",
                    )
                }),
            };
            if let Some(error) = conflict {
                return Err(record_refusal(app, attempt, error));
            }
        }
        Executor::PluginAdd | Executor::PluginReconfigure | Executor::PluginRemove => {
            if let Some(error) = plugin_actions::refusal(app, def.executor, attempt.target, &params)? {
                return Err(record_refusal(app, attempt, error));
            }
        }
        Executor::PasskeyRegister
        | Executor::Read
        | Executor::UserInvite
        | Executor::UserRemove
        | Executor::MachineApprove
        | Executor::ProxmoxRemoveHost => {}
    }
    Ok(Validated { def, target: attempt.target.to_string(), params })
}

/// What a confirmation or step-up commits to.
fn binding(action: &str, target: &str, params: &Value, session_ref: &str, prepared_id: &str) -> String {
    let body = json!([crate::domains::BINDING, action, target, params, session_ref, prepared_id]);
    sha256_hex(canonical_json(&body).as_bytes())
}

/// What counts as the same request while one is in flight. A non-idempotent
/// action is the same request whatever its parameters, because a second one
/// would do the thing again.
fn dedupe_key(def: &ActionDef, target: &str, params: &Value) -> String {
    let params = if def.idempotent { params } else { &Value::Null };
    sha256_hex(canonical_json(&json!([def.id, target, params])).as_bytes())
}

fn parse_channel(text: Option<&str>) -> Result<Channel, ApiError> {
    Channel::parse_client(text).ok_or_else(|| unprocessable("invalid_channel", "channel must be panel or palette"))
}

fn check_request_id(request_id: Option<&str>) -> Result<(), ApiError> {
    match request_id {
        Some(id)
            if id.is_empty()
                || id.len() > 64
                || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') =>
        {
            Err(unprocessable("invalid_request_id", "request_id must be 1 to 64 URL-safe characters"))
        }
        _ => Ok(()),
    }
}

// ----- direct execution

pub async fn execute(app: &App, actor: &Actor, request: ActionRequest) -> Result<RunView, ApiError> {
    let channel = parse_channel(request.channel.as_deref())?;
    check_request_id(request.request_id.as_deref())?;
    execute_on_channel(
        app,
        actor,
        &request.action,
        &request.target,
        &request.params,
        channel,
        request.request_id.as_deref(),
    )
    .await
}

/// Runs a tier 0, 1 or S action. Anything higher is refused here: this path
/// has no way to satisfy a confirmation or a step-up.
pub async fn execute_on_channel(
    app: &App,
    actor: &Actor,
    action: &str,
    target: &str,
    params: &Value,
    channel: Channel,
    request_id: Option<&str>,
) -> Result<RunView, ApiError> {
    let attempt = Attempt { actor, channel, action, target, params, step_up: "none" };
    let validated = validate(app, &attempt)?;
    match policy::flow(validated.def.tier) {
        Flow::Direct => {}
        Flow::Confirm => {
            let error = ApiError::new(
                StatusCode::FORBIDDEN,
                "confirmation_required",
                "This action must be prepared and then confirmed.",
            );
            return Err(record_refusal(app, &attempt, error));
        }
        Flow::StepUp => {
            let error = ApiError::new(
                StatusCode::FORBIDDEN,
                "step_up_required",
                "This action must be prepared and then approved with a passkey.",
            );
            return Err(record_refusal(app, &attempt, error));
        }
    }
    if matches!(channel, Channel::Voice | Channel::Assistant)
        && policy::voice_route(validated.def.tier) != policy::VoiceRoute::Execute
    {
        let error = ApiError::new(StatusCode::FORBIDDEN, "not_available_by_voice", "Voice cannot run this.");
        return Err(record_refusal(app, &attempt, error));
    }
    run_action(app, actor, validated, channel, "none", None, request_id).await
}

// ----- prepare

pub fn prepare(app: &App, actor: &Actor, request: ActionRequest) -> Result<PreparedView, ApiError> {
    let channel = parse_channel(request.channel.as_deref())?;
    prepare_on_channel(app, actor, &request.action, &request.target, &request.params, channel)
}

pub fn prepare_on_channel(
    app: &App,
    actor: &Actor,
    action: &str,
    target: &str,
    params: &Value,
    channel: Channel,
) -> Result<PreparedView, ApiError> {
    let attempt = Attempt { actor, channel, action, target, params, step_up: "none" };
    let validated = validate(app, &attempt)?;
    let def = validated.def;
    let flow = policy::flow(def.tier);
    if flow == Flow::Direct {
        let error = ApiError::new(StatusCode::CONFLICT, "not_two_step", "This action runs directly.");
        return Err(record_refusal(app, &attempt, error));
    }
    let now = app.now();
    if !def.idempotent {
        let recent = app.store.with(|conn| recently_run(conn, app, def.id, &validated.target, now))?;
        if recent {
            return Err(record_refusal(app, &attempt, duplicate_recent()));
        }
    }

    let id = random_id();
    let bound = binding(def.id, &validated.target, &validated.params, &actor.session_ref, &id);
    let (token, ttl, needs) = match flow {
        Flow::Confirm => (Some(random_token()), policy::CONFIRMATION_TTL_MS, "tap"),
        _ => (None, policy::CRITICAL_PREPARE_TTL_MS, "passkey"),
    };
    let token_hash = token.as_ref().map(|t| sha256_hex(t.as_bytes()));
    let expires_at = now + ttl;
    let params_text = canonical_json(&validated.params);
    app.store.tx(|tx| -> Result<(), ApiError> {
        tx.execute(
            "INSERT INTO prepared_actions
                 (id, token_hash, action, target, params, binding, tier, session_ref, channel, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                id,
                token_hash,
                def.id,
                validated.target,
                params_text,
                bound,
                def.tier.code(),
                actor.session_ref,
                channel.name(),
                now,
                expires_at
            ],
        )?;
        app.append_audit(tx, &event_for(actor, channel, &validated, "none", "prepared", now, None, None))?;
        Ok(())
    })?;

    let mut text = describe(app, def, &validated.target, &validated.params)?;
    if is_plugin_action(def) {
        plugin_actions::describe(app, def, &validated.target, &validated.params, &mut text)?;
    }
    if def.executor == Executor::PasskeyRevoke {
        if let Some(name) = app.store.with(|conn| passkeys::credential_name(conn, &validated.target))? {
            text.what = format!("Passkey \"{name}\" can no longer sign in to this console.");
            text.affected = vec![format!("{name} passkey")];
            text.target_name = name;
        }
    }
    Ok(PreparedView {
        prepared_id: id,
        action: def.id.to_string(),
        target: validated.target,
        params: validated.params,
        tier: def.tier.wire(),
        tier_name: def.tier.name().to_string(),
        label: text.label,
        target_name: text.target_name,
        what: text.what,
        consequences: text.consequences,
        affected: text.affected,
        warning: text.warning,
        channel: channel.name().to_string(),
        needs: needs.to_string(),
        confirmation: token,
        expires_at,
    })
}

/// What the console shows before this action runs: the device by the name the
/// index has for it, the owner's notes on it, and the stored scene it names.
fn describe(app: &App, def: &ActionDef, target: &str, params: &Value) -> Result<catalog::Description, ApiError> {
    let index = app.registry.index();
    let notes = match index.get(target) {
        Some(_) => app.store.with(|conn| notes::get(conn, target))?,
        None => Vec::new(),
    };
    let scene = stored_scene(app, def, target)?;
    let runs_on = runs_on(app);
    let facts = Facts { index: &index, runs_on: runs_on.as_deref(), notes: &notes, scene: scene.as_ref() };
    Ok(catalog::describe(def, target, params, &facts))
}

#[allow(clippy::too_many_arguments)]
fn event_for(
    actor: &Actor,
    channel: Channel,
    validated: &Validated,
    step_up: &str,
    result: &str,
    ts: i64,
    rtt_ms: Option<i64>,
    run_id: Option<&str>,
) -> Event {
    Event {
        ts,
        action: validated.def.id.to_string(),
        target: validated.target.clone(),
        params: validated.params.clone(),
        credential_ref: actor.credential_id.clone(),
        session_ref: actor.session_ref.clone(),
        user_ref: actor.user_id.clone(),
        device: actor.device.clone(),
        channel: channel.name().to_string(),
        step_up: step_up.to_string(),
        result: result.to_string(),
        rtt_ms,
        run_id: run_id.map(str::to_string),
    }
}

fn duplicate_recent() -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "duplicate_recent", "The same action ran moments ago. It was not sent again.")
}

fn recently_run(conn: &Connection, app: &App, action: &str, target: &str, now: i64) -> rusqlite::Result<bool> {
    let since = now - app.config.timing.duplicate_window_ms;
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM action_runs WHERE action = ?1 AND target = ?2 AND started_at > ?3)",
        params![action, target, since],
        |row| row.get(0),
    )
}

// ----- prepared rows

struct PreparedRow {
    id: String,
    token_hash: Option<String>,
    action: String,
    target: String,
    params: Value,
    binding: String,
    session_ref: String,
    channel: String,
    expires_at: i64,
    consumed_at: Option<i64>,
    run_id: Option<String>,
}

fn load_prepared(conn: &Connection, id: &str) -> Result<Option<PreparedRow>, ApiError> {
    let row = conn
        .query_row(
            "SELECT id, token_hash, action, target, params, binding, session_ref, channel,
                    expires_at, consumed_at, run_id
             FROM prepared_actions WHERE id = ?1",
            params![id],
            |row| {
                Ok((
                    PreparedRow {
                        id: row.get(0)?,
                        token_hash: row.get(1)?,
                        action: row.get(2)?,
                        target: row.get(3)?,
                        params: Value::Null,
                        binding: row.get(5)?,
                        session_ref: row.get(6)?,
                        channel: row.get(7)?,
                        expires_at: row.get(8)?,
                        consumed_at: row.get(9)?,
                        run_id: row.get(10)?,
                    },
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;
    Ok(match row {
        Some((mut prepared, params_text)) => {
            prepared.params = serde_json::from_str(&params_text)?;
            Some(prepared)
        }
        None => None,
    })
}

/// Marks a prepared action used. False means something else got there first.
fn consume(app: &App, id: &str, now: i64) -> rusqlite::Result<bool> {
    app.store.with(|conn| {
        conn.execute(
            "UPDATE prepared_actions SET consumed_at = ?1 WHERE id = ?2 AND consumed_at IS NULL",
            params![now, id],
        )
        .map(|changed| changed == 1)
    })
}

fn restated_binding(
    def: &ActionDef,
    action: &str,
    target: &str,
    params: &Value,
    session_ref: &str,
    id: &str,
) -> Option<String> {
    let clean = def.validate_params(params).ok()?;
    Some(binding(action, target, &clean, session_ref, id))
}

fn unknown_prepared() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "unknown_prepared", "No such prepared action.")
}

fn wrong_session() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "wrong_session", "This was prepared in another session.")
}

fn binding_mismatch() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "binding_mismatch",
        "This does not match what was prepared. Nothing was sent, and the prepared action is cancelled.",
    )
}

fn expired() -> ApiError {
    ApiError::new(StatusCode::GONE, "expired", "This has expired. Prepare it again.")
}

fn already_used(app: &App, prepared: &PreparedRow) -> ApiError {
    let phase = prepared
        .run_id
        .as_deref()
        .and_then(|run_id| app.store.with(|conn| load_run(conn, run_id)).ok().flatten().map(|run| run.phase));
    ApiError::new(StatusCode::CONFLICT, "already_used", "This confirmation was already used. Nothing was sent again.")
        .with_extra(json!({ "run_id": prepared.run_id, "phase": phase }))
}

// ----- step-up

/// Issues the passkey challenge for a prepared critical action. The client
/// restates what it is approving, and that must match what was prepared.
pub fn begin_step_up(app: &App, actor: &Actor, request: StepUpRequest) -> Result<StepUpOptions, ApiError> {
    let now = app.now();
    let Some(prepared) = app.store.with(|conn| load_prepared(conn, &request.prepared_id))? else {
        return Err(unknown_prepared());
    };
    let attempt = Attempt {
        actor,
        channel: Channel::from_stored(&prepared.channel),
        action: &prepared.action,
        target: &prepared.target,
        params: &prepared.params,
        step_up: "none",
    };
    let Some(def) = catalog::find(&prepared.action) else {
        return Err(unknown_prepared());
    };
    if prepared.session_ref != actor.session_ref {
        return Err(record_refusal(app, &attempt, wrong_session()));
    }
    if policy::flow(def.tier) != Flow::StepUp {
        let error =
            ApiError::new(StatusCode::CONFLICT, "step_up_not_applicable", "This action does not use a step-up.");
        return Err(record_refusal(app, &attempt, error));
    }
    if prepared.consumed_at.is_some() {
        return Err(record_refusal(app, &attempt, already_used(app, &prepared)));
    }
    if now >= prepared.expires_at {
        consume(app, &prepared.id, now)?;
        return Err(record_refusal(app, &attempt, expired()));
    }
    let restated =
        restated_binding(def, &request.action, &request.target, &request.params, &actor.session_ref, &prepared.id);
    if restated.as_deref() != Some(prepared.binding.as_str()) {
        consume(app, &prepared.id, now)?;
        return Err(record_refusal(app, &attempt, binding_mismatch()));
    }
    let issued = app.store.tx(|tx| -> Result<_, ApiError> {
        let issued =
            app.passkeys.step_up_start(tx, now, &actor.session_ref, &prepared.id, &prepared.binding, &actor.user_id)?;
        // From here the challenge's own 60 seconds govern the prepared action.
        tx.execute("UPDATE prepared_actions SET expires_at = ?1 WHERE id = ?2", params![issued.1, prepared.id])?;
        Ok(issued)
    });
    let (options, expires_at) = issued.map_err(|error| record_refusal(app, &attempt, error))?;
    Ok(StepUpOptions { prepared_id: prepared.id, options, expires_at })
}

// ----- confirm

/// Completes a prepared action: with the single-use token for tier 2, or a
/// passkey assertion over the bound challenge for tier 3.
pub async fn confirm(app: &App, actor: &Actor, request: ConfirmRequest) -> Result<RunView, ApiError> {
    let now = app.now();
    let Some(prepared) = app.store.with(|conn| load_prepared(conn, &request.prepared_id))? else {
        return Err(unknown_prepared());
    };
    let channel = Channel::from_stored(&prepared.channel);
    let attempt = Attempt {
        actor,
        channel,
        action: &prepared.action,
        target: &prepared.target,
        params: &prepared.params,
        step_up: "none",
    };
    let Some(def) = catalog::find(&prepared.action) else {
        return Err(unknown_prepared());
    };
    if prepared.session_ref != actor.session_ref {
        return Err(record_refusal(app, &attempt, wrong_session()));
    }
    if prepared.consumed_at.is_some() {
        return Err(record_refusal(app, &attempt, already_used(app, &prepared)));
    }
    if policy::flow(def.tier) == Flow::Confirm && now >= prepared.expires_at {
        consume(app, &prepared.id, now)?;
        return Err(record_refusal(app, &attempt, expired()));
    }

    let restated =
        restated_binding(def, &request.action, &request.target, &request.params, &actor.session_ref, &prepared.id);
    if restated.as_deref() != Some(prepared.binding.as_str()) {
        consume(app, &prepared.id, now)?;
        return Err(record_refusal(app, &attempt, binding_mismatch()));
    }

    let mut runner = actor.clone();
    let step_up = match policy::flow(def.tier) {
        Flow::Confirm => {
            let given = request.confirmation.as_deref().map(|t| sha256_hex(t.as_bytes()));
            let matches = match (&given, &prepared.token_hash) {
                (Some(given), Some(stored)) => constant_time_eq(given, stored),
                _ => false,
            };
            if !matches {
                consume(app, &prepared.id, now)?;
                let error =
                    ApiError::new(StatusCode::FORBIDDEN, "invalid_confirmation", "That confirmation is not valid.");
                return Err(record_refusal(app, &attempt, error));
            }
            "confirm"
        }
        Flow::StepUp => {
            let Some(assertion) = request.assertion.as_ref() else {
                let error = ApiError::new(
                    StatusCode::PRECONDITION_REQUIRED,
                    "step_up_required",
                    "This action needs a fresh passkey assertion.",
                );
                return Err(record_refusal(app, &attempt, error));
            };
            let verified = app.passkeys.step_up_finish(
                &app.store,
                now,
                &actor.session_ref,
                &prepared.id,
                &prepared.binding,
                assertion,
                &actor.user_id,
            );
            match verified {
                Ok(verified) => {
                    runner.credential_id = verified.credential_id;
                    runner.device = verified.name;
                }
                Err(error) => {
                    // Another request holds this challenge and is checking its
                    // signature. A repeat is suppressed, and must not cancel the
                    // approval it repeats.
                    if error.code != "challenge_used" {
                        consume(app, &prepared.id, now)?;
                    }
                    return Err(record_refusal(app, &attempt, error));
                }
            }
            "passkey"
        }
        Flow::Direct => {
            let error = ApiError::new(StatusCode::CONFLICT, "not_two_step", "This action runs directly.");
            return Err(record_refusal(app, &attempt, error));
        }
    };

    if !consume(app, &prepared.id, now)? {
        return Err(record_refusal(app, &attempt, already_used(app, &prepared)));
    }

    // Checked again from the stored row: the device or credential may have
    // changed since this was prepared.
    let run_attempt = Attempt { actor: &runner, step_up, ..attempt };
    let validated = validate(app, &run_attempt)?;
    let view = run_action(app, &runner, validated, channel, step_up, Some(&prepared.id), None).await?;
    Ok(view)
}

// ----- running

enum Begin {
    Started(String),
    Duplicate(Box<RunView>),
}

fn begin_run(
    app: &App,
    actor: &Actor,
    validated: &Validated,
    channel: Channel,
    step_up: &str,
    prepared_id: Option<&str>,
    request_id: Option<&str>,
) -> Result<Begin, ApiError> {
    let now = app.now();
    let def = validated.def;
    let key = dedupe_key(def, &validated.target, &validated.params);
    let begun = app.store.tx(|tx| {
        if let Some(request_id) = request_id {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT id FROM action_runs WHERE session_ref = ?1 AND request_id = ?2",
                    params![actor.session_ref, request_id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(existing) = existing {
                let mut view =
                    load_run(tx, &existing)?.ok_or_else(|| ApiError::internal("duplicate run row vanished"))?;
                if view.action != def.id || view.target != validated.target || view.params != validated.params {
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        "request_id_reused",
                        "That request_id was used for a different action.",
                    ));
                }
                view.duplicate = true;
                return Ok(Begin::Duplicate(Box::new(view)));
            }
        }
        let inflight: Option<String> = tx
            .query_row(
                "SELECT id FROM action_runs WHERE dedupe_key = ?1 AND phase = 'commanded'",
                params![key],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(inflight) = inflight {
            let mut view = load_run(tx, &inflight)?.ok_or_else(|| ApiError::internal("in-flight run row vanished"))?;
            // One command reaches the device, but everyone who asked for it is on
            // the record, and can follow the run this answer points them to.
            if !started_run(tx, &inflight, &actor.user_id)? {
                app.append_audit(
                    tx,
                    &event_for(actor, channel, validated, step_up, JOINED_RUN, now, None, Some(&inflight)),
                )?;
            }
            view.duplicate = true;
            return Ok(Begin::Duplicate(Box::new(view)));
        }
        if !def.idempotent && recently_run(tx, app, def.id, &validated.target, now)? {
            return Err(duplicate_recent());
        }
        let id = random_id();
        tx.execute(
            "INSERT INTO action_runs
                 (id, prepared_id, request_id, action, target, params, dedupe_key, session_ref,
                  credential_id, device, channel, step_up, phase, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'commanded', ?13)",
            params![
                id,
                prepared_id,
                request_id,
                def.id,
                validated.target,
                canonical_json(&validated.params),
                key,
                actor.session_ref,
                actor.credential_id,
                actor.device,
                channel.name(),
                step_up,
                now
            ],
        )?;
        if let Some(prepared_id) = prepared_id {
            tx.execute("UPDATE prepared_actions SET run_id = ?1 WHERE id = ?2", params![id, prepared_id])?;
        }
        // Written before any device is touched, in the same transaction as the
        // run. If this fails nothing is commanded.
        app.append_audit(tx, &event_for(actor, channel, validated, step_up, "commanded", now, None, Some(&id)))?;
        Ok(Begin::Started(id))
    })?;
    // A run joined or repeated is not a new phase of anything.
    if let Begin::Started(run_id) = &begun {
        publish_phase(app, run_id, def.id, &validated.target, Phase::Commanded, &actor.user_id);
    }
    Ok(begun)
}

/// Tells the event stream that a run is in a new phase.
fn publish_phase(app: &App, run_id: &str, action: &str, target: &str, phase: Phase, started_by: &str) {
    app.events.publish(events::Event::Run {
        run_id: run_id.to_string(),
        action: action.to_string(),
        target: target.to_string(),
        phase,
        started_by: started_by.to_string(),
    });
}

struct StepReport {
    target: String,
    phase: Phase,
    detail: Option<String>,
}

struct Outcome {
    phase: Phase,
    detail: Option<String>,
    steps: Vec<StepReport>,
    rtt_ms: i64,
}

fn confirm_timeout(app: &App, def: &ActionDef) -> Duration {
    Duration::from_millis(app.config.timing.confirm_timeout_ms.unwrap_or(def.confirm_timeout_ms))
}

fn report(step: &Step, phase: Phase, detail: Option<String>) -> StepReport {
    StepReport { target: step.target.clone(), phase, detail }
}

/// Whether nothing can show this step worked, so it ends "sent": a key press, or a
/// custom action whose manifest says its effect cannot be read.
fn cannot_be_read_back(def: &ActionDef, step: &Step) -> bool {
    step.op.cannot_be_read_back() || def.expect == Some(&Expect::Unreadable)
}

/// Whether `state` shows this step has really taken effect. A custom action is
/// confirmed by its manifest's `Expect`, anything else by the operation's own rule.
fn confirmed_by(def: &ActionDef, step: &Step, state: &State, baseline: Option<&State>) -> bool {
    match (&step.op, def.expect) {
        (Op::Custom { args, .. }, Some(expect)) => expect.is_met(args, state, baseline),
        _ => step.op.is_confirmed_by(state, baseline),
    }
}

/// Sends one step and waits for the device to show it took effect.
async fn perform_step(app: &App, def: &ActionDef, step: &Step, timeout: Duration) -> StepReport {
    let deadline = Instant::now() + timeout;
    let registry = &app.registry;
    let needs_baseline = def.expect.map_or_else(|| step.op.needs_baseline(), Expect::needs_baseline);

    let baseline = if registry.is_live() && needs_baseline {
        match tokio::time::timeout_at(deadline, registry.read_state(&step.target)).await {
            Ok(Ok(state)) => state,
            Ok(Err(error)) => {
                return report(step, Phase::Failed, Some(format!("{error}. Nothing was sent.")));
            }
            Err(_) => {
                return report(
                    step,
                    Phase::Failed,
                    Some("No reply when reading the device first. Nothing was sent.".to_string()),
                );
            }
        }
    } else {
        None
    };

    // Only an idempotent command may be sent again, and only after silence.
    let attempts: u32 = if def.idempotent { 2 } else { 1 };
    let per_attempt = timeout / attempts;
    let mut silence: Option<DeviceError> = None;
    let mut dispatched = None;
    for _ in 0..attempts {
        match tokio::time::timeout(per_attempt, registry.command(&step.target, &step.op)).await {
            Ok(Ok(result)) => {
                dispatched = Some(result);
                break;
            }
            Ok(Err(error)) if error.is_silence() => silence = Some(error),
            Ok(Err(error)) => return report(step, Phase::Failed, Some(error.to_string())),
            Err(_) => silence = Some(DeviceError::Timeout),
        }
    }
    match dispatched {
        None => {
            let detail = silence.unwrap_or(DeviceError::Timeout).to_string();
            return report(step, Phase::NoResponse, Some(detail));
        }
        Some(Dispatched::Simulated) => {
            return report(step, Phase::Simulated, Some(DRY_RUN_DETAIL.to_string()));
        }
        Some(Dispatched::Sent(_)) if step.op.cannot_be_read_back() => {
            return report(step, Phase::Sent, Some("Sent. A key press cannot be read back.".to_string()));
        }
        Some(Dispatched::Sent(_)) if cannot_be_read_back(def, step) => {
            return report(step, Phase::Sent, Some("Sent. Nothing can show that this took effect.".to_string()));
        }
        Some(Dispatched::Sent(_)) => {}
    }

    let poll = Duration::from_millis(app.config.timing.poll_ms);
    let name = registry.index().get(&step.target).map_or_else(|| step.target.clone(), |device| device.name.clone());
    let no_reply = || {
        Some(match step.op {
            // A TV that really is off drops off the network, so this is the usual
            // end of a power off. It is still not a confirmation.
            Op::TvPower { on: false } => format!(
                "{name} accepted power off and then stopped answering. It may have turned off, but nothing could confirm it."
            ),
            // The same goes for a host, and this service may run on it.
            Op::HostShutdown => format!(
                "{name} accepted the shutdown and then stopped answering. It has probably gone down, but nothing could confirm it."
            ),
            Op::HostReboot => {
                format!("{name} accepted the reboot but has not shown a fresh start yet. It can take a few minutes.")
            }
            _ => format!("{name} did not show the change in time."),
        })
    };
    loop {
        match tokio::time::timeout_at(deadline, registry.read_state(&step.target)).await {
            Ok(Ok(Some(state))) if confirmed_by(def, step, &state, baseline.as_ref()) => {
                return report(step, Phase::Confirmed, None);
            }
            Ok(Err(error)) if !error.is_silence() && !step.op.takes_the_device_away() => {
                return report(step, Phase::Failed, Some(error.to_string()));
            }
            Err(_) => return report(step, Phase::NoResponse, no_reply()),
            _ => {}
        }
        if Instant::now() + poll >= deadline {
            return report(step, Phase::NoResponse, no_reply());
        }
        tokio::time::sleep(poll).await;
    }
}

async fn perform(app: &App, def: &ActionDef, steps: &[Step]) -> Outcome {
    let started = Instant::now();
    let timeout = confirm_timeout(app, def);
    // A light among several steps that does not answer says nothing about the next
    // step, so it is passed over. Anything else that goes wrong ends the run.
    let several = steps.len() > 1;
    let mut stopped = false;
    let mut reports = Vec::with_capacity(steps.len());
    let mut phase = Phase::Confirmed;
    let mut detail = None;
    for step in steps {
        let result = perform_step(app, def, step, timeout).await;
        let passed_over = several && step.op.is_light_op() && result.phase == Phase::NoResponse;
        stopped = !passed_over && !matches!(result.phase, Phase::Confirmed | Phase::Simulated | Phase::Sent);
        phase = result.phase;
        detail = result.detail.clone();
        reports.push(result);
        if stopped {
            break;
        }
    }
    if !stopped {
        let silent: Vec<&StepReport> = reports.iter().filter(|report| report.phase == Phase::NoResponse).collect();
        if let Some(first) = silent.first() {
            if reports.iter().any(|report| report.phase == Phase::Confirmed) {
                phase = Phase::Confirmed;
                let lights = steps.iter().filter(|step| step.op.is_light_op()).count();
                detail = Some(format!("{} of {lights} lights did not answer.", silent.len()));
            } else {
                phase = Phase::NoResponse;
                detail = first.detail.clone();
            }
        }
    }
    Outcome { phase, detail, steps: reports, rtt_ms: started.elapsed().as_millis() as i64 }
}

fn steps_json(steps: &[StepReport]) -> Value {
    Value::Array(
        steps.iter().map(|s| json!({ "target": s.target, "phase": s.phase.as_str(), "detail": s.detail })).collect(),
    )
}

/// Records how a run ended, in its row and in the audit log, and then tells the
/// event stream.
fn finalize(app: &App, run_id: &str, outcome: &Outcome) -> Result<RunView, ApiError> {
    let now = app.now();
    let (view, started_by) = app.store.tx(|tx| {
        tx.execute(
            "UPDATE action_runs SET phase = ?1, detail = ?2, steps = ?3, finished_at = ?4, rtt_ms = ?5
             WHERE id = ?6",
            params![
                outcome.phase.as_str(),
                outcome.detail,
                steps_json(&outcome.steps).to_string(),
                now,
                outcome.rtt_ms,
                run_id
            ],
        )?;
        let view = load_run(tx, run_id)?.ok_or_else(|| ApiError::internal("run row vanished"))?;
        let (credential_ref, session_ref, device, user_ref): (String, String, String, String) = tx.query_row(
            "SELECT r.credential_id, r.session_ref, r.device, COALESCE(c.user_id, '-')
             FROM action_runs r LEFT JOIN credentials c ON c.id = r.credential_id WHERE r.id = ?1",
            params![run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        app.append_audit(
            tx,
            &Event {
                ts: now,
                action: view.action.clone(),
                target: view.target.clone(),
                params: view.params.clone(),
                credential_ref,
                session_ref,
                user_ref: user_ref.clone(),
                device,
                channel: view.channel.clone(),
                step_up: view.step_up.clone(),
                result: outcome.phase.as_str().to_string(),
                rtt_ms: Some(outcome.rtt_ms),
                run_id: Some(run_id.to_string()),
            },
        )?;
        Ok::<_, ApiError>((view, user_ref))
    })?;
    publish_phase(app, run_id, &view.action, &view.target, outcome.phase, &started_by);
    Ok(view)
}

pub fn load_run(conn: &Connection, id: &str) -> rusqlite::Result<Option<RunView>> {
    conn.query_row(
        "SELECT id, action, target, params, channel, step_up, phase, detail, steps, started_at,
                finished_at, rtt_ms
         FROM action_runs WHERE id = ?1",
        params![id],
        run_from_row,
    )
    .optional()
}

pub fn recent_runs(conn: &Connection, limit: i64) -> rusqlite::Result<Vec<RunView>> {
    let mut statement = conn.prepare(
        "SELECT id, action, target, params, channel, step_up, phase, detail, steps, started_at,
                finished_at, rtt_ms
         FROM action_runs ORDER BY started_at DESC, rowid DESC LIMIT ?1",
    )?;
    let rows = statement.query_map(params![limit], run_from_row)?;
    rows.collect()
}

/// One person's own runs.
pub fn recent_runs_of(conn: &Connection, user_id: &str, limit: i64) -> rusqlite::Result<Vec<RunView>> {
    let mut statement = conn.prepare(
        "SELECT id, action, target, params, channel, step_up, phase, detail, steps, started_at,
                finished_at, rtt_ms
         FROM action_runs
         WHERE credential_id IN (SELECT id FROM credentials WHERE user_id = ?1)
         ORDER BY started_at DESC, rowid DESC LIMIT ?2",
    )?;
    let rows = statement.query_map(params![user_id, limit], run_from_row)?;
    rows.collect()
}

fn started_run(conn: &Connection, run_id: &str, user_id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM action_runs r JOIN credentials c ON c.id = r.credential_id
                       WHERE r.id = ?1 AND c.user_id = ?2)",
        params![run_id, user_id],
        |row| row.get(0),
    )
}

/// Whether this person may follow this run: they started it, or their own request
/// was joined to it, which the audit log records.
pub fn run_belongs_to(conn: &Connection, run_id: &str, user_id: &str) -> rusqlite::Result<bool> {
    if started_run(conn, run_id, user_id)? {
        return Ok(true);
    }
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM audit_log WHERE run_id = ?1 AND user_ref = ?2 AND result = ?3)",
        params![run_id, user_id, JOINED_RUN],
        |row| row.get(0),
    )
}

fn run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunView> {
    let params_text: String = row.get(3)?;
    let phase: String = row.get(6)?;
    let steps_text: Option<String> = row.get(8)?;
    let action: String = row.get(1)?;
    Ok(RunView {
        run_id: Some(row.get(0)?),
        warning: catalog::node_power_warning(&action).map(str::to_string),
        action,
        target: row.get(2)?,
        params: serde_json::from_str(&params_text).unwrap_or(Value::Null),
        channel: row.get(4)?,
        step_up: row.get(5)?,
        simulated: phase == "simulated",
        phase,
        detail: row.get(7)?,
        steps: steps_text.and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_else(|| json!([])),
        started_at: row.get(9)?,
        finished_at: row.get(10)?,
        rtt_ms: row.get(11)?,
        duplicate: false,
        state: None,
        registration: None,
        console: None,
        invitation: None,
        pairing: None,
        setup: None,
    })
}

async fn run_action(
    app: &App,
    actor: &Actor,
    validated: Validated,
    channel: Channel,
    step_up: &str,
    prepared_id: Option<&str>,
    request_id: Option<&str>,
) -> Result<RunView, ApiError> {
    match validated.def.executor {
        Executor::Read => run_read(app, actor, validated, channel).await,
        Executor::Device => run_device(app, actor, validated, channel, step_up, prepared_id, request_id).await,
        Executor::ConsoleOpen => run_console(app, actor, validated, channel, step_up, prepared_id).await,
        Executor::AgentPair => run_agent_pair(app, actor, validated, channel, step_up, prepared_id).await,
        Executor::PasskeyRegister
        | Executor::PasskeyRevoke
        | Executor::UserInvite
        | Executor::UserRemove
        | Executor::MachineApprove
        | Executor::MachineRemove
        | Executor::ProxmoxAddHost
        | Executor::ProxmoxRemoveHost => run_local(app, actor, validated, channel, step_up, prepared_id),
        Executor::PluginAdd | Executor::PluginReconfigure | Executor::PluginRemove => {
            run_plugin(app, actor, validated, channel, step_up, prepared_id).await
        }
    }
}

/// Whether this action adds, sets up again or removes an integration.
fn is_plugin_action(def: &ActionDef) -> bool {
    matches!(def.executor, Executor::PluginAdd | Executor::PluginReconfigure | Executor::PluginRemove)
}

/// Adds, sets up again or removes an integration (`plugin_actions`). The setup it
/// opens goes only to this response; the session that confirmed it drives it.
async fn run_plugin(
    app: &App,
    actor: &Actor,
    validated: Validated,
    channel: Channel,
    step_up: &str,
    prepared_id: Option<&str>,
) -> Result<RunView, ApiError> {
    let run_id = match begin_run(app, actor, &validated, channel, step_up, prepared_id, None)? {
        Begin::Started(run_id) => run_id,
        Begin::Duplicate(view) => return Ok(*view),
    };
    let opener =
        setup::Opener { actor: actor.clone(), channel: channel.name().to_string(), step_up: step_up.to_string() };
    match plugin_actions::execute(app, &opener, validated.def.executor, &validated.target, &validated.params).await {
        Ok(done) => {
            let mut view = finish_quietly(app, &run_id, Phase::Confirmed, Some(done.detail))?;
            view.setup = done.setup;
            Ok(view)
        }
        Err(error) => {
            finish_quietly(app, &run_id, Phase::Failed, Some(error.message.clone()))?;
            Err(error)
        }
    }
}

/// How a driver's answer ends a run.
fn failure_outcome(error: &swifty_plugin_api::DeviceError) -> (Phase, Option<String>) {
    let phase = if error.is_silence() { Phase::NoResponse } else { Phase::Failed };
    (phase, Some(error.to_string()))
}

fn finish_quietly(app: &App, run_id: &str, phase: Phase, detail: Option<String>) -> Result<RunView, ApiError> {
    finalize(app, run_id, &Outcome { phase, detail, steps: Vec::new(), rtt_ms: 0 })
}

/// Asks Proxmox for a VNC proxy for one approved VM, and issues the ticket that
/// will open the websocket. The ticket and the VNC credential go to this one
/// response and are not written anywhere.
async fn run_console(
    app: &App,
    actor: &Actor,
    validated: Validated,
    channel: Channel,
    step_up: &str,
    prepared_id: Option<&str>,
) -> Result<RunView, ApiError> {
    let run_id = match begin_run(app, actor, &validated, channel, step_up, prepared_id, None)? {
        Begin::Started(run_id) => run_id,
        Begin::Duplicate(view) => return Ok(*view),
    };
    match app.registry.open_console(&validated.target).await {
        Ok(None) => finish_quietly(app, &run_id, Phase::Simulated, Some(DRY_RUN_DETAIL.to_string())),
        Err(error) => {
            let (phase, detail) = failure_outcome(&error);
            finish_quietly(app, &run_id, phase, detail)
        }
        Ok(Some((instance, grant))) => {
            let credential = grant.ticket.expose().to_string();
            let issued = app.consoles.issue(app.now(), &actor.session_ref, &validated.target, &instance, grant);
            let (ticket, expires_at) = match issued {
                Ok(issued) => issued,
                Err(error) => {
                    finish_quietly(app, &run_id, Phase::Failed, Some(error.message.clone()))?;
                    return Err(error);
                }
            };
            let mut view = finish_quietly(app, &run_id, Phase::Confirmed, None)?;
            view.console = Some(json!({
                "ticket": ticket,
                "expires_at": expires_at,
                "path": format!("{}/console/connect", crate::http::API_PREFIX),
                "subprotocols": ["binary", format!("{}{ticket}", console::TICKET_PROTOCOL_PREFIX)],
                "vnc_credential": credential,
            }));
            Ok(view)
        }
    }
}

/// Starts the collector's own pairing flow. What it answers goes only to this
/// response.
async fn run_agent_pair(
    app: &App,
    actor: &Actor,
    validated: Validated,
    channel: Channel,
    step_up: &str,
    prepared_id: Option<&str>,
) -> Result<RunView, ApiError> {
    let name = validated.params.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
    let run_id = match begin_run(app, actor, &validated, channel, step_up, prepared_id, None)? {
        Begin::Started(run_id) => run_id,
        Begin::Duplicate(view) => return Ok(*view),
    };
    let target = pairs_agents(app).unwrap_or_else(|| "-".to_string());
    match app.registry.pair_agent(&target, &name).await {
        Ok(None) => finish_quietly(app, &run_id, Phase::Simulated, Some(DRY_RUN_DETAIL.to_string())),
        Ok(Some(pairing)) => {
            let mut view = finish_quietly(app, &run_id, Phase::Confirmed, None)?;
            view.pairing = Some(pairing);
            Ok(view)
        }
        Err(error) => {
            let (phase, detail) = failure_outcome(&error);
            finish_quietly(app, &run_id, phase, detail)
        }
    }
}

async fn run_device(
    app: &App,
    actor: &Actor,
    validated: Validated,
    channel: Channel,
    step_up: &str,
    prepared_id: Option<&str>,
    request_id: Option<&str>,
) -> Result<RunView, ApiError> {
    let steps = plan_for(app, validated.def, &validated.target, &validated.params)?;
    if let [step] = steps.as_slice() {
        if cannot_be_read_back(validated.def, step) {
            return run_press(app, actor, validated, channel, step_up, step.clone()).await;
        }
    }
    let begun = begin_run(app, actor, &validated, channel, step_up, prepared_id, request_id);
    let run_id = match begun {
        Ok(Begin::Started(run_id)) => run_id,
        Ok(Begin::Duplicate(view)) => return Ok(*view),
        Err(error) if error.code == "duplicate_recent" => {
            let attempt = Attempt {
                actor,
                channel,
                action: validated.def.id,
                target: &validated.target,
                params: &validated.params,
                step_up,
            };
            return Err(record_refusal(app, &attempt, error));
        }
        Err(error) => return Err(error),
    };

    // The work continues if the client goes away, so a run always ends in a
    // recorded result.
    let (sender, receiver) = oneshot::channel();
    let task_app = app.clone();
    let def = validated.def;
    let task_run = run_id.clone();
    tokio::spawn(async move {
        let outcome = perform(&task_app, def, &steps).await;
        let view = finalize(&task_app, &task_run, &outcome);
        if let Err(error) = &view {
            tracing::error!(run = %task_run, message = %error.message, "could not record a run's result");
        }
        let _ = sender.send(view);
    });

    let wait = Duration::from_millis(app.config.timing.sync_wait_ms);
    match tokio::time::timeout(wait, receiver).await {
        Ok(Ok(view)) => view,
        // Still in flight. The client follows it at /api/v1/actions/runs/{id}.
        _ => app.store.with(|conn| load_run(conn, &run_id))?.ok_or_else(|| ApiError::internal("run row vanished")),
    }
}

/// One press of a button, or one custom action that nothing can read back. It leaves
/// no run row: two presses in a row are two presses, so there is nothing for a later
/// request to join or be refused as a repeat of, and nothing can read a press back.
/// Both audit rows are still written, the first before the device is touched. Sent
/// once, and the result is recorded if the client goes away.
async fn run_press(
    app: &App,
    actor: &Actor,
    validated: Validated,
    channel: Channel,
    step_up: &str,
    step: Step,
) -> Result<RunView, ApiError> {
    let started_at = app.now();
    let step_up = step_up.to_string();
    let commanded = event_for(actor, channel, &validated, &step_up, "commanded", started_at, None, None);
    app.store.with(|conn| app.append_audit(conn, &commanded))?;
    let (task_app, task_actor) = (app.clone(), actor.clone());
    let task = tokio::spawn(async move {
        let outcome = perform(&task_app, validated.def, std::slice::from_ref(&step)).await;
        let finished_at = task_app.now();
        let phase = outcome.phase.as_str();
        let event =
            event_for(&task_actor, channel, &validated, &step_up, phase, finished_at, Some(outcome.rtt_ms), None);
        task_app.store.with(|conn| task_app.append_audit(conn, &event))?;
        Ok::<RunView, ApiError>(RunView {
            run_id: None,
            action: validated.def.id.to_string(),
            target: validated.target,
            params: validated.params,
            channel: channel.name().to_string(),
            step_up,
            phase: phase.to_string(),
            simulated: outcome.phase == Phase::Simulated,
            detail: outcome.detail,
            steps: steps_json(&outcome.steps),
            started_at,
            finished_at: Some(finished_at),
            rtt_ms: Some(outcome.rtt_ms),
            duplicate: false,
            state: None,
            registration: None,
            warning: None,
            console: None,
            invitation: None,
            pairing: None,
            setup: None,
        })
    });
    task.await.map_err(|_| ApiError::internal("a key press did not finish"))?
}

fn run_local(
    app: &App,
    actor: &Actor,
    validated: Validated,
    channel: Channel,
    step_up: &str,
    prepared_id: Option<&str>,
) -> Result<RunView, ApiError> {
    let now = app.now();
    let def = validated.def;
    let run_id = match begin_run(app, actor, &validated, channel, step_up, prepared_id, None)? {
        Begin::Started(run_id) => run_id,
        Begin::Duplicate(view) => return Ok(*view),
    };

    let mut registration = None;
    let mut invitation = None;
    let result: Result<Option<String>, ApiError> = match def.executor {
        Executor::UserInvite => {
            let name = validated.params.get("name").and_then(Value::as_str).unwrap_or_default();
            app.store
                .with(|conn| users::create_invitation(conn, now, name, &actor.user_id))
                .map(|(token, created)| {
                    invitation = Some(json!({
                        "id": created.id,
                        "name": created.name,
                        "token": token,
                        "expires_at": created.expires_at,
                    }));
                    Some("Invitation created. It works once.".to_string())
                })
                .map_err(ApiError::from)
        }
        Executor::UserRemove => app.store.tx(|tx| {
            let ended = users::remove_family(tx, now, &validated.target)?;
            Ok(Some(format!("Removed. {ended} session(s) ended.")))
        }),
        Executor::MachineApprove => app
            .store
            .with(|conn| inventory::approve(conn, now, &validated.target, &actor.user_id))
            .map_err(ApiError::from)
            .and_then(|changed| match changed {
                true => Ok(Some("Approved.".to_string())),
                false => {
                    Err(ApiError::new(StatusCode::CONFLICT, "already_approved", "This machine is already approved."))
                }
            }),
        Executor::MachineRemove => app
            .store
            .tx(|tx| -> Result<_, ApiError> { Ok(inventory::remove_machine(tx, &validated.target)?) })
            .map(|_| Some("Removed from the list.".to_string())),
        Executor::ProxmoxAddHost => validate_host(app, &validated.params).and_then(|spec| {
            app.registry
                .add_host(&spec)
                .map_err(|reason| ApiError::new(StatusCode::CONFLICT, "host_unusable", reason))?;
            app.store.with(|conn| inventory::store_host(conn, now, &spec, &actor.user_id))?;
            Ok(Some("Added. VMs seen there are listed as pending.".to_string()))
        }),
        Executor::ProxmoxRemoveHost => app
            .store
            .tx(|tx| -> Result<_, ApiError> { Ok(inventory::remove_host(tx, &validated.target)?) })
            .map(|machines| {
                app.registry.remove_host(&validated.target);
                Some(format!("Removed, with {machines} machine(s) listed on it."))
            }),
        Executor::PasskeyRevoke => app.store.tx(|tx| {
            if let Some(error) = passkeys::revocation_refusal(tx, &validated.target, &actor.credential_id)? {
                return Err(error);
            }
            let ended = passkeys::revoke_credential(tx, now, &validated.target)?;
            Ok(Some(format!("Revoked. {ended} session(s) ended.")))
        }),
        Executor::PasskeyRegister => {
            let name = validated.params.get("name").and_then(Value::as_str).unwrap_or_default();
            let recovery = validated.params.get("recovery").and_then(Value::as_bool).unwrap_or(false);
            app.store.tx(|tx| app.passkeys.register_start(tx, now, &actor.session_ref, name, recovery)).map(
                |(ceremony_id, options, expires_at)| {
                    registration = Some(json!({
                        "ceremony_id": ceremony_id,
                        "options": options,
                        "expires_at": expires_at,
                    }));
                    Some("Step-up accepted. The registration window is open.".to_string())
                },
            )
        }
        _ => Err(ApiError::internal("not a local action")),
    };

    let (phase, detail, failure) = match result {
        Ok(detail) => (Phase::Confirmed, detail, None),
        Err(error) => (Phase::Failed, Some(error.message.clone()), Some(error)),
    };
    let outcome = Outcome { phase, detail, steps: Vec::new(), rtt_ms: 0 };
    let mut view = finalize(app, &run_id, &outcome)?;
    if matches!(def.executor, Executor::MachineApprove | Executor::MachineRemove | Executor::ProxmoxRemoveHost) {
        app.sync_directory()?;
    }
    // Their sessions ended with the passkey or the person: their streams end now.
    if matches!(def.executor, Executor::PasskeyRevoke | Executor::UserRemove) && failure.is_none() {
        app.events.recheck_sessions();
    }
    if let Some(error) = failure {
        return Err(error);
    }
    view.registration = registration;
    view.invitation = invitation;
    Ok(view)
}

async fn run_read(app: &App, actor: &Actor, validated: Validated, channel: Channel) -> Result<RunView, ApiError> {
    let now = app.now();
    let simulated = !app.registry.is_live();
    let targets: Vec<String> = if validated.target == "all_vms" {
        app.store.with(inventory::approved_targets)?
    } else {
        catalog::expand_target(&app.registry.index(), &validated.target)
    };
    let reads = read_devices(app, targets, ReadBy::Person).await;
    let (phase, detail) =
        if simulated { (Phase::Simulated, Some(DRY_RUN_DETAIL.to_string())) } else { phase_of_reads(&reads) };
    let mut state = Map::new();
    let mut steps = Vec::with_capacity(reads.len());
    for read in reads {
        state.insert(read.report.target.clone(), read.value);
        steps.push(read.report);
    }
    if let Some(metric) = validated.params.get("metric") {
        state.insert("metric".to_string(), metric.clone());
    }
    app.store.with(|conn| {
        app.append_audit(conn, &event_for(actor, channel, &validated, "none", phase.as_str(), now, None, None))
    })?;
    Ok(RunView {
        run_id: None,
        action: validated.def.id.to_string(),
        target: validated.target,
        params: validated.params,
        channel: channel.name().to_string(),
        step_up: "none".to_string(),
        phase: phase.as_str().to_string(),
        simulated,
        detail,
        steps: steps_json(&steps),
        started_at: now,
        finished_at: Some(app.now()),
        rtt_ms: None,
        duplicate: false,
        state: Some(Value::Object(state)),
        registration: None,
        warning: None,
        console: None,
        invitation: None,
        pairing: None,
        setup: None,
    })
}

// ----- reads

const DRY_RUN_DETAIL: &str = crate::runtime::simulated::NOTHING_CONTACTED;

/// Who wants this state. A person's request may open the first connection to a
/// device, and a background poll is not allowed to when the device's driver says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadBy {
    Person,
    Poll,
}

/// One device's state, as the client sees it and as the result of a read.
struct DeviceRead {
    value: Value,
    report: StepReport,
}

async fn read_device(app: &App, target: &str, by: ReadBy) -> DeviceRead {
    let report = |phase, detail: Option<String>| StepReport { target: target.to_string(), phase, detail };
    let index = app.registry.index();
    let Some(info) = index.get(target).filter(|device| !device.pending) else {
        return DeviceRead {
            value: json!({ "available": false, "error": "unknown device" }),
            report: report(Phase::Failed, Some("unknown device".to_string())),
        };
    };
    let base = json!({ "target": info.target, "name": info.name, "plugin": info.plugin, "instance": info.instance });
    let merge = |extra: Value| {
        let mut out = base.clone();
        if let (Some(out), Some(extra)) = (out.as_object_mut(), extra.as_object()) {
            out.extend(extra.clone());
        }
        out
    };
    let unavailable = |phase, reason: String| DeviceRead {
        value: merge(json!({ "available": false, "error": reason })),
        report: report(phase, Some(reason)),
    };
    if let Availability::Disabled(reason) = app.registry.availability(target) {
        return unavailable(Phase::Failed, reason);
    }
    if by == ReadBy::Poll {
        if let Some(reason) = app.registry.poll_refusal(target) {
            // Nothing is wrong with it yet, so the page can still offer its controls.
            let mut read = unavailable(Phase::Failed, reason);
            read.value["not_contacted"] = json!(true);
            return read;
        }
    }
    match tokio::time::timeout(READ_TIMEOUT, app.registry.read_state(target)).await {
        Ok(Ok(Some(state))) => DeviceRead {
            value: merge(json!({ "available": true, "state": state })),
            report: report(Phase::Confirmed, None),
        },
        Ok(Ok(None)) => DeviceRead {
            value: merge(json!({ "available": false, "simulated": true, "error": DRY_RUN_DETAIL })),
            report: report(Phase::Simulated, Some(DRY_RUN_DETAIL.to_string())),
        },
        Ok(Err(error)) if error.is_silence() => unavailable(Phase::NoResponse, error.to_string()),
        Ok(Err(error)) => unavailable(Phase::Failed, error.to_string()),
        Err(_) => unavailable(Phase::NoResponse, "no reply from the device".to_string()),
    }
}

/// The result of a read: confirmed only if every device gave its state.
fn phase_of_reads(reads: &[DeviceRead]) -> (Phase, Option<String>) {
    if reads.is_empty() {
        return (Phase::Failed, Some("No device could be read.".to_string()));
    }
    let with_phase = |phase: Phase| reads.iter().map(|read| &read.report).find(|report| report.phase == phase);
    match with_phase(Phase::Failed).or_else(|| with_phase(Phase::NoResponse)) {
        Some(report) => {
            (report.phase, Some(format!("{}: {}", report.target, report.detail.as_deref().unwrap_or("no answer"))))
        }
        None => (Phase::Confirmed, None),
    }
}

async fn read_devices(app: &App, targets: Vec<String>, by: ReadBy) -> Vec<DeviceRead> {
    let mut tasks = JoinSet::new();
    let unique: HashSet<String> = targets.into_iter().collect();
    for target in unique {
        let app = app.clone();
        tasks.spawn(async move { read_device(&app, &target, by).await });
    }
    let mut reads = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        if let Ok(read) = joined {
            reads.push(read);
        }
    }
    reads.sort_by(|a, b| a.report.target.cmp(&b.report.target));
    reads
}

/// The state of one device, with its availability, for a route that a page polls.
pub async fn read_one(app: &App, target: &str) -> Value {
    read_device(app, target, ReadBy::Poll).await.value
}

/// The state of several devices, for a route that a page polls.
pub async fn read_many(app: &App, targets: Vec<String>) -> Map<String, Value> {
    read_devices(app, targets, ReadBy::Poll).await.into_iter().map(|read| (read.report.target, read.value)).collect()
}

/// After a restart, runs that were in flight can never finish. They are closed
/// as `no_response` rather than being left open or reported as success.
pub fn recover_interrupted_runs(app: &App) -> rusqlite::Result<()> {
    let interrupted: Vec<String> = app.store.with(|conn| {
        let mut statement = conn.prepare("SELECT id FROM action_runs WHERE phase = 'commanded'")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<Vec<String>>>()
    })?;
    for run_id in interrupted {
        let outcome = Outcome {
            phase: Phase::NoResponse,
            detail: Some("The service restarted before the device confirmed.".to_string()),
            steps: Vec::new(),
            rtt_ms: 0,
        };
        if let Err(error) = finalize(app, &run_id, &outcome) {
            tracing::error!(run = %run_id, message = %error.message, "could not close interrupted run");
        }
    }
    Ok(())
}
