//! Machines and hosts, their charts, health, and the console.

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use swifty_plugin_api::{DeviceClass, HistoryRange};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::guards::Owner;
use super::{plain_event, unprocessable, RouterState};
use crate::app::App;
use crate::background;
use crate::console;
use crate::error::ApiError;
use crate::health;
use crate::history;
use crate::inventory::{self, HostView, Machine, MachineStatus};

pub(crate) fn routes() -> OpenApiRouter<RouterState> {
    OpenApiRouter::new()
        .routes(routes!(machines_list))
        .routes(routes!(machines_scan))
        .routes(routes!(history_read))
        .routes(routes!(health_read))
        .routes(routes!(console_connect))
}

// ----- machines

/// The machines and the further hosts.
#[derive(Serialize, ToSchema)]
pub struct Machines {
    /// The VMs the hosts listed, approved or waiting for approval.
    pub machines: Vec<Machine>,
    /// The hosts besides the first.
    pub hosts: Vec<HostView>,
}

/// What a scan of the hosts added.
#[derive(Serialize, ToSchema)]
pub struct Scanned {
    /// Dry-run: no host was asked, and nothing was added.
    pub simulated: bool,
    /// The targets of the machines it found for the first time. Each waits for approval.
    pub added: Vec<String>,
}

/// The machines and the further hosts.
///
/// A machine is approved with the critical action `machine.approve`, and taken off
/// the list with `machine.remove`.
#[utoipa::path(
    get,
    path = "/machines",
    tag = "machines",
    security(("session" = ["owner"])),
    responses((status = 200, body = Machines))
)]
async fn machines_list(State(app): State<App>, Owner(_session): Owner) -> Result<Json<Machines>, ApiError> {
    let (machines, hosts) =
        app.store.with(|conn| -> rusqlite::Result<_> { Ok((inventory::machines(conn)?, inventory::hosts(conn)?)) })?;
    Ok(Json(Machines { machines, hosts }))
}

/// Asks the hosts which VMs they run, and lists the new ones as waiting for approval.
#[utoipa::path(
    post,
    path = "/machines/scan",
    tag = "machines",
    security(("session" = ["owner"])),
    responses((status = 200, body = Scanned))
)]
async fn machines_scan(State(app): State<App>, Owner(session): Owner) -> Result<Json<Scanned>, ApiError> {
    match background::scan_machines(&app).await {
        Ok(None) => {
            app.audit(plain_event(&app, "machine.scan", "-", Some(&session), "simulated"))?;
            Ok(Json(Scanned { simulated: true, added: Vec::new() }))
        }
        Ok(Some(added)) => {
            let mut event = plain_event(&app, "machine.scan", "-", Some(&session), "done");
            event.params = json!({ "added": added });
            app.audit(event)?;
            Ok(Json(Scanned { simulated: false, added }))
        }
        Err(error) => {
            app.audit(plain_event(&app, "machine.scan", "-", Some(&session), "refused: scan_failed"))?;
            Err(ApiError::new(StatusCode::BAD_GATEWAY, "scan_failed", error.to_string()))
        }
    }
}

// ----- charts and health

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct HistoryQuery {
    /// `1h`, `24h` or `7d`. `1h` when left out.
    range: Option<String>,
    /// One metric of the stored samples, such as `cpu_temp`. All of them when left out.
    metric: Option<String>,
}

/// A chart of a device.
#[derive(Serialize, ToSchema)]
pub struct History {
    pub target: String,
    /// `1h`, `24h` or `7d`.
    pub range: &'static str,
    /// `stored_samples` (the telemetry source's readings, kept by the hub) or
    /// `proxmox_rrd` (a host's or VM's own charts).
    pub source: &'static str,
    /// Dry-run: nothing was asked.
    pub simulated: bool,
    /// With `stored_samples`: the series, by metric.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series: Option<Value>,
    /// With `proxmox_rrd`: the points, oldest first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub points: Option<Vec<Value>>,
}

