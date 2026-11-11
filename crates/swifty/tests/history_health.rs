//! The chart and health APIs, against mock Proxmox and collector servers on
//! loopback. The rule that matters most here: a reading that is not known comes
//! back as `not_reporting`, and never as a zero.

mod common;

use std::time::Duration;

use axum::http::StatusCode;
use common::devices::*;
use common::*;
use serde_json::{json, Value};
use swifty_hub::history;
use swifty_plugin_mock::Behavior;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;

const MINUTE: i64 = 60_000;

async fn live() -> (Rig, Harness, SoftPasskey, String) {
    rig_harness(true, |_| {}).await
}

// ----- Proxmox history

#[tokio::test]
async fn the_three_ranges_ask_proxmox_for_the_right_timeframe() {
    let (rig, harness, _auth, cookie) = live().await;
    for (range, timeframe) in [("1h", "hour"), ("24h", "day"), ("7d", "week")] {
        let chart = harness.get(&format!("/api/v1/history/host?range={range}"), &cookie).await;
        chart.assert_ok();
        assert_eq!(chart.body["range"], json!(range));
        assert_eq!(chart.body["source"], json!("proxmox_rrd"));
        assert_eq!(chart.body["simulated"], json!(false));
        assert_eq!(chart.body["points"].as_array().unwrap().len(), 2);
        let last = rig.proxmox.node.lock().unwrap().rrd_requests.last().unwrap().clone();
        assert_eq!(last, format!("node?timeframe={timeframe}&cf=AVERAGE"));
    }
}

#[tokio::test]
async fn a_vm_has_its_own_history_and_only_numbers_come_back() {
    let (rig, harness, _auth, cookie) = live().await;
    let chart = harness.get("/api/v1/history/vm101?range=24h", &cookie).await;
    chart.assert_ok();
    assert_eq!(chart.body["points"], json!([{ "time": 2000, "cpu": 0.75, "netin": 12.5 }]));
    assert_eq!(rig.proxmox.node.lock().unwrap().rrd_requests, ["101?timeframe=day&cf=AVERAGE"]);

    let host = harness.get("/api/v1/history/host", &cookie).await;
    assert!(!host.text.contains("not-a-number"), "{}", host.text);
    assert_eq!(host.body["range"], json!("1h"), "an hour is the default");
}

#[tokio::test]
async fn only_known_targets_and_ranges_are_asked_about() {
    let (rig, harness, _auth, cookie) = live().await;
    harness
        .get("/api/v1/history/vm100?range=2y", &cookie)
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_range");
    harness.get("/api/v1/history/vm999", &cookie).await.assert_refused(StatusCode::NOT_FOUND, "unknown_device");
    harness.get("/api/v1/history/bulb1", &cookie).await.assert_refused(StatusCode::NOT_FOUND, "unknown_device");
    assert_eq!(rig.traffic(), 0);
}

#[tokio::test]
async fn a_pending_machine_has_no_history() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.proxmox.node.lock().unwrap().listed.push((150, "new".to_string(), false));
    harness.post("/api/v1/machines/scan", &cookie, json!({})).await.assert_ok();
    rig.proxmox.http.seen.lock().unwrap().clear();
    harness.get("/api/v1/history/vm150", &cookie).await.assert_refused(StatusCode::FORBIDDEN, "machine_not_approved");
    assert!(rig.proxmox.http.requests().is_empty());
}

#[tokio::test]
async fn in_dry_run_history_asks_nothing() {
    let (rig, harness, _auth, cookie) = rig_harness(false, |_| {}).await;
    for path in ["/api/v1/history/host", "/api/v1/history/vm100?range=7d", "/api/v1/history/collector"] {
        let chart = harness.get(path, &cookie).await;
        chart.assert_ok();
        assert_eq!(chart.body["simulated"], json!(true), "{path}");
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.traffic(), 0);
}

#[tokio::test]
async fn a_proxmox_refusal_is_reported_and_not_an_empty_chart() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.proxmox.node.lock().unwrap().down = true;
    harness.get("/api/v1/history/host", &cookie).await.assert_refused(StatusCode::BAD_GATEWAY, "history_unavailable");
}

