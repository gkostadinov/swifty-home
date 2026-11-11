//! Energy and what it costs.
//!
//! A telemetry node keeps a counter of the energy its machine has used (`energy` in
//! its snapshot: `since`, the unix second the counter began, and `total_kwh`). The node
//! measures; the hub prices. The owner sets one tariff for the house: a currency, a day
//! price per kilowatt-hour and, for a two-rate meter, a night price with the hours night
//! runs (local times, which may cross midnight), optionally other hours while daylight
//! saving time is in effect, all read in the tariff's time zone. The hub keeps a ledger
//! per telemetry target: the last total it saw, the counter's `since`, everything it has
//! counted, and the energy and cost so far by band.
//!
//! Each time the hub reads the node ([`crate::history::sample_collector`]), the energy
//! added since the last read is charged at the tariff in force:
//!
//! - the added energy is `new - last`. When the counter's `since` changed or the total
//!   went down, the counter restarted, and the new total itself is the added energy;
//! - the added energy is spread evenly over the time since the last read, and each minute
//!   of that time is day or night as the tariff says for that local minute. So a read
//!   that spans 22:00 is charged partly at each price, and a long gap (the hub was down)
//!   is split the same way;
//! - a tariff change applies from that moment on, and the cost already counted is never
//!   recomputed. The read that follows a change is charged at the new tariff for the
//!   whole time since the read before it, about a minute;
//! - when the first tariff is set, what had been counted before it is charged once at its
//!   day price, so the cost covers the whole counter, and the ledger records that it did so
//!   (`backfilled_kwh`, `priced_since`). A new currency is a new first tariff: its amounts
//!   cannot be added to the old ones, so everything counted is charged once more, at the
//!   new day price, in the new currency;
//! - with no tariff set, the energy is counted and there is no cost at all.
//!
//! Money is an estimate: it is stored as a float and rounded only for display. Nothing
//! here sends anything to a node, and in dry-run nothing is read, so nothing is counted.

use jiff::tz::TimeZone;
use jiff::Timestamp;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

/// The largest price per kWh accepted, in any currency.
pub const MAX_PRICE: f64 = 1_000_000.0;
/// A counter above this is nonsense, not a reading.
const MAX_KWH: f64 = 1.0e9;
/// The time between reads is split into slices of about a minute, each day or night.
const SLICE_MS: i64 = 60_000;
/// A longer gap is split into this many slices, each longer than a minute.
const MAX_SLICES: i64 = 20_160;

/// Local hours, as minutes after midnight: from `start` up to `end`, across midnight when
/// `end` comes first. Never empty: `start` and `end` differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hours {
    pub start: u16,
    pub end: u16,
}

impl Hours {
    /// Whether this minute of the day falls in these hours.
    pub fn holds(self, minute: u16) -> bool {
        if self.start < self.end {
            (self.start..self.end).contains(&minute)
        } else {
            minute >= self.start || minute < self.end
        }
    }
}

/// `HH:MM` on a 24-hour clock, as minutes after midnight.
pub fn clock_minutes(text: &str) -> Option<u16> {
    let (hours, minutes) = text.trim().split_once(':')?;
    if hours.is_empty() || hours.len() > 2 || minutes.len() != 2 {
        return None;
    }
    let (hours, minutes): (u16, u16) = (hours.parse().ok()?, minutes.parse().ok()?);
    (hours < 24 && minutes < 60).then_some(hours * 60 + minutes)
}

/// Minutes after midnight as `HH:MM`.
pub fn clock_text(minutes: u16) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// Which of the tariff's two prices a moment is charged at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    Day,
    Night,
}

impl Band {
    pub fn word(self) -> &'static str {
        match self {
            Band::Day => "day",
            Band::Night => "night",
        }
    }
}

/// The tariff as the owner gives it, before it is stored.
#[derive(Debug, Clone, PartialEq)]
pub struct TariffInput {
    /// An ISO 4217 code: three capital letters.
    pub currency: String,
    pub day_price: f64,
    /// For a two-rate meter; `None` is one price all day.
    pub night_price: Option<f64>,
    /// When night runs. Set exactly when `night_price` is.
    pub night: Option<Hours>,
    /// When night runs while daylight saving time is in effect, when that differs.
    pub summer_night: Option<Hours>,
    /// The IANA name the hours are read in, such as `Europe/Berlin`. Set when `night` is.
    pub time_zone: Option<String>,
}

/// The tariff in force.
#[derive(Debug, Clone, PartialEq)]
pub struct Tariff {
    pub currency: String,
    pub day_price: f64,
    pub night_price: Option<f64>,
    pub night: Option<Hours>,
    pub summer_night: Option<Hours>,
    pub time_zone: Option<String>,
    /// When it was set, in milliseconds since the epoch.
    pub set_at: i64,
}

impl Tariff {
    /// The night hours and the zone they are read in, when the tariff has two prices and the
    /// zone is known here. Without them every moment is day.
    fn clock(&self) -> Option<(Hours, TimeZone)> {
        self.night_price?;
        let zone = TimeZone::get(self.time_zone.as_deref()?).ok()?;
        Some((self.night?, zone))
    }

