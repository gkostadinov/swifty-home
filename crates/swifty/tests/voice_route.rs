//! The voice route against a stand-in speech service bound to loopback.

mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use common::*;
use serde_json::{json, Value};
use swifty_plugin_api::Op;
use swifty_plugin_mock::Behavior;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;

async fn setup() -> (Harness, SoftPasskey, String, FakeSpeech) {
    let speech = FakeSpeech::start().await;
    let config = speech.config();
    let (harness, auth, cookie) = Harness::signed_in_with(move |c| c.speech = Some(config)).await;
    (harness, auth, cookie, speech)
}

async fn say(harness: &Harness, cookie: &str, speech: &FakeSpeech, text: &str) -> Value {
    speech.say(text);
    let response = post_voice(harness, cookie).await;
    response.assert_ok();
    response.body
}

#[tokio::test]
async fn audio_is_forwarded_with_the_bearer_token_from_the_file() {
    let (harness, _auth, cookie, speech) = setup().await;
    say(&harness, &cookie, &speech, "bulb 1 off").await;
    let log = speech.log.lock().unwrap();
    assert_eq!(log.calls, 1);
    assert_eq!(log.authorization.as_deref(), Some(format!("Bearer {SPEECH_TOKEN}").as_str()));
    assert_eq!(log.content_type.as_deref(), Some("audio/wav"));
    assert_eq!(log.body_len, FAKE_WAV.len());
}

#[tokio::test]
async fn tier_1_executes_and_the_run_is_marked_voice() {
    let (harness, _auth, cookie, speech) = setup().await;
    let reply = say(&harness, &cookie, &speech, "Set the volume to eighty").await;
    assert_eq!(reply["kind"], json!("executed"));
    assert_eq!(reply["tier"], json!(1));
    assert_eq!(reply["run"]["phase"], json!("confirmed"));
    assert_eq!(reply["run"]["channel"], json!("voice"));
    assert_eq!(harness.devices.commands(), vec![("sonos".to_string(), Op::Volume { level: 80 })]);
}

