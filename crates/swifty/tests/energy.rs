//! The electricity tariff and the energy cost ledger, through the API:
//! `GET /api/v1/energy` and `PUT /api/v1/energy/tariff`, for the owner only.
//!
//! A telemetry node counts the energy; the hub prices it. Each time the hub reads the node
//! (the sampler), what the counter added is charged at the tariff in force (by day at the day
//! price, at night at the night price), a change never reaches back, the energy counted before
//! the first tariff is charged once at its day price, and a counter that restarted is followed
//! by its new total.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{json, Value};
use swifty_hub::history;
use swifty_plugin_api::State;

const ENERGY: &str = "/api/v1/energy";
const TARIFF: &str = "/api/v1/energy/tariff";
/// A counter that began on a day long past the manual clock's start.
const SINCE: i64 = 1_794_380_000;

/// What a node says about its counter.
fn counter(since: i64, kwh: f64) -> Value {
    json!({
        "energy": {
            "since": since,
            "updated_at": since + 60,
            "total_kwh": kwh,
            "domains": [{ "id": "cpu-package", "label": "CPU package", "kwh": kwh, "source": "rapl", "watts": 40.0 }],
            "scope": "components",
        }
    })
}

async fn put_tariff(harness: &Harness, cookie: &str, body: Value) -> Resp {
    harness.send(Method::PUT, TARIFF, Some(cookie), Some(ORIGIN), Some(body)).await
}

