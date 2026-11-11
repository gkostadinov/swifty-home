//! The node's HTTP contract, as pure functions: what is sent and how a snapshot
//! is read. No socket here. The README describes the same contract for people.

use serde_json::{json, Value};
use swifty_net::device_text;

/// The largest snapshot the hub reads. A node's snapshot can be large.
pub const MAX_SNAPSHOT: usize = 8 * 1024 * 1024;

/// The flat names the hub's charts and health page read, and where a node that
/// nests its readings keeps each of them (a JSON pointer).
pub const NESTED_READINGS: &[(&str, &str)] = &[
    ("cpu_usage", "/host/cpu_pct"),
    ("memory_used_bytes", "/host/mem_used_bytes"),
    ("pkg_watts", "/power/package_w"),
    ("pkg_temp_c", "/power/cpu_temp_c"),
    ("cpu_temp_c", "/power/cpu_temp_c"),
    ("energy_kwh", "/energy/total_kwh"),
];

/// The most energy domains whose power is added into `energy_watts`: the contract's own bound.
pub const MAX_ENERGY_DOMAINS: usize = 8;

/// The readings taken from the first GPU whose `kind` is `"discrete"`: the flat
/// name, then the field in that GPU's entry.
pub const DISCRETE_GPU_READINGS: &[(&str, &str)] = &[("gpu_temp_c", "temp_c"), ("gpu_watts", "power_w")];

/// The node nests its readings, and the page and the charts use flat names. The flat names are
/// added only where the node gave a number, so a missing reading stays a gap and never a zero.
pub fn with_flat_readings(mut snapshot: Value) -> Value {
    let number = |value: Option<&Value>| value.filter(|v| v.is_number()).cloned();
    let mut flat: Vec<(&str, Value)> = NESTED_READINGS
        .iter()
        .filter_map(|(name, path)| number(snapshot.pointer(path)).map(|value| (*name, value)))
        .collect();
    let discrete_gpu = snapshot
        .get("gpus")
        .and_then(Value::as_array)
        .and_then(|gpus| gpus.iter().find(|gpu| gpu.get("kind").and_then(Value::as_str) == Some("discrete")));
    if let Some(gpu) = discrete_gpu {
        for &(name, field) in DISCRETE_GPU_READINGS {
            if let Some(value) = number(gpu.get(field)) {
                flat.push((name, value));
            }
        }
    }
    let cores = snapshot.pointer("/host/cpu_cores").filter(|value| value.is_array()).cloned();
    let energy_watts = energy_watts(&snapshot);
    if let Some(fields) = snapshot.as_object_mut() {
        for (name, value) in flat {
            fields.entry(name).or_insert(value);
        }
        if let Some(watts) = energy_watts {
            fields.entry("energy_watts").or_insert(watts);
        }
        if let Some(cores) = cores {
            fields.entry("cores").or_insert(cores);
        }
    }
    snapshot
}

/// The power of everything the energy counter covers: the sum of the domains' `watts`. A domain
/// that gave none adds nothing, and when none gave a number there is no reading at all, never zero.
fn energy_watts(snapshot: &Value) -> Option<Value> {
    let domains = snapshot.pointer("/energy/domains")?.as_array()?;
    let watts: Vec<f64> = domains
        .iter()
        .take(MAX_ENERGY_DOMAINS)
        .filter_map(|domain| domain.get("watts").and_then(Value::as_f64))
        .filter(|watts| watts.is_finite())
        .collect();
    if watts.is_empty() {
        return None;
    }
    serde_json::Number::from_f64(watts.iter().sum()).map(Value::Number)
}

/// The snapshot with every piece of text in it made safe to show and store:
/// each string and each key goes through `device_text`, as anything a device
/// says must. Numbers, booleans and the shape stay as they are.
pub fn cleaned(value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(device_text(&text)),
        Value::Array(items) => Value::Array(items.into_iter().map(cleaned).collect()),
        Value::Object(fields) => {
            Value::Object(fields.into_iter().map(|(key, value)| (device_text(&key), cleaned(value))).collect())
        }
        other => other,
    }
}

