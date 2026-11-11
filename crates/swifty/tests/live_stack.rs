//! The whole service with the real plugins and `live = true`, against mock
//! devices on loopback: tiers, two-phase results, retries and the voice route.

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::http::StatusCode;
use common::devices::*;
use common::*;
use serde_json::{json, Value};
use swifty_hub::catalog::ACK_PARAM;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;

async fn live() -> (Rig, Harness, SoftPasskey, String) {
    live_with_speech(None).await
}

async fn live_with_speech(speech: Option<&FakeSpeech>) -> (Rig, Harness, SoftPasskey, String) {
    let rig = Rig::start().await;
    let speech = speech.map(FakeSpeech::config);
    let harness = Harness::with_site(|site| {
        site.live = true;
        site.timing.confirm_timeout_ms = Some(1_200);
        site.timing.poll_ms = 5;
        site.speech = speech;
        rig.configure(site);
    });
    let mut auth = authenticator();
    harness.enroll(&mut auth, "iPhone").await;
    let cookie = harness.login(&mut auth).await;
    (rig, harness, auth, cookie)
}

async fn tap(harness: &Harness, cookie: &str, action: &str, target: &str, params: Value) -> Resp {
    let prepared = harness.prepare(cookie, action, target, params).await;
    prepared.assert_ok();
    harness.confirm_tap(cookie, &prepared).await
}

// ----- WiZ

#[tokio::test]
async fn a_bulb_is_confirmed_only_after_a_get_pilot() {
    let (rig, harness, _auth, cookie) = live().await;
    let done = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(rig.bulbs[0].methods(), ["setPilot", "getPilot"]);
    assert!(!rig.bulbs[0].state.lock().unwrap().on);
    assert_eq!(rig.bulbs[1].traffic(), 0);
}

#[tokio::test]
async fn a_bulb_that_says_yes_but_stays_the_same_is_no_response() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.bulbs[0].set_mode(BulbMode::AcksOnly);
    let done = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    assert_eq!(done.phase(), "no_response", "{}", done.text);
    assert!(rig.bulbs[0].count("getPilot") >= 1);
    assert_eq!(rig.bulbs[0].count("setPilot"), 1);
    assert!(harness.audit_has("no_response"));
    assert!(!harness.audit_has("confirmed"));
}

#[tokio::test]
async fn a_silent_bulb_is_no_response_after_one_more_try() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.bulbs[0].set_mode(BulbMode::Silent);
    let done = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    assert_eq!(done.phase(), "no_response", "{}", done.text);
    assert_eq!(rig.bulbs[0].count("setPilot"), 2, "an idempotent action gets exactly one more try");
}

#[tokio::test]
async fn a_bulb_that_refuses_is_failed() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.bulbs[0].set_mode(BulbMode::Error);
    let done = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    assert_eq!(done.phase(), "failed", "{}", done.text);
    assert_eq!(rig.bulbs[0].count("setPilot"), 1, "a refusal is not retried");
}

#[tokio::test]
async fn all_four_bulbs_are_commanded_and_each_read_back() {
    let (rig, harness, _auth, cookie) = live().await;
    let done = harness.execute(&cookie, "lights.brightness", "all_bulbs", json!({ "percent": 35 })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    for bulb in &rig.bulbs {
        assert_eq!(bulb.state.lock().unwrap().dimming, 35);
        assert!(bulb.count("getPilot") >= 1);
    }
}

#[tokio::test]
async fn a_colour_and_a_scene_are_confirmed_by_read_back() {
    let (rig, harness, _auth, cookie) = live().await;
    let done = harness.execute(&cookie, "light.hue", "bulb2", json!({ "hue": 120 })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(rig.bulbs[1].state.lock().unwrap().rgb, Some((0, 255, 0)));

    let done = harness.execute(&cookie, "scene.apply", "evening", json!({})).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert!(!rig.bulbs[2].state.lock().unwrap().on);
    assert_eq!(rig.bulbs[0].state.lock().unwrap().dimming, 40);
}

// ----- Sonos

#[tokio::test]
async fn sonos_commands_are_confirmed_by_read_back() {
    let (rig, harness, _auth, cookie) = live().await;
    let done = harness.execute(&cookie, "speaker.volume", "sonos", json!({ "level": 40 })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(rig.sonos.state.lock().unwrap().volume, 40);
    assert!(rig.sonos.actions().contains(&"GetVolume".to_string()));

    let done = harness.execute(&cookie, "speaker.mute", "sonos", json!({ "muted": true })).await;
    assert_eq!(done.phase(), "confirmed");
    let done = harness.execute(&cookie, "speaker.pause", "sonos", json!({})).await;
    assert_eq!(done.phase(), "confirmed");
    let done = harness.execute(&cookie, "speaker.play", "sonos", json!({})).await;
    assert_eq!(done.phase(), "confirmed");
}

#[tokio::test]
async fn night_mode_is_confirmed_by_read_back_and_shown_in_the_state() {
    let (rig, harness, _auth, cookie) = live().await;
    let done = harness.execute(&cookie, "speaker.night", "sonos", json!({ "on": true })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert!(rig.sonos.state.lock().unwrap().night);
    assert_eq!(harness.get("/api/v1/state/sonos", &cookie).await.body["state"]["night"], json!(true));

    let done = harness.execute(&cookie, "speaker.night", "sonos", json!({ "on": false })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert!(!rig.sonos.state.lock().unwrap().night);
    harness
        .execute(&cookie, "speaker.night", "sub", json!({ "on": true }))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
}

#[tokio::test]
async fn a_speaker_with_no_night_mode_is_still_available() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.sonos.state.lock().unwrap().eq_refused = true;
    let state = harness.get("/api/v1/state/sonos", &cookie).await;
    assert_eq!(state.body["available"], json!(true), "{}", state.text);
    assert!(state.body["state"].get("night").is_none(), "{}", state.text);
    assert_eq!(state.body["state"]["vol"], json!(24), "{}", state.text);
}

#[tokio::test]
async fn a_speaker_that_ignores_a_change_is_no_response() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.sonos.state.lock().unwrap().apply = false;
    let done = harness.execute(&cookie, "speaker.volume", "sonos", json!({ "level": 40 })).await;
    assert_eq!(done.phase(), "no_response", "{}", done.text);
}

#[tokio::test]
async fn a_upnp_fault_is_failed() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.sonos.state.lock().unwrap().fault = Some(701);
    let done = harness.execute(&cookie, "speaker.play", "sonos", json!({})).await;
    assert_eq!(done.phase(), "failed", "{}", done.text);
    assert!(done.body["detail"].as_str().unwrap().contains("701"));
}

// ----- the TV

#[tokio::test]
async fn tv_power_is_two_steps_and_a_tv_that_disappears_is_not_a_confirmation() {
    let (rig, harness, _auth, cookie) = live().await;
    harness
        .execute(&cookie, "tv.power", "tv", json!({ "on": false }))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "confirmation_required");
    assert_eq!(rig.tv.traffic(), 0);

    let done = tap(&harness, &cookie, "tv.power", "tv", json!({ "on": false })).await;
    assert_eq!(done.phase(), "no_response", "{}", done.text);
    assert!(done.body["detail"].as_str().unwrap().contains("stopped answering"), "{}", done.text);
    assert_eq!(rig.tv.state.lock().unwrap().power, "Off");
    assert!(!harness.audit_has("confirmed"));
}

#[tokio::test]
async fn a_tv_that_accepts_power_off_stays_on_and_goes_silent_is_never_confirmed() {
    let (rig, harness, _auth, cookie) = live().await;
    {
        let mut tv = rig.tv.state.lock().unwrap();
        tv.apply = false;
        tv.silent_after_off = true;
    }
    let done = tap(&harness, &cookie, "tv.power", "tv", json!({ "on": false })).await;
    assert_eq!(done.phase(), "no_response", "{}", done.text);
    assert_eq!(rig.tv.state.lock().unwrap().power, "Active", "the mock really did stay on");
    assert!(!harness.audit_has("confirmed"), "{:?}", harness.audit_results());

    let state = harness.get("/api/v1/state/tv", &cookie).await;
    assert_eq!(state.body["available"], json!(false), "{}", state.text);
    assert!(state.body.get("state").is_none(), "{}", state.text);
}

#[tokio::test]
async fn a_tv_that_reports_being_off_is_confirmed() {
    let (rig, harness, _auth, cookie) = live().await;
    {
        let mut tv = rig.tv.state.lock().unwrap();
        tv.apply = false;
        tv.power = "Screen Off".to_string();
    }
    let done = tap(&harness, &cookie, "tv.power", "tv", json!({ "on": false })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
}

#[tokio::test]
async fn a_poll_never_makes_the_first_connection_to_the_tv() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.tv.state.lock().unwrap().accepted_key = "changed".to_string();

    let (a, b, c) = tokio::join!(
        harness.get("/api/v1/state/tv", &cookie),
        harness.get("/api/v1/state/tv", &cookie),
        harness.get("/api/v1/state", &cookie),
    );
    assert_eq!(a.body["available"], json!(false), "{}", a.text);
    assert!(a.body["error"].as_str().unwrap().contains("first time"), "{}", a.text);
    assert_eq!(b.body["available"], json!(false), "{}", b.text);
    assert_eq!(c.body["devices"]["tv"]["available"], json!(false), "{}", c.text);
    assert_eq!(rig.tv.prompts.load(Ordering::SeqCst), 0, "a poll raised a pairing prompt");
    assert_eq!(rig.tv.traffic(), 0, "a poll connected to the TV");
}

#[tokio::test]
async fn deliberate_reads_of_a_stale_key_raise_one_prompt_at_most() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.tv.state.lock().unwrap().accepted_key = "changed".to_string();
    let read = || harness.execute(&cookie, "state.read", "tv", json!({}));
    let (a, b, c) = tokio::join!(read(), read(), read());
    for done in [a, b, c] {
        assert_eq!(done.phase(), "failed", "{}", done.text);
        assert!(done.body["detail"].as_str().unwrap().contains("Nothing here starts pairing"), "{}", done.text);
    }
    assert_eq!(rig.tv.prompts.load(Ordering::SeqCst), 1);
    assert_eq!(rig.tv.traffic(), 1);
}