// ----- stored samples

fn cpu_snapshot(temp: f64, watts: f64) -> Value {
    json!({
        "pkg_temp_c": temp,
        "pkg_watts": watts,
        "cores": [{ "usage": 10.0, "temp_c": 50.0 }, { "usage": 30.0, "temp_c": 54.0 }],
    })
}

#[tokio::test]
async fn collector_readings_are_stored_for_charts() {
    let (rig, harness, _auth, cookie) = live().await;
    for minute in 0..3 {
        *rig.snapshot.lock().unwrap() = cpu_snapshot(60.0 + f64::from(minute), 100.0);
        history::sample_collector(&harness.app).await.unwrap();
        harness.advance(MINUTE);
    }
    let chart = harness.get("/api/v1/history/collector?range=1h", &cookie).await;
    chart.assert_ok();
    assert_eq!(chart.body["source"], json!("stored_samples"));
    let series = chart.body["series"].as_object().unwrap();
    assert_eq!(series["pkg_temp_c"].as_array().unwrap().len(), 3, "{}", chart.text);
    assert_eq!(series["pkg_temp_c"][0][1], json!(60.0));
    assert_eq!(series["pkg_watts"][2][1], json!(100.0));
    assert_eq!(series["core.1.temp_c"][0][1], json!(54.0));
    assert_eq!(series["core.0.usage"][0][1], json!(10.0));

    let one = harness.get("/api/v1/history/collector?range=1h&metric=pkg_watts", &cookie).await;
    assert_eq!(one.body["series"].as_object().unwrap().len(), 1);
    harness
        .get("/api/v1/history/collector?metric=a;drop", &cookie)
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_metric");
}

#[tokio::test]
async fn longer_ranges_average_into_buckets() {
    let (rig, harness, _auth, cookie) = live().await;
    let bucket_start = harness.now() - harness.now() % (5 * MINUTE);
    harness.advance(bucket_start + 5 * MINUTE - harness.now());
    for watts in [100.0, 200.0, 300.0] {
        *rig.snapshot.lock().unwrap() = cpu_snapshot(60.0, watts);
        history::sample_collector(&harness.app).await.unwrap();
        harness.advance(MINUTE);
    }
    let chart = harness.get("/api/v1/history/collector?range=24h&metric=pkg_watts", &cookie).await;
    let points = chart.body["series"]["pkg_watts"].as_array().unwrap();
    assert_eq!(points.len(), 1, "{}", chart.text);
    assert_eq!(points[0][1], json!(200.0));
}

#[tokio::test]
async fn a_range_only_goes_back_as_far_as_it_says() {
    let (rig, harness, mut auth, _cookie) = live().await;
    *rig.snapshot.lock().unwrap() = cpu_snapshot(60.0, 100.0);
    history::sample_collector(&harness.app).await.unwrap();
    harness.advance(2 * HOUR_MS);
    let cookie = harness.login(&mut auth).await;
    let hour = harness.get("/api/v1/history/collector?range=1h", &cookie).await;
    assert_eq!(hour.body["series"], json!({}));
    let day = harness.get("/api/v1/history/collector?range=24h", &cookie).await;
    assert!(day.body["series"].get("pkg_watts").is_some());
}

