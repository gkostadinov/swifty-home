//! Energy and what it costs: `GET /api/v1/energy` and `PUT /api/v1/energy/tariff`, for the
//! owner only.
//!
//! A telemetry node counts the energy its machine has used; the owner sets the house's
//! electricity tariff (one price, or a day and a night price with the night hours), and
//! [`crate::energy`] keeps a ledger per telemetry target. The tariff is an arrangement:
//! each `PUT` is audited as `energy.tariff` with the prices, hours and currency, which are
//! not secret.

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::guards::{Body, Owner};
use super::{audit_arrangement, unprocessable, RouterState};
use crate::app::App;
use crate::energy::{self, clock_minutes, clock_text, Hours, TargetEnergy, Tariff, TariffInput};
use crate::error::ApiError;

pub(crate) fn routes() -> OpenApiRouter<RouterState> {
    OpenApiRouter::new().routes(routes!(energy_read)).routes(routes!(energy_set_tariff))
}

/// The house's electricity tariff, and what each telemetry node has used and what it cost.
#[derive(Serialize, ToSchema)]
pub struct Energy {
    /// The tariff, or null while the owner has set none. Then kWh are shown and no cost is.
    pub tariff: Option<TariffView>,
    /// Each telemetry device whose energy counter the hub has read, in target order.
    pub targets: Vec<EnergyTarget>,
}

/// Local hours on a 24-hour clock. `end` before `start` runs across midnight.
#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, Debug)]
#[serde(deny_unknown_fields)]
pub struct HoursView {
    /// `HH:MM`, such as `22:00`.
    #[schema(value_type = String, example = "22:00")]
    pub start: ClockTime,
    /// `HH:MM`, such as `06:00`.
    #[schema(value_type = String, example = "06:00")]
    pub end: ClockTime,
}

/// A time of day, `HH:MM`, kept as minutes after midnight.
#[derive(Clone, Copy, Debug)]
pub struct ClockTime(u16);

impl Serialize for ClockTime {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&clock_text(self.0))
    }
}

impl<'de> Deserialize<'de> for ClockTime {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        clock_minutes(&text)
            .map(ClockTime)
            .ok_or_else(|| serde::de::Error::custom(format!("{text} is not a time of day such as 22:00")))
    }
}

impl From<Hours> for HoursView {
    fn from(hours: Hours) -> Self {
        Self { start: ClockTime(hours.start), end: ClockTime(hours.end) }
    }
}

impl From<HoursView> for Hours {
    fn from(view: HoursView) -> Self {
        Self { start: view.start.0, end: view.end.0 }
    }
}

/// The tariff in force.
#[derive(Serialize, ToSchema)]
pub struct TariffView {
    /// An ISO 4217 code such as `EUR`.
    pub currency: String,
    /// What a kWh costs by day, or all day on a one-rate tariff.
    pub day_price_per_kwh: f64,
    /// What a kWh costs at night; null on a one-rate tariff.
    pub night_price_per_kwh: Option<f64>,
    /// When night runs; null on a one-rate tariff.
    pub night: Option<HoursView>,
    /// When night runs while daylight saving time is in effect, when that differs; else null.
    pub summer_night: Option<HoursView>,
    /// The IANA time zone the hours are read in, such as `Europe/Berlin`; null on a one-rate tariff.
    pub time_zone: Option<String>,
    /// When it was set, in milliseconds since the epoch.
    pub set_at: i64,
    /// Which price applies right now: `day` or `night`.
    #[schema(example = "day")]
    pub band_now: &'static str,
}

impl TariffView {
    fn of(tariff: Tariff, now: i64) -> Self {
        Self {
            band_now: tariff.band_at(now).word(),
            currency: tariff.currency,
            day_price_per_kwh: tariff.day_price,
            night_price_per_kwh: tariff.night_price,
            night: tariff.night.map(HoursView::from),
            summer_night: tariff.summer_night.map(HoursView::from),
            time_zone: tariff.time_zone,
            set_at: tariff.set_at,
        }
    }
}

/// One telemetry device's energy counter and its cost.
#[derive(Serialize, ToSchema)]
pub struct EnergyTarget {
    /// The device's target.
    pub target: String,
    /// The counter as the node last reported it, in kWh since it began.
    pub kwh: f64,
    /// When the node's counter began, in milliseconds since the epoch; null when the node did not say.
    pub since: Option<i64>,
    /// The estimated cost so far, day and night together, in the tariff's currency, unrounded; null until a
    /// tariff has been set. It covers the whole counter: the energy counted before the first tariff was charged
    /// once at its day price, and a later change never reaches back.
    pub cost: Option<f64>,
    /// The part of `cost` charged at the day price; null until a tariff has been set.
    pub cost_day: Option<f64>,
    /// The part of `cost` charged at the night price; null until a tariff has been set.
    pub cost_night: Option<f64>,
    /// The kWh charged at the day price, the energy counted before the first tariff included.
    pub kwh_day: Option<f64>,
    /// The kWh charged at the night price.
    pub kwh_night: Option<f64>,
    /// When the first charge was made, in milliseconds since the epoch; null until a tariff has been set.
    pub priced_since: Option<i64>,
    /// The kWh that had been counted before the first tariff and were charged once at its day price.
    pub backfilled_kwh: Option<f64>,
    /// When the hub last read the counter, in milliseconds since the epoch.
    pub updated_at: i64,
}