#[tokio::test]
async fn a_poll_reaches_the_tv_once_its_key_has_been_accepted() {
    let (rig, harness, _auth, cookie) = live().await;
    let read = harness.execute(&cookie, "state.read", "tv", json!({})).await;
    assert_eq!(read.phase(), "confirmed", "{}", read.text);
    let state = harness.get("/api/v1/state/tv", &cookie).await;
    assert_eq!(state.body["available"], json!(true), "{}", state.text);
    assert_eq!(rig.tv.prompts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn tv_on_wakes_it_and_waits_to_see_it_up() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.tv.state.lock().unwrap().power = "Off".to_string();
    let done = tap(&harness, &cookie, "tv.power", "tv", json!({ "on": true })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert!(rig.wol.traffic() >= 1);
}

#[tokio::test]
async fn tv_input_is_confirmed_by_the_foreground_app() {
    let (rig, harness, _auth, cookie) = live().await;
    let done = harness.execute(&cookie, "tv.input", "tv", json!({ "input": "HDMI 2" })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(rig.tv.state.lock().unwrap().app, "com.webos.app.hdmi2");
}

#[tokio::test]
async fn a_stale_tv_key_fails_without_a_second_prompt() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.tv.state.lock().unwrap().accepted_key = "changed".to_string();
    let done = harness.execute(&cookie, "tv.input", "tv", json!({ "input": "HDMI 2" })).await;
    assert_eq!(done.phase(), "failed", "{}", done.text);
    assert!(done.body["detail"].as_str().unwrap().contains("Nothing here starts pairing"));
    harness.get("/api/v1/state/tv", &cookie).await.assert_ok();
    harness.execute(&cookie, "tv.input", "tv", json!({ "input": "HDMI 3" })).await;
    assert_eq!(rig.tv.prompts.load(Ordering::SeqCst), 1);
    assert_eq!(rig.tv.traffic(), 1);
}

async fn pointer_frames(rig: &Rig, count: usize) -> Vec<String> {
    for _ in 0..200 {
        if rig.tv.frames.lock().unwrap().len() >= count {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    rig.tv.frames.lock().unwrap().clone()
}

#[tokio::test]
async fn a_key_press_is_sent_and_never_called_confirmed() {
    let (rig, harness, _auth, cookie) = live().await;
    let done = harness.execute(&cookie, "tv.key", "tv", json!({ "key": "up" })).await;
    done.assert_ok();
    assert_eq!(done.phase(), "sent", "{}", done.text);
    assert!(done.body["detail"].as_str().unwrap().contains("cannot be read back"), "{}", done.text);
    assert_eq!(pointer_frames(&rig, 1).await, ["type:button\nname:UP\n\n"]);
    assert!(harness.audit_has("commanded"));
    assert!(harness.audit_has("sent"));
    assert!(!harness.audit_has("confirmed"));
    assert!(!rig.tv.uris().iter().any(|uri| uri.contains("getPowerState")), "a key press has nothing to read back");
    assert_eq!(harness.dump("action_runs"), "", "a press leaves no run to join or repeat");
}

#[tokio::test]
async fn pressing_the_same_key_again_presses_it_again() {
    let (rig, harness, _auth, cookie) = live().await;
    for _ in 0..3 {
        let done = harness.execute(&cookie, "tv.key", "tv", json!({ "key": "volume_up" })).await;
        assert_eq!(done.phase(), "sent", "{}", done.text);
        assert_eq!(done.body["duplicate"], json!(false));
    }
    assert_eq!(pointer_frames(&rig, 3).await.len(), 3);

    // And when they overlap.
    let press = || harness.execute(&cookie, "tv.key", "tv", json!({ "key": "volume_up" }));
    let (a, b, c) = tokio::join!(press(), press(), press());
    for done in [a, b, c] {
        assert_eq!(done.phase(), "sent", "{}", done.text);
    }
    assert_eq!(pointer_frames(&rig, 6).await.len(), 6);
    assert_eq!(harness.audit_count("sent"), 6);
}

#[tokio::test]
async fn a_tv_nobody_has_contacted_yet_says_so_and_a_key_press_connects_to_it() {
    let (rig, harness, _auth, cookie) = live().await;
    let waiting = harness.get("/api/v1/state/tv", &cookie).await;
    assert_eq!(waiting.body["available"], json!(false), "{}", waiting.text);
    assert_eq!(waiting.body["not_contacted"], json!(true), "{}", waiting.text);
    assert_eq!(rig.tv.traffic(), 0);
    let done = harness.execute(&cookie, "tv.key", "tv", json!({ "key": "home" })).await;
    assert_eq!(done.phase(), "sent", "{}", done.text);
    let state = harness.get("/api/v1/state/tv", &cookie).await;
    assert_eq!(state.body["available"], json!(true), "{}", state.text);
    assert!(state.body.get("not_contacted").is_none(), "{}", state.text);
}

#[tokio::test]
async fn a_key_press_with_no_reply_is_not_sent_again() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.tv.hang.store(true, Ordering::SeqCst);
    let done = harness.execute(&cookie, "tv.key", "tv", json!({ "key": "ok" })).await;
    assert_eq!(done.phase(), "no_response", "{}", done.text);
    assert_eq!(rig.tv.traffic(), 1, "a press is never tried twice");
    assert!(rig.tv.frames.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_key_press_on_a_tv_that_is_off_is_no_response() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.tv.state.lock().unwrap().power = "Off".to_string();
    let done = harness.execute(&cookie, "tv.key", "tv", json!({ "key": "home" })).await;
    assert_eq!(done.phase(), "no_response", "{}", done.text);
    assert!(rig.tv.frames.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_key_that_is_not_on_the_remote_is_refused_before_the_tv() {
    let (rig, harness, _auth, cookie) = live().await;
    for bad in [
        json!({ "key": "power" }),
        json!({ "key": "UP" }),
        json!({ "key": 1 }),
        json!({}),
        json!({ "key": "up", "x": 1 }),
    ] {
        harness
            .execute(&cookie, "tv.key", "tv", bad)
            .await
            .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_params");
    }
    harness
        .execute(&cookie, "tv.key", "sonos", json!({ "key": "up" }))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
    assert_eq!(rig.tv.traffic(), 0);
}

#[tokio::test]
async fn a_key_press_needs_no_second_step_and_goes_nowhere_a_reply_says() {
    let (rig, harness, _auth, cookie) = live().await;
    let other = MockTv::start().await;
    rig.tv.state.lock().unwrap().socket_path = format!("{}{POINTER_PATH}", other.url);
    let done = harness.execute(&cookie, "tv.key", "tv", json!({ "key": "up" })).await;
    assert_eq!(done.phase(), "failed", "{}", done.text);
    assert!(done.body["detail"].as_str().unwrap().contains("somewhere else"), "{}", done.text);
    assert_eq!(other.traffic(), 0);
}

#[tokio::test]
async fn in_dry_run_a_key_press_sends_nothing() {
    let (rig, harness, _auth, cookie) = rig_harness(false, |_| {}).await;
    let done = harness.execute(&cookie, "tv.key", "tv", json!({ "key": "up" })).await;
    done.assert_ok();
    assert_eq!(done.phase(), "simulated", "{}", done.text);
    assert_eq!(done.body["simulated"], json!(true));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.tv.traffic(), 0);
    assert!(!harness.audit_has("sent"));
}

// ----- smart sockets

#[tokio::test]
async fn a_socket_is_switched_and_confirmed_by_reading_it_back() {
    let (rig, harness, _auth, cookie) = live_with_plugs().await;
    let target = rig.plugs[0].target();
    let done = harness.execute(&cookie, "plug.power", &target, json!({ "on": true })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert!(rig.plugs[0].state.lock().unwrap().on);
    assert!(!rig.plugs[1].state.lock().unwrap().on, "only the socket that was asked");
    let commands: Vec<u32> = rig.plugs[0].requests.lock().unwrap().iter().map(|(command, _)| *command).collect();
    assert_eq!(commands[0], 7, "the command goes first");
    assert!(commands[1..].contains(&10), "then it is read back");

    let done = harness.execute(&cookie, "plug.power", &target, json!({ "on": false })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert!(!rig.plugs[0].state.lock().unwrap().on);
    assert!(harness.audit_has("confirmed"));
}

async fn live_with_plugs() -> (Rig, Harness, SoftPasskey, String) {
    rig_harness(true, |_| {}).await
}

#[tokio::test]
async fn sockets_are_listed_by_their_place_in_the_configuration_and_can_be_renamed() {
    let (rig, harness, _auth, cookie) = live_with_plugs().await;
    let devices = harness.get("/api/v1/devices", &cookie).await;
    let name_of = |body: &Value, target: &str| -> Option<String> {
        body["devices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["target"] == json!(target))
            .map(|d| d["name"].as_str().unwrap().to_string())
    };
    assert_eq!(name_of(&devices.body, &rig.plugs[0].target()).as_deref(), Some("Socket 1"), "{}", devices.text);
    assert_eq!(name_of(&devices.body, &rig.plugs[1].target()).as_deref(), Some("Socket 2"), "{}", devices.text);

    let path = format!("/api/v1/devices/{}/name", rig.plugs[0].target());
    harness
        .send(axum::http::Method::PUT, &path, Some(&cookie), Some(ORIGIN), Some(json!({ "name": "Desk lamp" })))
        .await
        .assert_ok();
    let devices = harness.get("/api/v1/devices", &cookie).await;
    assert_eq!(name_of(&devices.body, &rig.plugs[0].target()).as_deref(), Some("Desk lamp"), "{}", devices.text);
}

#[tokio::test]
async fn a_socket_with_a_configured_name_goes_by_it() {
    let rig = Rig::start().await;
    let config = tuya_config(&[&rig.plugs[0]], &[Some("Heater")]);
    let (_other, harness, _auth, cookie) = rig_harness(true, move |site| {
        *site.instance("tuya") = config;
    })
    .await;
    let devices = harness.get("/api/v1/devices", &cookie).await;
    assert!(devices.text.contains("Heater"), "{}", devices.text);
}

#[tokio::test]
async fn a_socket_reads_as_on_or_off_on_every_state_route() {
    let (rig, harness, _auth, cookie) = live_with_plugs().await;
    rig.plugs[1].state.lock().unwrap().on = true;
    let state = harness.get("/api/v1/state", &cookie).await;
    let shown = &state.body["devices"][rig.plugs[1].target()];
    assert_eq!(shown["available"], json!(true), "{}", state.text);
    assert_eq!(shown["plugin"], json!("tuya"), "{}", state.text);
    assert_eq!(shown["instance"], json!("tuya"), "{}", state.text);
    assert_eq!(shown["state"], json!({ "kind": "plug", "on": true }), "{}", state.text);
    let read = harness.execute(&cookie, "state.read", &rig.plugs[0].target(), json!({})).await;
    assert_eq!(read.phase(), "confirmed", "{}", read.text);
}

#[tokio::test]
async fn an_unreachable_socket_shows_as_unavailable() {
    let (rig, harness, _auth, cookie) = live_with_plugs().await;
    let target = rig.plugs[0].target();
    rig.plugs[0].state.lock().unwrap().mode = TuyaMode::Silent;
    let state = harness.get(&format!("/api/v1/state/{target}"), &cookie).await;
    assert_eq!(state.body["available"], json!(false), "{}", state.text);
    assert!(state.body["error"].is_string(), "{}", state.text);
    assert!(state.body.get("state").is_none(), "{}", state.text);
    let done = harness.execute(&cookie, "plug.power", &target, json!({ "on": true })).await;
    assert_eq!(done.phase(), "no_response", "{}", done.text);
    assert!(!harness.audit_has("confirmed"));
}

#[tokio::test]
async fn a_socket_that_ignores_a_command_is_not_confirmed() {
    let (rig, harness, _auth, cookie) = live_with_plugs().await;
    rig.plugs[0].state.lock().unwrap().mode = TuyaMode::Ignores;
    let done = harness.execute(&cookie, "plug.power", &rig.plugs[0].target(), json!({ "on": true })).await;
    assert_eq!(done.phase(), "no_response", "{}", done.text);
}

#[tokio::test]
async fn a_socket_refused_or_damaged_reply_is_a_failure_and_the_key_stays_hidden() {
    let (rig, harness, _auth, cookie) = live_with_plugs().await;
    let target = rig.plugs[0].target();
    for mode in [TuyaMode::Refuses(9), TuyaMode::BadCrc] {
        rig.plugs[0].state.lock().unwrap().mode = mode;
        let done = harness.execute(&cookie, "plug.power", &target, json!({ "on": true })).await;
        assert_eq!(done.phase(), "failed", "{mode:?}: {}", done.text);
        assert!(!done.text.contains(TUYA_KEY_BASE64), "{}", done.text);
        harness.advance(10_001);
    }
    for table in ["audit_log", "action_runs"] {
        assert!(!harness.dump(table).contains(TUYA_KEY_BASE64), "the key reached {table}");
    }
}

#[tokio::test]
async fn only_a_configured_socket_and_a_plain_on_or_off_are_accepted() {
    let (rig, harness, _auth, cookie) = live_with_plugs().await;
    let target = rig.plugs[0].target();
    for bad in ["plug-bf0000000000000099", "plug-short", "bulb1", "plug-BF0123456789ABCDEF"] {
        harness
            .execute(&cookie, "plug.power", bad, json!({ "on": true }))
            .await
            .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
    }
    for bad in [json!({ "on": "yes" }), json!({}), json!({ "on": true, "dps": 1 })] {
        harness
            .execute(&cookie, "plug.power", &target, bad)
            .await
            .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_params");
    }
    assert_eq!(rig.plugs[0].traffic(), 0);
}

#[tokio::test]
async fn family_cannot_see_or_use_a_socket() {
    let (rig, harness, mut owner, cookie) = live_with_plugs().await;
    let (_family, family_cookie, _id) = harness.add_family(&cookie, &mut owner, "Alice").await;
    let target = rig.plugs[0].target();
    harness
        .execute(&family_cookie, "plug.power", &target, json!({ "on": true }))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "owner_only");
    let state = harness.get("/api/v1/state", &family_cookie).await;
    assert!(!state.text.contains(&target), "{}", state.text);
    assert_eq!(rig.plugs[0].traffic(), 0);
}

#[tokio::test]
async fn in_dry_run_a_socket_is_simulated_and_hears_nothing() {
    let (rig, harness, _auth, cookie) = rig_harness(false, |_| {}).await;
    let target = rig.plugs[0].target();
    let done = harness.execute(&cookie, "plug.power", &target, json!({ "on": true })).await;
    done.assert_ok();
    assert_eq!(done.phase(), "simulated", "{}", done.text);
    harness.get("/api/v1/state", &cookie).await.assert_ok();
    harness.execute(&cookie, "state.read", &target, json!({})).await.assert_ok();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.plugs[0].traffic(), 0, "dry-run reached a socket");
    assert_eq!(rig.traffic(), 0, "dry-run reached a device");
}

#[tokio::test]
async fn with_no_sockets_configured_there_are_none() {
    let (_rig, harness, _auth, cookie) = rig_harness(true, |site| site.remove("tuya")).await;
    let devices = harness.get("/api/v1/devices", &cookie).await;
    assert!(!devices.text.contains("plug-"), "{}", devices.text);
    harness
        .execute(&cookie, "plug.power", "plug-bf0123456789abcdef", json!({ "on": true }))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
}

// ----- the air conditioner

async fn live_with_cloud() -> (Rig, Harness, SoftPasskey, String) {
    rig_harness(true, |_| {}).await
}

#[tokio::test]
async fn an_air_conditioner_takes_each_command_and_shows_it_in_a_reading() {
    let (rig, harness, _auth, cookie) = live_with_cloud().await;
    let target = rig.toshiba.target(0);
    for (action, params) in [
        ("ac.power", json!({ "on": true })),
        ("ac.mode", json!({ "mode": "heat" })),
        ("ac.temperature", json!({ "celsius": 25 })),
        ("ac.fan", json!({ "fan": "quiet" })),
    ] {
        let done = harness.execute(&cookie, action, &target, params).await;
        assert_eq!(done.phase(), "confirmed", "{action}: {}", done.text);
    }
    assert_eq!(rig.toshiba.state(0), "3043193131640010180affffffff10ffffffff");
    assert_eq!(rig.toshiba.state(1), AC_OFF, "only the unit that was asked");
    assert!(harness.audit_has("confirmed"));
}

#[tokio::test]
async fn an_air_conditioner_confirms_once_the_cloud_catches_up() {
    let (rig, harness, _auth, cookie) = rig_harness(true, |c| c.timing.confirm_timeout_ms = Some(6_000)).await;
    rig.toshiba.cloud.lock().unwrap().lag = 1;
    let done = harness.execute(&cookie, "ac.power", &rig.toshiba.target(0), json!({ "on": true })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert!(rig.toshiba.cloud.lock().unwrap().state_reads >= 2, "it had to look again");
}

#[tokio::test]
async fn an_air_conditioner_reads_on_every_state_route_and_can_be_named() {
    let (rig, harness, _auth, cookie) = live_with_cloud().await;
    let target = rig.toshiba.target(0);
    let state = harness.get("/api/v1/state", &cookie).await;
    let shown = &state.body["devices"][&target];
    assert_eq!(shown["available"], json!(true), "{}", state.text);
    assert_eq!(shown["plugin"], json!("toshiba"), "{}", state.text);
    assert_eq!(shown["instance"], json!("toshiba"), "{}", state.text);
    assert_eq!(
        shown["state"],
        json!({ "kind": "aircon", "on": false, "mode": "cool", "target": 22, "fan": "auto", "indoor": 24, "outdoor": 10 }),
        "{}",
        state.text
    );
    let read = harness.execute(&cookie, "state.read", &target, json!({})).await;
    assert_eq!(read.phase(), "confirmed", "{}", read.text);

    let devices = harness.get("/api/v1/devices", &cookie).await;
    assert!(devices.text.contains("Air conditioner 1"), "{}", devices.text);
    let path = format!("/api/v1/devices/{target}/name");
    harness
        .send(axum::http::Method::PUT, &path, Some(&cookie), Some(ORIGIN), Some(json!({ "name": "Lounge AC" })))
        .await
        .assert_ok();
    assert!(harness.get("/api/v1/devices", &cookie).await.text.contains("Lounge AC"));

    let room = harness.post("/api/v1/rooms", &cookie, json!({ "name": "Lounge" })).await;
    room.assert_ok();
    let id = room.body["id"].as_str().or_else(|| room.body["room"]["id"].as_str()).unwrap().to_string();
    harness
        .send(
            axum::http::Method::PUT,
            &format!("/api/v1/rooms/{id}/devices"),
            Some(&cookie),
            Some(ORIGIN),
            Some(json!({ "targets": [target] })),
        )
        .await
        .assert_ok();
    let rooms = harness.get("/api/v1/rooms", &cookie).await;
    assert!(rooms.text.contains(&target), "{}", rooms.text);
}

#[tokio::test]
async fn an_air_conditioner_that_ignores_a_command_is_not_confirmed() {
    let (rig, harness, _auth, cookie) = live_with_cloud().await;
    rig.toshiba.cloud.lock().unwrap().mode = CloudMode::Ignores;
    let done = harness.execute(&cookie, "ac.power", &rig.toshiba.target(0), json!({ "on": true })).await;
    assert_eq!(done.phase(), "no_response", "{}", done.text);
    assert!(!harness.audit_has("confirmed"));
}

#[tokio::test]
async fn an_air_conditioner_refused_sign_in_is_a_failure_and_stays_private() {
    let (rig, harness, _auth, cookie) = live_with_cloud().await;
    rig.toshiba.cloud.lock().unwrap().mode = CloudMode::BadPassword;
    let done = harness.execute(&cookie, "ac.fan", &rig.toshiba.target(0), json!({ "fan": "low" })).await;
    assert_eq!(done.phase(), "failed", "{}", done.text);
    for secret in [TOSHIBA_PASSWORD, TOSHIBA_BEARER, TOSHIBA_SIGNATURE] {
        assert!(!done.text.contains(secret), "{}", done.text);
        for table in ["audit_log", "action_runs"] {
            assert!(!harness.dump(table).contains(secret), "{table}");
        }
    }
}

#[tokio::test]
async fn only_a_configured_unit_and_values_it_takes_are_accepted() {
    let (rig, harness, _auth, cookie) = live_with_cloud().await;
    let target = rig.toshiba.target(0);
    for bad in
        ["ac-99999999-1111-2222-3333-444455556666", "ac-short", "bulb1", "ac-0A1B2C3D-1111-2222-3333-444455556666"]
    {
        harness
            .execute(&cookie, "ac.power", bad, json!({ "on": true }))
            .await
            .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
    }
    for (action, bad) in [
        ("ac.power", json!({ "on": "yes" })),
        ("ac.power", json!({})),
        ("ac.mode", json!({ "mode": "boil" })),
        ("ac.temperature", json!({ "celsius": 16 })),
        ("ac.temperature", json!({ "celsius": 31 })),
        ("ac.temperature", json!({ "celsius": "22" })),
        ("ac.fan", json!({ "fan": "turbo" })),
    ] {
        harness
            .execute(&cookie, action, &target, bad)
            .await
            .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_params");
    }
    assert!(rig.toshiba.cloud.lock().unwrap().commands.is_empty());
}

#[tokio::test]
async fn family_cannot_see_or_use_the_air_conditioner() {
    let (rig, harness, mut owner, cookie) = live_with_cloud().await;
    let (_family, family_cookie, _id) = harness.add_family(&cookie, &mut owner, "Alice").await;
    let target = rig.toshiba.target(0);
    harness
        .execute(&family_cookie, "ac.power", &target, json!({ "on": true }))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "owner_only");
    let state = harness.get("/api/v1/state", &family_cookie).await;
    assert!(!state.text.contains(&target), "{}", state.text);
}

#[tokio::test]
async fn in_dry_run_an_air_conditioner_is_simulated_and_hears_nothing() {
    let (rig, harness, _auth, cookie) = rig_harness(false, |_| {}).await;
    let target = rig.toshiba.target(0);
    let done = harness.execute(&cookie, "ac.temperature", &target, json!({ "celsius": 24 })).await;
    done.assert_ok();
    assert_eq!(done.phase(), "simulated", "{}", done.text);
    harness.get("/api/v1/state", &cookie).await.assert_ok();
    harness.execute(&cookie, "state.read", &target, json!({})).await.assert_ok();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.toshiba.traffic(), 0, "dry-run reached the cloud");
}

#[tokio::test]
async fn with_no_toshiba_configured_there_is_no_air_conditioner() {
    let (_rig, harness, _auth, cookie) = rig_harness(true, |site| site.remove("toshiba")).await;
    let devices = harness.get("/api/v1/devices", &cookie).await;
    assert!(!devices.text.contains("ac-0a1b"), "{}", devices.text);
    harness
        .execute(&cookie, "ac.power", "ac-0a1b2c3d-1111-2222-3333-444455556666", json!({ "on": true }))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
}

#[tokio::test]
async fn scene_movie_runs_across_three_adapters() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.sonos.state.lock().unwrap().volume = 80;
    rig.tv.state.lock().unwrap().power = "Off".to_string();
    let done = tap(&harness, &cookie, "scene.movie", "movie", json!({})).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(rig.sonos.state.lock().unwrap().volume, 30);
    assert_eq!(rig.tv.state.lock().unwrap().app, "com.webos.app.hdmi1");
    // The second bulb asked for 8%, which WiZ cannot do. It reads back as 10.
    assert_eq!(rig.bulbs[1].state.lock().unwrap().dimming, 10);
}

// ----- the printer

#[tokio::test]
async fn print_start_is_two_steps_and_reads_back() {
    let (rig, harness, _auth, cookie) = live().await;
    harness
        .execute(&cookie, "print.start", "printer", json!({}))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "confirmation_required");
    assert!(rig.moonraker.http.posts().is_empty());

    let done = tap(&harness, &cookie, "print.start", "printer", json!({})).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    let posts = rig.moonraker.http.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].query, "filename=benchy.gcode");
    assert_eq!(posts[0].header("x-api-key"), MOONRAKER_KEY);
}

#[tokio::test]
async fn print_start_that_never_shows_up_is_not_sent_again() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.moonraker.state.lock().unwrap().apply = false;
    let done = tap(&harness, &cookie, "print.start", "printer", json!({})).await;
    assert_eq!(done.phase(), "no_response", "{}", done.text);
    assert_eq!(rig.moonraker.http.posts().len(), 1);
}