    /// The band of a moment, in milliseconds since the epoch.
    pub fn band_at(&self, at_ms: i64) -> Band {
        match self.clock() {
            Some((night, zone)) => band_in(&zone, night, self.summer_night, at_ms),
            None => Band::Day,
        }
    }

    /// The price of a band. Night is the day price on a one-rate tariff.
    pub fn price(&self, band: Band) -> f64 {
        match band {
            Band::Day => self.day_price,
            Band::Night => self.night_price.unwrap_or(self.day_price),
        }
    }

    /// The share of the time from `from_ms` to `to_ms` that is day, and the share that is night;
    /// together they make 1. Each slice of about a minute takes the band of its middle. With no
    /// time between the two, the moment `to_ms` decides.
    pub fn split(&self, from_ms: i64, to_ms: i64) -> (f64, f64) {
        let Some((night, zone)) = self.clock() else {
            return (1.0, 0.0);
        };
        let span = to_ms.saturating_sub(from_ms);
        if span <= 0 {
            return match band_in(&zone, night, self.summer_night, to_ms) {
                Band::Day => (1.0, 0.0),
                Band::Night => (0.0, 1.0),
            };
        }
        let slices = ((span + SLICE_MS - 1) / SLICE_MS).clamp(1, MAX_SLICES);
        let width = span as f64 / slices as f64;
        let nights = (0..slices)
            .filter(|slice| {
                let middle = from_ms as f64 + (*slice as f64 + 0.5) * width;
                band_in(&zone, night, self.summer_night, middle as i64) == Band::Night
            })
            .count();
        let night_share = nights as f64 / slices as f64;
        (1.0 - night_share, night_share)
    }
}

/// The band of one moment: night when its local minute is in the night hours, or in the summer
/// night hours while daylight saving time is in effect and those are set.
fn band_in(zone: &TimeZone, night: Hours, summer_night: Option<Hours>, at_ms: i64) -> Band {
    let Ok(at) = Timestamp::from_millisecond(at_ms) else {
        return Band::Day;
    };
    let local = zone.to_datetime(at);
    let minute = (local.hour() as u16) * 60 + local.minute() as u16;
    let hours = match summer_night {
        Some(summer) if zone.to_offset_info(at).dst().is_dst() => summer,
        _ => night,
    };
    if hours.holds(minute) {
        Band::Night
    } else {
        Band::Day
    }
}

/// What a node's snapshot says about its energy counter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    /// When the counter began, in unix seconds, when the node said.
    pub since_s: Option<i64>,
    pub total_kwh: f64,
}

/// The counter in a snapshot (`energy.since`, `energy.total_kwh`), when there is a usable one.
/// A total that is missing, not a number, negative or absurd is no reading, never a zero.
pub fn reading(snapshot: &Value) -> Option<Reading> {
    let energy = snapshot.get("energy")?;
    let total_kwh = energy.get("total_kwh")?.as_f64().filter(|kwh| kwh.is_finite() && (0.0..=MAX_KWH).contains(kwh))?;
    let since_s = energy.get("since").and_then(Value::as_f64).filter(|s| s.is_finite() && *s >= 0.0).map(|s| s as i64);
    Some(Reading { since_s, total_kwh })
}

fn hours_of(start: Option<i64>, end: Option<i64>) -> Option<Hours> {
    let minute = |value: i64| u16::try_from(value).ok().filter(|m| *m < 24 * 60);
    Some(Hours { start: minute(start?)?, end: minute(end?)? })
}

/// The tariff in force, if the owner has set one.
pub fn tariff(conn: &Connection) -> rusqlite::Result<Option<Tariff>> {
    conn.query_row(
        "SELECT currency, day_price, night_price, night_start, night_end, summer_night_start, summer_night_end,
                time_zone, set_at
         FROM energy_tariff WHERE id = 1",
        [],
        |row| {
            Ok(Tariff {
                currency: row.get(0)?,
                day_price: row.get(1)?,
                night_price: row.get(2)?,
                night: hours_of(row.get(3)?, row.get(4)?),
                summer_night: hours_of(row.get(5)?, row.get(6)?),
                time_zone: row.get(7)?,
                set_at: row.get(8)?,
            })
        },
    )
    .optional()
}

fn price_problem(what: &str, price: f64) -> Option<String> {
    (!price.is_finite() || price <= 0.0 || price > MAX_PRICE)
        .then(|| format!("The {what} per kWh must be more than 0 and at most {MAX_PRICE}."))
}

