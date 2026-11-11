//! Time series for charts. A host's plugin keeps its own history for hosts and VMs.
//! What it does not keep (per-core detail, temperature, package power) is sampled
//! from the telemetry node and stored here.
//!
//! One telemetry node feeds the charts and the health page: the one `[health]
//! telemetry` names, or else the first active device of class `telemetry` with
//! readings, in target order ([`telemetry_source`]). Every telemetry node's energy
//! counter goes into the cost ledger ([`crate::energy`]) each time it is read here.

use std::collections::BTreeMap;

use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};
use swifty_plugin_api::{CapKind, DeviceClass, HistoryRange, State};

use crate::app::App;
use crate::health;

/// A little over the longest chart.
pub const RETENTION_MS: i64 = 8 * 24 * 3_600_000;
const MAX_METRICS_PER_SAMPLE: usize = 512;
const MAX_METRIC_LEN: usize = 64;
/// The most series one chart answer will carry.
const MAX_SERIES: usize = 256;

/// Collector readings with a single value, by the name they are stored under.
const SCALAR_METRICS: &[&str] = &[
    "pkg_temp_c",
    "pkg_watts",
    "cpu_temp_c",
    "cpu_usage",
    "gpu_temp_c",
    "gpu_watts",
    "wall_watts",
    "memory_used_bytes",
    "energy_watts",
    "energy_kwh",
];
/// The per-core readings kept, where the snapshot gives a core as an object.
const CORE_FIELDS: &[&str] = &["usage", "util_pct", "temp_c", "mhz", "watts"];
/// What reports the temperature and power readings, in the collector's `sources`.
const SENSOR_SOURCES: &[&str] = &["cpu", "sensors", "hwmon", "rapl"];

fn finite(value: &Value) -> Option<f64> {
    value.as_f64().filter(|number| number.is_finite())
}

/// The readings worth keeping from one snapshot. A reading that is missing stays
/// missing: it becomes a gap in the chart, never a zero.
pub fn sample_metrics(snapshot: &Value) -> Vec<(String, f64)> {
    if health::named_source_stopped(snapshot, SENSOR_SOURCES) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for name in SCALAR_METRICS {
        if let Some(value) = snapshot.get(*name).and_then(finite) {
            out.push(((*name).to_string(), value));
        }
    }
    for list in ["cores", "per_core"] {
        let Some(cores) = snapshot.get(list).and_then(Value::as_array) else { continue };
        for (index, core) in cores.iter().enumerate().take(MAX_METRICS_PER_SAMPLE) {
            if let Some(value) = finite(core) {
                out.push((format!("core.{index}"), value));
                continue;
            }
            for field in CORE_FIELDS {
                if let Some(value) = core.get(*field).and_then(finite) {
                    out.push((format!("core.{index}.{field}"), value));
                }
            }
        }
    }
    out.truncate(MAX_METRICS_PER_SAMPLE);
    out
}

pub fn store_samples(conn: &Connection, ts: i64, samples: &[(String, f64)]) -> rusqlite::Result<()> {
    for (metric, value) in samples {
        conn.execute(
            "INSERT INTO collector_samples (ts, metric, value) VALUES (?1, ?2, ?3)",
            params![ts, metric, value],
        )?;
    }
    Ok(())
}

pub fn prune(conn: &Connection, now: i64) -> rusqlite::Result<usize> {
    conn.execute("DELETE FROM collector_samples WHERE ts < ?1", params![now - RETENTION_MS])
}

/// A metric name as asked for by a client.
pub fn is_metric_name(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_METRIC_LEN
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_'))
}

/// Stored samples averaged into time buckets, one series per metric.
pub fn series(conn: &Connection, now: i64, range: HistoryRange, metric: Option<&str>) -> rusqlite::Result<Value> {
    let bucket = range.bucket_ms();
    let since = now - range.span_ms();
    let mut statement = conn.prepare(
        "SELECT metric, (ts / ?1) * ?1 AS bucket, AVG(value) FROM collector_samples
         WHERE ts > ?2 AND (?3 IS NULL OR metric = ?3)
         GROUP BY metric, bucket ORDER BY metric ASC, bucket ASC",
    )?;
    let rows = statement.query_map(params![bucket, since, metric], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, f64>(2)?))
    })?;
    let mut grouped: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for row in rows {
        let (name, at, value) = row?;
        if grouped.len() < MAX_SERIES || grouped.contains_key(&name) {
            grouped.entry(name).or_default().push(json!([at, value]));
        }
    }
    let map: Map<String, Value> = grouped.into_iter().map(|(name, points)| (name, Value::Array(points))).collect();
    Ok(Value::Object(map))
}