#[tokio::test]
async fn a_print_start_with_no_reply_is_never_retried() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.moonraker.http.hang_posts.store(true, Ordering::SeqCst);
    let done = tap(&harness, &cookie, "print.start", "printer", json!({})).await;
    assert_eq!(done.phase(), "no_response", "{}", done.text);
    assert_eq!(rig.moonraker.http.posts().len(), 1, "print.start must never be sent twice");
    assert_eq!(harness.audit_count("commanded"), 1);
    assert!(!harness.audit_has("confirmed"));
}

#[tokio::test]
async fn an_idempotent_command_with_no_reply_gets_one_more_try() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.moonraker.state.lock().unwrap().print = "printing".to_string();
    rig.moonraker.http.hang_posts.store(true, Ordering::SeqCst);
    let done = tap(&harness, &cookie, "print.pause", "printer", json!({})).await;
    assert_eq!(done.phase(), "no_response", "{}", done.text);
    assert_eq!(rig.moonraker.http.posts().len(), 2);
}

#[tokio::test]
async fn pause_resume_and_the_emergency_stop_are_read_back() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.moonraker.state.lock().unwrap().print = "printing".to_string();
    let done = tap(&harness, &cookie, "print.pause", "printer", json!({})).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(rig.moonraker.state.lock().unwrap().print, "paused");

    let done = tap(&harness, &cookie, "print.resume", "printer", json!({})).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);

    let done = harness.execute(&cookie, "print.estop", "printer", json!({})).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(rig.moonraker.state.lock().unwrap().webhooks, "shutdown");
}

