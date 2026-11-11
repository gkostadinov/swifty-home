//! The catalog, running actions, the runs, and the audit log.
//!
//! An action runs in one of three flows, which its catalog entry names (`flow`):
//! `direct` (`POST /actions/execute`), `prepare_confirm` (`POST /actions/prepare`,
//! then `POST /actions/confirm` with the single-use confirmation) and
//! `prepare_step_up` (prepare, `POST /auth/step-up/start`, then confirm with a
//! passkey assertion bound to the action). `docs/api/README.md` walks through all three.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use swifty_plugin_api::{CapKind, DeviceClass, ParamKind};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::guards::{Authed, Body, Owner};
use super::{actor, RouterState};
use crate::actions::{self, ActionRequest, ConfirmRequest, Phase, PreparedView, RunView};
use crate::app::App;
use crate::audit;
use crate::catalog::{self, ActionDef, Targets};
use crate::error::ApiError;
use crate::inventory;
use crate::passkeys;
use crate::policy::{self, Role};
use crate::rooms;
use crate::users;

pub(crate) fn routes() -> OpenApiRouter<RouterState> {
    OpenApiRouter::new()
        .routes(routes!(catalog_list))
        .routes(routes!(action_execute))
        .routes(routes!(action_prepare))
        .routes(routes!(action_confirm))
        .routes(routes!(runs_list))
        .routes(routes!(run_get))
        .routes(routes!(audit_read))
        .routes(routes!(audit_verify))
}

// ----- catalog

/// The actions this person may run.
#[derive(Debug, Serialize, ToSchema)]
pub struct Catalog {
    pub actions: Vec<CatalogEntry>,
}

/// One action of the catalog, and the targets this person may aim it at now.
#[derive(Debug, Serialize, ToSchema)]
pub struct CatalogEntry {
    /// The action's id, such as `light.power`.
    pub id: &'static str,
    /// Ids it was known by before (`bulb.power`). Accepted on input for the life of v1,
    /// answered with `id`.
    pub aliases: Vec<&'static str>,
    pub label: &'static str,
    /// 0 read, 1 comfort, 2 consequential, 3 critical, or `"S"`: toward safety.
    #[schema(value_type = TierWire)]
    pub tier: Value,
    /// `read`, `comfort`, `consequential`, `critical` or `safe_direction`.
    pub tier_name: &'static str,
    /// How it runs: `direct`, `prepare_confirm` or `prepare_step_up`.
    pub flow: &'static str,
    /// What kind of target it takes, such as `class`, `fixed`, `room` or `instance`.
    pub target_kind: &'static str,
    /// The class of device it takes, when it takes devices of one class.
    pub class: Option<DeviceClass>,
    /// What a device must be able to do for this action, when that narrows it.
    pub requires: Option<CapKind>,
    /// The targets this person may use now.
    pub targets: Vec<String>,
    pub params: Vec<CatalogParam>,
    /// What will happen.
    pub description: &'static str,
    /// What follows from it.
    pub consequences: Vec<&'static str>,
    /// False means a repeat would do the thing twice, so it is never retried.
    pub idempotent: bool,
}

/// An action's tier on the wire: a number from 0 to 3, or `"S"`.
#[derive(Serialize, ToSchema)]
#[serde(untagged)]
pub enum TierWire {
    /// 0 read, 1 comfort, 2 consequential, 3 critical.
    Number(u8),
    /// `S`: moves the system toward safety.
    Safe(String),
}

/// How one device answered a run.
#[derive(Serialize, ToSchema)]
pub struct RunStep {
    pub target: String,
    /// `confirmed`, `no_response`, `failed`, `simulated` or `sent`.
    pub phase: String,
    pub detail: Option<String>,
}

/// A passkey registration a `passkey.register` run opened: hand `options` to
/// `navigator.credentials.create`, then send what it gives back with `ceremony_id`
/// to `POST /passkeys/register/finish` before `expires_at`.
#[derive(Serialize, ToSchema)]
pub struct PasskeyRegistration {
    pub ceremony_id: String,
    /// WebAuthn creation options.
    pub options: Value,
    pub expires_at: i64,
}

/// One parameter of an action.
#[derive(Debug, Serialize, ToSchema)]
pub struct CatalogParam {
    /// Its name in the action's `params`.
    pub name: &'static str,
    pub required: bool,
    /// One sentence for the person filling it in.
    pub help: &'static str,
    pub schema: ParamSchema,
}

/// What a parameter may hold.
#[derive(Debug, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ParamSchema {
    /// `true` or `false`.
    Boolean,
    /// A whole number from `min` to `max`.
    Integer { min: i64, max: i64 },
    /// One of these words.
    Enum { values: Vec<&'static str> },
    /// A short label: printable characters, at most `max_len` of them.
    Label { max_len: usize },
}

