//! Swifty against a stand-in model, weather service and search API. What matters most
//! here is what the model cannot do: run anything off its list, skip a confirmation,
//! or act on something it read on the web.

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::http::{header, HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use common::devices::write_secret;
use common::*;
use serde_json::{json, Value};
use swifty_hub::config::{AssistantConfig, WeatherConfig};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;

const MODEL_KEY: &str = "model-key-for-tests-0123456789";

#[derive(Default)]
struct Log {
    asked: Vec<Value>,
    authorization: Option<String>,
    forecasts: usize,
}

struct Stand {
    url: String,
    log: Arc<Mutex<Log>>,
    replies: Arc<Mutex<VecDeque<String>>>,
    status: Arc<Mutex<StatusCode>>,
    dir: TempDir,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Stand {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Stand {
    async fn start() -> Self {
        let log = Arc::new(Mutex::new(Log::default()));
        let replies = Arc::new(Mutex::new(VecDeque::new()));
        let status = Arc::new(Mutex::new(StatusCode::OK));
        let (l1, r1, s1) = (log.clone(), replies.clone(), status.clone());
        let l2 = log.clone();
        let router = Router::new()
            .route(
                "/v1/chat/completions",
                post(move |headers: HeaderMap, Json(body): Json<Value>| {
                    let (log, replies, status) = (l1.clone(), r1.clone(), s1.clone());
                    async move {
                        {
                            let mut log = log.lock().unwrap();
                            log.asked.push(body);
                            log.authorization = headers.get(header::AUTHORIZATION).map(|v| v.to_str().unwrap().to_string());
                        }
                        let content = replies.lock().unwrap().pop_front().unwrap_or_else(|| "Nothing more to say.".to_string());
                        (*status.lock().unwrap(), Json(json!({ "choices": [{ "message": { "role": "assistant", "content": content } }] })))
                    }
                }),
            )
            .route(
                "/v1/forecast",
                get(move || {
                    let log = l2.clone();
                    async move {
                        log.lock().unwrap().forecasts += 1;
                        Json(json!({
                            "current": { "temperature_2m": 7.0, "weather_code": 3 },
                            "daily": {
                                "time": ["2026-11-10"], "weather_code": [61], "temperature_2m_min": [2.0],
                                "temperature_2m_max": [9.0], "precipitation_probability_max": [80], "wind_speed_10m_max": [20.0]
                            }
                        }))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { url, log, replies, status, dir: tempfile::tempdir().unwrap(), task }
    }

    fn config(&self) -> AssistantConfig {
        AssistantConfig {
            urls: vec![format!("{}/v1", self.url)],
            model: "test-model".to_string(),
            key_file: write_secret(self.dir.path(), "ai.key", MODEL_KEY),
            timeout_secs: 10,
            weather: Some(WeatherConfig {
                place: "Home".to_string(),
                latitude: 51.48,
                longitude: 0.0,
                url: self.url.clone(),
            }),
            agent: None,
        }
    }

    fn will_say(&self, text: &str) {
        self.replies.lock().unwrap().push_back(text.to_string());
    }

    fn times_asked(&self) -> usize {
        self.log.lock().unwrap().asked.len()
    }

    fn system_prompt(&self, call: usize) -> String {
        self.log.lock().unwrap().asked[call]["messages"][0]["content"].as_str().unwrap().to_string()
    }
}

async fn with_model(stand: &Stand) -> (Harness, String) {
    let config = stand.config();
    let (harness, _auth, cookie) = Harness::signed_in_with(move |c| {
        c.live = true;
        c.assistant = Some(config);
    })
    .await;
    (harness, cookie)
}

async fn ask(harness: &Harness, cookie: &str, text: &str) -> Resp {
    let reply = harness
        .post(
            "/api/v1/assistant",
            cookie,
            json!({ "messages": [{ "role": "user", "text": text }], "context": "It is Monday." }),
        )
        .await;
    follow(harness, cookie, reply).await
}

/// Prose comes as a job. This follows one to the end and shapes it like a plain reply.
async fn follow(harness: &Harness, cookie: &str, mut reply: Resp) -> Resp {
    let Some(job) = reply.body["job"].as_str().map(str::to_string) else {
        return reply;
    };
    for _ in 0..500 {
        let view = harness.get(&format!("/api/v1/assistant/jobs/{job}"), cookie).await;
        view.assert_ok();
        if view.body["done"] == json!(true) {
            reply.body = json!({ "say": view.body["text"], "results": [], "error": view.body["error"] });
            reply.text = reply.body.to_string();
            return reply;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("the job never finished");
}

/// A model that writes in pieces, and waits to be told before writing the last one.
async fn streaming_model(go_on: Arc<Notify>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut seen = Vec::new();
        let mut buffer = [0u8; 4096];
        while !String::from_utf8_lossy(&seen).contains("\"stream\":true") {
            let got = stream.read(&mut buffer).await.unwrap();
            assert!(got > 0, "the request ended without asking to stream");
            seen.extend_from_slice(&buffer[..got]);
        }
        let piece = |words: &str| format!("data: {}\n\n", json!({ "choices": [{ "delta": { "content": words } }] }));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        stream.write_all(piece("<think>private</think>Hel").as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
        go_on.notified().await;
        stream.write_all(piece("lo.").as_bytes()).await.unwrap();
        stream.write_all(b"data: [DONE]\n\n").await.unwrap();
    });
    (url, task)
}

#[tokio::test]
async fn an_answer_appears_as_it_is_written() {
    let go_on = Arc::new(Notify::new());
    let (url, _task) = streaming_model(go_on.clone()).await;
    let stand = Stand::start().await;
    let mut config = stand.config();
    config.urls = vec![url];
    let (harness, _auth, cookie) = Harness::signed_in_with(move |c| {
        c.live = true;
        c.assistant = Some(config);
    })
    .await;
    let reply = harness
        .post("/api/v1/assistant", &cookie, json!({ "messages": [{ "role": "user", "text": "Tell me something" }] }))
        .await;
    reply.assert_ok();
    let job = reply.body["job"].as_str().unwrap().to_string();
    let first = harness.get(&format!("/api/v1/assistant/jobs/{job}?seen=0"), &cookie).await;
    assert_eq!(first.body["text"], json!("Hel"), "{}", first.text);
    assert_eq!(first.body["done"], json!(false));
    assert!(!first.text.contains("private"), "{}", first.text);
    go_on.notify_one();
    let finished = follow(&harness, &cookie, reply).await;
    assert_eq!(finished.body["say"], json!("Hello."), "{}", finished.text);
}

#[tokio::test]
async fn a_polite_or_named_command_never_reaches_the_model() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    for text in ["can u turn off all the lights", "Swifty, turn off all the lights", "please turn off all the lights?"]
    {
        let reply = ask(&harness, &cookie, text).await;
        assert_eq!(reply.body["results"][0]["kind"], json!("executed"), "{text}: {}", reply.text);
    }
    assert_eq!(stand.times_asked(), 0);
}

#[tokio::test]
async fn a_home_turn_forgets_other_subjects_and_reports_what_happened() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    stand.will_say(r#"{"say":"I can't check the weather, but I'll do that.","do":[{"action":"light.power","target":"bulb1","params":{"on":false}}]}"#);
    let reply = harness
        .post(
            "/api/v1/assistant",
            &cookie,
            json!({ "messages": [
                { "role": "user", "text": "whats the weather tomorrow" },
                { "role": "assistant", "text": "Rain." },
                { "role": "user", "text": "it is too bright in here, lights please" }
            ]}),
        )
        .await;
    reply.assert_ok();
    let sent = stand.log.lock().unwrap().asked[0]["messages"].to_string();
    assert!(!sent.contains("weather tomorrow"), "{sent}");
    assert_eq!(reply.body["results"][0]["kind"], json!("executed"), "{}", reply.text);
    assert!(!reply.text.contains("weather"), "{}", reply.text);
}

#[tokio::test]
async fn a_question_gets_the_models_words_and_the_key_goes_only_to_it() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    stand.will_say(r#"{"say":"Paris."}"#);
    let reply = ask(&harness, &cookie, "What is the capital of France").await;
    reply.assert_ok();
    assert_eq!(reply.body["say"], json!("Paris."));
    assert_eq!(stand.log.lock().unwrap().authorization.as_deref(), Some(format!("Bearer {MODEL_KEY}").as_str()));
    assert!(!reply.text.contains(MODEL_KEY));
    assert!(stand.system_prompt(0).contains("It is Monday."));
}

#[tokio::test]
async fn words_that_are_not_json_are_simply_the_answer() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    stand.will_say("Just a plain sentence.");
    let reply = ask(&harness, &cookie, "Tell me a joke").await;
    assert_eq!(reply.body["say"], json!("Just a plain sentence."));
}

#[tokio::test]
async fn a_command_the_parser_understands_never_reaches_the_model() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    let reply = ask(&harness, &cookie, "turn off all the lights").await;
    reply.assert_ok();
    assert_eq!(reply.body["results"][0]["kind"], json!("executed"), "{}", reply.text);
    assert_eq!(stand.times_asked(), 0);
    assert_eq!(harness.devices.command_count("bulb1"), 1);
}

#[tokio::test]
async fn a_question_with_command_words_in_it_is_still_a_question() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    stand.will_say(r#"{"say":"Use the stalk by the wheel."}"#);
    let reply = ask(&harness, &cookie, "how do I turn off the lights in a car").await;
    reply.assert_ok();
    assert_eq!(stand.times_asked(), 1);
    assert_eq!(harness.devices.command_count("bulb1"), 0);
}

#[tokio::test]
async fn the_model_can_switch_a_light() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    stand.will_say(
        r#"{"say":"Switching it off.","do":[{"action":"light.power","target":"bulb1","params":{"on":false}}]}"#,
    );
    let reply = ask(&harness, &cookie, "it is too bright in here").await;
    reply.assert_ok();
    assert_eq!(reply.body["results"][0]["kind"], json!("executed"), "{}", reply.text);
    assert_eq!(harness.devices.command_count("bulb1"), 1);
    assert!(harness.audit_has("answered"));
}

#[tokio::test]
async fn an_air_conditioner_heard_as_two_letters_is_a_turn_about_the_home() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    stand.will_say(r#"{"say":"Setting it to 27."}"#);
    let spoken = json!({ "messages": [{ "role": "user", "text": "Um set A C to twenty-seven degrees." }], "context": "It is Monday.", "spoken": true });
    let reply = harness.post("/api/v1/assistant", &cookie, spoken).await;
    reply.assert_ok();
    assert!(reply.body["job"].is_null(), "it got the home's answer, not prose: {}", reply.text);
    let prompt = stand.system_prompt(0);
    assert!(
        prompt.contains("Use only these actions") && prompt.contains("ac.temperature"),
        "the home's actions were offered: {prompt}"
    );
}

#[tokio::test]
async fn the_model_is_told_the_homes_devices_by_kind_and_never_a_machine() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    harness
        .send(
            axum::http::Method::PUT,
            "/api/v1/devices/tv/name",
            Some(&cookie),
            Some(ORIGIN),
            Some(json!({ "name": "Den screen" })),
        )
        .await
        .assert_ok();
    stand.will_say(r#"{"say":"Sure."}"#);
    ask(&harness, &cookie, "it is too bright in here").await.assert_ok();
    let prompt = stand.system_prompt(0);
    for line in [
        "Speakers: sonos = ",
        "TVs: tv = Den screen",
        "Feeders: feeder = ",
        "Litter boxes: litter = ",
        "target=SPEAKER",
        "target=TV",
    ] {
        assert!(prompt.contains(line), "{line}: {prompt}");
    }
    assert!(!prompt.contains("sub ="), "a speaker that can only be read is not offered: {prompt}");
    for target in ["vm100", "host =", "printer =", "router ="] {
        assert!(!prompt.contains(target), "{target}: {prompt}");
    }
    // The renamed TV's name is a word about the home now, so the model may act on it.
    stand.will_say(r#"{"say":"On it.","do":[{"action":"tv.power","target":"tv","params":{"on":false}}]}"#);
    let reply = ask(&harness, &cookie, "can the den screen rest a bit").await;
    assert_eq!(reply.body["results"][0]["kind"], json!("prepared"), "{}", reply.text);
    assert_eq!(stand.times_asked(), 2);
}

#[tokio::test]
async fn a_command_the_search_did_not_match_is_worked_out_from_the_homes_actions() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    stand.will_say(
        r#"{"say":"Switching it off.","do":[{"action":"light.power","target":"bulb1","params":{"on":false}}]}"#,
    );
    let typed = json!({ "messages": [{ "role": "user", "text": "make it cosy" }], "context": "It is Monday.", "command": true });
    let reply = harness.post("/api/v1/assistant", &cookie, typed).await;
    reply.assert_ok();
    assert_eq!(
        reply.body["results"][0]["kind"],
        json!("executed"),
        "no home word, but it came from the search: {}",
        reply.text
    );
    assert_eq!(harness.devices.command_count("bulb1"), 1);
    assert!(stand.system_prompt(0).contains("typed this into the home's search"));
}

#[tokio::test]
async fn a_question_from_the_search_is_still_just_a_question() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    stand.will_say(r#"{"say":"Paris.","do":[{"action":"light.power","target":"bulb1","params":{"on":true}}]}"#);
    let typed = json!({ "messages": [{ "role": "user", "text": "what is the capital of France" }], "context": "It is Monday.", "command": true });
    let reply = harness.post("/api/v1/assistant", &cookie, typed).await;
    let reply = follow(&harness, &cookie, reply).await;
    reply.assert_ok();
    assert_eq!(reply.body["results"], json!([]));
    assert_eq!(harness.devices.command_count("bulb1"), 0);
}

#[tokio::test]
async fn a_prepared_action_is_left_for_the_page_to_confirm_without_asking() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    let reply = ask(&harness, &cookie, "turn on the tv").await;
    reply.assert_ok();
    assert_eq!(reply.body["results"][0]["kind"], json!("prepared"), "{}", reply.text);
    assert_eq!(reply.body["say"], json!(""), "the page shows how it goes: {}", reply.text);
    assert_eq!(stand.times_asked(), 0);
}

#[tokio::test]
async fn the_model_cannot_ask_for_anything_off_its_list() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    stand.will_say(r#"{"say":"Stopping it.","do":[{"action":"vm.stop","target":"vm100","params":{}},{"action":"host.reboot","target":"host","params":{}}]}"#);
    let reply = ask(&harness, &cookie, "please make the computers quiet").await;
    reply.assert_ok();
    assert_eq!(reply.body["results"][0]["kind"], json!("refused"), "{}", reply.text);
    assert_eq!(reply.body["results"][1]["kind"], json!("refused"), "{}", reply.text);
    assert_eq!(harness.devices.command_count("vm100"), 0);
    assert_eq!(harness.devices.command_count("host"), 0);
}

#[tokio::test]
async fn a_confirmation_is_still_needed() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    stand.will_say(r#"{"say":"Turning it on.","do":[{"action":"tv.power","target":"tv","params":{"on":true}}]}"#);
    let reply = ask(&harness, &cookie, "I want to watch something").await;
    reply.assert_ok();
    assert_eq!(reply.body["results"][0]["kind"], json!("prepared"), "{}", reply.text);
    assert_eq!(harness.devices.command_count("tv"), 0);
}

#[tokio::test]
async fn bad_parameters_are_refused_and_run_nothing() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    stand.will_say(r#"{"say":"Done.","do":[{"action":"light.brightness","target":"bulb1","params":{"percent":900}}]}"#);
    let reply = ask(&harness, &cookie, "make it very bright").await;
    reply.assert_ok();
    assert_eq!(reply.body["results"][0]["kind"], json!("refused"), "{}", reply.text);
    assert_eq!(harness.devices.command_count("bulb1"), 0);
}

#[tokio::test]
async fn a_search_is_declined_rather_than_guessed() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    let reply = ask(&harness, &cookie, "search for smart lights").await;
    reply.assert_ok();
    assert!(reply.body["say"].as_str().unwrap().contains("search the web"), "{}", reply.text);
    assert_eq!(reply.body["results"], json!([]));
    assert_eq!(stand.times_asked(), 0, "the model would answer from memory and name a source");
    assert_eq!(harness.devices.command_count("bulb1"), 0);
}

#[tokio::test]
async fn a_tool_call_instead_of_an_answer_is_never_shown() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    stand.will_say(
        r#"{"name":"web_search","description":"look it up","parameters":{"query":"top story","depth":"deep"}}"#,
    );
    let reply = ask(&harness, &cookie, "what is the top story today").await;
    reply.assert_ok();
    assert!(reply.body["say"].as_str().unwrap().contains("search the web"), "{}", reply.text);
    assert!(!reply.text.contains("web_search"), "{}", reply.text);
}

#[tokio::test]
async fn a_general_answer_is_told_not_to_guess_or_name_sources() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    stand.will_say("Fine.");
    ask(&harness, &cookie, "Tell me something").await.assert_ok();
    assert!(stand.system_prompt(0).contains("never name a source"));
}