// ----- Proxmox

#[tokio::test]
async fn vm_start_is_two_steps_and_confirmed_by_status() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.proxmox.vms.lock().unwrap().get_mut(&103).unwrap().status = "stopped".to_string();
    harness
        .execute(&cookie, "vm.start", "vm103", json!({}))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "confirmation_required");
    assert!(rig.proxmox.http.posts().is_empty());
    let done = tap(&harness, &cookie, "vm.start", "vm103", json!({})).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(rig.proxmox.http.posts()[0].path, format!("/api2/json/nodes/{NODE}/qemu/103/status/start"));
}

#[tokio::test]
async fn vm_stop_needs_a_real_passkey_and_is_confirmed_by_status() {
    let (rig, harness, mut auth, cookie) = live().await;
    harness
        .execute(&cookie, "vm.stop", "vm101", json!({}))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "step_up_required");
    assert!(rig.proxmox.http.posts().is_empty());

    let done = harness.critical(&cookie, &mut auth, "vm.stop", "vm101", json!({})).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(done.body["step_up"], json!("passkey"));
    assert_eq!(rig.proxmox.http.posts().len(), 1);
    assert_eq!(rig.proxmox.vms.lock().unwrap()[&101].status, "stopped");
}

#[tokio::test]
async fn a_vm_reboot_needs_the_uptime_to_reset() {
    let (rig, harness, mut auth, cookie) = live().await;
    rig.proxmox.apply.store(false, Ordering::SeqCst);
    let done = harness.critical(&cookie, &mut auth, "vm.restart", "vm100", json!({})).await;
    assert_eq!(done.phase(), "no_response", "{}", done.text);
    assert_eq!(rig.proxmox.http.posts().len(), 1, "a reboot is never sent twice");

    harness.advance(10_001);
    rig.proxmox.apply.store(true, Ordering::SeqCst);
    let done = harness.critical(&cookie, &mut auth, "vm.restart", "vm100", json!({})).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
}