impl CatalogEntry {
    pub fn new(def: &ActionDef, targets: &[String]) -> Self {
        let params = def
            .params
            .iter()
            .map(|param| CatalogParam {
                name: param.name,
                required: param.required,
                help: param.help,
                schema: match param.kind {
                    ParamKind::Bool => ParamSchema::Boolean,
                    ParamKind::Int { min, max } => ParamSchema::Integer { min, max },
                    ParamKind::Enum(values) => ParamSchema::Enum { values: values.to_vec() },
                    ParamKind::Label { max_len } => ParamSchema::Label { max_len },
                },
            })
            .collect();
        Self {
            id: def.id,
            aliases: def.aliases.to_vec(),
            label: def.label,
            tier: def.tier.wire(),
            tier_name: def.tier.name(),
            flow: policy::flow(def.tier).name(),
            target_kind: def.targets.kind_name(),
            class: def.targets.class(),
            requires: def.targets.requires(),
            targets: targets.to_vec(),
            params,
            description: def.description,
            consequences: def.consequences.to_vec(),
            idempotent: def.idempotent,
        }
    }
}

/// The actions this person may run.
///
/// Each entry lists the targets this person may aim it at now: devices from the
/// index, rooms, people, passkeys, further hosts and plugin instances. An action is
/// listed when there is something this person may do with it now, or would be once
/// there is a target for it.
#[utoipa::path(
    get,
    path = "/catalog",
    tag = "actions",
    security(("session" = [])),
    responses((status = 200, body = Catalog))
)]
async fn catalog_list(State(app): State<App>, Authed(session): Authed) -> Result<Json<Catalog>, ApiError> {
    let role = session.person.role;
    let index = app.registry.index();
    let stored = app.store.with(|conn| -> Result<StoredTargets, ApiError> {
        Ok(StoredTargets {
            rooms: rooms::list(conn, &index, role)?.into_iter().map(|room| room.id).collect(),
            users: users::list_users(conn)?
                .into_iter()
                .filter(|user| user.role == Role::Family)
                .map(|user| user.id)
                .collect(),
            credentials: passkeys::list_credentials(conn)?
                .into_iter()
                .filter(|credential| credential.revoked_at.is_none())
                .map(|credential| credential.id)
                .collect(),
            hosts: inventory::hosts(conn)?.into_iter().map(|host| host.id).collect(),
            instances: inventory::instance_specs(conn)?.into_iter().map(|spec| spec.id).collect(),
        })
    })?;
    let actions = catalog::all()
        .into_iter()
        .filter_map(|def| {
            let mut targets = match def.targets {
                Targets::Room => stored.rooms.clone(),
                Targets::User => stored.users.clone(),
                Targets::Credential => stored.credentials.clone(),
                Targets::FurtherHost => stored.hosts.clone(),
                Targets::Instance => stored.instances.clone(),
                _ => def.admitted(&index),
            };
            targets.retain(|target| policy::may_run(&index, role, def.id, target));
            // Listed when there is something this person may do with it now, or would
            // be once there is a target for it.
            let visible = !targets.is_empty() || policy::may_run(&index, role, def.id, "-");
            visible.then(|| CatalogEntry::new(def, &targets))
        })
        .collect();
    Ok(Json(Catalog { actions }))
}

/// The targets of the catalog that live in the store rather than the index.
struct StoredTargets {
    rooms: Vec<String>,
    users: Vec<String>,
    credentials: Vec<String>,
    hosts: Vec<String>,
    instances: Vec<String>,
}

// ----- actions

/// 202 while the device has not answered yet, 200 once the result is final.
fn run_response(view: RunView) -> Response {
    let status = if view.phase == Phase::Commanded.as_str() { StatusCode::ACCEPTED } else { StatusCode::OK };
    (status, Json(view)).into_response()
}

/// Runs an action whose flow is `direct`.
///
/// For tiers 0 and 1 and the safe direction. A consequential action is refused here
/// with `403 confirmation_required`, a critical one with `403 step_up_required`:
/// prepare those instead. Send `request_id` so a
/// resubmitted request runs once. The answer is the run: `200` once its result is
/// final, `202` while the device has not answered (follow it with
/// `GET /actions/runs/{run_id}` or the `run` event).
#[utoipa::path(
    post,
    path = "/actions/execute",
    tag = "actions",
    security(("session" = [])),
    request_body = ActionRequest,
    responses(
        (status = 200, description = "The run, with its final phase.", body = RunView),
        (status = 202, description = "The run, still `commanded`.", body = RunView),
    )
)]
async fn action_execute(
    State(app): State<App>,
    Authed(session): Authed,
    Body(body): Body<ActionRequest>,
) -> Result<Response, ApiError> {
    Ok(run_response(actions::execute(&app, &actor(&session), body).await?))
}

/// Prepares an action whose flow is `prepare_confirm` or `prepare_step_up`.
///
/// Nothing runs yet. The answer says what will happen and what follows from it, for
/// the person to read, and what the confirmation needs: `tap` (send the
/// `confirmation` back) or `passkey` (start a step-up). It expires at `expires_at`
/// (`410 expired` after). An action whose flow is `direct` is `409 not_two_step`.
#[utoipa::path(
    post,
    path = "/actions/prepare",
    tag = "actions",
    security(("session" = [])),
    request_body = ActionRequest,
    responses((status = 200, body = PreparedView))
)]
async fn action_prepare(
    State(app): State<App>,
    Authed(session): Authed,
    Body(body): Body<ActionRequest>,
) -> Result<Json<PreparedView>, ApiError> {
    Ok(Json(actions::prepare(&app, &actor(&session), body)?))
}