/// What the owner reads for one target.
async fn target(harness: &Harness, cookie: &str, target: &str) -> Value {
    let energy = harness.get(ENERGY, cookie).await;
    energy.assert_ok();
    energy.body["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["target"] == json!(target))
        .unwrap_or_else(|| panic!("no {target} in {}", energy.text))
        .clone()
}

#[track_caller]
fn close(value: &Value, want: f64) {
    let got = value.as_f64().unwrap_or_else(|| panic!("not a number: {value}"));
    assert!((got - want).abs() < 1e-9, "{got} is not {want}");
}

/// The audit rows of one action: `(target, user_ref, params, result)`, oldest first.
fn audit_rows(harness: &Harness, action: &str) -> Vec<(String, String, Value, String)> {
    harness
        .app
        .store
        .with(|conn| {
            let mut statement =
                conn.prepare("SELECT target, user_ref, params, result FROM audit_log WHERE action = ?1 ORDER BY seq")?;
            let rows = statement.query_map([action], |row| {
                let params: String = row.get(2)?;
                Ok((row.get(0)?, row.get(1)?, serde_json::from_str(&params).unwrap_or(Value::Null), row.get(3)?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap()
}

fn owner_id(harness: &Harness) -> String {
    harness
        .app
        .store
        .with(|conn| conn.query_row("SELECT id FROM users WHERE role = 'owner'", [], |row| row.get::<_, String>(0)))
        .unwrap()
}

async fn sampled(harness: &Harness, snapshot: Value) {
    harness.devices.set_collector(snapshot);
    history::sample_collector(&harness.app).await.unwrap();
    harness.advance(MINUTE_MS);
}

#[tokio::test]
async fn without_a_price_the_kwh_show_and_the_cost_does_not() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let before = harness.get(ENERGY, &cookie).await;
    before.assert_ok();
    assert_eq!(before.body, json!({ "tariff": null, "targets": [] }), "nothing has been read yet");

    sampled(&harness, counter(SINCE, 10.0)).await;
    sampled(&harness, counter(SINCE, 10.5)).await;
    let seen = target(&harness, &cookie, "collector").await;
    assert_eq!(seen["kwh"], json!(10.5));
    assert_eq!(seen["since"], json!(SINCE * 1000), "milliseconds, like every time in the API");
    for unset in ["cost", "cost_day", "cost_night", "kwh_day", "kwh_night", "priced_since", "backfilled_kwh"] {
        assert!(seen[unset].is_null(), "{unset} with no tariff: {seen}");
    }
    assert!(seen["updated_at"].as_i64().unwrap() > START_MS);
    assert!(harness.get(ENERGY, &cookie).await.body["tariff"].is_null());
}

#[tokio::test]
async fn the_ledger_charges_what_the_counter_adds_at_the_price_in_force() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    // 10 kWh are counted before there is a price.
    sampled(&harness, counter(SINCE, 10.0)).await;

    // The first price charges them once, and the ledger says it did.
    let set_at = harness.now();
    let first = put_tariff(&harness, &cookie, json!({ "day_price_per_kwh": 0.30, "currency": "EUR" })).await;
    first.assert_ok();
    assert_eq!(first.body["tariff"]["day_price_per_kwh"], json!(0.3));
    assert_eq!(first.body["tariff"]["currency"], json!("EUR"));
    assert_eq!(first.body["tariff"]["set_at"], json!(set_at));
    assert_eq!(first.body["tariff"]["band_now"], json!("day"), "one price is always the day price");
    for none in ["night_price_per_kwh", "night", "summer_night", "time_zone"] {
        assert!(first.body["tariff"][none].is_null(), "{none}: {}", first.text);
    }
    let entry = &first.body["targets"][0];
    close(&entry["cost"], 3.0);
    assert_eq!((entry["backfilled_kwh"].clone(), entry["priced_since"].clone()), (json!(10.0), json!(set_at)));
    assert_eq!(entry["kwh"], json!(10.0));

    // From then on only what is added is charged.
    harness.advance(MINUTE_MS);
    sampled(&harness, counter(SINCE, 12.0)).await;
    close(&target(&harness, &cookie, "collector").await["cost"], 3.6);

    // The price changes (the code in lower case is stored in capitals): the past stays as it was.
    let changed = put_tariff(&harness, &cookie, json!({ "day_price_per_kwh": 0.50, "currency": "eur" })).await;
    changed.assert_ok();
    assert_eq!(changed.body["tariff"]["currency"], json!("EUR"));
    close(&changed.body["targets"][0]["cost"], 3.6);
    assert_eq!(changed.body["targets"][0]["backfilled_kwh"], json!(10.0), "charged once, not again");
    assert_eq!(changed.body["targets"][0]["priced_since"], json!(set_at), "charging began with the first price");
    sampled(&harness, counter(SINCE, 14.0)).await;
    close(&target(&harness, &cookie, "collector").await["cost"], 3.6 + 2.0 * 0.5);

    // The node's counter restarts with a new `since`: its new total is what was added.
    sampled(&harness, counter(SINCE + 5_000, 1.0)).await;
    let restarted = target(&harness, &cookie, "collector").await;
    close(&restarted["cost"], 4.6 + 0.5);
    assert_eq!((restarted["kwh"].clone(), restarted["since"].clone()), (json!(1.0), json!((SINCE + 5_000) * 1000)));

    // And a counter that simply went down is a restart too.
    sampled(&harness, counter(SINCE + 5_000, 0.4)).await;
    close(&target(&harness, &cookie, "collector").await["cost"], 5.1 + 0.2);
    sampled(&harness, counter(SINCE + 5_000, 0.6)).await;
    close(&target(&harness, &cookie, "collector").await["cost"], 5.3 + 0.1);

    // Two audit rows, one for each price, with the amount and the currency.
    let rows = audit_rows(&harness, "energy.tariff");
    assert_eq!(rows.len(), 2);
    let owner = owner_id(&harness);
    assert!(rows.iter().all(|(target, user, _, result)| target == "-" && *user == owner && result == "done"));
    assert_eq!(rows[0].2["day_price_per_kwh"], json!(0.3));
    assert_eq!(rows[0].2["before"], Value::Null);
    assert_eq!(rows[0].2["charged_once"], json!([{ "target": "collector", "kwh": 10.0 }]));
    assert_eq!(
        rows[1].2["before"],
        json!({ "currency": "EUR", "day_price_per_kwh": 0.3, "night_price_per_kwh": null, "night": null, "summer_night": null, "time_zone": null })
    );
    assert_eq!(rows[1].2["charged_once"], json!([]));
}

#[tokio::test]
async fn a_new_currency_charges_what_was_counted_once_more_in_it() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    sampled(&harness, counter(SINCE, 10.0)).await;
    put_tariff(&harness, &cookie, json!({ "day_price_per_kwh": 0.30, "currency": "EUR" })).await.assert_ok();
    let now = put_tariff(&harness, &cookie, json!({ "day_price_per_kwh": 1.0, "currency": "GBP" })).await;
    now.assert_ok();
    close(&now.body["targets"][0]["cost"], 10.0);
    assert_eq!(now.body["tariff"]["currency"], json!("GBP"));
    assert_eq!(
        audit_rows(&harness, "energy.tariff")[1].2["charged_once"],
        json!([{ "target": "collector", "kwh": 10.0 }])
    );
}

#[tokio::test]
async fn every_telemetry_node_has_its_own_ledger_line() {
    let second = |house: &mut swifty_plugin_mock::House| {
        let row = json!({ "id": "node2", "class": "telemetry", "name": "Second node", "target": "node2" });
        house.instances.push(swifty_plugin_mock::HouseInstance {
            id: "lab".to_string(),
            plugin: "mock".to_string(),
            config: json!({ "devices": [row] }),
            config_version: 1,
            enabled: true,
        });
    };
    let harness = Harness::with_house(Vec::new(), second, |_| {});
    let (_auth, cookie) = harness.sign_in().await;
    harness.devices.add_state("node2", State::Telemetry { data: counter(SINCE, 100.0) });
    sampled(&harness, counter(SINCE, 10.0)).await;
    put_tariff(&harness, &cookie, json!({ "day_price_per_kwh": 0.2, "currency": "EUR" })).await.assert_ok();
    harness.devices.add_state("node2", State::Telemetry { data: counter(SINCE, 101.0) });
    sampled(&harness, counter(SINCE, 11.0)).await;

    let energy = harness.get(ENERGY, &cookie).await;
    let targets: Vec<&str> =
        energy.body["targets"].as_array().unwrap().iter().map(|t| t["target"].as_str().unwrap()).collect();
    assert_eq!(targets, ["collector", "node2"], "in target order");
    close(&target(&harness, &cookie, "collector").await["cost"], 2.2);
    close(&target(&harness, &cookie, "node2").await["cost"], 20.2);
}

#[tokio::test]
async fn a_node_with_no_usable_counter_adds_nothing_to_the_ledger() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    for snapshot in [
        json!({ "pkg_watts": 118 }),
        json!({ "energy": null }),
        json!({ "energy": { "since": SINCE } }),
        json!({ "energy": { "since": SINCE, "total_kwh": "n/a" } }),
        json!({ "energy": { "since": SINCE, "total_kwh": -3.0 } }),
    ] {
        sampled(&harness, snapshot).await;
    }
    assert_eq!(harness.get(ENERGY, &cookie).await.body["targets"], json!([]));
}

#[tokio::test]
async fn only_the_owner_reads_or_sets_the_tariff() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let (_family, family_cookie, family_id) = harness.add_family(&cookie, &mut owner, "Bob").await;
    put_tariff(&harness, &cookie, json!({ "day_price_per_kwh": 0.25, "currency": "EUR" })).await.assert_ok();

    harness.get(ENERGY, &family_cookie).await.assert_refused(StatusCode::FORBIDDEN, "owner_only");
    put_tariff(&harness, &family_cookie, json!({ "day_price_per_kwh": 0.01, "currency": "USD" }))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "owner_only");
    // Before anything about the body is looked at.
    put_tariff(&harness, &family_cookie, json!({ "nonsense": true }))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "owner_only");
    harness.send(Method::GET, ENERGY, None, None, None).await.assert_refused(StatusCode::UNAUTHORIZED, "not_signed_in");
    harness
        .send(Method::PUT, TARIFF, None, Some(ORIGIN), Some(json!({ "day_price_per_kwh": 1.0, "currency": "EUR" })))
        .await
        .assert_refused(StatusCode::UNAUTHORIZED, "not_signed_in");

    // The refusals are recorded, nothing changed, and no price row was written for them.
    let refusals = audit_rows(&harness, "access.owner_only");
    assert_eq!(refusals.len(), 3);
    assert!(refusals.iter().all(|(route, user, _, _)| route.starts_with("/api/v1/energy") && *user == family_id));
    let price = harness.get(ENERGY, &cookie).await;
    assert_eq!(
        (price.body["tariff"]["day_price_per_kwh"].clone(), price.body["tariff"]["currency"].clone()),
        (json!(0.25), json!("EUR"))
    );
    assert_eq!(audit_rows(&harness, "energy.tariff").len(), 1, "only the owner's own tariff");
}