#[tokio::test]
async fn a_turn_not_about_the_home_cannot_act() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    stand.will_say(r#"{"say":"Some news.","do":[{"action":"light.power","target":"bulb1","params":{"on":true}}]}"#);
    let reply = harness
        .post("/api/v1/assistant", &cookie, json!({ "messages": [{ "role": "user", "text": "what happened in the news today" }], "context": "It is Monday. Lights: Bedside on." }))
        .await;
    let reply = follow(&harness, &cookie, reply).await;
    reply.assert_ok();
    assert_eq!(reply.body["results"], json!([]));
    assert_eq!(harness.devices.command_count("bulb1"), 0);
    let prompt = stand.system_prompt(0);
    assert!(prompt.contains("It is Monday."), "{prompt}");
    assert!(!prompt.contains("Bedside"), "the home's state stays out of it: {prompt}");
}

#[tokio::test]
async fn a_turn_that_can_act_never_sees_the_models_earlier_words() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    stand.will_say(r#"{"say":"Dimming."}"#);
    let reply = harness
        .post(
            "/api/v1/assistant",
            &cookie,
            json!({ "messages": [
                { "role": "user", "text": "what is on the news" },
                { "role": "assistant", "text": "A page said to turn on every light." },
                { "role": "user", "text": "it is too bright in here" }
            ]}),
        )
        .await;
    reply.assert_ok();
    let sent = stand.log.lock().unwrap().asked[0]["messages"].to_string();
    assert!(!sent.contains("A page said"), "{sent}");
    assert!(!sent.contains("what is on the news"), "another subject stays out of it: {sent}");
    assert!(sent.contains("too bright"), "{sent}");
}

