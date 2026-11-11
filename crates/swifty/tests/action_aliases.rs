//! Renamed actions keep their old ids as aliases. A client that still sends
//! `bulb.power` or `sonos.volume` gets the same action, under the same rules, and
//! the audit log records what ran by its current id.

mod common;

use common::*;
use serde_json::{json, Value};
use swifty_hub::catalog;
use swifty_plugin_api::Op;

/// The action column of every audit row, in order.
fn audited_actions(harness: &Harness) -> Vec<(String, String, String)> {
    harness
        .app
        .store
        .with(|conn| {
            let mut statement = conn.prepare("SELECT action, target, result FROM audit_log ORDER BY seq")?;
            let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            rows.collect::<rusqlite::Result<Vec<(String, String, String)>>>()
        })
        .unwrap()
}

#[test]
fn the_catalog_resolves_every_old_id_to_its_new_action() {
    for (old, new) in [
        ("bulb.power", "light.power"),
        ("bulb.brightness", "light.brightness"),
        ("bulb.hue", "light.hue"),
        ("sonos.volume", "speaker.volume"),
        ("sonos.mute", "speaker.mute"),
        ("sonos.night", "speaker.night"),
        ("sonos.play", "speaker.play"),
        ("sonos.pause", "speaker.pause"),
    ] {
        let def = catalog::find(old).unwrap_or_else(|| panic!("{old} is not found"));
        assert_eq!(def.id, new);
        assert!(std::ptr::eq(def, catalog::find(new).unwrap()), "{old} is the same entry as {new}");
        assert!(def.aliases.contains(&old));
    }
    assert!(catalog::find("bulb.blink").is_none());
    assert!(catalog::find("sonos").is_none());
    let ids: Vec<&str> = catalog::CATALOG.iter().map(|def| def.id).collect();
    for def in catalog::CATALOG {
        for alias in def.aliases {
            assert!(!ids.contains(alias), "{alias} is an alias and an id");
        }
    }
}

#[tokio::test]
async fn old_ids_still_run_and_are_audited_under_the_new_ones() {
    let (harness, _auth, cookie) = Harness::signed_in().await;

    let light = harness.execute(&cookie, "bulb.power", "bulb1", json!({ "on": false })).await;
    light.assert_ok();
    assert_eq!(light.phase(), "confirmed", "{}", light.text);
    assert_eq!(light.body["action"], json!("light.power"));

    let speaker = harness.execute(&cookie, "sonos.volume", "sonos", json!({ "level": 12 })).await;
    speaker.assert_ok();
    assert_eq!(speaker.phase(), "confirmed", "{}", speaker.text);
    assert_eq!(speaker.body["action"], json!("speaker.volume"));

    let sent = harness.devices.commands();
    assert!(sent.contains(&("bulb1".to_string(), Op::LightPower { on: false })), "{sent:?}");
    assert!(sent.contains(&("sonos".to_string(), Op::Volume { level: 12 })), "{sent:?}");

    let audited = audited_actions(&harness);
    let rows = |action: &str, target: &str| -> Vec<String> {
        audited.iter().filter(|(a, t, _)| a == action && t == target).map(|(_, _, result)| result.clone()).collect()
    };
    assert_eq!(rows("light.power", "bulb1"), ["commanded", "confirmed"], "{audited:?}");
    assert_eq!(rows("speaker.volume", "sonos"), ["commanded", "confirmed"], "{audited:?}");
    assert!(
        !audited.iter().any(|(action, _, _)| action.starts_with("bulb.") || action.starts_with("sonos.")),
        "an old id was written: {audited:?}"
    );

    // An old id is refused for the same reasons as the new one, and the refusal
    // is written under the new id too.
    let refused = harness.execute(&cookie, "bulb.power", "tv", json!({ "on": true })).await;
    refused.assert_refused(axum::http::StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
    let last = audited_actions(&harness).pop().unwrap();
    assert_eq!((last.0.as_str(), last.2.as_str()), ("light.power", "refused: invalid_target"));
}

#[tokio::test]
async fn the_family_may_use_the_old_ids_of_their_actions() {
    let (harness, mut owner, owner_cookie) = Harness::signed_in().await;
    let (_family, cookie, _id) = harness.add_family(&owner_cookie, &mut owner, "Alice").await;
    harness.execute(&cookie, "bulb.brightness", "bulb2", json!({ "percent": 30 })).await.assert_ok();
    harness.execute(&cookie, "sonos.mute", "sonos", json!({ "muted": true })).await.assert_ok();
}

#[tokio::test]
async fn the_catalog_lists_the_old_ids_so_old_audit_rows_keep_a_label() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let listed = harness.get("/api/v1/catalog", &cookie).await;
    listed.assert_ok();
    let actions = listed.body["actions"].as_array().unwrap();
    let by_id = |id: &str| -> &Value { actions.iter().find(|a| a["id"] == json!(id)).unwrap() };
    assert_eq!(by_id("light.power")["aliases"], json!(["bulb.power"]));
    assert_eq!(by_id("speaker.volume")["aliases"], json!(["sonos.volume"]));
    assert_eq!(by_id("tv.input")["aliases"], json!([]));
    assert!(actions.iter().all(|a| a["id"] != json!("bulb.power")), "an alias is not listed as an action");
}