/// Why a tariff is not acceptable, in a sentence for the person.
pub fn tariff_problem(tariff: &TariffInput) -> Option<String> {
    if let Some(problem) = price_problem("day price", tariff.day_price) {
        return Some(problem);
    }
    if let Some(problem) = tariff.night_price.and_then(|price| price_problem("night price", price)) {
        return Some(problem);
    }
    if tariff.night_price.is_some() != tariff.night.is_some() {
        return Some("A night price needs the hours night runs, and night hours need a night price.".to_string());
    }
    if tariff.summer_night.is_some() && tariff.night.is_none() {
        return Some("Summer night hours need the usual night hours and a night price.".to_string());
    }
    if [tariff.night, tariff.summer_night].into_iter().flatten().any(|hours| hours.start == hours.end) {
        return Some("Night has to start and end at different times.".to_string());
    }
    if tariff.night.is_some() {
        let Some(zone) = tariff.time_zone.as_deref() else {
            return Some("Night hours need the time zone they are read in, such as Europe/Berlin.".to_string());
        };
        if TimeZone::get(zone).is_err() {
            return Some(format!("{zone} is not a time zone this hub knows. Use a name such as Europe/Berlin."));
        }
    }
    None
}

/// A currency code as stored: the three letters of ISO 4217, in capitals.
pub fn currency_code(text: &str) -> Option<String> {
    let text = text.trim();
    (text.len() == 3 && text.bytes().all(|b| b.is_ascii_alphabetic())).then(|| text.to_ascii_uppercase())
}

/// What the ledger holds for one target, as it is shown.
#[derive(Debug, Clone, PartialEq)]
pub struct TargetEnergy {
    pub target: String,
    /// The node's counter, as it last reported it, in kWh.
    pub kwh: f64,
    /// When that counter began, in milliseconds since the epoch, when the node said.
    pub since: Option<i64>,
    /// The cost so far, day and night together: `None` until a tariff has been set.
    pub cost: Option<f64>,
    /// The cost and the energy charged at the day price, and at the night price.
    pub cost_day: Option<f64>,
    pub cost_night: Option<f64>,
    pub kwh_day: Option<f64>,
    pub kwh_night: Option<f64>,
    /// When charging began, in milliseconds since the epoch.
    pub priced_since: Option<i64>,
    /// The energy counted before the first tariff, charged once at its day price.
    pub backfilled_kwh: Option<f64>,
    /// When the hub last read the counter.
    pub updated_at: i64,
}

/// Every target the ledger holds, in target order.
pub fn ledger(conn: &Connection) -> rusqlite::Result<Vec<TargetEnergy>> {
    let mut statement = conn.prepare(
        "SELECT target, last_kwh, since_s, cost_day, cost_night, kwh_day, kwh_night, priced_since, backfilled_kwh,
                updated_at
         FROM energy_ledger ORDER BY target ASC",
    )?;
    let rows = statement.query_map([], |row| {
        let priced_since: Option<i64> = row.get(7)?;
        let since_s: Option<i64> = row.get(2)?;
        let priced = |value: f64| priced_since.map(|_| value);
        let (cost_day, cost_night): (f64, f64) = (row.get(3)?, row.get(4)?);
        Ok(TargetEnergy {
            target: row.get(0)?,
            kwh: row.get(1)?,
            since: since_s.map(|seconds| seconds.saturating_mul(1000)),
            cost: priced(cost_day + cost_night),
            cost_day: priced(cost_day),
            cost_night: priced(cost_night),
            kwh_day: priced(row.get(5)?),
            kwh_night: priced(row.get(6)?),
            priced_since,
            backfilled_kwh: row.get(8)?,
            updated_at: row.get(9)?,
        })
    })?;
    rows.collect()
}

/// A ledger row, as far as charging needs it.
struct Row {
    since_s: Option<i64>,
    last_kwh: f64,
    counted_kwh: f64,
    cost_day: f64,
    cost_night: f64,
    kwh_day: f64,
    kwh_night: f64,
    priced_since: Option<i64>,
    backfilled_kwh: Option<f64>,
    updated_at: i64,
}