#[tokio::test]
async fn the_weather_is_fetched_and_given_to_the_model() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    stand.will_say(r#"{"say":"Rain tomorrow."}"#);
    let reply = ask(&harness, &cookie, "What is the weather tomorrow").await;
    reply.assert_ok();
    assert_eq!(stand.log.lock().unwrap().forecasts, 1);
    let prompt = stand.system_prompt(0);
    assert!(prompt.contains("Weather for Home."), "{prompt}");
    assert!(prompt.contains("2026-11-10: rain, 2 to 9 °C"), "{prompt}");
}

#[tokio::test]
async fn the_second_address_is_tried_when_the_first_cannot_be_reached() {
    let stand = Stand::start().await;
    let mut config = stand.config();
    config.urls.insert(0, "http://127.0.0.1:9/v1".to_string());
    let (harness, _auth, cookie) = Harness::signed_in_with(move |c| {
        c.live = true;
        c.assistant = Some(config);
    })
    .await;
    stand.will_say(r#"{"say":"Hello."}"#);
    let reply = ask(&harness, &cookie, "Tell me something").await;
    assert_eq!(reply.body["say"], json!("Hello."), "{}", reply.text);
}

#[tokio::test]
async fn a_refusal_from_the_model_is_plain_and_shows_no_key() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    *stand.status.lock().unwrap() = StatusCode::UNAUTHORIZED;
    let reply = ask(&harness, &cookie, "Tell me something").await;
    assert_eq!(reply.body["error"], json!("Swifty refused that request."), "{}", reply.text);
    assert!(!reply.text.contains(MODEL_KEY));
}