#[tokio::test]
async fn a_missing_reading_stays_a_gap() {
    let (rig, harness, _auth, cookie) = live().await;
    *rig.snapshot.lock().unwrap() = json!({ "pkg_temp_c": 60.0 });
    history::sample_collector(&harness.app).await.unwrap();
    harness.advance(MINUTE);
    *rig.snapshot.lock().unwrap() = json!({ "pkg_watts": 100.0 });
    history::sample_collector(&harness.app).await.unwrap();
    let chart = harness.get("/api/v1/history/collector?range=1h", &cookie).await;
    assert_eq!(chart.body["series"]["pkg_temp_c"].as_array().unwrap().len(), 1);
    assert_eq!(chart.body["series"]["pkg_watts"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn old_samples_are_removed() {
    let (rig, harness, _auth, _cookie) = live().await;
    *rig.snapshot.lock().unwrap() = cpu_snapshot(60.0, 100.0);
    history::sample_collector(&harness.app).await.unwrap();
    let count = |h: &Harness| -> i64 {
        h.app.store.with(|c| c.query_row("SELECT COUNT(*) FROM collector_samples", [], |r| r.get(0))).unwrap()
    };
    let first = count(&harness);
    assert!(first > 0);
    harness.advance(history::RETENTION_MS + MINUTE);
    history::sample_collector(&harness.app).await.unwrap();
    assert_eq!(count(&harness), first, "the old ones are gone and the new ones are in");
}

#[tokio::test]
async fn in_dry_run_nothing_is_sampled() {
    let (rig, harness, _auth, _cookie) = rig_harness(false, |_| {}).await;
    assert_eq!(history::sample_collector(&harness.app).await.unwrap(), 0);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.traffic(), 0);
}

// ----- health

fn healthy_snapshot() -> Value {
    json!({
        "faults": { "segfault_count_24h": 0, "oom_count_24h": 1, "mce_count_24h": 0 },
        "disks": [
            { "name": "nvme0n1", "health": "PASSED", "media_errors": 0 },
            { "name": "sda", "health": "FAILING", "media_errors": 7 },
        ],
        "storage": {
            "read_errors": 0, "write_errors": 0, "checksum_errors": 2, "scrub_errors": 0,
            "pools": [{ "name": "fast", "read_errors": 0, "write_errors": 0, "checksum_errors": 2, "scrub_errors": 0 }],
        },
        "sources": { "faults": { "status": "ok" }, "disks": { "status": "ok" }, "storage": { "status": "ok" } },
    })
}

#[tokio::test]
async fn the_health_page_reads_the_collector() {
    let (rig, harness, _auth, cookie) = live().await;
    *rig.snapshot.lock().unwrap() = healthy_snapshot();
    let report = harness.get("/api/v1/health", &cookie).await;
    report.assert_ok();
    assert_eq!(report.body["collector"]["status"], json!("ok"));
    assert_eq!(report.body["faults"]["counters"]["segfault_count_24h"], json!({ "status": "ok", "value": 0 }));
    assert_eq!(report.body["faults"]["counters"]["oom_count_24h"], json!({ "status": "ok", "value": 1 }));
    assert_eq!(report.body["disks"]["disks"][1]["health"]["value"], json!("FAILING"));
    assert_eq!(report.body["disks"]["disks"][1]["media_errors"]["value"], json!(7));
    assert_eq!(report.body["storage"]["totals"]["checksum_errors"]["value"], json!(2));
    assert_eq!(report.body["storage"]["pools"][0]["scrub_errors"]["value"], json!(0));
    assert_eq!(report.body["sources"]["faults"]["status"], json!("ok"));
}

#[tokio::test]
async fn a_source_that_stopped_is_not_reporting_and_shows_no_zero() {
    let (rig, harness, _auth, cookie) = live().await;
    let mut snapshot = healthy_snapshot();
    snapshot["sources"]["storage"] = json!({ "status": "stale" });
    snapshot["sources"]["faults"] = json!({ "ok": false });
    *rig.snapshot.lock().unwrap() = snapshot;
    let report = harness.get("/api/v1/health", &cookie).await;
    assert_eq!(report.body["storage"]["status"], json!("not_reporting"));
    assert_eq!(report.body["storage"]["totals"]["read_errors"], json!({ "status": "not_reporting" }));
    assert_eq!(report.body["storage"]["pools"], json!([]));
    assert_eq!(report.body["faults"]["counters"]["segfault_count_24h"], json!({ "status": "not_reporting" }));
    assert_eq!(report.body["sources"]["storage"]["status"], json!("not_reporting"));
    assert_eq!(report.body["disks"]["status"], json!("ok"), "only the source that stopped is affected");
}

#[tokio::test]
async fn a_collector_that_cannot_be_read_reports_nothing_at_all() {
    let (rig, harness, _auth, cookie) = live().await;
    *rig.snapshot.lock().unwrap() = healthy_snapshot();
    let rig_gone = rig;
    drop(rig_gone);
    let report = harness.get("/api/v1/health", &cookie).await;
    report.assert_ok();
    assert_eq!(report.body["collector"]["status"], json!("not_reporting"), "{}", report.text);
    for section in ["faults", "disks", "storage", "bluetooth_drops"] {
        assert_eq!(report.body[section]["status"], json!("not_reporting"), "{section}");
    }
    assert!(!report.body["storage"].to_string().contains("\"value\""));
}

#[tokio::test]
async fn bluetooth_drops_wait_for_an_agent_to_report_them() {
    let (rig, harness, _auth, cookie) = live().await;
    *rig.snapshot.lock().unwrap() = healthy_snapshot();
    let report = harness.get("/api/v1/health", &cookie).await;
    assert_eq!(report.body["bluetooth_drops"]["status"], json!("not_reporting"));
    assert_eq!(report.body["bluetooth_drops"]["controllers"], json!({ "status": "not_reporting" }));
    assert_eq!(report.body["bluetooth_drops"]["headsets"], json!({ "status": "not_reporting" }));

    let mut snapshot = healthy_snapshot();
    snapshot["sources"]["bluetooth"] = json!({ "status": "ok" });
    snapshot["bluetooth"] = json!({ "controllers": { "drops_24h": 3 }, "headsets": { "drops_24h": 0 } });
    *rig.snapshot.lock().unwrap() = snapshot;
    let report = harness.get("/api/v1/health", &cookie).await;
    assert_eq!(report.body["bluetooth_drops"]["controllers"], json!({ "status": "ok", "value": 3 }));
    assert_eq!(report.body["bluetooth_drops"]["headsets"], json!({ "status": "ok", "value": 0 }));
}

#[tokio::test]
async fn the_canary_is_read_from_its_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("canary.json");
    let path = file.clone();
    let (_rig, harness, _auth, cookie) = rig_harness(true, move |c| c.health.cpu_canary_file = Some(path)).await;
    assert_eq!(harness.get("/api/v1/health", &cookie).await.body["cpu_canary"], json!({ "status": "not_reporting" }));
    std::fs::write(&file, r#"{"result":"pass","ran_at":1900000000,"cores_tested":32}"#).unwrap();
    let report = harness.get("/api/v1/health", &cookie).await;
    assert_eq!(
        report.body["cpu_canary"],
        json!({ "status": "ok", "result": "pass", "ran_at": 1900000000, "cores_tested": 32 })
    );
}

#[tokio::test]
async fn with_no_canary_configured_it_is_not_reporting() {
    let (_rig, harness, _auth, cookie) = live().await;
    assert_eq!(harness.get("/api/v1/health", &cookie).await.body["cpu_canary"], json!({ "status": "not_reporting" }));
}

#[tokio::test]
async fn in_dry_run_health_asks_nothing_and_claims_nothing() {
    let (rig, harness, _auth, cookie) = rig_harness(false, |_| {}).await;
    let report = harness.get("/api/v1/health", &cookie).await;
    report.assert_ok();
    assert_eq!(report.body["simulated"], json!(true));
    assert_eq!(report.body["collector"]["simulated"], json!(true));
    assert_eq!(report.body["faults"]["status"], json!("not_reporting"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.traffic(), 0);
}

#[tokio::test]
async fn polling_health_and_charts_does_not_keep_a_session_alive() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    for _ in 0..3 {
        harness.advance(8 * MINUTE);
        harness.get("/api/v1/health", &cookie).await.assert_ok();
        harness.get("/api/v1/history/host", &cookie).await.assert_ok();
    }
    harness.advance(8 * MINUTE);
    let idle = harness.get("/api/v1/catalog", &cookie).await;
    assert_eq!(idle.status, StatusCode::UNAUTHORIZED, "{}", idle.text);
}

#[tokio::test]
async fn silence_from_the_collector_is_not_a_reading() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    harness.devices.set_collector(healthy_snapshot());
    harness.devices.set_behavior("collector", Behavior::Silent);
    let report = harness.get("/api/v1/health", &cookie).await;
    assert_eq!(report.body["collector"]["status"], json!("not_reporting"));
    assert_eq!(report.body["faults"]["status"], json!("not_reporting"));
}