#[tokio::test]
async fn tier_0_executes() {
    let (harness, _auth, cookie, speech) = setup().await;
    let reply = say(&harness, &cookie, &speech, "What is the CPU temperature?").await;
    assert_eq!(reply["kind"], json!("executed"));
    assert_eq!(reply["tier"], json!(0));
    assert_eq!(reply["run"]["state"]["metric"], json!("cpu_temp"));
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn tier_2_is_only_prepared_until_someone_taps() {
    let (harness, _auth, cookie, speech) = setup().await;
    let reply = say(&harness, &cookie, &speech, "feed the pets twenty grams").await;
    assert_eq!(reply["kind"], json!("prepared"));
    assert_eq!(reply["needs"], json!("tap"));
    assert_eq!(reply["prepared"]["params"], json!({ "grams": 20 }));
    assert_eq!(harness.devices.total_commands(), 0);

    let prepared = Resp {
        status: StatusCode::OK,
        headers: Default::default(),
        body: reply["prepared"].clone(),
        text: String::new(),
    };
    let tapped = harness.confirm_tap(&cookie, &prepared).await;
    tapped.assert_ok();
    assert_eq!(tapped.phase(), "confirmed");
    assert_eq!(tapped.body["channel"], json!("voice"));
    assert_eq!(harness.devices.command_count("feeder"), 1);
}

#[tokio::test]
async fn tier_3_can_only_be_prepared_and_needs_a_real_passkey() {
    let (harness, mut auth, cookie, speech) = setup().await;
    let reply = say(&harness, &cookie, &speech, "stop bravo").await;
    assert_eq!(reply["kind"], json!("prepared"));
    assert_eq!(reply["needs"], json!("passkey"));
    assert_eq!(reply["tier"], json!(3));
    assert!(reply["prepared"]["confirmation"].is_null());
    assert_eq!(harness.devices.total_commands(), 0);

    let prepared = Resp {
        status: StatusCode::OK,
        headers: Default::default(),
        body: reply["prepared"].clone(),
        text: String::new(),
    };

    // Saying yes does nothing.
    let spoken = say(&harness, &cookie, &speech, "yes confirm").await;
    assert_eq!(spoken["kind"], json!("ignored"));
    assert_eq!(harness.devices.total_commands(), 0);

    // No assertion, no action.
    let bare = harness
        .post(
            "/api/v1/actions/confirm",
            &cookie,
            json!({
                "prepared_id": prepared.body["prepared_id"],
                "action": prepared.body["action"],
                "target": prepared.body["target"],
                "params": prepared.body["params"],
            }),
        )
        .await;
    bare.assert_refused(StatusCode::PRECONDITION_REQUIRED, "step_up_required");

    let started = harness.step_up_start(&cookie, &prepared).await;
    let assertion = assert_with(&mut auth, &started.body["options"], origin_url());
    let done = harness.confirm_assertion(&cookie, &prepared, &assertion).await;
    done.assert_ok();
    assert_eq!(done.body["channel"], json!("voice"));
    assert_eq!(done.body["step_up"], json!("passkey"));
    assert_eq!(harness.devices.command_count("vm101"), 1);
}

#[tokio::test]
async fn voice_can_trigger_the_emergency_stop_by_its_name() {
    let (harness, _auth, cookie, speech) = setup().await;
    let reply = say(&harness, &cookie, &speech, "emergency stop").await;
    assert_eq!(reply["kind"], json!("executed"), "{reply}");
    assert_eq!(reply["run"]["channel"], json!("voice"));
    assert_eq!(harness.devices.command_count("printer"), 1);

    let reply = say(&harness, &cookie, &speech, "stop the printer").await;
    assert_eq!(reply["kind"], json!("nothing"), "a plain stop is never the emergency stop: {reply}");
    assert_eq!(harness.devices.command_count("printer"), 1);
}

#[tokio::test]
async fn low_confidence_returns_choices_and_runs_nothing() {
    let (harness, _auth, cookie, speech) = setup().await;
    let reply = say(&harness, &cookie, &speech, "mumble mumble mute garble").await;
    assert_eq!(reply["kind"], json!("choices"));
    assert!(reply["why"].as_str().unwrap().contains("confident"));
    assert_eq!(reply["choices"][0]["action"], json!("speaker.mute"));
    assert_eq!(reply["choices"][0]["tier"], json!(1));
    assert_eq!(harness.devices.total_commands(), 0);
    assert!(harness.audit_has("choices: nothing ran"));
}

#[tokio::test]
async fn ambiguity_returns_choices_and_runs_nothing() {
    let (harness, _auth, cookie, speech) = setup().await;
    let reply = say(&harness, &cookie, &speech, "turn off the light").await;
    assert_eq!(reply["kind"], json!("choices"));
    assert_eq!(reply["choices"].as_array().unwrap().len(), 5);

    let reply = say(&harness, &cookie, &speech, "stop the vm").await;
    assert_eq!(reply["kind"], json!("choices"));
    assert_eq!(reply["choices"][0]["tier"], json!(3));

    let reply = say(&harness, &cookie, &speech, "dim the lights").await;
    assert_eq!(reply["kind"], json!("choices"));
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn known_mishearings_work_through_the_whole_route() {
    let (harness, _auth, cookie, speech) = setup().await;
    let reply = say(&harness, &cookie, &speech, "Whiz bulb one off").await;
    assert_eq!(reply["run"]["action"], json!("light.power"));
    assert_eq!(reply["run"]["target"], json!("bulb1"));

    let reply = say(&harness, &cookie, &speech, "LGTV HDMI two").await;
    assert_eq!(reply["run"]["action"], json!("tv.input"));
    assert_eq!(reply["run"]["params"], json!({ "input": "HDMI 2" }));

    let reply = say(&harness, &cookie, &speech, "pause clipper").await;
    assert_eq!(reply["kind"], json!("prepared"));
    assert_eq!(reply["prepared"]["action"], json!("print.pause"));

    let reply = say(&harness, &cookie, &speech, "feed with the pet feeder").await;
    assert_eq!(reply["prepared"]["action"], json!("pets.feed"));

    let reply = say(&harness, &cookie, &speech, "blue tooth delay").await;
    assert_eq!(reply["run"]["state"]["metric"], json!("bluetooth"));

    let reply = say(&harness, &cookie, &speech, "restart bravo").await;
    assert_eq!(reply["prepared"]["target"], json!("vm101"));
}

#[tokio::test]
async fn a_renamed_vm_answers_to_its_new_name_and_not_its_old_one() {
    let (harness, _auth, cookie, speech) = setup().await;
    harness
        .send(
            Method::PUT,
            "/api/v1/devices/vm101/name",
            Some(&cookie),
            Some(ORIGIN),
            Some(json!({ "name": "Archive" })),
        )
        .await
        .assert_ok();
    let reply = say(&harness, &cookie, &speech, "restart the archive").await;
    assert_eq!(reply["kind"], json!("prepared"), "{reply}");
    assert_eq!(reply["prepared"]["action"], json!("vm.restart"));
    assert_eq!(reply["prepared"]["target"], json!("vm101"));

    let reply = say(&harness, &cookie, &speech, "restart bravo").await;
    assert_ne!(reply["kind"], json!("prepared"), "the old name still reached it: {reply}");
    assert!(reply["choices"].as_array().is_none_or(|choices| choices.len() != 1), "{reply}");
    // Its target still names it.
    let reply = say(&harness, &cookie, &speech, "restart vm 101").await;
    assert_eq!(reply["prepared"]["target"], json!("vm101"), "{reply}");
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn a_light_added_by_name_is_reached_by_its_name() {
    let (harness, _auth, cookie, speech) = setup().await;
    harness
        .send(
            Method::PUT,
            "/api/v1/devices/bulb3/name",
            Some(&cookie),
            Some(ORIGIN),
            Some(json!({ "name": "Desk lamp" })),
        )
        .await
        .assert_ok();
    let reply = say(&harness, &cookie, &speech, "turn off the desk lamp").await;
    assert_eq!(reply["kind"], json!("executed"), "{reply}");
    assert_eq!(reply["label"], json!("Turn off Desk lamp"));
    assert_eq!(harness.devices.commands(), vec![("bulb3".to_string(), Op::LightPower { on: false })]);
}

#[tokio::test]
async fn a_value_out_of_range_runs_nothing() {
    let (harness, _auth, cookie, speech) = setup().await;
    let reply = say(&harness, &cookie, &speech, "volume 150").await;
    assert_eq!(reply["kind"], json!("refused"));
    assert_eq!(reply["code"], json!("invalid_params"));
    let reply = say(&harness, &cookie, &speech, "feed 500 grams").await;
    assert_eq!(reply["kind"], json!("refused"));
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn relative_volume_reads_the_device_first() {
    let (harness, _auth, cookie, speech) = setup().await;
    let reply = say(&harness, &cookie, &speech, "louder").await;
    assert_eq!(reply["run"]["params"], json!({ "level": 29 }));
    let reply = say(&harness, &cookie, &speech, "quieter").await;
    assert_eq!(reply["run"]["params"], json!({ "level": 24 }));
}

#[tokio::test]
async fn relative_volume_runs_nothing_when_the_volume_is_unknown() {
    let (harness, _auth, cookie, speech) = setup().await;
    harness.devices.set_behavior("sonos", Behavior::Silent);
    let reply = say(&harness, &cookie, &speech, "louder").await;
    assert_eq!(reply["kind"], json!("nothing"));
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn stopping_the_console_by_voice_asks_for_the_acknowledgement() {
    let (harness, _auth, cookie, speech) = setup().await;
    let reply = say(&harness, &cookie, &speech, "stop charlie").await;
    assert_eq!(reply["kind"], json!("acknowledgement_required"));
    assert!(reply["warning"].as_str().unwrap().contains("console"));
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn family_is_refused_the_console_vm_like_any_other_computer() {
    let (harness, mut owner, cookie, speech) = setup().await;
    let (_family, family_cookie, _id) = harness.add_family(&cookie, &mut owner, "Alice").await;
    for text in ["stop charlie", "stop alpha"] {
        let reply = say(&harness, &family_cookie, &speech, text).await;
        assert_eq!(reply["kind"], json!("refused"), "{reply}");
        assert_eq!(reply["code"], json!("owner_only"), "{reply}");
        assert!(reply.get("warning").is_none(), "{reply}");
        assert!(reply.get("target").is_none(), "{reply}");
    }
    assert_eq!(harness.audit_count("refused: owner_only"), 2);
    assert!(!harness.audit_has("refused: acknowledgement_required"));
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn a_lock_by_voice_locks() {
    let (harness, _auth, cookie, speech) = setup().await;
    let reply = say(&harness, &cookie, &speech, "lock").await;
    assert_eq!(reply["kind"], json!("locked"));
    harness.get("/api/v1/catalog", &cookie).await.assert_refused(StatusCode::LOCKED, "session_locked");
}

#[tokio::test]
async fn navigation_and_help_run_no_action() {
    let (harness, _auth, cookie, speech) = setup().await;
    assert_eq!(say(&harness, &cookie, &speech, "go to monitor").await["page"], json!("monitor"));
    assert_eq!(say(&harness, &cookie, &speech, "help").await["kind"], json!("help"));
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn audio_and_transcripts_are_never_stored() {
    let (harness, _auth, cookie, speech) = setup().await;
    const PHRASES: &[(&str, &str)] = &[
        ("whiz bulb one off with extra words nobody should keep", "nobody"),
        ("banana phone yellow submarine", "submarine"),
        ("set the volume to eighty please", "eighty"),
    ];
    for (phrase, _) in PHRASES {
        speech.say(phrase);
        post_voice(&harness, &cookie).await.assert_ok();
    }
    for table in ["audit_log", "action_runs", "prepared_actions", "ceremonies", "sessions", "meta"] {
        let dump = harness.dump(table);
        for (phrase, marker) in PHRASES {
            assert!(!dump.contains(phrase), "{table} kept a transcript");
            assert!(!dump.contains(marker), "{table} kept part of a transcript");
        }
        assert!(!dump.contains("fake-audio-bytes"), "{table} kept audio");
        assert!(!dump.contains(SPEECH_TOKEN), "{table} kept the speech token");
    }
    // The action itself is recorded.
    let log = harness.dump("audit_log");
    assert!(log.contains("speaker.volume"));
    assert!(log.contains("voice"));
}

#[tokio::test]
async fn the_voice_route_is_guarded_like_any_other() {
    let (harness, _auth, cookie, speech) = setup().await;
    speech.say("bulb 1 off");

    let request = |origin: &str, cookie: Option<&str>, content_type: &str| {
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/voice")
            .header(header::ORIGIN, origin)
            .header(header::CONTENT_TYPE, content_type);
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        builder.body(Body::from(FAKE_WAV.to_vec())).unwrap()
    };

    let wrong_origin = harness.dispatch(request(EVIL_ORIGIN, Some(&cookie), "audio/wav")).await;
    wrong_origin.assert_refused(StatusCode::FORBIDDEN, "bad_origin");
    let no_session = harness.dispatch(request(ORIGIN, None, "audio/wav")).await;
    no_session.assert_refused(StatusCode::UNAUTHORIZED, "not_signed_in");
    let wrong_type = harness.dispatch(request(ORIGIN, Some(&cookie), "text/plain")).await;
    wrong_type.assert_refused(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type");
    assert_eq!(speech.calls(), 0, "nothing should reach the speech service");

    let empty = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/voice")
        .header(header::ORIGIN, ORIGIN)
        .header(header::COOKIE, &cookie)
        .header(header::CONTENT_TYPE, "audio/wav")
        .body(Body::empty())
        .unwrap();
    harness.dispatch(empty).await.assert_refused(StatusCode::BAD_REQUEST, "empty_audio");
}

fn body_request(body: Vec<u8>, cookie: &str) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/api/v1/voice")
        .header(header::ORIGIN, ORIGIN)
        .header(header::COOKIE, cookie)
        .header(header::CONTENT_TYPE, "audio/wav")
        .body(Body::from(body))
        .unwrap()
}

fn wav_of(size: usize, cookie: &str) -> Request<Body> {
    let mut body = vec![0u8; size];
    body[..12].copy_from_slice(b"RIFF\0\0\0\0WAVE");
    body_request(body, cookie)
}

#[tokio::test]
async fn bytes_that_are_not_a_wav_never_reach_the_speech_service() {
    let (harness, _auth, cookie, speech) = setup().await;
    speech.say("bulb 1 off");
    for body in [
        b"this is not a wav file at all".to_vec(),
        b"RIFF".to_vec(),
        b"RIFF\0\0\0\0AVI LIST".to_vec(),
        b"\0\0\0\0\0\0\0\0WAVE".to_vec(),
    ] {
        let refused = harness.dispatch(body_request(body, &cookie)).await;
        refused.assert_refused(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type");
    }
    assert_eq!(speech.calls(), 0);
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn negated_and_ambiguous_speech_runs_nothing() {
    let (harness, _auth, cookie, speech) = setup().await;
    for (text, count) in [
        ("do not turn off the lights", 1),
        ("turn the lights on or off", 2),
        ("turn off bulb one and bulb two", 2),
        ("never mute the volume", 1),
        ("mute or unmute", 2),
        ("set the volume to twenty or thirty", 2),
        ("stop alpha and charlie", 2),
    ] {
        let reply = say(&harness, &cookie, &speech, text).await;
        assert_eq!(reply["kind"], json!("choices"), "{text}: {reply}");
        assert_eq!(reply["choices"].as_array().unwrap().len(), count, "{text}: {reply}");
    }
    assert_eq!(harness.devices.total_commands(), 0);
    assert_eq!(harness.devices.total_reads(), 0);
    assert!(!harness.audit_has("prepared"));
}

#[tokio::test]
async fn the_voice_reply_kind_follows_the_run() {
    let (harness, _auth, cookie, speech) = setup().await;
    let reply = say(&harness, &cookie, &speech, "bulb 1 off").await;
    assert_eq!(reply["kind"], json!("executed"), "{reply}");
    assert_eq!(reply["phase"], json!("confirmed"), "{reply}");

    harness.devices.set_behavior("bulb2", Behavior::Silent);
    let reply = say(&harness, &cookie, &speech, "bulb 2 off").await;
    assert_eq!(reply["kind"], json!("no_response"), "{reply}");

    harness.devices.set_behavior("bulb3", Behavior::Rejects("no".into()));
    let reply = say(&harness, &cookie, &speech, "bulb 3 on").await;
    assert_eq!(reply["kind"], json!("failed"), "{reply}");

    let reply = say(&harness, &cookie, &speech, "bulb 2").await;
    assert_eq!(reply["kind"], json!("no_response"), "a silent read is not a success: {reply}");
}

#[tokio::test]
async fn voice_takes_a_real_recording_but_has_a_limit() {
    let (harness, _auth, cookie, speech) = setup().await;
    speech.say("help");
    // Well past the JSON limit, and within what the speech service accepts.
    harness.dispatch(wav_of(1024 * 1024, &cookie)).await.assert_ok();
    assert_eq!(speech.log.lock().unwrap().body_len, 1024 * 1024);

    let oversized = harness.dispatch(wav_of(8 * 1024 * 1024 + 1, &cookie)).await;
    assert_eq!(oversized.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(speech.calls(), 1);
}

#[tokio::test]
async fn a_speech_failure_runs_nothing() {
    let (harness, _auth, cookie, speech) = setup().await;
    speech.task.abort();
    while !speech.task.is_finished() {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    speech.say("bulb 1 off");
    let response = post_voice(&harness, &cookie).await;
    response.assert_refused(StatusCode::BAD_GATEWAY, "speech_unavailable");
    assert_eq!(harness.devices.total_commands(), 0);
}

#[tokio::test]
async fn voice_without_a_speech_service_is_refused() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let response = post_voice(&harness, &cookie).await;
    response.assert_refused(StatusCode::SERVICE_UNAVAILABLE, "speech_not_configured");
}
