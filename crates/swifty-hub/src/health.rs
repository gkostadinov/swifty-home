//! What the health page reads, from the collector's snapshot and one local file.
//!
//! A zero here has to mean zero. So every reading is either a value from a source
//! that is reporting, or `not_reporting` with no value at all: a source that has
//! stopped, a field the snapshot does not have, or a collector that cannot be read.

use std::io::Read;
use std::path::Path;

use serde_json::{json, Map, Value};
use swifty_net::device_text;
use swifty_plugin_api::State;

use crate::app::App;

pub const NOT_REPORTING: &str = "not_reporting";
/// Why there is nothing to report: no telemetry node is set up.
const NO_TELEMETRY: &str = "No telemetry node is set up.";
const REPORTING: &str = "ok";
const MAX_CANARY_BYTES: u64 = 64 * 1024;
const MAX_ENTRIES: usize = 128;
/// A source older than this has stopped, when its age is all it says.
const MAX_SOURCE_AGE_S: f64 = 600.0;

/// The names a part of the snapshot may be reported under in `sources`.
const FAULT_SOURCES: &[&str] = &["faults", "kernel", "journal", "dmesg"];
const DISK_SOURCES: &[&str] = &["disks", "smart", "smartctl"];
const STORAGE_SOURCES: &[&str] = &["storage", "zfs", "zpool", "truenas"];
const BLUETOOTH_SOURCES: &[&str] = &["bluetooth", "bt"];

/// The counters that are always answered for, even when nothing has been read.
const NAMED_FAULTS: &[&str] = &["segfault_count_24h"];
/// What each storage counter is called here, then in a snapshot.
const STORAGE_COUNTERS: &[(&str, &[&str])] = &[
    ("read_errors", &["read_errors", "read"]),
    ("write_errors", &["write_errors", "write"]),
    ("checksum_errors", &["checksum_errors", "checksum", "cksum"]),
    ("scrub_errors", &["scrub_errors", "scrub"]),
];

fn not_reporting() -> Value {
    json!({ "status": NOT_REPORTING })
}

fn reading(value: Option<&Value>) -> Value {
    match value {
        Some(value) if value.is_number() => json!({ "status": REPORTING, "value": value }),
        _ => not_reporting(),
    }
}

fn good_word(text: &str) -> bool {
    matches!(text.to_ascii_lowercase().as_str(), "ok" | "up" | "reporting" | "fresh" | "healthy" | "online" | "running")
}

/// Whether one entry in `sources` says it is reporting. If it says nothing that
/// can be understood, it is not.
fn source_reporting(entry: &Value) -> bool {
    match entry {
        Value::Bool(flag) => *flag,
        Value::String(text) => good_word(text),
        Value::Object(fields) => {
            if fields.get("stale").and_then(Value::as_bool) == Some(true) {
                return false;
            }
            if let Some(age) = ["age_s", "age_seconds"].iter().find_map(|k| fields.get(*k)).and_then(Value::as_f64) {
                if age > MAX_SOURCE_AGE_S {
                    return false;
                }
            }
            for key in ["reporting", "ok", "up", "available"] {
                if let Some(flag) = fields.get(key).and_then(Value::as_bool) {
                    return flag;
                }
            }
            ["status", "state"].iter().find_map(|key| fields.get(*key)).and_then(Value::as_str).is_some_and(good_word)
        }
        _ => false,
    }
}

fn source_entry<'a>(snapshot: &'a Value, names: &[&str]) -> Option<(&'a str, &'a Value)> {
    let sources = snapshot.get("sources")?.as_object()?;
    sources.iter().find(|(name, _)| names.contains(&name.as_str())).map(|(name, entry)| (name.as_str(), entry))
}

/// Whether a named source is present and has stopped. Absent is not stopped.
pub fn named_source_stopped(snapshot: &Value, names: &[&str]) -> bool {
    source_entry(snapshot, names).is_some_and(|(_, entry)| !source_reporting(entry))
}