/// Confirms a prepared action, which runs it.
///
/// Send the same `action`, `target` and `params` as were prepared, with the
/// `confirmation` (flow `prepare_confirm`) or the passkey's `assertion` (flow
/// `prepare_step_up`). A preparation is used once (`409 already_used` after);
/// anything that differs from what was prepared is `409 binding_mismatch` and cancels
/// it. The answer is the run, as for `execute`.
#[utoipa::path(
    post,
    path = "/actions/confirm",
    tag = "actions",
    security(("session" = [])),
    request_body = ConfirmRequest,
    responses(
        (status = 200, description = "The run, with its final phase.", body = RunView),
        (status = 202, description = "The run, still `commanded`.", body = RunView),
    )
)]
async fn action_confirm(
    State(app): State<App>,
    Authed(session): Authed,
    Body(body): Body<ConfirmRequest>,
) -> Result<Response, ApiError> {
    Ok(run_response(actions::confirm(&app, &actor(&session), body).await?))
}

/// The last runs.
#[derive(Debug, Serialize, ToSchema)]
pub struct RunList {
    /// The 50 most recent, newest first.
    pub runs: Vec<RunView>,
}

/// The most recent runs.
///
/// The owner sees every run, anyone else their own.
#[utoipa::path(
    get,
    path = "/actions/runs",
    tag = "actions",
    security(("session" = [])),
    responses((status = 200, body = RunList))
)]
async fn runs_list(State(app): State<App>, Authed(session): Authed) -> Result<Json<RunList>, ApiError> {
    let person = &session.person;
    let runs = app.store.with(|conn| {
        if person.role == Role::Owner {
            actions::recent_runs(conn, 50)
        } else {
            actions::recent_runs_of(conn, &person.user_id, 50)
        }
    })?;
    Ok(Json(RunList { runs }))
}

/// One run.
///
/// Someone else's run is `404 unknown_run` to anyone but the owner.
#[utoipa::path(
    get,
    path = "/actions/runs/{run_id}",
    tag = "actions",
    security(("session" = [])),
    params(("run_id" = String, Path, description = "The run's id, from the run or a `run` event.")),
    responses((status = 200, body = RunView))
)]
async fn run_get(
    State(app): State<App>,
    Authed(session): Authed,
    Path(run_id): Path<String>,
) -> Result<Json<RunView>, ApiError> {
    let person = &session.person;
    let run = app.store.with(|conn| -> rusqlite::Result<Option<RunView>> {
        if person.role != Role::Owner && !actions::run_belongs_to(conn, &run_id, &person.user_id)? {
            return Ok(None);
        }
        actions::load_run(conn, &run_id)
    })?;
    run.map(Json).ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown_run", "No such run."))
}

// ----- audit

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct AuditQuery {
    /// How many rows, 1 to 500. 100 when left out.
    limit: Option<i64>,
    /// Only rows before this `seq`: the `next_before` of the page before.
    before: Option<i64>,
}

/// A page of the audit log.
#[derive(Debug, Serialize, ToSchema)]
pub struct AuditPage {
    /// Newest first.
    pub rows: Vec<audit::Row>,
    /// The `seq` to ask for the next page with, or null when this page is empty.
    pub next_before: Option<i64>,
}

/// The audit log, newest first.
#[utoipa::path(
    get,
    path = "/audit",
    tag = "audit",
    security(("session" = ["owner"])),
    params(AuditQuery),
    responses((status = 200, body = AuditPage))
)]
async fn audit_read(
    State(app): State<App>,
    Owner(_session): Owner,
    Query(query): Query<AuditQuery>,
) -> Result<Json<AuditPage>, ApiError> {
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let now = app.now();
    app.store.tx(|tx| app.flush_dropped_refusals(tx, now))?;
    let rows = app.store.with(|conn| audit::read(conn, limit, query.before))?;
    let next_before = rows.last().map(|row| row.seq);
    Ok(Json(AuditPage { rows, next_before }))
}

/// Checks the audit log's hash chain and its signed head.
#[utoipa::path(
    get,
    path = "/audit/verify",
    tag = "audit",
    security(("session" = ["owner"])),
    responses((status = 200, body = audit::VerifyReport))
)]
async fn audit_verify(State(app): State<App>, Owner(_session): Owner) -> Result<Json<audit::VerifyReport>, ApiError> {
    Ok(Json(app.store.with(|conn| app.verify_audit(conn))?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The typed entry is the JSON the catalog has always written for each action.
    #[test]
    fn a_catalog_entry_is_what_the_catalog_writes() {
        let targets = vec!["bulb1".to_string(), "living".to_string()];
        let all = catalog::all();
        assert!(all.len() > 40);
        for def in all {
            let typed = serde_json::to_value(CatalogEntry::new(def, &targets)).unwrap();
            assert_eq!(typed, def.to_json(&targets), "{}", def.id);
        }
    }
}