#[tokio::test]
async fn stopping_the_console_vm_needs_the_acknowledgement() {
    let (rig, harness, mut auth, cookie) = live().await;
    let refused = harness.prepare(&cookie, "vm.stop", "vm102", json!({})).await;
    refused.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "acknowledgement_required");
    assert!(refused.text.contains("offline"), "{}", refused.text);
    assert!(rig.proxmox.http.posts().is_empty());

    let done = harness.critical(&cookie, &mut auth, "vm.stop", "vm102", json!({ ACK_PARAM: true })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
}

#[tokio::test]
async fn actions_outside_the_adapter_scope_are_refused_before_anything_is_sent() {
    let (rig, harness, _auth, cookie) = live().await;
    for (action, target) in [("vm.force_stop", "vm101"), ("router.wireguard_restart", "router")] {
        let refused = harness.prepare(&cookie, action, target, json!({})).await;
        refused.assert_refused(StatusCode::CONFLICT, "device_unavailable");
    }
    assert!(rig.proxmox.http.requests().is_empty());
}

// ----- ranges

#[tokio::test]
async fn values_out_of_range_never_reach_a_device() {
    let (rig, harness, _auth, cookie) = live().await;
    for (action, target, params) in [
        ("light.brightness", "bulb1", json!({ "percent": 0 })),
        ("light.brightness", "bulb1", json!({ "percent": 101 })),
        ("light.hue", "bulb1", json!({ "hue": 360 })),
        ("speaker.volume", "sonos", json!({ "level": 101 })),
        ("speaker.volume", "sonos", json!({ "level": -1 })),
        ("tv.input", "tv", json!({ "input": "HDMI 9" })),
    ] {
        harness
            .execute(&cookie, action, target, params)
            .await
            .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_params");
    }
    harness
        .prepare(&cookie, "vm.start", "vm104", json!({}))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
    harness
        .execute(&cookie, "light.power", "bulb9", json!({ "on": true }))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
    assert_eq!(rig.traffic(), 0);
}

