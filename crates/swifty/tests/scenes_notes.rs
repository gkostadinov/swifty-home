//! House scenes and the owner's notes on devices, through the API:
//! `GET /api/v1/scenes` (anyone signed in), `PUT /api/v1/scenes/{id}` (owner),
//! `GET` and `PUT /api/v1/devices/{target}/notes` (owner).
//!
//! Every write is audited like the other arrangements, and a change is published
//! as a `devices` event so open pages fetch again. What a scene or a note says is
//! what the next run of the scene does and what its confirmation shows.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{json, Value};
use swifty_hub::events::Event;
use swifty_hub::{notes, scenes};
use swifty_plugin_api::State;

const SCENES: &str = "/api/v1/scenes";

async fn put(harness: &Harness, path: &str, cookie: &str, body: Value) -> Resp {
    harness.send(Method::PUT, path, Some(cookie), Some(ORIGIN), Some(body)).await
}

fn notes_path(target: &str) -> String {
    format!("/api/v1/devices/{target}/notes")
}

/// How many `devices` events the hub published after `seq`.
fn devices_events_since(harness: &Harness, seq: u64) -> usize {
    harness.app.events.since(seq).unwrap().iter().filter(|published| published.event == Event::Devices).count()
}

/// `(target, user_ref, params, result)` of every audit row of this action, oldest first.
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

fn evening_body() -> Value {
    json!({
        "name": "Late evening",
        "percent": 30,
        "hue": 20,
        "overrides": [{ "target": "bulb2", "percent": 0, "hue": 0 }],
    })
}

#[tokio::test]
async fn everyone_signed_in_reads_the_house_scenes_in_order() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let (_family, family_cookie, _) = harness.add_family(&cookie, &mut owner, "Bob").await;

    let listed = harness.get(SCENES, &cookie).await;
    listed.assert_ok();
    let scenes = listed.body["scenes"].as_array().unwrap();
    let ids: Vec<&str> = scenes.iter().map(|scene| scene["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["evening", "focus", "all_off", "movie"]);
    let evening = &scenes[0];
    assert_eq!(
        (evening["name"].as_str(), evening["percent"].as_u64(), evening["hue"].as_u64()),
        (Some("Evening"), Some(40), Some(34))
    );
    assert_eq!(evening["overrides"][0], json!({ "target": "bulb2", "percent": 35, "hue": 30 }));
    assert_eq!(scenes[3]["extras"], json!({ "tv_input": "HDMI 1", "volume_cap": 30 }));

    // A family member reads the same scenes: every light is theirs to see.
    let theirs = harness.get(SCENES, &family_cookie).await;
    theirs.assert_ok();
    assert_eq!(theirs.body, listed.body);

    harness.send(Method::GET, SCENES, None, None, None).await.assert_refused(StatusCode::UNAUTHORIZED, "not_signed_in");
}

#[tokio::test]
async fn a_family_member_can_read_scenes_and_cannot_write_them() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let (_family, family_cookie, family_id) = harness.add_family(&cookie, &mut owner, "Bob").await;
    let before = harness.app.store.with(scenes::all).unwrap();
    let seq = harness.app.events.latest();

    harness.get(SCENES, &family_cookie).await.assert_ok();
    put(&harness, "/api/v1/scenes/evening", &family_cookie, evening_body())
        .await
        .assert_refused(StatusCode::FORBIDDEN, "owner_only");
    // A scene that does not exist is refused the same way: the refusal says nothing.
    put(&harness, "/api/v1/scenes/party", &family_cookie, evening_body())
        .await
        .assert_refused(StatusCode::FORBIDDEN, "owner_only");

    assert_eq!(harness.app.store.with(scenes::all).unwrap(), before, "nothing changed");
    assert!(audit_rows(&harness, "scene.save").is_empty());
    let refusals = audit_rows(&harness, "access.owner_only");
    assert_eq!(refusals.len(), 2);
    for (route, user, _, result) in refusals {
        assert_eq!(
            (route.as_str(), user.as_str(), result.as_str()),
            ("/api/v1/scenes/{id}", family_id.as_str(), "refused: owner_only")
        );
    }
    assert_eq!(devices_events_since(&harness, seq), 0);
}