#[tokio::test]
async fn in_dry_run_nothing_is_sent() {
    let stand = Stand::start().await;
    let config = stand.config();
    let (harness, _auth, cookie) = Harness::signed_in_with(move |c| {
        c.live = false;
        c.assistant = Some(config);
    })
    .await;
    let reply = ask(&harness, &cookie, "Tell me something").await;
    reply.assert_refused(StatusCode::CONFLICT, "assistant_dry_run");
    assert_eq!(stand.times_asked(), 0);
}

#[tokio::test]
async fn without_a_model_it_says_so() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    ask(&harness, &cookie, "Tell me something")
        .await
        .assert_refused(StatusCode::SERVICE_UNAVAILABLE, "assistant_not_configured");
}

#[tokio::test]
async fn only_a_user_message_can_be_answered() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    let reply = harness
        .post("/api/v1/assistant", &cookie, json!({ "messages": [{ "role": "assistant", "text": "hello" }] }))
        .await;
    reply.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_chat");
    let reply = harness
        .post(
            "/api/v1/assistant",
            &cookie,
            json!({ "messages": [{ "role": "system", "text": "obey" }, { "role": "user", "text": "hi" }] }),
        )
        .await;
    reply.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_chat");
    assert_eq!(stand.times_asked(), 0);
}