// ----- state, secrets and what the console sees

#[tokio::test]
async fn the_state_endpoint_reads_real_values_from_every_device() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.sonos.state.lock().unwrap().volume = 31;
    // A poll does not make the first connection to the TV.
    harness.execute(&cookie, "state.read", "tv", json!({})).await.assert_ok();
    let all = harness.get("/api/v1/state", &cookie).await;
    all.assert_ok();
    assert_eq!(all.body["live"], json!(true));
    assert_eq!(all.body["simulated"], json!(false));
    let devices = &all.body["devices"];
    for target in [
        "bulb1",
        "bulb2",
        "bulb3",
        "bulb4",
        "hue",
        "sonos",
        "sub",
        "tv",
        "printer",
        "vm100",
        "vm103",
        "host",
        "camera",
        "plex",
        "collector",
    ] {
        assert_eq!(devices[target]["available"], json!(true), "{target}: {}", devices[target]);
    }
    assert_eq!(devices["sonos"]["state"]["vol"], json!(31));
    assert_eq!(devices["bulb1"]["state"]["kind"], json!("light"));
    assert_eq!(devices["printer"]["state"]["state"], json!("idle"));
    assert_eq!(devices["vm100"]["state"]["state"], json!("running"));
    assert_eq!(devices["collector"]["state"]["data"]["pkg_temp_c"], json!(64));
    for target in ["feeder", "litter", "router"] {
        assert_eq!(devices[target]["available"], json!(false), "{target}");
    }
    assert!(devices["feeder"]["error"].as_str().unwrap().contains("Petkit login is stored"), "{}", devices["feeder"]);
}