/// Whether a part of the snapshot can be believed. No `sources`, or no entry for
/// this part, means it cannot.
fn reporting(snapshot: &Value, names: &[&str]) -> bool {
    source_entry(snapshot, names).is_some_and(|(_, entry)| source_reporting(entry))
}

fn clean_key(key: &str) -> String {
    device_text(key)
}

fn faults_section(snapshot: &Value) -> Value {
    if !reporting(snapshot, FAULT_SOURCES) {
        let counters: Map<String, Value> =
            NAMED_FAULTS.iter().map(|name| ((*name).to_string(), not_reporting())).collect();
        return json!({ "status": NOT_REPORTING, "counters": counters });
    }
    let faults = snapshot.get("faults").and_then(Value::as_object);
    let mut counters: Map<String, Value> =
        NAMED_FAULTS.iter().map(|name| ((*name).to_string(), reading(faults.and_then(|f| f.get(*name))))).collect();
    if let Some(faults) = faults {
        for (name, value) in faults.iter().take(MAX_ENTRIES) {
            if value.is_number() {
                counters.insert(clean_key(name), reading(Some(value)));
            }
        }
    }
    json!({ "status": REPORTING, "counters": counters })
}

/// The items in a list of things, given as an array or as an object keyed by name.
fn named_items(value: &Value) -> Vec<(Option<String>, &Value)> {
    match value {
        Value::Array(items) => items.iter().take(MAX_ENTRIES).map(|item| (None, item)).collect(),
        Value::Object(items) => items.iter().take(MAX_ENTRIES).map(|(name, item)| (Some(name.clone()), item)).collect(),
        _ => Vec::new(),
    }
}

fn item_name(key: Option<String>, item: &Value, fields: [&str; 2]) -> Option<String> {
    fields
        .iter()
        .find_map(|field| item.get(*field))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or(key)
        .map(|name| device_text(&name))
}

fn disk_view(key: Option<String>, disk: &Value) -> Value {
    let health = match disk.get("health") {
        Some(Value::String(text)) => json!({ "status": REPORTING, "value": device_text(text) }),
        Some(Value::Bool(passed)) => json!({ "status": REPORTING, "value": if *passed { "PASSED" } else { "FAILED" } }),
        _ => not_reporting(),
    };
    json!({ "name": item_name(key, disk, ["name", "device"]), "health": health, "media_errors": reading(disk.get("media_errors")) })
}

fn disks_section(snapshot: &Value) -> Value {
    if !reporting(snapshot, DISK_SOURCES) {
        return json!({ "status": NOT_REPORTING, "disks": [] });
    }
    let Some(disks) = snapshot.get("disks") else {
        return json!({ "status": NOT_REPORTING, "disks": [] });
    };
    let disks: Vec<Value> = named_items(disks).into_iter().map(|(key, disk)| disk_view(key, disk)).collect();
    json!({ "status": REPORTING, "disks": disks })
}

fn pool_view(key: Option<String>, pool: &Value) -> Value {
    let mut view = Map::new();
    view.insert("name".to_string(), json!(item_name(key, pool, ["name", "pool"])));
    for (counter, names) in STORAGE_COUNTERS {
        view.insert((*counter).to_string(), reading(names.iter().find_map(|name| pool.get(*name))));
    }
    Value::Object(view)
}

fn storage_section(snapshot: &Value) -> Value {
    let unknown = || {
        let empty = json!({});
        let totals = pool_view(None, &empty);
        json!({ "status": NOT_REPORTING, "totals": without_name(totals), "pools": [] })
    };
    if !reporting(snapshot, STORAGE_SOURCES) {
        return unknown();
    }
    let Some(storage) = snapshot.get("storage") else {
        return unknown();
    };
    let pools: Vec<Value> = storage
        .get("pools")
        .map(|pools| named_items(pools).into_iter().map(|(key, pool)| pool_view(key, pool)).collect())
        .unwrap_or_default();
    json!({ "status": REPORTING, "totals": without_name(pool_view(None, storage)), "pools": pools })
}