/// The body of a pairing request: the name the person gave the agent.
pub fn pairing_request(name: &str) -> Value {
    json!({ "name": name })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The field names and nesting a node really sends.
    fn snapshot() -> Value {
        json!({
            "host": { "cpu_pct": 12.5, "mem_used_bytes": 1024.0, "cpu_cores": [10.0, 20.0] },
            "power": { "package_w": 45.0, "cpu_temp_c": 61.0 },
            "gpus": [
                { "kind": "integrated", "temp_c": 40.0, "power_w": 3.0 },
                { "kind": "discrete", "temp_c": 52.0, "power_w": null },
            ],
        })
    }

    #[test]
    fn nested_readings_get_the_names_the_page_uses() {
        let flat = with_flat_readings(snapshot());
        assert_eq!(flat["cpu_usage"], 12.5);
        assert_eq!(flat["memory_used_bytes"], 1024.0);
        assert_eq!(flat["pkg_watts"], 45.0);
        assert_eq!(flat["pkg_temp_c"], 61.0);
        assert_eq!(flat["cpu_temp_c"], 61.0);
        assert_eq!(flat["gpu_temp_c"], 52.0, "the discrete card, not the integrated one");
        assert_eq!(flat["cores"], json!([10.0, 20.0]));
    }

    #[test]
    fn a_reading_the_collector_did_not_give_stays_missing() {
        let flat = with_flat_readings(snapshot());
        assert!(flat.get("gpu_watts").is_none(), "null must not become a reading");
        assert!(flat.get("wall_watts").is_none());
        let flat = with_flat_readings(json!({ "host": { "cpu_pct": null } }));
        assert!(flat.get("cpu_usage").is_none());
    }

    #[test]
    fn a_flat_reading_already_there_is_kept() {
        let flat = with_flat_readings(json!({ "cpu_usage": 1.0, "host": { "cpu_pct": 99.0 } }));
        assert_eq!(flat["cpu_usage"], 1.0);
    }

    #[test]
    fn something_that_is_not_an_object_passes_through() {
        assert_eq!(with_flat_readings(json!([1, 2])), json!([1, 2]));
    }

    #[test]
    fn every_text_in_a_snapshot_is_made_safe_and_the_rest_kept() {
        let long = "x".repeat(1_000);
        let snapshot = json!({
            "disks": [{ "name": format!("nvme0\u{1b}[2J{long}"), "health": "PASSED", "media_errors": 0 }],
            "storage": { format!("tank\u{7}{long}"): { "used_pct": 41.5 } },
            "pkg_watts": 118,
            "ok": true,
        });
        let clean = cleaned(snapshot);
        let name = clean["disks"][0]["name"].as_str().unwrap();
        assert!(name.starts_with("nvme0[2J") && name.chars().count() == 160, "{name}");
        assert_eq!(clean["disks"][0]["health"], "PASSED");
        assert_eq!(clean["disks"][0]["media_errors"], 0);
        let key = clean["storage"].as_object().unwrap().keys().next().unwrap().clone();
        assert!(key.starts_with("tank") && !key.contains('\u{7}') && key.chars().count() == 160);
        assert_eq!(clean["storage"][&key]["used_pct"], 41.5);
        assert_eq!((clean["pkg_watts"].clone(), clean["ok"].clone()), (json!(118), json!(true)));
    }

    /// A hybrid host with three guests and the energy counter, as the contract describes them.
    fn hybrid_snapshot() -> Value {
        json!({
            "host": {
                "cpu_pct": 12.5,
                "mem_used_bytes": 1024.0,
                "mem_total_bytes": 4096.0,
                "cpu_cores": [10.0, 20.0, 30.0, 40.0],
                "cpu_core_kinds": ["performance", "performance", "efficiency", null],
            },
            "guests": [
                { "id": 100, "name": "media", "type": "vm", "status": "running", "mem_used_bytes": 700,
                  "mem_total_bytes": 1000, "mem_host_bytes": 900, "mem_assigned_bytes": 1000,
                  "cpu_affinity": [0, 1], "cpu_count": 2 },
                { "id": 101, "name": "files", "type": "vm", "status": "running", "cpu_affinity": null, "cpu_count": 4 },
                { "id": 102, "name": "build", "type": "lxc", "status": "stopped" },
            ],
            "energy": {
                "since": 1794380000,
                "updated_at": 1794381234,
                "total_kwh": 12.3456,
                "domains": [
                    { "id": "cpu-package", "label": "CPU package", "kwh": 11.9, "source": "rapl", "watts": 48.25 },
                    { "id": "gpu-0", "label": "GPU 0", "kwh": 0.44, "source": "power", "watts": 3.5 },
                    { "id": "gpu-1", "label": "GPU 1", "kwh": 0.0, "source": "power", "watts": null },
                ],
                "scope": "components",
            },
        })
    }

    #[test]
    fn the_threads_guests_and_energy_pass_through_untouched() {
        let original = hybrid_snapshot();
        let out = cleaned(with_flat_readings(original.clone()));
        assert_eq!(out["host"]["cpu_core_kinds"], original["host"]["cpu_core_kinds"]);
        assert_eq!(out["host"]["mem_total_bytes"], original["host"]["mem_total_bytes"]);
        assert_eq!(out["guests"], original["guests"], "affinity lists, null affinity, counts and memory");
        assert_eq!(out["energy"], original["energy"]);
        assert!(out["guests"][1]["cpu_affinity"].is_null(), "a guest that may run anywhere stays null");
        assert!(out["guests"][2].get("cpu_affinity").is_none(), "an unreadable config stays absent");
    }

    #[test]
    fn energy_is_flattened_into_two_readings_for_the_charts() {
        let flat = with_flat_readings(hybrid_snapshot());
        assert_eq!(flat["energy_kwh"], 12.3456);
        assert_eq!(flat["energy_watts"], 48.25 + 3.5, "the domains that gave watts, added; the null one adds nothing");
    }

    #[test]
    fn a_node_without_energy_gets_no_energy_readings_and_never_a_zero() {
        for snapshot in [
            json!({}),
            json!({ "energy": null }),
            json!({ "energy": { "domains": [] } }),
            json!({ "energy": { "total_kwh": "n/a", "domains": [{ "watts": null }, { "watts": "x" }, {}] } }),
        ] {
            let flat = with_flat_readings(snapshot.clone());
            assert!(flat.get("energy_kwh").is_none(), "{snapshot}");
            assert!(flat.get("energy_watts").is_none(), "{snapshot}");
        }
        let only_kwh = with_flat_readings(json!({ "energy": { "total_kwh": 3.0, "domains": [{ "kwh": 3.0 }] } }));
        assert_eq!(only_kwh["energy_kwh"], 3.0);
        assert!(only_kwh.get("energy_watts").is_none(), "no domain gave a power");
    }

    #[test]
    fn only_the_first_eight_domains_are_added_and_a_flat_reading_already_there_is_kept() {
        let domains: Vec<Value> = (0..12).map(|n| json!({ "id": format!("gpu-{n}"), "watts": 1.0 })).collect();
        let flat = with_flat_readings(json!({ "energy": { "total_kwh": 1.0, "domains": domains } }));
        assert_eq!(flat["energy_watts"], 8.0);
        let kept = with_flat_readings(json!({ "energy_kwh": 5.0, "energy": { "total_kwh": 1.0 } }));
        assert_eq!(kept["energy_kwh"], 5.0);
    }

    #[test]
    fn text_inside_the_new_fields_is_made_safe() {
        let mut snapshot = hybrid_snapshot();
        snapshot["guests"][0]["name"] = json!(format!("media\u{1b}[2J{}", "x".repeat(400)));
        snapshot["energy"]["domains"][0]["label"] = json!("CPU\u{7} package");
        let out = cleaned(with_flat_readings(snapshot));
        let name = out["guests"][0]["name"].as_str().unwrap();
        assert!(name.starts_with("media[2J") && name.chars().count() == 160, "{name}");
        assert_eq!(out["energy"]["domains"][0]["label"], "CPU package");
        assert_eq!(out["guests"][0]["cpu_affinity"], json!([0, 1]), "numbers are not text");
    }

    #[test]
    fn a_pairing_request_carries_only_the_name() {
        assert_eq!(pairing_request("Desk PC"), json!({ "name": "Desk PC" }));
    }
}