#[tokio::test]
async fn the_owner_changes_a_scene_and_its_next_run_follows() {
    let (harness, _owner, cookie) = Harness::signed_in().await;
    let seq = harness.app.events.latest();

    let saved = put(&harness, "/api/v1/scenes/evening", &cookie, evening_body()).await;
    saved.assert_ok();
    let scene = &saved.body["scene"];
    assert_eq!(scene["id"], "evening");
    assert_eq!(scene["name"], "Late evening");
    assert_eq!(
        (scene["percent"].as_u64(), scene["hue"].as_u64(), scene["position"].as_i64()),
        (Some(30), Some(20), Some(0))
    );
    assert_eq!(scene["overrides"], json!([{ "target": "bulb2", "percent": 0, "hue": 0 }]));
    assert_eq!(scene["extras"], json!({}), "left out, so kept");
    assert_eq!(harness.get(SCENES, &cookie).await.body["scenes"][0], *scene, "the list says the same");

    // Audited like any arrangement, and announced once.
    let rows = audit_rows(&harness, "scene.save");
    assert_eq!(rows.len(), 1);
    let (target, user, params, result) = &rows[0];
    assert_eq!((target.as_str(), user.as_str(), result.as_str()), ("evening", owner_id(&harness).as_str(), "done"));
    assert_eq!(*params, json!({ "name": "Late evening", "percent": 30, "hue": 20, "overrides": 1 }));
    assert_eq!(devices_events_since(&harness, seq), 1);

    // The same scene again is written and audited, but nothing changed to announce.
    let seq = harness.app.events.latest();
    put(&harness, "/api/v1/scenes/evening", &cookie, evening_body()).await.assert_ok();
    assert_eq!(audit_rows(&harness, "scene.save").len(), 2);
    assert_eq!(devices_events_since(&harness, seq), 0);

    // The next run of the scene sets what was saved.
    let level = |target: &str| match harness.devices.state(target) {
        Some(State::Light { on, bri, hue, .. }) => (on, bri, hue),
        other => panic!("{target}: {other:?}"),
    };
    let done = harness.execute(&cookie, "scene.apply", "evening", json!({})).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    for bulb in ["bulb1", "bulb3", "bulb4"] {
        assert_eq!(level(bulb), (true, 30, 20), "{bulb}");
    }
    assert!(!level("bulb2").0, "bulb 2 is off in this scene now");

    // Movie's input and cap, and its place in the list.
    let movie = json!({
        "name": "Movie",
        "percent": 10,
        "hue": 14,
        "overrides": [],
        "position": 0,
        "extras": { "tv_input": "HDMI 2", "volume_cap": 15 },
    });
    put(&harness, "/api/v1/scenes/movie", &cookie, movie).await.assert_ok();
    let listed = harness.get(SCENES, &cookie).await;
    let ids: Vec<&str> = listed.body["scenes"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["evening", "movie", "focus", "all_off"], "the same place sorts by id");
    let prepared = harness.prepare(&cookie, "scene.movie", "movie", json!({})).await;
    assert!(prepared.body["what"].as_str().unwrap().contains("HDMI 2"), "{}", prepared.text);
}