fn without_name(mut view: Value) -> Value {
    if let Some(fields) = view.as_object_mut() {
        fields.remove("name");
    }
    view
}

/// Bluetooth drops come from an agent on the machine that holds the radios. They
/// are not reported until such a source exists.
fn bluetooth_section(snapshot: &Value) -> Value {
    let read = |group: &str| {
        let value = snapshot.pointer(&format!("/bluetooth/{group}/drops_24h"));
        reading(value)
    };
    if !reporting(snapshot, BLUETOOTH_SOURCES) {
        return json!({ "status": NOT_REPORTING, "controllers": not_reporting(), "headsets": not_reporting() });
    }
    json!({ "status": REPORTING, "controllers": read("controllers"), "headsets": read("headsets") })
}

fn sources_section(snapshot: &Value) -> Value {
    let Some(sources) = snapshot.get("sources").and_then(Value::as_object) else {
        return json!({});
    };
    let view: Map<String, Value> = sources
        .iter()
        .take(MAX_ENTRIES)
        .map(|(name, entry)| {
            let status = if source_reporting(entry) { REPORTING } else { NOT_REPORTING };
            (clean_key(name), json!({ "status": status }))
        })
        .collect();
    Value::Object(view)
}

/// Reads the last canary result. The file's shape is not fixed, so one JSON object
/// or one line of text are both accepted. No file, an empty one, or one that cannot
/// be read is not a result.
pub fn cpu_canary(path: Option<&Path>) -> Value {
    let Some(path) = path else {
        return not_reporting();
    };
    let Ok(file) = std::fs::File::open(path) else {
        return not_reporting();
    };
    let mut text = String::new();
    if file.take(MAX_CANARY_BYTES).read_to_string(&mut text).is_err() {
        return not_reporting();
    }
    let text = text.trim();
    if text.is_empty() {
        return not_reporting();
    }
    if let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(text) {
        let mut out = Map::new();
        out.insert("status".to_string(), json!(REPORTING));
        for (name, value) in fields.iter().take(MAX_ENTRIES) {
            let value = match value {
                Value::Number(_) | Value::Bool(_) => value.clone(),
                Value::String(text) => json!(device_text(text)),
                _ => continue,
            };
            let key = clean_key(name);
            if key != "status" {
                out.insert(key, value);
            } else {
                out.insert("result".to_string(), value);
            }
        }
        return Value::Object(out);
    }
    let first_line = text.lines().next().unwrap_or_default();
    json!({ "status": REPORTING, "result": device_text(first_line) })
}

fn sections(snapshot: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("faults".to_string(), faults_section(snapshot));
    out.insert("disks".to_string(), disks_section(snapshot));
    out.insert("storage".to_string(), storage_section(snapshot));
    out.insert("bluetooth_drops".to_string(), bluetooth_section(snapshot));
    out.insert("sources".to_string(), sources_section(snapshot));
    out
}