#[tokio::test]
async fn a_tariff_that_is_not_one_is_refused_and_not_recorded() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    for (body, code) in [
        (json!({ "day_price_per_kwh": 0, "currency": "EUR" }), "invalid_tariff"),
        (json!({ "day_price_per_kwh": -0.1, "currency": "EUR" }), "invalid_tariff"),
        (json!({ "day_price_per_kwh": 2_000_000.0, "currency": "EUR" }), "invalid_tariff"),
        (json!({ "day_price_per_kwh": 0.3, "currency": "EURO" }), "invalid_currency"),
        (json!({ "day_price_per_kwh": 0.3, "currency": "E1R" }), "invalid_currency"),
        (json!({ "day_price_per_kwh": 0.3, "currency": "" }), "invalid_currency"),
        (json!({ "day_price_per_kwh": "0.3", "currency": "EUR" }), "invalid_body"),
        (json!({ "day_price_per_kwh": 0.3 }), "invalid_body"),
        (json!({ "day_price_per_kwh": 0.3, "currency": "EUR", "night_price_per_kwh": 0.1 }), "invalid_tariff"),
        (
            json!({ "day_price_per_kwh": 0.3, "currency": "EUR", "night_price_per_kwh": 0.1,
                    "night": { "start": "22:00", "end": "06:00" } }),
            "invalid_tariff",
        ),
        (
            json!({ "day_price_per_kwh": 0.3, "currency": "EUR", "night_price_per_kwh": 0.1,
                    "night": { "start": "22:00", "end": "06:00" }, "time_zone": "Nowhere/Atlantis" }),
            "invalid_tariff",
        ),
        (
            json!({ "day_price_per_kwh": 0.3, "currency": "EUR", "night_price_per_kwh": 0.1,
                    "night": { "start": "25:00", "end": "06:00" }, "time_zone": "UTC" }),
            "invalid_body",
        ),
        (
            json!({ "day_price_per_kwh": 0.3, "currency": "EUR", "night_price_per_kwh": 0,
                    "night": { "start": "22:00", "end": "06:00" }, "time_zone": "UTC" }),
            "invalid_tariff",
        ),
        (json!({ "day_price_per_kwh": 0.3, "currency": "EUR", "note": "x" }), "invalid_body"),
    ] {
        put_tariff(&harness, &cookie, body.clone()).await.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, code);
    }
    assert!(audit_rows(&harness, "energy.tariff").is_empty());
    assert!(harness.get(ENERGY, &cookie).await.body["tariff"].is_null());
    // A request from another origin changes nothing either.
    harness
        .send(
            Method::PUT,
            TARIFF,
            Some(&cookie),
            Some(EVIL_ORIGIN),
            Some(json!({ "day_price_per_kwh": 1.0, "currency": "EUR" })),
        )
        .await
        .assert_refused(StatusCode::FORBIDDEN, "bad_origin");
    assert!(harness.get(ENERGY, &cookie).await.body["tariff"].is_null());
}