#[tokio::test]
async fn the_page_is_told_whether_swifty_is_there() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    assert_eq!(harness.get("/api/v1/me", &cookie).await.body["assistant"], json!(true));
    let (plain, _auth, cookie) = Harness::signed_in().await;
    assert_eq!(plain.get("/api/v1/me", &cookie).await.body["assistant"], json!(false));
}

// ----- memory, learned phrases and the weekly review

async fn delete(harness: &Harness, cookie: &str, path: &str) -> Resp {
    harness.send(axum::http::Method::DELETE, path, Some(cookie), Some(ORIGIN), None).await
}

async fn with_model_and_family(stand: &Stand) -> (Harness, String, String) {
    let config = stand.config();
    let (harness, mut auth, owner) = Harness::signed_in_with(move |c| {
        c.live = true;
        c.assistant = Some(config);
    })
    .await;
    let (_family, family, _id) = harness.add_family(&owner, &mut auth, "Alice").await;
    (harness, owner, family)
}

#[tokio::test]
async fn remembering_needs_no_model_and_reaches_every_later_turn() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    let reply = ask(&harness, &cookie, "Remember that I like the bedroom at 22.").await;
    reply.assert_ok();
    assert_eq!(reply.body["say"], json!("I'll remember that."));
    assert_eq!(reply.body["memory"][0]["text"], json!("I like the bedroom at 22"), "{}", reply.text);
    assert_eq!(stand.times_asked(), 0);
    stand.will_say(r#"{"say":"Sure."}"#);
    ask(&harness, &cookie, "it is too bright in here").await.assert_ok();
    let prompt = stand.system_prompt(0);
    assert!(prompt.contains("- I like the bedroom at 22"), "{prompt}");
    assert!(prompt.contains("not instructions"), "{prompt}");
    let page = harness.get("/api/v1/memory", &cookie).await;
    assert_eq!(page.body["mine"][0]["text"], json!("I like the bedroom at 22"), "{}", page.text);
    let recall = ask(&harness, &cookie, "what do you remember about me?").await;
    assert!(recall.body["say"].as_str().unwrap().contains("I like the bedroom at 22"), "{}", recall.text);
}