/// Takes one reading of a target's counter into the ledger, at the tariff in force.
/// The first sight of a target counts what it has used so far, once.
pub fn record(conn: &Connection, now: i64, target: &str, reading: &Reading) -> rusqlite::Result<()> {
    let tariff = tariff(conn)?;
    let row = conn
        .query_row(
            "SELECT since_s, last_kwh, counted_kwh, cost_day, cost_night, kwh_day, kwh_night, priced_since,
                    backfilled_kwh, updated_at
             FROM energy_ledger WHERE target = ?1",
            params![target],
            |row| {
                Ok(Row {
                    since_s: row.get(0)?,
                    last_kwh: row.get(1)?,
                    counted_kwh: row.get(2)?,
                    cost_day: row.get(3)?,
                    cost_night: row.get(4)?,
                    kwh_day: row.get(5)?,
                    kwh_night: row.get(6)?,
                    priced_since: row.get(7)?,
                    backfilled_kwh: row.get(8)?,
                    updated_at: row.get(9)?,
                })
            },
        )
        .optional()?;
    let total = reading.total_kwh;
    let Some(row) = row else {
        // First sight: what the counter holds is charged once at the day price, if there is a tariff.
        let (cost_day, priced_since, backfilled) = match &tariff {
            Some(tariff) => (total * tariff.day_price, Some(now), Some(total)),
            None => (0.0, None, None),
        };
        let kwh_day = if tariff.is_some() { total } else { 0.0 };
        conn.execute(
            "INSERT INTO energy_ledger
                 (target, since_s, last_kwh, counted_kwh, cost_day, cost_night, kwh_day, kwh_night, priced_since,
                  backfilled_kwh, first_seen, updated_at)
             VALUES (?1, ?2, ?3, ?3, ?4, 0, ?5, 0, ?6, ?7, ?8, ?8)",
            params![target, reading.since_s, total, cost_day, kwh_day, priced_since, backfilled, now],
        )?;
        return Ok(());
    };
    let restarted = row.since_s != reading.since_s || total < row.last_kwh;
    let added = if restarted { total } else { total - row.last_kwh };
    let counted = row.counted_kwh + added;
    let mut next = Row { since_s: reading.since_s, last_kwh: total, counted_kwh: counted, updated_at: now, ..row };
    match (&tariff, row.priced_since) {
        (Some(tariff), Some(_)) => {
            let (day_share, night_share) = tariff.split(row.updated_at, now);
            let (day, night) = (added * day_share, added * night_share);
            next.kwh_day += day;
            next.kwh_night += night;
            next.cost_day += day * tariff.price(Band::Day);
            next.cost_night += night * tariff.price(Band::Night);
        }
        // A tariff exists and this target was never charged: the whole count is charged once.
        (Some(tariff), None) => {
            next.cost_day = counted * tariff.day_price;
            next.cost_night = 0.0;
            next.kwh_day = counted;
            next.kwh_night = 0.0;
            next.priced_since = Some(now);
            next.backfilled_kwh = Some(counted);
        }
        (None, _) => {}
    }
    conn.execute(
        "UPDATE energy_ledger SET since_s = ?2, last_kwh = ?3, counted_kwh = ?4, cost_day = ?5, cost_night = ?6,
             kwh_day = ?7, kwh_night = ?8, priced_since = ?9, backfilled_kwh = ?10, updated_at = ?11
         WHERE target = ?1",
        params![
            target,
            next.since_s,
            next.last_kwh,
            next.counted_kwh,
            next.cost_day,
            next.cost_night,
            next.kwh_day,
            next.kwh_night,
            next.priced_since,
            next.backfilled_kwh,
            next.updated_at
        ],
    )?;
    Ok(())
}

/// What setting a tariff did to the ledger.
#[derive(Debug, Clone, PartialEq)]
pub struct Set {
    /// The tariff before, when there was one.
    pub previous: Option<Tariff>,
    /// The targets whose whole count was charged at this tariff's day price, once, with the energy
    /// charged: all of them for a first tariff or a new currency, none otherwise.
    pub charged_once: Vec<(String, f64)>,
}