/// The whole report. Nothing here can fail: whatever is missing is reported as
/// missing.
pub async fn report(app: &App) -> Value {
    let live = app.registry.is_live();
    let read = match crate::history::telemetry_source(app) {
        Some(source) => app.registry.read_state(&source).await,
        None if !live => Ok(None),
        None => Err(swifty_plugin_api::DeviceError::Disabled(NO_TELEMETRY.to_string())),
    };
    let (collector, snapshot) = match read {
        Ok(Some(State::Telemetry { data })) => (json!({ "status": REPORTING }), data),
        Ok(None) => (json!({ "status": NOT_REPORTING, "simulated": true }), json!({})),
        Ok(Some(_)) => (json!({ "status": NOT_REPORTING, "error": "unexpected reading" }), json!({})),
        Err(error) => (json!({ "status": NOT_REPORTING, "error": error.to_string() }), json!({})),
    };
    let mut body = sections(&snapshot);
    body.insert("live".to_string(), json!(live));
    body.insert("simulated".to_string(), json!(!live));
    body.insert("read_at".to_string(), json!(app.now()));
    body.insert("collector".to_string(), collector);
    body.insert("cpu_canary".to_string(), cpu_canary(app.config.health.cpu_canary_file.as_deref()));
    Value::Object(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_snapshot() -> Value {
        json!({
            "faults": { "segfault_count_24h": 0, "oom_count_24h": 2, "label": "x" },
            "disks": [{ "name": "nvme0", "health": "PASSED", "media_errors": 0 }],
            "storage": {
                "read_errors": 1, "write_errors": 0, "checksum_errors": 3, "scrub_errors": 0,
                "pools": [{ "name": "fast", "read_errors": 1, "write_errors": 0, "checksum_errors": 3, "scrub_errors": 0 }],
            },
            "sources": {
                "faults": { "status": "ok" },
                "disks": true,
                "storage": { "reporting": true, "age_s": 30 },
            },
        })
    }

    #[test]
    fn a_reporting_source_gives_real_values_including_a_real_zero() {
        let out = sections(&full_snapshot());
        assert_eq!(out["faults"]["status"], json!("ok"));
        assert_eq!(out["faults"]["counters"]["segfault_count_24h"], json!({ "status": "ok", "value": 0 }));
        assert_eq!(out["faults"]["counters"]["oom_count_24h"], json!({ "status": "ok", "value": 2 }));
        assert!(out["faults"]["counters"].get("label").is_none());
        assert_eq!(out["disks"]["disks"][0]["health"]["value"], json!("PASSED"));
        assert_eq!(out["disks"]["disks"][0]["media_errors"], json!({ "status": "ok", "value": 0 }));
        assert_eq!(out["storage"]["totals"]["checksum_errors"]["value"], json!(3));
        assert_eq!(out["storage"]["pools"][0]["name"], json!("fast"));
    }

    #[test]
    fn a_source_that_stopped_reports_nothing_and_never_a_zero() {
        for stopped in [
            json!(false),
            json!("stale"),
            json!({ "status": "stale" }),
            json!({ "ok": false }),
            json!({ "stale": true, "status": "ok" }),
            json!({ "age_s": 86_400, "status": "ok" }),
            json!({}),
            json!(0),
            json!(null),
        ] {
            let mut snapshot = full_snapshot();
            snapshot["sources"]["faults"] = stopped.clone();
            snapshot["sources"]["disks"] = stopped.clone();
            snapshot["sources"]["storage"] = stopped.clone();
            let out = sections(&snapshot);
            assert_eq!(out["faults"]["status"], json!(NOT_REPORTING), "{stopped}");
            assert_eq!(
                out["faults"]["counters"]["segfault_count_24h"],
                json!({ "status": NOT_REPORTING }),
                "{stopped}"
            );
            assert_eq!(out["disks"]["status"], json!(NOT_REPORTING), "{stopped}");
            assert_eq!(out["disks"]["disks"], json!([]), "{stopped}");
            assert_eq!(out["storage"]["status"], json!(NOT_REPORTING), "{stopped}");
            assert_eq!(out["storage"]["totals"]["read_errors"], json!({ "status": NOT_REPORTING }), "{stopped}");
            let text = out["storage"].to_string();
            assert!(!text.contains("value"), "{text}");
        }
    }

    #[test]
    fn with_no_sources_object_nothing_is_believed() {
        let mut snapshot = full_snapshot();
        snapshot.as_object_mut().unwrap().remove("sources");
        let out = sections(&snapshot);
        for section in ["faults", "disks", "storage", "bluetooth_drops"] {
            assert_eq!(out[section]["status"], json!(NOT_REPORTING), "{section}");
        }
    }

    #[test]
    fn a_missing_field_under_a_reporting_source_is_not_a_zero() {
        let snapshot = json!({
            "faults": {},
            "disks": [{ "name": "sda" }],
            "storage": { "read_errors": 0 },
            "sources": { "faults": true, "disks": true, "storage": true },
        });
        let out = sections(&snapshot);
        assert_eq!(out["faults"]["counters"]["segfault_count_24h"], json!({ "status": NOT_REPORTING }));
        assert_eq!(out["disks"]["disks"][0]["media_errors"], json!({ "status": NOT_REPORTING }));
        assert_eq!(out["disks"]["disks"][0]["health"], json!({ "status": NOT_REPORTING }));
        assert_eq!(out["storage"]["totals"]["read_errors"], json!({ "status": "ok", "value": 0 }));
        assert_eq!(out["storage"]["totals"]["scrub_errors"], json!({ "status": NOT_REPORTING }));
    }

    #[test]
    fn lists_can_be_arrays_or_keyed_by_name() {
        let snapshot = json!({
            "disks": { "nvme0": { "health": true, "media_errors": 1 } },
            "storage": { "pools": { "fast": { "read": 2, "write": 0, "cksum": 3, "scrub": 0 } } },
            "sources": { "disks": true, "storage": true },
        });
        let out = sections(&snapshot);
        assert_eq!(out["disks"]["disks"][0]["name"], json!("nvme0"));
        assert_eq!(out["disks"]["disks"][0]["health"]["value"], json!("PASSED"));
        assert_eq!(out["storage"]["pools"][0]["name"], json!("fast"));
        assert_eq!(out["storage"]["pools"][0]["read_errors"]["value"], json!(2));
        assert_eq!(out["storage"]["pools"][0]["checksum_errors"]["value"], json!(3));
    }

    #[test]
    fn bluetooth_drops_need_a_source() {
        let out = sections(&full_snapshot());
        assert_eq!(out["bluetooth_drops"]["status"], json!(NOT_REPORTING));
        assert_eq!(out["bluetooth_drops"]["controllers"], json!({ "status": NOT_REPORTING }));
        assert_eq!(out["bluetooth_drops"]["headsets"], json!({ "status": NOT_REPORTING }));

        let mut snapshot = full_snapshot();
        snapshot["sources"]["bluetooth"] = json!({ "status": "ok" });
        snapshot["bluetooth"] = json!({ "controllers": { "drops_24h": 4 }, "headsets": { "drops_24h": 0 } });
        let out = sections(&snapshot);
        assert_eq!(out["bluetooth_drops"]["controllers"], json!({ "status": "ok", "value": 4 }));
        assert_eq!(out["bluetooth_drops"]["headsets"], json!({ "status": "ok", "value": 0 }));
    }

    #[test]
    fn the_sources_are_listed_with_what_each_is_doing() {
        let mut snapshot = full_snapshot();
        snapshot["sources"]["old"] = json!({ "status": "stale" });
        let out = sections(&snapshot);
        assert_eq!(out["sources"]["faults"], json!({ "status": "ok" }));
        assert_eq!(out["sources"]["old"], json!({ "status": NOT_REPORTING }));
    }

    #[test]
    fn the_canary_is_only_what_the_file_says() {
        assert_eq!(cpu_canary(None), json!({ "status": NOT_REPORTING }));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("canary");
        assert_eq!(cpu_canary(Some(&path)), json!({ "status": NOT_REPORTING }));
        std::fs::write(&path, "  \n").unwrap();
        assert_eq!(cpu_canary(Some(&path)), json!({ "status": NOT_REPORTING }));
        std::fs::write(&path, "PASS all cores 2026-11-09\nsecond line").unwrap();
        assert_eq!(cpu_canary(Some(&path)), json!({ "status": "ok", "result": "PASS all cores 2026-11-09" }));
        std::fs::write(&path, r#"{"status":"pass","ran_at":1900,"nested":{"a":1}}"#).unwrap();
        assert_eq!(cpu_canary(Some(&path)), json!({ "status": "ok", "result": "pass", "ran_at": 1900 }));
    }
}