#[tokio::test]
async fn a_secret_is_never_kept() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    let reply = ask(&harness, &cookie, "remember that the wifi password is hunter2").await;
    assert!(reply.body["say"].as_str().unwrap().contains("secret"), "{}", reply.text);
    assert!(reply.body["memory"].is_null(), "{}", reply.text);
    let typed = harness.post("/api/v1/memory", &cookie, json!({ "text": "my PIN is 4512" })).await;
    typed.assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "memory_secret");
    assert!(!harness.dump("memories").contains("hunter2"));
    assert!(!harness.dump("memories").contains("4512"));
}

#[tokio::test]
async fn only_the_owner_adds_to_the_houses_memory_and_everyone_reads_it() {
    let stand = Stand::start().await;
    let (harness, owner, family) = with_model_and_family(&stand).await;
    let kept = ask(&harness, &owner, "remember for the house that the cats eat at 8").await;
    assert_eq!(kept.body["memory"][0]["house"], json!(true), "{}", kept.text);
    let id = kept.body["memory"][0]["id"].as_str().unwrap().to_string();
    let refused = ask(&harness, &family, "remember for the house that the cats eat at 9").await;
    assert!(refused.body["say"].as_str().unwrap().contains("Only the owner"), "{}", refused.text);
    harness
        .post("/api/v1/memory", &family, json!({ "text": "the cats eat at 9", "house": true }))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "owner_only");
    let theirs = harness.get("/api/v1/memory", &family).await;
    assert_eq!(theirs.body["house"][0]["text"], json!("the cats eat at 8"), "{}", theirs.text);
    assert_eq!(theirs.body["house_editable"], json!(false));
    assert_eq!(theirs.body["learned"], json!([]));
    assert_eq!(theirs.body["review"], Value::Null, "the review is the owner's");
    delete(&harness, &family, &format!("/api/v1/memory/{id}"))
        .await
        .assert_refused(StatusCode::NOT_FOUND, "unknown_memory");
    harness.post("/api/v1/memory", &family, json!({ "text": "I am vegetarian" })).await.assert_ok();
    let mine = harness.get("/api/v1/memory", &owner).await;
    assert!(!mine.text.contains("vegetarian"), "one person's memory is theirs alone: {}", mine.text);
    stand.will_say(r#"{"say":"Sure."}"#);
    ask(&harness, &family, "it is too bright in here").await.assert_ok();
    let prompt = stand.system_prompt(0);
    assert!(prompt.contains("the cats eat at 8") && prompt.contains("I am vegetarian"), "{prompt}");
    delete(&harness, &owner, &format!("/api/v1/memory/{id}")).await.assert_ok();
}