/// The telemetry node whose readings feed the charts and the health page: the one
/// the configuration names when the index has it, or else the first active device
/// of class `telemetry` with readings.
pub fn telemetry_source(app: &App) -> Option<String> {
    let index = app.registry.index();
    let chosen = app.config.health.telemetry.as_deref();
    match chosen {
        Some(target) => index.get(target).filter(|device| !device.pending).map(|device| device.target.clone()),
        None => index
            .of_class(DeviceClass::Telemetry)
            .find(|device| device.has(CapKind::Metrics))
            .map(|device| device.target.clone()),
    }
}

/// Every active telemetry device that can report readings, in target order, and the one
/// that feeds the charts when it is not among them.
fn telemetry_targets(app: &App) -> Vec<String> {
    let index = app.registry.index();
    let mut targets: Vec<String> = index
        .of_class(DeviceClass::Telemetry)
        .filter(|device| !device.pending && device.has(CapKind::Metrics))
        .map(|device| device.target.clone())
        .collect();
    if let Some(source) = telemetry_source(app) {
        if !targets.contains(&source) {
            targets.push(source);
        }
    }
    targets
}

/// Takes one sample from the telemetry node, and takes each telemetry node's energy counter
/// into the cost ledger. Returns how many readings went to the charts. In dry-run the
/// registry contacts nothing and this stores nothing.
pub async fn sample_collector(app: &App) -> Result<usize, crate::error::ApiError> {
    let source = telemetry_source(app);
    let mut charted = 0;
    for target in telemetry_targets(app) {
        let reading = app.registry.read_state(&target).await;
        let Ok(Some(State::Telemetry { data })) = reading else {
            continue;
        };
        let samples = if source.as_deref() == Some(target.as_str()) { sample_metrics(&data) } else { Vec::new() };
        let energy = crate::energy::reading(&data);
        let now = app.now();
        app.store.tx(|tx| -> Result<(), crate::error::ApiError> {
            if source.as_deref() == Some(target.as_str()) {
                store_samples(tx, now, &samples)?;
                prune(tx, now)?;
            }
            if let Some(energy) = &energy {
                crate::energy::record(tx, now, &target, energy)?;
            }
            Ok(())
        })?;
        charted += samples.len();
    }
    Ok(charted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_readings_are_kept() {
        let samples = sample_metrics(&json!({
            "pkg_temp_c": 64.5,
            "pkg_watts": 118,
            "hostname": "pve",
            "unknown_number": 7,
            "cores": [{ "usage": 10.0, "temp_c": 50, "name": "x" }, 22.5],
        }));
        let names: Vec<&str> = samples.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["pkg_temp_c", "pkg_watts", "core.0.usage", "core.0.temp_c", "core.1"]);
    }

    #[test]
    fn a_missing_reading_is_a_gap_and_never_a_zero() {
        assert!(sample_metrics(&json!({})).is_empty());
        assert!(sample_metrics(&json!({ "pkg_temp_c": null, "pkg_watts": "n/a" })).is_empty());
    }

    #[test]
    fn a_source_that_stopped_contributes_no_samples() {
        let snapshot = json!({ "pkg_temp_c": 0, "sources": { "sensors": { "status": "stale" } } });
        assert!(sample_metrics(&snapshot).is_empty());
        let snapshot = json!({ "pkg_temp_c": 44, "sources": { "sensors": { "status": "ok" } } });
        assert_eq!(sample_metrics(&snapshot).len(), 1);
    }

    #[test]
    fn metric_names_are_plain() {
        assert!(is_metric_name("core.12.temp_c"));
        for bad in ["", "a b", "a;b", "x'", &"m".repeat(65)] {
            assert!(!is_metric_name(bad), "{bad}");
        }
    }
}