/// A device's chart: the telemetry source's stored readings, or a host's or VM's own.
///
/// A VM waiting for approval is `403 machine_not_approved`.
#[utoipa::path(
    get,
    path = "/history/{target}",
    tag = "machines",
    security(("session" = ["owner"])),
    params(("target" = String, Path, description = "The telemetry source, a host or an approved VM."), HistoryQuery),
    responses((status = 200, body = History))
)]
async fn history_read(
    State(app): State<App>,
    Owner(_session): Owner,
    Path(target): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<History>, ApiError> {
    let range = HistoryRange::parse(query.range.as_deref().unwrap_or("1h"))
        .ok_or_else(|| unprocessable("invalid_range", "range must be 1h, 24h or 7d."))?;
    let live = app.registry.is_live();
    if history::telemetry_source(&app).as_deref() == Some(target.as_str()) {
        if query.metric.as_deref().is_some_and(|metric| !history::is_metric_name(metric)) {
            return Err(unprocessable("invalid_metric", "That is not a metric name."));
        }
        let now = app.now();
        let series = app.store.with(|conn| history::series(conn, now, range, query.metric.as_deref()))?;
        return Ok(Json(History {
            target,
            range: range.name(),
            source: "stored_samples",
            simulated: !live,
            series: Some(series),
            points: None,
        }));
    }
    let index = app.registry.index();
    if index.class_of(&target) != Some(DeviceClass::Host) {
        match app.store.with(|conn| inventory::machine_status(conn, &target))? {
            None => return Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_device", "No such device.")),
            Some(MachineStatus::Pending) => {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "machine_not_approved",
                    "This machine is waiting for the owner to approve it.",
                ));
            }
            Some(MachineStatus::Approved) => {}
        }
    }
    let points = app
        .registry
        .history(&target, range)
        .await
        .map_err(|error| ApiError::new(StatusCode::BAD_GATEWAY, "history_unavailable", error.to_string()))?;
    Ok(Json(History {
        target,
        range: range.name(),
        source: "proxmox_rrd",
        simulated: points.is_none(),
        series: None,
        points: Some(points.unwrap_or_default()),
    }))
}

/// What the health page reads: the telemetry source's latest snapshot, every reading
/// either a value from a source that is reporting or `not_reporting`.
#[utoipa::path(
    get,
    path = "/health",
    tag = "machines",
    security(("session" = ["owner"])),
    responses((status = 200, description = "The report. Not typed yet: see `docs/api/untyped.md`.", body = Object))
)]
async fn health_read(State(app): State<App>, Owner(_session): Owner) -> Json<Value> {
    Json(health::report(&app).await)
}

// ----- console

/// Opens the console of a VM, as a WebSocket.
///
/// The critical action `console.open` opens a console after its step-up, and its run
/// carries a single-use ticket in `console`. Connect within its time with the
/// subprotocols `binary` and `swifty-ticket.<ticket>`, from the hub's own origin.
/// The socket relays the VM's VNC stream both ways until either side closes or the
/// session ends.
#[utoipa::path(
    get,
    path = "/console/connect",
    tag = "machines",
    security(("session" = ["owner"])),
    params(
        ("Sec-WebSocket-Protocol" = String, Header, description = "`binary, swifty-ticket.<ticket>`."),
        ("Origin" = String, Header, description = "The hub's own origin."),
    ),
    responses((status = 101, description = "Switched to the WebSocket that relays the console."))
)]
async fn console_connect(
    State(app): State<App>,
    Owner(principal): Owner,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    // The relay outlives this request and keeps checking the session it runs for.
    let session = principal.session;
    // Until the ticket is redeemed, nothing says which VM this is about.
    let refused = |target: &str, error: ApiError| -> Result<Response, ApiError> {
        console::record(&app, &session, "console.connect", target, &format!("refused: {}", error.code))?;
        Err(error)
    };
    let origin = headers.get(header::ORIGIN).and_then(|value| value.to_str().ok());
    if origin != Some(app.config.webauthn.origin.as_str()) {
        return refused("-", ApiError::new(StatusCode::FORBIDDEN, "bad_origin", "Wrong origin."));
    }
    let ticket = headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .and_then(console::ticket_from_protocols)
        .unwrap_or_default();
    let redeemed = match app.consoles.redeem(app.now(), &session.session_ref, ticket) {
        Ok(redeemed) => redeemed,
        Err(error) => return refused("-", error),
    };
    let target = redeemed.target.clone();
    let slot = match app.consoles.try_open() {
        Ok(slot) => slot,
        Err(error) => return refused(&target, error),
    };
    let link = match app.registry.connect_console(&redeemed.instance, redeemed.grant).await {
        Ok(Some(link)) => link,
        Ok(None) => {
            return refused(
                &target,
                ApiError::new(StatusCode::CONFLICT, "dry_run", crate::runtime::simulated::NOTHING_CONTACTED),
            );
        }
        Err(error) => {
            return refused(&target, ApiError::new(StatusCode::BAD_GATEWAY, "console_unreachable", error.to_string()));
        }
    };
    console::record(&app, &session, "console.connect", &target, "connected")?;
    let relay_app = app.clone();
    Ok(upgrade
        .protocols(["binary"])
        .max_frame_size(console::MAX_BROWSER_MESSAGE)
        .max_message_size(console::MAX_BROWSER_MESSAGE)
        .on_upgrade(move |socket| console::relay(relay_app, session, target, socket, link, slot)))
}