#[tokio::test]
async fn a_wrong_scene_is_refused_and_nothing_changes() {
    let (harness, _owner, cookie) = Harness::signed_in().await;
    let before = harness.app.store.with(scenes::all).unwrap();
    let seq = harness.app.events.latest();
    let with = |change: Value| {
        let mut body = evening_body();
        for (key, value) in change.as_object().unwrap() {
            body[key] = value.clone();
        }
        body
    };

    put(&harness, "/api/v1/scenes/party", &cookie, evening_body())
        .await
        .assert_refused(StatusCode::NOT_FOUND, "unknown_scene");
    put(&harness, "/api/v1/scenes/Evening", &cookie, evening_body())
        .await
        .assert_refused(StatusCode::NOT_FOUND, "unknown_scene");
    for wrong in [
        json!({ "percent": 101 }),
        json!({ "hue": 360 }),
        json!({ "name": "" }),
        json!({ "name": " Evening" }),
        json!({ "position": -1 }),
        json!({ "overrides": [{ "target": "tv", "percent": 10, "hue": 0 }] }),
        json!({ "overrides": [{ "target": "vm100", "percent": 10, "hue": 0 }] }),
        json!({ "overrides": [{ "target": "bulb9", "percent": 10, "hue": 0 }] }),
        json!({ "overrides": [{ "target": "bulb1", "percent": 10, "hue": 0 }, { "target": "bulb1", "percent": 20, "hue": 0 }] }),
        json!({ "extras": { "tv_input": "HDMI 2" } }),
    ] {
        let refused = put(&harness, "/api/v1/scenes/evening", &cookie, with(wrong.clone())).await;
        assert_eq!(
            (refused.status, refused.code()),
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_scene"),
            "{wrong}: {}",
            refused.text
        );
    }
    for unreadable in [
        json!({ "colour": "red" }),
        json!({ "percent": 300 }),
        json!({ "overrides": [{ "target": "bulb1", "percent": 10 }] }),
        json!({ "extras": { "volume": 20 } }),
    ] {
        let refused = put(&harness, "/api/v1/scenes/movie", &cookie, with(unreadable.clone())).await;
        assert_eq!(refused.code(), "invalid_body", "{unreadable}: {}", refused.text);
    }
    let movie = |extras: Value| json!({ "name": "Movie", "percent": 10, "hue": 14, "overrides": [], "extras": extras });
    for wrong in [json!({ "volume_cap": 101 }), json!({ "tv_input": "" }), json!({ "tv_input": "HDMI\n2" })] {
        put(&harness, "/api/v1/scenes/movie", &cookie, movie(wrong))
            .await
            .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_scene");
    }
    // A write from another origin is refused before anything is read.
    harness
        .send(Method::PUT, "/api/v1/scenes/evening", Some(&cookie), Some(EVIL_ORIGIN), Some(evening_body()))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "bad_origin");

    assert_eq!(harness.app.store.with(scenes::all).unwrap(), before);
    assert!(audit_rows(&harness, "scene.save").is_empty());
    assert_eq!(devices_events_since(&harness, seq), 0);
}

#[tokio::test]
async fn a_level_for_a_light_that_is_away_is_kept() {
    let (harness, _owner, cookie) = Harness::signed_in().await;
    // Bulb 4 leaves its instance; Evening still names it.
    let rows: Vec<Value> = (1..=3)
        .map(|n| json!({ "id": format!("bulb{n}"), "class": "light", "name": format!("Bulb {n}"), "target": format!("bulb{n}") }))
        .collect();
    harness.set_instance_config("wiz", json!({ "devices": rows }));
    assert!(harness.app.registry.index().get("bulb4").is_none());
    assert!(harness.app.registry.index().get("bulb3").is_some());
    let stored = harness.app.store.with(|conn| scenes::get(conn, "evening")).unwrap().unwrap();
    let mut body = json!({ "name": "Evening", "percent": 40, "hue": 34, "overrides": stored.overrides });
    put(&harness, "/api/v1/scenes/evening", &cookie, body.clone()).await.assert_ok();

    // A light the scene did not name before must be one the index has.
    body["overrides"] = json!([{ "target": "bulb4", "percent": 1, "hue": 1 }]);
    put(&harness, "/api/v1/scenes/evening", &cookie, body.clone()).await.assert_ok();
    body["overrides"] = json!([{ "target": "bulb4", "percent": 1, "hue": 1 }, { "target": "wiz-a8bb50aabbcc", "percent": 1, "hue": 1 }]);
    put(&harness, "/api/v1/scenes/evening", &cookie, body)
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_scene");
}