#[tokio::test]
async fn devices_report_their_availability() {
    let (_rig, harness, _auth, cookie) = live().await;
    let devices = harness.get("/api/v1/devices", &cookie).await;
    let list = devices.body["devices"].as_array().unwrap();
    let find = |target: &str| list.iter().find(|d| d["target"] == json!(target)).unwrap().clone();
    for target in ["bulb1", "hue", "sonos", "sub", "tv", "printer", "vm100", "host", "camera", "plex", "collector"] {
        assert_eq!(find(target)["enabled"], json!(true), "{target}");
    }
    for target in ["feeder", "litter", "router"] {
        assert_eq!(find(target)["enabled"], json!(false), "{target}");
    }
}

#[tokio::test]
async fn no_device_secret_is_stored_or_returned() {
    let (_rig, harness, mut auth, cookie) = live().await;
    harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    harness.execute(&cookie, "tv.input", "tv", json!({ "input": "HDMI 2" })).await;
    let state = harness.get("/api/v1/state", &cookie).await;
    let vm = harness.critical(&cookie, &mut auth, "vm.stop", "vm101", json!({})).await;
    let audit = harness.get("/api/v1/audit", &cookie).await;
    for secret in ALL_SECRETS {
        for table in ["audit_log", "action_runs", "prepared_actions", "meta"] {
            assert!(!harness.dump(table).contains(secret), "{table} holds a device secret");
        }
        for reply in [&state, &vm, &audit] {
            assert!(!reply.text.contains(secret), "a response held a device secret");
        }
    }
}

// ----- reads report what the devices did

#[tokio::test]
async fn reading_silent_real_devices_is_no_response_and_shows_them_unavailable() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.bulbs[0].set_mode(BulbMode::Silent);
    rig.hue.hang.store(true, Ordering::SeqCst);
    rig.plex.hang.store(true, Ordering::SeqCst);
    rig.camera.hang.store(true, Ordering::SeqCst);

    for target in ["bulb1", "hue", "plex", "camera"] {
        let read = harness.execute(&cookie, "state.read", target, json!({})).await;
        read.assert_ok();
        assert_eq!(read.phase(), "no_response", "{target}: {}", read.text);
        assert_eq!(read.body["state"][target]["available"], json!(false), "{target}: {}", read.text);
        assert!(read.body["state"][target].get("state").is_none(), "{target}: {}", read.text);

        let state = harness.get(&format!("/api/v1/state/{target}"), &cookie).await;
        assert_eq!(state.body["available"], json!(false), "{target}: {}", state.text);
    }
    assert_eq!(harness.audit_count("no_response"), 4);
    assert!(!harness.audit_has("confirmed"), "{:?}", harness.audit_results());
}

#[tokio::test]
async fn one_silent_bulb_of_four_is_not_a_confirmed_read() {
    let (rig, harness, _auth, cookie) = live().await;
    rig.bulbs[2].set_mode(BulbMode::Silent);
    let read = harness.execute(&cookie, "state.read", "all_bulbs", json!({})).await;
    assert_eq!(read.phase(), "no_response", "{}", read.text);
    assert!(read.body["detail"].as_str().unwrap().contains("bulb3"), "{}", read.text);
    assert_eq!(read.body["state"]["bulb1"]["available"], json!(true));
    let steps = read.body["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 4);
    assert_eq!(steps[2]["phase"], json!("no_response"));
    assert_eq!(steps[0]["phase"], json!("confirmed"));
}

#[tokio::test]
async fn reading_a_device_with_no_local_control_is_failed() {
    let (_rig, harness, _auth, cookie) = live().await;
    let read = harness.execute(&cookie, "state.read", "feeder", json!({})).await;
    assert_eq!(read.phase(), "failed", "{}", read.text);
    assert_eq!(read.body["state"]["feeder"]["available"], json!(false));
    assert!(harness.audit_has("failed"));
    assert!(!harness.audit_has("confirmed"));
}

#[tokio::test]
async fn reading_every_device_that_answers_is_confirmed() {
    let (_rig, harness, _auth, cookie) = live().await;
    for target in ["bulb1", "hue", "plex", "camera", "sonos", "printer", "vm100", "collector", "all_bulbs"] {
        let read = harness.execute(&cookie, "state.read", target, json!({})).await;
        assert_eq!(read.phase(), "confirmed", "{target}: {}", read.text);
        assert_eq!(read.body["detail"], Value::Null, "{target}");
    }
}

// ----- nothing configured

#[tokio::test]
async fn live_with_no_device_configuration_contacts_nothing_and_says_why() {
    // No WiZ or Sonos instance at all, and every other one without its credential.
    let rig = Rig::start().await;
    let harness = Harness::with_site(|site| {
        site.live = true;
        rig.configure(site);
        site.remove("wiz");
        site.remove("sonos");
        site.secrets.clear();
    });
    let (_auth, cookie) = harness.sign_in().await;

    for (action, target, params) in
        [("light.power", "bulb1", json!({ "on": true })), ("speaker.volume", "sonos", json!({ "level": 5 }))]
    {
        let refused = harness.execute(&cookie, action, target, params).await;
        refused.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
    }
    let refused = harness.execute(&cookie, "tv.input", "tv", json!({ "input": "HDMI 1" })).await;
    refused.assert_refused(StatusCode::CONFLICT, "device_unavailable");
    assert!(refused.text.contains("not paired"), "{}", refused.text);
    let refused = harness.prepare(&cookie, "vm.start", "vm100", json!({})).await;
    refused.assert_refused(StatusCode::CONFLICT, "device_unavailable");
    assert!(refused.text.contains("API token"), "{}", refused.text);

    let state = harness.get("/api/v1/state/hue", &cookie).await;
    assert!(state.body["error"].as_str().unwrap().contains("not paired"), "{}", state.text);
    let state = harness.get("/api/v1/state/camera", &cookie).await;
    assert!(state.body["error"].as_str().unwrap().contains("login"), "{}", state.text);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.traffic(), 0);
}

#[tokio::test]
async fn one_bad_credential_disables_only_its_own_device() {
    let rig = Rig::start().await;
    let harness = Harness::with_site(|site| {
        site.live = true;
        site.timing.confirm_timeout_ms = Some(1_200);
        rig.configure(site);
        site.secret("proxmox", "token", "not a token at all");
    });
    let mut auth = authenticator();
    harness.enroll(&mut auth, "iPhone").await;
    let cookie = harness.login(&mut auth).await;

    harness
        .prepare(&cookie, "vm.start", "vm100", json!({}))
        .await
        .assert_refused(StatusCode::CONFLICT, "device_unavailable");
    let done = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(rig.proxmox.http.traffic(), 0);
}