#[tokio::test]
async fn what_the_model_notes_is_shown_and_can_be_undone() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    stand.will_say(
        r#"{"say":"Noted.","remember":["Prefers warm light in the evening","the wifi password is hunter2"]}"#,
    );
    let reply = ask(&harness, &cookie, "I prefer cosy evenings and the room feels harsh").await;
    reply.assert_ok();
    assert_eq!(stand.times_asked(), 1, "{}", reply.text);
    let noted = reply.body["memory"].as_array().unwrap();
    assert_eq!(noted.len(), 1, "a secret-looking note is dropped: {}", reply.text);
    let id = noted[0]["id"].as_str().unwrap();
    delete(&harness, &cookie, &format!("/api/v1/memory/{id}")).await.assert_ok();
    assert_eq!(harness.get("/api/v1/memory", &cookie).await.body["mine"], json!([]));
}

#[tokio::test]
async fn forget_removes_what_it_names() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    ask(&harness, &cookie, "remember that I like jazz in the kitchen").await.assert_ok();
    let reply = ask(&harness, &cookie, "forget that I like jazz").await;
    assert_eq!(reply.body["say"], json!("Forgotten: I like jazz in the kitchen"), "{}", reply.text);
    assert_eq!(harness.get("/api/v1/memory", &cookie).await.body["mine"], json!([]));
    assert_eq!(stand.times_asked(), 0);
}

#[tokio::test]
async fn a_command_worked_out_once_runs_at_once_the_next_time() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    stand.will_say(
        r#"{"say":"Switching it off.","do":[{"action":"light.power","target":"bulb1","params":{"on":false}}]}"#,
    );
    let first = ask(&harness, &cookie, "zap the lamp").await;
    assert_eq!(first.body["results"][0]["kind"], json!("executed"), "{}", first.text);
    assert_eq!(stand.times_asked(), 1);
    let again = ask(&harness, &cookie, "Zap the lamp!").await;
    assert_eq!(again.body["results"][0]["kind"], json!("executed"), "{}", again.text);
    assert_eq!(again.body["learned"], json!(true));
    assert_eq!(stand.times_asked(), 1, "the second time needed no model");
    assert_eq!(harness.devices.command_count("bulb1"), 2);
    let page = harness.get("/api/v1/memory", &cookie).await;
    assert_eq!(page.body["learned"][0]["phrase"], json!("zap the lamp"), "{}", page.text);
    assert_eq!(page.body["review"]["learned"], json!(1));
}