#[tokio::test]
async fn the_owner_keeps_notes_and_the_confirmation_shows_them() {
    let (harness, _owner, cookie) = Harness::signed_in().await;
    let seq = harness.app.events.latest();

    let read = harness.get(&notes_path("vm100"), &cookie).await;
    read.assert_ok();
    assert_eq!(read.body, json!({ "target": "vm100", "notes": ["The shared folders go offline while this is down."] }));
    let none = harness.get(&notes_path("vm101"), &cookie).await;
    assert_eq!(none.body, json!({ "target": "vm101", "notes": [] }), "no notes is an empty list");

    let lines = json!(["Backups stop while this is down.", "Ask before stopping it in the evening."]);
    let written = put(&harness, &notes_path("vm101"), &cookie, json!({ "notes": lines })).await;
    written.assert_ok();
    assert_eq!(written.body, json!({ "target": "vm101", "notes": lines }));
    assert_eq!(harness.get(&notes_path("vm101"), &cookie).await.body["notes"], lines);
    assert_eq!(devices_events_since(&harness, seq), 1);

    // The confirmation of an action on it carries the notes.
    let prepared = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    assert!(prepared.text.contains("Backups stop while this is down."), "{}", prepared.text);
    assert!(prepared.text.contains("Ask before stopping it in the evening."), "{}", prepared.text);

    // Audited with how many lines, not what they say.
    let rows = audit_rows(&harness, "device.notes");
    assert_eq!(rows.len(), 1);
    let (target, user, params, result) = &rows[0];
    assert_eq!((target.as_str(), user.as_str(), result.as_str()), ("vm101", owner_id(&harness).as_str(), "done"));
    assert_eq!(*params, json!({ "lines": 2 }));
    assert!(!harness.dump("audit_log").contains("Backups stop"), "the text stays out of the log");

    // The same lines again change nothing to announce; no lines removes them.
    let seq = harness.app.events.latest();
    put(&harness, &notes_path("vm101"), &cookie, json!({ "notes": lines })).await.assert_ok();
    assert_eq!(devices_events_since(&harness, seq), 0);
    put(&harness, &notes_path("vm101"), &cookie, json!({ "notes": [] })).await.assert_ok();
    assert_eq!(devices_events_since(&harness, seq), 1);
    assert!(harness.app.store.with(|conn| notes::get(conn, "vm101")).unwrap().is_empty());
    let prepared = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    assert!(!prepared.text.contains("Backups stop"), "{}", prepared.text);
    assert_eq!(audit_rows(&harness, "device.notes").len(), 3);
}

#[tokio::test]
async fn notes_are_the_owners_alone_to_read_and_write() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let (_family, family_cookie, family_id) = harness.add_family(&cookie, &mut owner, "Bob").await;
    let seq = harness.app.events.latest();

    // Even on a light, which the family member uses.
    for target in ["vm100", "bulb1"] {
        harness.get(&notes_path(target), &family_cookie).await.assert_refused(StatusCode::FORBIDDEN, "owner_only");
        put(&harness, &notes_path(target), &family_cookie, json!({ "notes": ["Mine now."] }))
            .await
            .assert_refused(StatusCode::FORBIDDEN, "owner_only");
    }
    let refusals = audit_rows(&harness, "access.owner_only");
    assert_eq!(refusals.len(), 4);
    assert!(refusals.iter().all(|(route, user, _, _)| route == "/api/v1/devices/{target}/notes" && *user == family_id));
    assert_eq!(
        harness.app.store.with(|conn| notes::get(conn, "vm100")).unwrap(),
        ["The shared folders go offline while this is down."]
    );
    assert!(harness.app.store.with(|conn| notes::get(conn, "bulb1")).unwrap().is_empty());
    assert!(audit_rows(&harness, "device.notes").is_empty());
    assert_eq!(devices_events_since(&harness, seq), 0);
}

#[tokio::test]
async fn wrong_notes_or_a_device_that_is_not_here_are_refused() {
    let (harness, _owner, cookie) = Harness::signed_in().await;
    let seq = harness.app.events.latest();

    harness.get(&notes_path("vm999"), &cookie).await.assert_refused(StatusCode::NOT_FOUND, "unknown_device");
    put(&harness, &notes_path("vm999"), &cookie, json!({ "notes": ["Nobody home."] }))
        .await
        .assert_refused(StatusCode::NOT_FOUND, "unknown_device");

    let long = "x".repeat(notes::MAX_LINE + 1);
    for wrong in [
        json!([""]),
        json!([" padded"]),
        json!(["two\nlines"]),
        json!([long]),
        json!(vec!["One more than allowed."; notes::MAX_LINES + 1]),
    ] {
        put(&harness, &notes_path("vm100"), &cookie, json!({ "notes": wrong }))
            .await
            .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_notes");
    }
    for unreadable in
        [json!({ "lines": ["A note."] }), json!({ "notes": "A note." }), json!({ "notes": [], "more": 1 })]
    {
        let refused = put(&harness, &notes_path("vm100"), &cookie, unreadable.clone()).await;
        assert_eq!(refused.code(), "invalid_body", "{unreadable}: {}", refused.text);
    }
    harness
        .send(Method::PUT, &notes_path("vm100"), Some(&cookie), Some(EVIL_ORIGIN), Some(json!({ "notes": [] })))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "bad_origin");

    assert_eq!(
        harness.app.store.with(|conn| notes::get(conn, "vm100")).unwrap(),
        ["The shared folders go offline while this is down."]
    );
    assert!(audit_rows(&harness, "device.notes").is_empty());
    assert_eq!(devices_events_since(&harness, seq), 0);
}