#[tokio::test]
async fn through_the_glimdock_plugin_the_counter_reaches_the_ledger_and_the_charts() {
    let (rig, harness, _auth, cookie) = rig_harness(true, |_| {}).await;
    *rig.snapshot.lock().unwrap() = counter(SINCE, 20.0);
    history::sample_collector(&harness.app).await.unwrap();
    put_tariff(&harness, &cookie, json!({ "day_price_per_kwh": 0.2, "currency": "EUR" })).await.assert_ok();
    harness.advance(MINUTE_MS);
    *rig.snapshot.lock().unwrap() = counter(SINCE, 21.0);
    history::sample_collector(&harness.app).await.unwrap();

    let seen = target(&harness, &cookie, "collector").await;
    assert_eq!(seen["kwh"], json!(21.0));
    close(&seen["cost"], 4.2);

    // The flat readings the plugin adds are charted.
    let chart = harness.get("/api/v1/history/collector?range=1h", &cookie).await;
    chart.assert_ok();
    assert_eq!(chart.body["series"]["energy_kwh"].as_array().unwrap().len(), 2, "{}", chart.text);
    assert_eq!(chart.body["series"]["energy_watts"][0][1], json!(40.0));
}

#[tokio::test]
async fn in_dry_run_nothing_is_read_so_nothing_is_counted_but_the_tariff_can_be_set() {
    let (rig, harness, _auth, cookie) = rig_harness(false, |_| {}).await;
    *rig.snapshot.lock().unwrap() = counter(SINCE, 20.0);
    assert_eq!(history::sample_collector(&harness.app).await.unwrap(), 0);
    put_tariff(&harness, &cookie, json!({ "day_price_per_kwh": 0.2, "currency": "EUR" })).await.assert_ok();
    let energy = harness.get(ENERGY, &cookie).await;
    assert_eq!(energy.body["targets"], json!([]));
    assert_eq!(energy.body["tariff"]["day_price_per_kwh"], json!(0.2));
    assert_eq!(rig.traffic(), 0);
}