/// Sets the tariff. From now on the energy counted is charged at it, and the cost already counted
/// stays as it is. The first tariff, and a tariff in another currency, also charge the energy
/// counted so far once, at the day price (see the module's rules). Check it with
/// [`tariff_problem`] first.
pub fn set_tariff(conn: &Connection, now: i64, who: &str, tariff: &TariffInput) -> rusqlite::Result<Set> {
    let previous = self::tariff(conn)?;
    let minutes = |hours: Option<Hours>| (hours.map(|h| h.start), hours.map(|h| h.end));
    let ((night_start, night_end), (summer_start, summer_end)) = (minutes(tariff.night), minutes(tariff.summer_night));
    conn.execute(
        "INSERT INTO energy_tariff
             (id, currency, day_price, night_price, night_start, night_end, summer_night_start, summer_night_end,
              time_zone, set_at, set_by)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT (id) DO UPDATE SET currency = ?1, day_price = ?2, night_price = ?3, night_start = ?4,
             night_end = ?5, summer_night_start = ?6, summer_night_end = ?7, time_zone = ?8, set_at = ?9, set_by = ?10",
        params![
            tariff.currency,
            tariff.day_price,
            tariff.night_price,
            night_start,
            night_end,
            summer_start,
            summer_end,
            tariff.time_zone,
            now,
            who
        ],
    )?;
    let rebase = previous.as_ref().is_none_or(|before| before.currency != tariff.currency);
    let mut charged_once = Vec::new();
    if rebase {
        let counted: Vec<(String, f64)> = {
            let mut statement = conn.prepare("SELECT target, counted_kwh FROM energy_ledger ORDER BY target ASC")?;
            let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for (target, kwh) in counted {
            conn.execute(
                "UPDATE energy_ledger SET cost_day = ?2, cost_night = 0, kwh_day = ?3, kwh_night = 0,
                     priced_since = ?4, backfilled_kwh = ?3
                 WHERE target = ?1",
                params![target, kwh * tariff.day_price, kwh, now],
            )?;
            charged_once.push((target, kwh));
        }
    }
    Ok(Set { previous, charged_once })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::store::Store;

    const T: &str = "collector";

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("home.db")).unwrap();
        (dir, store)
    }

    fn counter(since_s: i64, total_kwh: f64) -> Reading {
        Reading { since_s: Some(since_s), total_kwh }
    }

    fn entry(conn: &Connection) -> TargetEnergy {
        ledger(conn).unwrap().into_iter().find(|entry| entry.target == T).unwrap()
    }

    fn close(left: f64, right: f64) {
        assert!((left - right).abs() < 1e-9, "{left} is not {right}");
    }

    /// One price all day.
    fn one(price: f64, currency: &str) -> TariffInput {
        TariffInput {
            currency: currency.to_string(),
            day_price: price,
            night_price: None,
            night: None,
            summer_night: None,
            time_zone: None,
        }
    }

    fn hours(start: &str, end: &str) -> Hours {
        Hours { start: clock_minutes(start).unwrap(), end: clock_minutes(end).unwrap() }
    }

    /// Day and night prices, night 22:00 to 06:00, in `zone`.
    fn two(day: f64, night: f64, zone: &str) -> TariffInput {
        TariffInput {
            currency: "EUR".to_string(),
            day_price: day,
            night_price: Some(night),
            night: Some(hours("22:00", "06:00")),
            summer_night: None,
            time_zone: Some(zone.to_string()),
        }
    }

    fn stored(input: &TariffInput) -> Tariff {
        Tariff {
            currency: input.currency.clone(),
            day_price: input.day_price,
            night_price: input.night_price,
            night: input.night,
            summer_night: input.summer_night,
            time_zone: input.time_zone.clone(),
            set_at: 0,
        }
    }

    /// A local time in a zone, in milliseconds since the epoch.
    fn at(zone: &str, (year, month, day): (i16, i8, i8), (hour, minute): (i8, i8)) -> i64 {
        jiff::civil::date(year, month, day)
            .at(hour, minute, 0, 0)
            .to_zoned(TimeZone::get(zone).unwrap())
            .unwrap()
            .timestamp()
            .as_millisecond()
    }

    const BERLIN: &str = "Europe/Berlin";
    const TOKYO: &str = "Asia/Tokyo";
    const WINTER: (i16, i8, i8) = (2026, 1, 15);
    const SUMMER: (i16, i8, i8) = (2026, 7, 15);

    #[test]
    fn a_reading_is_read_from_the_snapshot_and_a_bad_one_is_none() {
        let good = json!({ "energy": { "since": 1794380000, "total_kwh": 12.5, "domains": [] } });
        assert_eq!(reading(&good), Some(counter(1_794_380_000, 12.5)));
        let no_since = json!({ "energy": { "total_kwh": 1 } });
        assert_eq!(reading(&no_since), Some(Reading { since_s: None, total_kwh: 1.0 }));
        for bad in [
            json!({}),
            json!({ "energy": null }),
            json!({ "energy": { "since": 1 } }),
            json!({ "energy": { "since": 1, "total_kwh": "12" } }),
            json!({ "energy": { "since": 1, "total_kwh": null } }),
            json!({ "energy": { "since": 1, "total_kwh": -0.5 } }),
            json!({ "energy": { "since": 1, "total_kwh": 1.0e12 } }),
        ] {
            assert_eq!(reading(&bad), None, "{bad}");
        }
    }

    #[test]
    fn clock_times_are_read_and_written() {
        assert_eq!(clock_minutes("22:00"), Some(1320));
        assert_eq!(clock_minutes("6:05"), Some(365));
        assert_eq!(clock_minutes(" 00:00 "), Some(0));
        assert_eq!(clock_minutes("23:59"), Some(1439));
        for bad in ["24:00", "12:60", "12", "12:5", "ab:cd", "", ":30", "123:00", "-1:00"] {
            assert_eq!(clock_minutes(bad), None, "{bad}");
        }
        assert_eq!(clock_text(365), "06:05");
        assert_eq!(clock_text(1320), "22:00");
    }

    #[test]
    fn night_hours_may_cross_midnight() {
        let night = hours("22:00", "06:00");
        for (minute, held) in [(1319, false), (1320, true), (0, true), (359, true), (360, false), (720, false)] {
            assert_eq!(night.holds(minute), held, "{minute}");
        }
        let afternoon = hours("13:00", "16:00");
        assert!(afternoon.holds(13 * 60) && afternoon.holds(15 * 60 + 59) && !afternoon.holds(16 * 60));
    }

    #[test]
    fn a_read_that_spans_22_00_is_split_by_time() {
        let tariff = stored(&two(0.20, 0.10, BERLIN));
        let (day, night) = tariff.split(at(BERLIN, WINTER, (21, 30)), at(BERLIN, WINTER, (22, 30)));
        close(day, 0.5);
        close(night, 0.5);
        let (day, night) = tariff.split(at(BERLIN, WINTER, (21, 45)), at(BERLIN, WINTER, (22, 0)));
        assert_eq!((day, night), (1.0, 0.0), "night starts at 22:00, not before");
    }

    #[test]
    fn a_read_that_spans_06_00_is_split_by_time() {
        let tariff = stored(&two(0.20, 0.10, BERLIN));
        let (day, night) = tariff.split(at(BERLIN, WINTER, (5, 45)), at(BERLIN, WINTER, (6, 15)));
        close(day, 0.5);
        close(night, 0.5);
    }

    #[test]
    fn a_read_across_midnight_is_all_night_and_midday_all_day() {
        let tariff = stored(&two(0.20, 0.10, BERLIN));
        assert_eq!(tariff.split(at(BERLIN, WINTER, (23, 0)), at(BERLIN, (2026, 1, 16), (1, 0))), (0.0, 1.0));
        assert_eq!(tariff.split(at(BERLIN, WINTER, (12, 0)), at(BERLIN, WINTER, (13, 0))), (1.0, 0.0));
        assert_eq!(tariff.band_at(at(BERLIN, WINTER, (3, 0))), Band::Night);
        assert_eq!(tariff.band_at(at(BERLIN, WINTER, (12, 0))), Band::Day);
    }

    #[test]
    fn a_long_gap_is_split_the_same_way() {
        let tariff = stored(&two(0.20, 0.10, BERLIN));
        // Two whole days: 8 hours of night in each, a third.
        let (day, night) = tariff.split(at(BERLIN, WINTER, (12, 0)), at(BERLIN, (2026, 1, 17), (12, 0)));
        close(night, 1.0 / 3.0);
        close(day, 2.0 / 3.0);
    }

    #[test]
    fn summer_hours_apply_while_daylight_saving_time_is_in_effect() {
        let mut input = two(0.20, 0.10, BERLIN);
        input.summer_night = Some(hours("23:00", "07:00"));
        let tariff = stored(&input);
        // Summer time: night is 23:00 to 07:00.
        assert_eq!(tariff.band_at(at(BERLIN, SUMMER, (22, 30))), Band::Day);
        assert_eq!(tariff.band_at(at(BERLIN, SUMMER, (23, 30))), Band::Night);
        assert_eq!(tariff.band_at(at(BERLIN, SUMMER, (6, 30))), Band::Night);
        assert_eq!(tariff.band_at(at(BERLIN, SUMMER, (7, 0))), Band::Day);
        // Back on winter time: night is 22:00 to 06:00 again.
        assert_eq!(tariff.band_at(at(BERLIN, WINTER, (22, 30))), Band::Night);
        assert_eq!(tariff.band_at(at(BERLIN, WINTER, (6, 30))), Band::Day);
        // The day the clocks go back (25 October 2026): before 04:00 summer time, after it winter time.
        let (day, night) = tariff.split(at(BERLIN, (2026, 10, 24), (22, 0)), at(BERLIN, (2026, 10, 24), (23, 0)));
        assert_eq!((day, night), (1.0, 0.0), "still summer hours on the evening before");
        assert_eq!(tariff.band_at(at(BERLIN, (2026, 10, 25), (22, 30))), Band::Night, "winter hours from that day");
    }

    #[test]
    fn summer_hours_never_apply_in_a_zone_without_daylight_saving_time() {
        let mut input = two(0.20, 0.10, TOKYO);
        input.summer_night = Some(hours("23:00", "07:00"));
        let tariff = stored(&input);
        assert_eq!(tariff.band_at(at(TOKYO, SUMMER, (22, 30))), Band::Night);
        assert_eq!(tariff.band_at(at(TOKYO, SUMMER, (6, 30))), Band::Day);
    }

    #[test]
    fn one_price_all_day_has_no_night() {
        let tariff = stored(&one(0.30, "EUR"));
        assert_eq!(tariff.split(at(BERLIN, WINTER, (21, 0)), at(BERLIN, WINTER, (23, 0))), (1.0, 0.0));
        assert_eq!(tariff.band_at(at(BERLIN, WINTER, (3, 0))), Band::Day);
        assert_eq!(tariff.price(Band::Night), 0.30);
    }

    #[test]
    fn without_a_tariff_energy_is_counted_and_nothing_is_charged() {
        let (_dir, store) = store();
        store
            .with(|conn| -> rusqlite::Result<()> {
                assert!(tariff(conn)?.is_none());
                record(conn, 1_000, T, &counter(100, 10.0))?;
                record(conn, 2_000, T, &counter(100, 10.5))?;
                let seen = entry(conn);
                assert_eq!((seen.kwh, seen.since), (10.5, Some(100_000)));
                assert_eq!((seen.cost, seen.cost_day, seen.kwh_night, seen.priced_since), (None, None, None, None));
                assert_eq!((seen.backfilled_kwh, seen.updated_at), (None, 2_000));
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn a_read_across_22_00_is_charged_at_both_prices() {
        let (_dir, store) = store();
        store
            .with(|conn| -> rusqlite::Result<()> {
                set_tariff(conn, 1, "u-owner", &two(0.20, 0.10, BERLIN))?;
                record(conn, at(BERLIN, WINTER, (21, 30)), T, &counter(100, 0.0))?;
                record(conn, at(BERLIN, WINTER, (22, 30)), T, &counter(100, 1.0))?;
                let seen = entry(conn);
                close(seen.kwh_day.unwrap(), 0.5);
                close(seen.kwh_night.unwrap(), 0.5);
                close(seen.cost_day.unwrap(), 0.10);
                close(seen.cost_night.unwrap(), 0.05);
                close(seen.cost.unwrap(), 0.15);
                // All night: only the night price.
                record(conn, at(BERLIN, (2026, 1, 16), (2, 30)), T, &counter(100, 3.0))?;
                close(entry(conn).cost_night.unwrap(), 0.05 + 2.0 * 0.10);
                close(entry(conn).cost_day.unwrap(), 0.10);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn a_tariff_change_counts_from_the_next_read_and_never_reaches_back() {
        let (_dir, store) = store();
        store
            .with(|conn| -> rusqlite::Result<()> {
                set_tariff(conn, 1, "u-owner", &one(0.10, "EUR"))?;
                record(conn, at(BERLIN, WINTER, (21, 0)), T, &counter(100, 0.0))?;
                record(conn, at(BERLIN, WINTER, (21, 50)), T, &counter(100, 10.0))?;
                close(entry(conn).cost.unwrap(), 1.0);
                // A night rate from 21:55: the next read is charged at the tariff in force when it is taken.
                let set = set_tariff(conn, at(BERLIN, WINTER, (21, 55)), "u-owner", &two(0.10, 0.05, BERLIN))?;
                assert!(set.charged_once.is_empty(), "the same currency charges nothing again");
                close(entry(conn).cost.unwrap(), 1.0);
                record(conn, at(BERLIN, WINTER, (22, 10)), T, &counter(100, 12.0))?;
                let seen = entry(conn);
                // 21:50 to 22:10: half day, half night.
                close(seen.cost_day.unwrap(), 1.0 + 1.0 * 0.10);
                close(seen.cost_night.unwrap(), 1.0 * 0.05);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn the_first_tariff_charges_what_was_counted_before_it_once_at_the_day_price() {
        let (_dir, store) = store();
        store
            .with(|conn| -> rusqlite::Result<()> {
                record(conn, 1_000, T, &counter(100, 10.0))?;
                record(conn, 2_000, T, &counter(100, 12.0))?;
                let set = set_tariff(conn, 3_000, "u-owner", &two(0.5, 0.25, BERLIN))?;
                assert_eq!(set.previous, None);
                assert_eq!(set.charged_once, [(T.to_string(), 12.0)]);
                let seen = entry(conn);
                close(seen.cost.unwrap(), 6.0);
                assert_eq!((seen.kwh_day, seen.kwh_night), (Some(12.0), Some(0.0)));
                assert_eq!((seen.priced_since, seen.backfilled_kwh), (Some(3_000), Some(12.0)));
                let stored = tariff(conn)?.unwrap();
                assert_eq!((stored.day_price, stored.night_price, stored.set_at), (0.5, Some(0.25), 3_000));
                assert_eq!((stored.night, stored.time_zone.as_deref()), (Some(hours("22:00", "06:00")), Some(BERLIN)));
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn a_target_first_seen_once_a_tariff_is_set_is_charged_for_the_whole_counter() {
        let (_dir, store) = store();
        store
            .with(|conn| -> rusqlite::Result<()> {
                set_tariff(conn, 500, "u-owner", &one(0.25, "EUR"))?;
                record(conn, 1_000, T, &counter(100, 8.0))?;
                let seen = entry(conn);
                close(seen.cost.unwrap(), 2.0);
                assert_eq!((seen.priced_since, seen.backfilled_kwh), (Some(1_000), Some(8.0)));
                record(conn, 2_000, T, &counter(100, 9.0))?;
                close(entry(conn).cost.unwrap(), 2.25);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn a_counter_that_restarted_by_a_new_since_charges_its_new_total() {
        let (_dir, store) = store();
        store
            .with(|conn| -> rusqlite::Result<()> {
                set_tariff(conn, 500, "u-owner", &one(1.0, "EUR"))?;
                record(conn, 1_000, T, &counter(100, 10.0))?;
                record(conn, 2_000, T, &counter(200, 12.0))?;
                let seen = entry(conn);
                close(seen.cost.unwrap(), 10.0 + 12.0);
                assert_eq!((seen.kwh, seen.since), (12.0, Some(200_000)));
                record(conn, 3_000, T, &counter(200, 12.5))?;
                close(entry(conn).cost.unwrap(), 22.5);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn a_counter_that_went_down_is_a_restart_too() {
        let (_dir, store) = store();
        store
            .with(|conn| -> rusqlite::Result<()> {
                set_tariff(conn, 500, "u-owner", &one(1.0, "EUR"))?;
                record(conn, 1_000, T, &counter(100, 10.0))?;
                record(conn, 2_000, T, &counter(100, 1.5))?;
                close(entry(conn).cost.unwrap(), 11.5);
                record(conn, 3_000, T, &counter(100, 2.0))?;
                close(entry(conn).cost.unwrap(), 12.0);
                record(conn, 4_000, T, &counter(100, 2.0))?;
                close(entry(conn).cost.unwrap(), 12.0);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn energy_counted_while_unpriced_survives_a_restart_and_is_charged_with_the_first_tariff() {
        let (_dir, store) = store();
        store
            .with(|conn| -> rusqlite::Result<()> {
                record(conn, 1_000, T, &counter(100, 10.0))?;
                record(conn, 2_000, T, &counter(200, 3.0))?;
                let set = set_tariff(conn, 3_000, "u-owner", &one(2.0, "EUR"))?;
                assert_eq!(set.charged_once, [(T.to_string(), 13.0)], "both counters were used");
                close(entry(conn).cost.unwrap(), 26.0);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn a_new_currency_is_a_new_first_tariff() {
        let (_dir, store) = store();
        store
            .with(|conn| -> rusqlite::Result<()> {
                record(conn, 1_000, T, &counter(100, 0.0))?;
                set_tariff(conn, 1_500, "u-owner", &one(0.30, "EUR"))?;
                record(conn, 2_000, T, &counter(100, 10.0))?;
                close(entry(conn).cost.unwrap(), 3.0);
                let set = set_tariff(conn, 2_500, "u-owner", &one(0.50, "USD"))?;
                assert_eq!(set.charged_once, [(T.to_string(), 10.0)]);
                let seen = entry(conn);
                close(seen.cost.unwrap(), 5.0);
                assert_eq!((seen.priced_since, seen.backfilled_kwh), (Some(2_500), Some(10.0)));
                assert_eq!(tariff(conn)?.unwrap().currency, "USD");
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn a_tariff_may_be_set_with_nothing_counted_yet_and_two_targets_are_kept_apart() {
        let (_dir, store) = store();
        store
            .with(|conn| -> rusqlite::Result<()> {
                let set = set_tariff(conn, 1, "u-owner", &one(1.0, "EUR"))?;
                assert!(set.charged_once.is_empty() && set.previous.is_none());
                assert!(ledger(conn)?.is_empty());
                record(conn, 10, "a", &counter(1, 1.0))?;
                record(conn, 10, "b", &counter(1, 5.0))?;
                record(conn, 20, "a", &counter(1, 2.0))?;
                let costs: Vec<(String, f64)> =
                    ledger(conn)?.into_iter().map(|e| (e.target, e.cost.unwrap())).collect();
                assert_eq!(costs, [("a".to_string(), 2.0), ("b".to_string(), 5.0)]);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn tariffs_and_currencies_are_checked() {
        assert_eq!(tariff_problem(&one(0.31, "EUR")), None);
        assert_eq!(tariff_problem(&two(0.31, 0.15, BERLIN)), None);
        let mut summer = two(0.31, 0.15, BERLIN);
        summer.summer_night = Some(hours("23:00", "07:00"));
        assert_eq!(tariff_problem(&summer), None);
        for bad_price in [0.0, -0.1, f64::NAN, f64::INFINITY, MAX_PRICE * 2.0] {
            assert!(tariff_problem(&one(bad_price, "EUR")).is_some(), "{bad_price}");
            assert!(tariff_problem(&two(0.3, bad_price, BERLIN)).is_some(), "night {bad_price}");
        }
        let without_hours = TariffInput { night: None, ..two(0.3, 0.1, BERLIN) };
        assert!(tariff_problem(&without_hours).is_some(), "a night price needs night hours");
        let without_price = TariffInput { night_price: None, ..two(0.3, 0.1, BERLIN) };
        assert!(tariff_problem(&without_price).is_some(), "night hours need a night price");
        let summer_alone = TariffInput { summer_night: Some(hours("23:00", "07:00")), ..one(0.3, "EUR") };
        assert!(tariff_problem(&summer_alone).is_some(), "summer hours need the usual ones");
        let empty = TariffInput { night: Some(hours("22:00", "22:00")), ..two(0.3, 0.1, BERLIN) };
        assert!(tariff_problem(&empty).is_some(), "night must start and end at different times");
        let no_zone = TariffInput { time_zone: None, ..two(0.3, 0.1, BERLIN) };
        assert!(tariff_problem(&no_zone).is_some());
        let unknown = two(0.3, 0.1, "Mars/Olympus_Mons");
        assert!(tariff_problem(&unknown).unwrap().contains("Mars/Olympus_Mons"));
        assert_eq!(currency_code("EUR").as_deref(), Some("EUR"));
        assert_eq!(currency_code(" usd ").as_deref(), Some("USD"));
        for bad in ["", "EU", "EURO", "E1R", "€€€", "EU R"] {
            assert_eq!(currency_code(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_tariff_table_holds_one_row_and_positive_prices() {
        let (_dir, store) = store();
        store
            .with(|conn| -> rusqlite::Result<()> {
                let row = |id: i64, day: f64, night: Option<f64>| {
                    conn.execute(
                        "INSERT INTO energy_tariff (id, currency, day_price, night_price, set_at, set_by)
                         VALUES (?1, 'EUR', ?2, ?3, 1, 'u')",
                        params![id, day, night],
                    )
                };
                assert!(row(2, 0.3, None).is_err(), "only one tariff");
                assert!(row(1, 0.0, None).is_err(), "a day price above 0");
                assert!(row(1, 0.3, Some(0.0)).is_err(), "a night price above 0");
                assert!(row(1, 0.3, Some(0.1)).is_ok());
                Ok(())
            })
            .unwrap();
    }
}