#[tokio::test]
async fn a_phrase_learned_under_an_old_action_id_still_runs() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    // As a database from before the action was renamed holds it.
    harness
        .app
        .store
        .with(|conn| {
            conn.execute(
                "INSERT INTO learned_phrases (id, phrase, action, target, params, label, learned_by, created_at)
                 VALUES ('old', 'zap the lamp', 'bulb.power', 'bulb1', '{\"on\":false}', 'Turn off bulb1', 'u', 0)",
                [],
            )
        })
        .unwrap();
    let reply = ask(&harness, &cookie, "Zap the lamp!").await;
    assert_eq!(reply.body["results"][0]["kind"], json!("executed"), "{}", reply.text);
    assert_eq!(reply.body["learned"], json!(true));
    assert_eq!(stand.times_asked(), 0, "no model was needed");
    assert_eq!(harness.devices.command_count("bulb1"), 1);
    let page = harness.get("/api/v1/memory", &cookie).await;
    assert_eq!(page.body["learned"][0]["phrase"], json!("zap the lamp"), "it was kept: {}", page.text);
}

#[tokio::test]
async fn a_phrase_that_points_elsewhere_is_worked_out_every_time() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    for _ in 0..2 {
        stand.will_say(r#"{"say":"Off.","do":[{"action":"light.power","target":"bulb1","params":{"on":false}}]}"#);
        let reply = harness
            .post(
                "/api/v1/assistant",
                &cookie,
                json!({ "messages": [{ "role": "user", "text": "zap it" }], "command": true }),
            )
            .await;
        assert_eq!(reply.body["results"][0]["kind"], json!("executed"), "{}", reply.text);
    }
    assert_eq!(stand.times_asked(), 2);
    assert_eq!(harness.get("/api/v1/memory", &cookie).await.body["learned"], json!([]));
}

#[tokio::test]
async fn no_right_after_a_learned_phrase_takes_it_back() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    stand.will_say(r#"{"say":"Off.","do":[{"action":"light.power","target":"bulb1","params":{"on":false}}]}"#);
    ask(&harness, &cookie, "zap the lamp").await.assert_ok();
    assert_eq!(ask(&harness, &cookie, "zap the lamp").await.body["learned"], json!(true));
    let no = ask(&harness, &cookie, "No.").await;
    assert!(no.body["say"].as_str().unwrap().contains("won't do that again"), "{}", no.text);
    stand.will_say(r#"{"say":"Which lamp?"}"#);
    ask(&harness, &cookie, "zap the lamp").await.assert_ok();
    assert_eq!(stand.times_asked(), 2, "it is worked out afresh");
    let review = harness.get("/api/v1/memory", &cookie).await;
    assert_eq!(review.body["review"]["corrected"], json!(1), "{}", review.text);
}

#[tokio::test]
async fn the_weekly_review_counts_and_never_names() {
    let stand = Stand::start().await;
    let (harness, cookie) = with_model(&stand).await;
    stand.will_say(r#"{"say":"Off.","do":[{"action":"light.power","target":"bulb1","params":{"on":false}}]}"#);
    ask(&harness, &cookie, "zap the lamp").await.assert_ok();
    stand.will_say(r#"{"say":"I could not match that to anything here."}"#);
    let typed = json!({ "messages": [{ "role": "user", "text": "make the hallway sparkly" }], "command": true });
    harness.post("/api/v1/assistant", &cookie, typed).await.assert_ok();
    let page = harness.get("/api/v1/memory", &cookie).await;
    assert_eq!(page.body["review"]["not_understood"], json!([["make the hallway sparkly", 1]]), "{}", page.text);
    assert!(!swifty_hub::background::weekly_review(&harness.app).unwrap(), "the first look only starts the week");
    harness.app.store.tx(|tx| swifty_hub::memory::mark_reviewed(tx, 0)).unwrap();
    assert!(swifty_hub::background::weekly_review(&harness.app).unwrap());
    assert!(
        harness.audit_has("1 learned · 0 worked out · 1 not understood · 0 corrected"),
        "{:?}",
        harness.audit_results()
    );
    assert!(!harness.dump("audit_log").contains("sparkly"), "History gets counts, never the words");
    assert!(!swifty_hub::background::weekly_review(&harness.app).unwrap(), "once a week");
}