impl From<TargetEnergy> for EnergyTarget {
    fn from(entry: TargetEnergy) -> Self {
        Self {
            target: entry.target,
            kwh: entry.kwh,
            since: entry.since,
            cost: entry.cost,
            cost_day: entry.cost_day,
            cost_night: entry.cost_night,
            kwh_day: entry.kwh_day,
            kwh_night: entry.kwh_night,
            priced_since: entry.priced_since,
            backfilled_kwh: entry.backfilled_kwh,
            updated_at: entry.updated_at,
        }
    }
}

/// The electricity tariff: one price all day, or a day and a night price with the hours night runs.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct TariffBody {
    /// An ISO 4217 code: three letters, such as `EUR`. Lower case is accepted and stored in capitals.
    currency: String,
    /// More than 0 and at most 1000000, in `currency` per kWh: the day price, or the only one.
    day_price_per_kwh: f64,
    /// The night price, for a two-rate meter. Needs `night` and `time_zone`.
    #[serde(default)]
    night_price_per_kwh: Option<f64>,
    /// When night runs, such as `{"start": "22:00", "end": "06:00"}`.
    #[serde(default)]
    night: Option<HoursView>,
    /// When night runs while daylight saving time is in effect, if that differs.
    #[serde(default)]
    summer_night: Option<HoursView>,
    /// The IANA time zone the hours are read in, such as `Europe/Berlin`.
    #[serde(default)]
    time_zone: Option<String>,
}

fn view(app: &App) -> Result<Energy, ApiError> {
    let (tariff, ledger) =
        app.store.with(|conn| -> rusqlite::Result<_> { Ok((energy::tariff(conn)?, energy::ledger(conn)?)) })?;
    // A target the index no longer lists (its integration was removed) keeps its row, and is not shown.
    let index = app.registry.index();
    let targets = ledger
        .into_iter()
        .filter(|entry| index.get(&entry.target).is_some_and(|device| !device.pending))
        .map(EnergyTarget::from)
        .collect();
    Ok(Energy { tariff: tariff.map(|tariff| TariffView::of(tariff, app.now())), targets })
}

/// The electricity tariff, and the energy and cost of each telemetry node.
///
/// A node's counter is read about once a minute, and what it adds is charged at the tariff in force: by day at
/// the day price, at night at the night price, and a read that spans a change of band partly at each.
#[utoipa::path(
    get,
    path = "/energy",
    tag = "energy",
    security(("session" = ["owner"])),
    responses((status = 200, body = Energy))
)]
async fn energy_read(State(app): State<App>, Owner(_session): Owner) -> Result<Json<Energy>, ApiError> {
    Ok(Json(view(&app)?))
}

fn hours_json(hours: Option<Hours>) -> Value {
    hours.map_or(Value::Null, |hours| json!({ "start": clock_text(hours.start), "end": clock_text(hours.end) }))
}

/// Sets the electricity tariff.
///
/// From now on the energy counted is charged at it, and the cost already counted never changes. The first
/// tariff also charges the energy counted before it, once, at its day price, so the cost covers the whole
/// counter; so does a tariff in a different currency, whose amounts cannot be added to the old ones. Audited as
/// `energy.tariff` with the prices, hours, time zone and currency. The answer is what `GET /energy` gives.
#[utoipa::path(
    put,
    path = "/energy/tariff",
    tag = "energy",
    security(("session" = ["owner"])),
    request_body = TariffBody,
    responses((status = 200, body = Energy))
)]
async fn energy_set_tariff(
    State(app): State<App>,
    Owner(session): Owner,
    Body(body): Body<TariffBody>,
) -> Result<Json<Energy>, ApiError> {
    let Some(currency) = energy::currency_code(&body.currency) else {
        return Err(unprocessable(
            "invalid_currency",
            "The currency is an ISO 4217 code of three letters, such as EUR.",
        ));
    };
    let time_zone = body.time_zone.map(|zone| zone.trim().to_string()).filter(|zone| !zone.is_empty());
    let tariff = TariffInput {
        currency,
        day_price: body.day_price_per_kwh,
        night_price: body.night_price_per_kwh,
        night: body.night.map(Hours::from),
        summer_night: body.summer_night.map(Hours::from),
        // A one-rate tariff reads no clock, so it keeps no zone.
        time_zone: if body.night.is_some() { time_zone } else { None },
    };
    if let Some(problem) = energy::tariff_problem(&tariff) {
        return Err(unprocessable("invalid_tariff", problem));
    }
    app.store.tx(|tx| -> Result<(), ApiError> {
        let set = energy::set_tariff(tx, app.now(), &session.person.user_id, &tariff)?;
        let before = set.previous.map(|before| {
            json!({
                "currency": before.currency,
                "day_price_per_kwh": before.day_price,
                "night_price_per_kwh": before.night_price,
                "night": hours_json(before.night),
                "summer_night": hours_json(before.summer_night),
                "time_zone": before.time_zone,
            })
        });
        let params = json!({
            "currency": tariff.currency,
            "day_price_per_kwh": tariff.day_price,
            "night_price_per_kwh": tariff.night_price,
            "night": hours_json(tariff.night),
            "summer_night": hours_json(tariff.summer_night),
            "time_zone": tariff.time_zone,
            "before": before,
            "charged_once": set.charged_once.iter().map(|(target, kwh)| json!({ "target": target, "kwh": kwh })).collect::<Vec<_>>(),
        });
        audit_arrangement(&app, tx, &session, "energy.tariff", "-", params)
    })?;
    Ok(Json(view(&app)?))
}