// ----- voice through the real adapters

async fn voice() -> (FakeSpeech, Rig, Harness, SoftPasskey, String) {
    let speech = FakeSpeech::start().await;
    let (rig, harness, auth, cookie) = live_with_speech(Some(&speech)).await;
    (speech, rig, harness, auth, cookie)
}

async fn say(harness: &Harness, cookie: &str, speech: &FakeSpeech, text: &str) -> Value {
    speech.say(text);
    let response = post_voice(harness, cookie).await;
    response.assert_ok();
    response.body
}

#[tokio::test]
async fn a_misheard_wiz_command_reaches_the_bulb() {
    let (speech, rig, harness, _auth, cookie) = voice().await;
    let reply = say(&harness, &cookie, &speech, "Whiz bulb one off").await;
    assert_eq!(reply["kind"], json!("executed"));
    assert_eq!(reply["run"]["phase"], json!("confirmed"), "{reply}");
    assert_eq!(reply["run"]["channel"], json!("voice"));
    assert!(!rig.bulbs[0].state.lock().unwrap().on);
}

#[tokio::test]
async fn the_voice_reply_kind_follows_what_the_device_did() {
    let (speech, rig, harness, _auth, cookie) = voice().await;
    rig.bulbs[0].set_mode(BulbMode::Silent);
    let reply = say(&harness, &cookie, &speech, "bulb one off").await;
    assert_eq!(reply["kind"], json!("no_response"), "{reply}");
    assert_eq!(reply["phase"], json!("no_response"), "{reply}");
    assert_eq!(reply["run"]["phase"], json!("no_response"), "{reply}");

    rig.bulbs[1].set_mode(BulbMode::Error);
    let reply = say(&harness, &cookie, &speech, "bulb two off").await;
    assert_eq!(reply["kind"], json!("failed"), "{reply}");
    assert_eq!(reply["phase"], json!("failed"), "{reply}");
}

#[tokio::test]
async fn nothing_negated_or_ambiguous_reaches_a_device_by_voice() {
    let (speech, rig, harness, _auth, cookie) = voice().await;
    for text in [
        "do not turn off the lights",
        "turn the lights on or off",
        "turn off bulb one and bulb two",
        "never mute the volume",
        "mute or unmute",
        "set the volume to twenty or thirty",
        "stop alpha and charlie",
    ] {
        let reply = say(&harness, &cookie, &speech, text).await;
        assert_eq!(reply["kind"], json!("choices"), "{text}: {reply}");
    }
    assert_eq!(rig.traffic(), 0);
    assert!(!harness.audit_has("commanded"));
    assert!(!harness.audit_has("prepared"));
}

#[tokio::test]
async fn tier_0_by_voice_reads_a_real_device() {
    let (speech, rig, harness, _auth, cookie) = voice().await;
    let reply = say(&harness, &cookie, &speech, "clipper status").await;
    assert_eq!(reply["kind"], json!("executed"));
    assert_eq!(reply["run"]["state"]["printer"]["state"]["state"], json!("idle"), "{reply}");
    assert!(rig.moonraker.http.posts().is_empty());
}

#[tokio::test]
async fn relative_volume_uses_the_speaker_s_real_value() {
    let (speech, rig, harness, _auth, cookie) = voice().await;
    rig.sonos.state.lock().unwrap().volume = 50;
    let reply = say(&harness, &cookie, &speech, "louder").await;
    assert_eq!(reply["run"]["params"], json!({ "level": 55 }), "{reply}");
    assert_eq!(rig.sonos.state.lock().unwrap().volume, 55);
}

#[tokio::test]
async fn tier_2_by_voice_sends_nothing_until_the_tap() {
    let (speech, rig, harness, _auth, cookie) = voice().await;
    let reply = say(&harness, &cookie, &speech, "pause clipper").await;
    assert_eq!(reply["kind"], json!("prepared"));
    assert_eq!(reply["needs"], json!("tap"));
    assert!(rig.moonraker.http.posts().is_empty());

    let prepared = Resp {
        status: StatusCode::OK,
        headers: Default::default(),
        body: reply["prepared"].clone(),
        text: String::new(),
    };
    rig.moonraker.state.lock().unwrap().print = "printing".to_string();
    let done = harness.confirm_tap(&cookie, &prepared).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(done.body["channel"], json!("voice"));
    assert_eq!(rig.moonraker.http.posts().len(), 1);
}

#[tokio::test]
async fn tier_3_by_voice_is_only_prepared_and_needs_the_passkey() {
    let (speech, rig, harness, mut auth, cookie) = voice().await;
    let reply = say(&harness, &cookie, &speech, "stop bravo").await;
    assert_eq!(reply["kind"], json!("prepared"));
    assert_eq!(reply["needs"], json!("passkey"));
    assert!(reply["prepared"]["confirmation"].is_null());
    assert!(rig.proxmox.http.requests().is_empty());

    let spoken = say(&harness, &cookie, &speech, "yes confirm").await;
    assert_eq!(spoken["kind"], json!("ignored"));
    assert!(rig.proxmox.http.requests().is_empty());

    let prepared = Resp {
        status: StatusCode::OK,
        headers: Default::default(),
        body: reply["prepared"].clone(),
        text: String::new(),
    };
    let started = harness.step_up_start(&cookie, &prepared).await;
    let assertion = assert_with(&mut auth, &started.body["options"], origin_url());
    let done = harness.confirm_assertion(&cookie, &prepared, &assertion).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    assert_eq!(rig.proxmox.http.posts().len(), 1);
}

#[tokio::test]
async fn voice_gets_a_clear_refusal_for_the_feeder() {
    let (speech, rig, harness, _auth, cookie) = voice().await;
    let reply = say(&harness, &cookie, &speech, "feed with the pet feeder").await;
    assert_eq!(reply["kind"], json!("refused"));
    assert_eq!(reply["code"], json!("device_unavailable"));
    assert!(reply["why"].as_str().unwrap().contains("Petkit login is stored"), "{reply}");
    assert_eq!(rig.traffic(), 0);
}

#[tokio::test]
async fn ambiguity_and_low_confidence_reach_no_device() {
    let (speech, rig, harness, _auth, cookie) = voice().await;
    for text in ["turn off the light", "stop the vm", "mumble mumble mute garble", "banana phone"] {
        let reply = say(&harness, &cookie, &speech, text).await;
        assert_eq!(reply["kind"], json!("choices"), "{text}: {reply}");
    }
    assert_eq!(rig.traffic(), 0);
}

#[tokio::test]
async fn the_emergency_stop_reaches_the_printer_by_voice() {
    let (speech, rig, harness, _auth, cookie) = voice().await;
    let reply = say(&harness, &cookie, &speech, "emergency stop").await;
    assert_eq!(reply["label"], json!("Emergency stop"), "{reply}");
    assert_eq!(reply["run"]["channel"], json!("voice"), "{reply}");
    assert!(rig.traffic() > 0, "the printer was never asked");
}