/// Night from 17:00 to 19:00 UTC: the test clock starts at 17:46 UTC, so the reads below are at night.
fn two_rate() -> Value {
    json!({
        "currency": "EUR",
        "day_price_per_kwh": 0.30,
        "night_price_per_kwh": 0.10,
        "night": { "start": "17:00", "end": "19:00" },
        "time_zone": "UTC",
    })
}

#[tokio::test]
async fn a_two_rate_tariff_charges_night_energy_at_the_night_price() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    sampled(&harness, counter(SINCE, 10.0)).await;
    let set = put_tariff(&harness, &cookie, two_rate()).await;
    set.assert_ok();
    let tariff = &set.body["tariff"];
    assert_eq!(tariff["night"], json!({ "start": "17:00", "end": "19:00" }));
    assert_eq!((tariff["time_zone"].clone(), tariff["band_now"].clone()), (json!("UTC"), json!("night")));
    assert!(tariff["summer_night"].is_null());
    // The 10 kWh counted before the tariff are charged once at its day price.
    let first = &set.body["targets"][0];
    close(&first["cost"], 3.0);
    close(&first["kwh_day"], 10.0);
    close(&first["kwh_night"], 0.0);

    // From then on the reads are at night: 2 kWh at 0.10.
    sampled(&harness, counter(SINCE, 11.0)).await;
    sampled(&harness, counter(SINCE, 12.0)).await;
    let seen = target(&harness, &cookie, "collector").await;
    close(&seen["kwh_night"], 2.0);
    close(&seen["cost_night"], 0.2);
    close(&seen["cost_day"], 3.0);
    close(&seen["cost"], 3.2);

    let rows = audit_rows(&harness, "energy.tariff");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].2["night_price_per_kwh"], json!(0.1));
    assert_eq!(rows[0].2["night"], json!({ "start": "17:00", "end": "19:00" }));
    assert_eq!(rows[0].2["time_zone"], json!("UTC"));
}

#[tokio::test]
async fn summer_night_hours_are_kept_and_a_one_rate_tariff_keeps_no_clock() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let mut body = two_rate();
    body["summer_night"] = json!({ "start": "18:00", "end": "20:00" });
    body["time_zone"] = json!("Europe/Berlin");
    let set = put_tariff(&harness, &cookie, body).await;
    set.assert_ok();
    assert_eq!(set.body["tariff"]["summer_night"], json!({ "start": "18:00", "end": "20:00" }));
    assert_eq!(set.body["tariff"]["time_zone"], json!("Europe/Berlin"));
    // Back to one price: the hours and the zone are dropped, even if a zone is sent.
    let one =
        put_tariff(&harness, &cookie, json!({ "currency": "EUR", "day_price_per_kwh": 0.25, "time_zone": "UTC" }))
            .await;
    one.assert_ok();
    for none in ["night_price_per_kwh", "night", "summer_night", "time_zone"] {
        assert!(one.body["tariff"][none].is_null(), "{none}: {}", one.text);
    }
    assert_eq!(one.body["tariff"]["band_now"], json!("day"));
}
