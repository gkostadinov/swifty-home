//! The microphone's two ways into Swifty: the words shown while talking, and a sentence
//! that starts with its name. Neither may run anything by itself. And the code behind
//! them names no particular house.

mod common;

use common::devices::write_secret;
use common::*;
use serde_json::json;
use swifty_hub::config::AssistantConfig;
use tempfile::TempDir;

async fn with_speech(speech: &FakeSpeech, assistant: bool) -> (Harness, String, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = assistant.then(|| AssistantConfig {
        urls: vec!["http://127.0.0.1:9/v1".to_string()],
        model: "test-model".to_string(),
        key_file: write_secret(dir.path(), "ai.key", "model-key-for-tests-0123456789"),
        timeout_secs: 10,
        weather: None,
        agent: None,
    });
    let speech = speech.config();
    let (harness, _auth, cookie) = Harness::signed_in_with(move |c| {
        c.live = true;
        c.speech = Some(speech);
        c.assistant = config;
    })
    .await;
    (harness, cookie, dir)
}

#[tokio::test]
async fn the_words_are_shown_and_nothing_runs() {
    let speech = FakeSpeech::start().await;
    let (harness, cookie, _dir) = with_speech(&speech, true).await;
    speech.say("turn off all the lights");
    let reply = post_audio(&harness, &cookie, "/api/v1/voice?words=true").await;
    reply.assert_ok();
    assert_eq!(reply.body, json!({ "text": "turn off all the lights", "to_swifty": false }));
    assert_eq!(harness.devices.command_count("bulb1"), 0);
    assert!(!harness.dump("audit_log").contains("turn off"), "what was said is not written down");
}

#[tokio::test]
async fn the_name_is_noticed_while_talking() {
    let speech = FakeSpeech::start().await;
    let (harness, cookie, _dir) = with_speech(&speech, true).await;
    speech.say("Swifty, what is the time");
    let reply = post_audio(&harness, &cookie, "/api/v1/voice?words=true").await;
    assert_eq!(reply.body, json!({ "text": "what is the time", "to_swifty": true }));
}

#[tokio::test]
async fn a_sentence_to_swifty_goes_to_swifty_and_runs_nothing_here() {
    let speech = FakeSpeech::start().await;
    let (harness, cookie, _dir) = with_speech(&speech, true).await;
    speech.say("hey swifty turn off all the lights");
    let reply = post_voice(&harness, &cookie).await;
    reply.assert_ok();
    assert_eq!(reply.body["kind"], json!("swifty"), "{}", reply.text);
    assert_eq!(reply.body["ask"], json!("turn off all the lights"));
    assert_eq!(harness.devices.command_count("bulb1"), 0);
}

#[tokio::test]
async fn a_sentence_about_swiftys_memory_goes_to_swifty_and_runs_nothing() {
    let speech = FakeSpeech::start().await;
    let (harness, cookie, _dir) = with_speech(&speech, true).await;
    speech.say("remember that all the lights go off at 11");
    let reply = post_voice(&harness, &cookie).await;
    reply.assert_ok();
    assert_eq!(reply.body["kind"], json!("swifty"), "{}", reply.text);
    assert_eq!(reply.body["ask"], json!("remember that all the lights go off at 11"));
    assert_eq!(harness.devices.command_count("bulb1"), 0, "a memory is not a command");
}

#[tokio::test]
async fn without_swifty_the_name_is_just_ignored() {
    let speech = FakeSpeech::start().await;
    let (harness, cookie, _dir) = with_speech(&speech, false).await;
    speech.say("swifty turn off all the lights");
    let reply = post_voice(&harness, &cookie).await;
    reply.assert_ok();
    assert_eq!(reply.body["kind"], json!("executed"), "{}", reply.text);
}

#[tokio::test]
async fn the_words_need_a_signed_in_person() {
    let speech = FakeSpeech::start().await;
    let (harness, _cookie, _dir) = with_speech(&speech, true).await;
    let reply = post_audio(&harness, "", "/api/v1/voice?words=true").await;
    assert_eq!(reply.status, axum::http::StatusCode::UNAUTHORIZED, "{}", reply.text);
    assert_eq!(speech.calls(), 0);
}

/// SHA-256 of words that name one particular house: its host, machines, printer and
/// gear, and a private project. Kept as hashes, because a list of the words would be
/// the very thing it guards against. A machine name that is also a common word or a
/// Rust method is left out, since the code may well use it.
const SITE_WORD_HASHES: &[&str] = &[
    "934a11e602682e862b4b4d1f881ae8da1e097e3f6964bd780e5bb1fb798a6fdd",
    "6ff9770cf6986977f8ace21d8d41c7f3e503a547ab54e28c392fda3ef8a375f7",
    "047de0f543c7e3596bb6dbd0e65f2c30357720a63eca423f08c52fae314f6806",
    "751abe89e28f7eaf9f4115705bc9f6a5cc695258ec70d59d7ac9475ee345eb5b",
    "3e2c9e4840a6855396c6406a861b9b22aad6400e19864b105fcde43791f8ae85",
    "7804a56a5c7636cc05814736f44139e32920810d3bd51aa099a5df932e754ce9",
    "a0c0485eff9c817deee7190edf264622b1677fc992920657411b36a6aed48f83",
    "82ba7e1f6f4171d3648bb4f785ca31992efe32df6e024acb5bccd297feb2f263",
    "dde7e12aa0621bda49525bba98858d5908387df852f06f435c03f4e8e45fcde5",
    "e0ec026a6108385788ac74b5fd4557ca178e0f48f5c6f7b1d50fa34cbf48d5dd",
];

#[test]
fn voice_and_swifty_name_no_house() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../swifty-hub/src");
    let mut files = vec![src.join("voice.rs"), src.join("assistant.rs")];
    for entry in std::fs::read_dir(src.join("voice")).unwrap() {
        files.push(entry.unwrap().path());
    }
    assert!(files.len() >= 3, "{files:?}");
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap().to_lowercase();
        for (number, line) in text.lines().enumerate() {
            for word in line.split(|c: char| !c.is_ascii_alphanumeric()).filter(|word| !word.is_empty()) {
                let hash = swifty_hub::crypto::sha256_hex(word.as_bytes());
                assert!(
                    !SITE_WORD_HASHES.contains(&hash.as_str()),
                    "{}:{} names something of one particular house",
                    file.display(),
                    number + 1
                );
            }
        }
    }
}
