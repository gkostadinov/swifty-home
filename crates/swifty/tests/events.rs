//! The event stream, `GET /api/v1/events`: Server-Sent Events with resume, a
//! resync for clients that were away too long, filtering by who may see what,
//! a per-session limit, and an end when the session ends or locks.
//!
//! The tests of the stream itself publish to the bus by hand. The last ones drive
//! the hub and read what it publishes: a run's phases, a changed reading, a rebuilt
//! index and a changed instance.

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use common::*;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use swifty_hub::actions::Phase;
use swifty_hub::events::{Event, BUFFER, MAX_STREAMS_PER_SESSION};
use swifty_plugin_api::{Health, HealthStatus, Readiness, State};
use tower::ServiceExt;

/// Long enough for anything the stream does on its own, except the heartbeat.
const PATIENCE: Duration = Duration::from_secs(10);
/// How long to listen to be sure nothing more is coming.
const QUIET: Duration = Duration::from_millis(300);

/// One message off the wire.
#[derive(Debug, Default, Clone)]
struct Message {
    event: Option<String>,
    id: Option<String>,
    data: Option<String>,
}

impl Message {
    fn kind(&self) -> &str {
        self.event.as_deref().unwrap_or("")
    }

    fn json(&self) -> Value {
        serde_json::from_str(self.data.as_deref().unwrap_or("null")).unwrap()
    }

    fn id(&self) -> &str {
        self.id.as_deref().unwrap()
    }
}

/// An open stream, read message by message.
struct Stream {
    body: Body,
    buffer: String,
}

enum Read {
    Message(Message),
    Ended,
    Silent,
}

impl Stream {
    async fn read(&mut self, wait: Duration) -> Read {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if let Some(end) = self.buffer.find("\n\n") {
                let raw: String = self.buffer.drain(..end + 2).collect();
                let mut message = Message::default();
                let mut fields = 0;
                for line in raw.lines() {
                    let (name, value) = line.split_once(':').unwrap_or((line, ""));
                    let value = value.strip_prefix(' ').unwrap_or(value).to_string();
                    match name {
                        "event" => message.event = Some(value),
                        "id" => message.id = Some(value),
                        "data" => message.data = Some(value),
                        _ => continue,
                    }
                    fields += 1;
                }
                if fields == 0 {
                    continue; // a comment
                }
                return Read::Message(message);
            }
            match tokio::time::timeout_at(deadline, self.body.frame()).await {
                Err(_) => return Read::Silent,
                Ok(None) => return Read::Ended,
                Ok(Some(frame)) => {
                    if let Ok(data) = frame.unwrap().into_data() {
                        self.buffer.push_str(std::str::from_utf8(&data).unwrap());
                    }
                }
            }
        }
    }

    async fn next(&mut self) -> Message {
        match self.read(PATIENCE).await {
            Read::Message(message) => message,
            Read::Ended => panic!("the stream ended"),
            Read::Silent => panic!("nothing arrived"),
        }
    }

    async fn assert_quiet(&mut self) {
        match self.read(QUIET).await {
            Read::Silent => {}
            Read::Ended => panic!("the stream ended"),
            Read::Message(message) => panic!("unexpected {message:?}"),
        }
    }

    async fn take(&mut self, count: usize) -> Vec<Message> {
        let mut messages = Vec::new();
        for _ in 0..count {
            messages.push(self.next().await);
        }
        messages
    }

    fn kinds(messages: &[Message]) -> Vec<&str> {
        messages.iter().map(Message::kind).collect()
    }

    /// Fails if anything but the end arrives first.
    async fn assert_ends(&mut self) {
        self.assert_ends_within(PATIENCE).await;
    }

    async fn assert_ends_within(&mut self, wait: Duration) {
        match self.read(wait).await {
            Read::Ended => {}
            Read::Silent => panic!("the stream is still open"),
            Read::Message(message) => panic!("unexpected {message:?}"),
        }
    }
}

async fn request(harness: &Harness, cookie: Option<&str>, last_event_id: Option<&str>) -> axum::response::Response {
    let mut request = Request::builder().method(Method::GET).uri("/api/v1/events");
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    if let Some(id) = last_event_id {
        request = request.header("last-event-id", id);
    }
    harness.router.clone().oneshot(request.body(Body::empty()).unwrap()).await.unwrap()
}

async fn open(harness: &Harness, cookie: &str, last_event_id: Option<&str>) -> Stream {
    let response = request(harness, Some(cookie), last_event_id).await;
    assert_eq!(response.status(), StatusCode::OK);
    Stream { body: response.into_body(), buffer: String::new() }
}

async fn refused(harness: &Harness, cookie: Option<&str>) -> Resp {
    let response = request(harness, cookie, None).await;
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    Resp { status, headers, body: serde_json::from_str(&text).unwrap_or(Value::Null), text }
}

/// The session's ref and its person's user id.
async fn whoami(harness: &Harness, cookie: &str) -> (String, String) {
    let session = harness.get("/api/v1/session", cookie).await;
    session.assert_ok();
    let session = &session.body["session"];
    (session["session_ref"].as_str().unwrap().to_string(), session["user_id"].as_str().unwrap().to_string())
}

fn run(run_id: &str, target: &str, by: &str) -> Event {
    Event::Run {
        run_id: run_id.into(),
        action: "light.power".into(),
        target: target.into(),
        phase: Phase::Confirmed,
        started_by: by.into(),
    }
}

fn state(target: &str) -> Event {
    Event::State { target: target.into(), state: State::Plug { on: true } }
}

#[tokio::test]
async fn the_stream_is_server_sent_events_for_an_unlocked_session() {
    let (harness, _auth, cookie) = Harness::signed_in().await;

    refused(&harness, None).await.assert_refused(StatusCode::UNAUTHORIZED, "not_signed_in");
    refused(&harness, Some("__Host-swifty-session=forged"))
        .await
        .assert_refused(StatusCode::UNAUTHORIZED, "not_signed_in");

    let response = request(&harness, Some(&cookie), None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers();
    assert_eq!(headers[header::CONTENT_TYPE], "text/event-stream");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(headers["x-accel-buffering"], "no");
    drop(response);

    harness.post("/api/v1/session/lock", &cookie, json!({})).await.assert_ok();
    refused(&harness, Some(&cookie)).await.assert_refused(StatusCode::LOCKED, "session_locked");
}

#[tokio::test]
async fn events_arrive_in_order_and_a_client_resumes_from_its_last_event_id() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let (_, me) = whoami(&harness, &cookie).await;
    let bus = &harness.app.events;

    // Something from before the stream opened is not replayed to a new client.
    bus.publish(Event::Devices);
    let mut stream = open(&harness, &cookie, None).await;
    stream.assert_quiet().await;

    bus.publish(run("r1", "bulb1", &me));
    bus.publish(state("bulb1"));
    bus.publish(Event::Devices);
    let first = stream.next().await;
    assert_eq!(first.kind(), "run");
    assert_eq!(
        first.json(),
        json!({ "run_id": "r1", "action": "light.power", "target": "bulb1", "phase": "confirmed" })
    );
    let second = stream.next().await;
    assert_eq!(second.kind(), "state");
    assert_eq!(second.json(), json!({ "target": "bulb1", "state": { "kind": "plug", "on": true } }));
    let third = stream.next().await;
    assert_eq!(third.kind(), "devices");
    assert_eq!(third.json(), json!({}));
    let ids = [first.id(), second.id(), third.id()];
    assert!(ids.iter().all(|id| !id.is_empty()));
    assert!(ids[0] != ids[1] && ids[1] != ids[2]);
    stream.assert_quiet().await;
    drop(stream);

    // Away for a while: the client comes back with the id of the run.
    bus.publish(run("r2", "bulb2", &me));
    bus.publish(run("r3", "bulb3", &me));
    let mut resumed = open(&harness, &cookie, Some(first.id())).await;
    let replayed = resumed.take(4).await;
    assert_eq!(Stream::kinds(&replayed), ["state", "devices", "run", "run"]);
    assert_eq!(replayed[0].id(), second.id());
    assert_eq!(replayed[1].id(), third.id());
    assert_eq!(replayed[2].json()["run_id"], "r2");
    assert_eq!(replayed[3].json()["run_id"], "r3");
    resumed.assert_quiet().await;

    // And carries on live from there.
    bus.publish(run("r4", "bulb4", &me));
    assert_eq!(resumed.next().await.json()["run_id"], "r4");

    // Up to date: nothing to replay.
    let latest = resumed_id(&harness, &cookie, &me).await;
    let mut up_to_date = open(&harness, &cookie, Some(&latest)).await;
    up_to_date.assert_quiet().await;
}

/// Publishes one run and returns the id it reaches a stream with.
async fn resumed_id(harness: &Harness, cookie: &str, me: &str) -> String {
    let mut stream = open(harness, cookie, None).await;
    harness.app.events.publish(run("marker", "bulb1", me));
    stream.next().await.id().to_string()
}

#[tokio::test]
async fn a_client_whose_last_event_id_is_older_than_the_buffer_is_told_to_resync() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let bus = &harness.app.events;

    let mut stream = open(&harness, &cookie, None).await;
    bus.publish(Event::Devices);
    let old = stream.next().await;
    drop(stream);

    // One more than the buffer holds, so the event after `old` is gone.
    for _ in 0..=BUFFER {
        bus.publish(Event::Devices);
    }
    let mut late = open(&harness, &cookie, Some(old.id())).await;
    let resync = late.next().await;
    assert_eq!(resync.kind(), "resync");
    assert_eq!(resync.json(), json!({}));
    late.assert_quiet().await;
    bus.publish(state("bulb1"));
    let next = late.next().await;
    assert_eq!(next.kind(), "state", "after the resync the stream carries on live");

    // Resuming from the resync replays only what came after it.
    let mut after = open(&harness, &cookie, Some(resync.id())).await;
    assert_eq!(after.next().await.id(), next.id());
    after.assert_quiet().await;

    // Exactly at the edge of the buffer there is nothing missing yet.
    let edge = bus.latest() - BUFFER as u64;
    let mut at_edge = open(&harness, &cookie, Some(&bus.id(edge))).await;
    assert_eq!(at_edge.next().await.kind(), "devices");
    drop(at_edge);
    let mut past_edge = open(&harness, &cookie, Some(&bus.id(edge - 1))).await;
    assert_eq!(past_edge.next().await.kind(), "resync");

    // An id this hub never handed out, such as one from before a restart.
    for id in ["1", "garbage", "00000000-1", &bus.id(bus.latest() + 5)] {
        let mut stranger = open(&harness, &cookie, Some(id)).await;
        assert_eq!(stranger.next().await.kind(), "resync", "{id}");
        stranger.assert_quiet().await;
    }
}

#[tokio::test]
async fn a_family_session_never_receives_an_owner_only_event() {
    let (harness, mut owner_auth, owner_cookie) = Harness::signed_in().await;
    let (_kid_auth, kid_cookie, kid) = harness.add_family(&owner_cookie, &mut owner_auth, "Kid").await;
    let (owner_session, owner) = whoami(&harness, &owner_cookie).await;
    let (kid_session, _) = whoami(&harness, &kid_cookie).await;
    let bus = &harness.app.events;

    let mut owner_stream = open(&harness, &owner_cookie, None).await;
    let mut kid_stream = open(&harness, &kid_cookie, None).await;

    let owner_only = [
        Event::Plugin {
            instance: "wiz".into(),
            readiness: Readiness::NeedsSetup("Pair it first.".into()),
            health: Health { status: HealthStatus::Down, detail: None },
        },
        run("owner-run", "vm100", &owner),
        state("vm100"),
        state("router"),
        Event::Setup { setup_id: "setup-1".into(), session_ref: owner_session.clone() },
        Event::Assistant { job_id: "job-1".into(), user_id: owner.clone() },
    ];
    for event in owner_only.iter().cloned() {
        bus.publish(event);
    }
    bus.publish(Event::Devices);

    // The family member's first event is the one meant for everyone.
    let first = kid_stream.next().await;
    assert_eq!(first.kind(), "devices", "{first:?}");
    kid_stream.assert_quiet().await;

    let received = owner_stream.take(owner_only.len() + 1).await;
    assert_eq!(Stream::kinds(&received), ["plugin", "run", "state", "state", "setup", "assistant", "devices"]);
    owner_stream.assert_quiet().await;

    // What is theirs, or theirs to see, reaches them; the owner also sees their runs.
    bus.publish(run("kid-run", "bulb1", &kid));
    bus.publish(state("bulb1"));
    bus.publish(Event::Setup { setup_id: "setup-2".into(), session_ref: kid_session });
    bus.publish(Event::Assistant { job_id: "job-2".into(), user_id: kid.clone() });
    let theirs = kid_stream.take(4).await;
    assert_eq!(Stream::kinds(&theirs), ["run", "state", "setup", "assistant"]);
    assert_eq!(owner_stream.next().await.json()["run_id"], "kid-run");
    assert_eq!(owner_stream.next().await.kind(), "state");
    owner_stream.assert_quiet().await;

    // Resuming from before the owner-only events does not replay them either.
    let mut kid_resumed = open(&harness, &kid_cookie, Some(&bus.id(0))).await;
    let replayed = kid_resumed.take(5).await;
    assert_eq!(Stream::kinds(&replayed), ["devices", "run", "state", "setup", "assistant"]);
    kid_resumed.assert_quiet().await;
}

#[tokio::test]
async fn the_stream_ends_when_its_session_locks() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let mut stream = open(&harness, &cookie, None).await;
    harness.app.events.publish(Event::Devices);
    assert_eq!(stream.next().await.kind(), "devices");

    harness.post("/api/v1/session/lock", &cookie, json!({})).await.assert_ok();
    // Whatever is published after the lock is not sent.
    harness.app.events.publish(Event::Devices);
    stream.assert_ends().await;
    assert_eq!(harness.app.events.open_streams(&whoami_locked(&harness, &cookie).await), 0);

    // Locked with nothing published: the stream ends on its own.
    let cookie = harness.login(&mut auth).await;
    let mut quiet = open(&harness, &cookie, None).await;
    harness.post("/api/v1/session/lock", &cookie, json!({})).await.assert_ok();
    quiet.assert_ends().await;
}

/// A locked session's ref, read through the route that allows a locked session.
async fn whoami_locked(harness: &Harness, cookie: &str) -> String {
    let session = harness.get("/api/v1/session", cookie).await;
    session.assert_ok();
    assert_eq!(session.body["session"]["locked"], true);
    session.body["session"]["session_ref"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn the_stream_ends_when_its_session_ends() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let mut stream = open(&harness, &cookie, None).await;
    harness.post("/api/v1/auth/logout", &cookie, json!({})).await.assert_ok();
    stream.assert_ends().await;
    refused(&harness, Some(&cookie)).await.assert_refused(StatusCode::UNAUTHORIZED, "session_ended");
}

#[tokio::test]
async fn the_fifth_stream_of_a_session_is_refused() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let mut streams = Vec::new();
    for _ in 0..MAX_STREAMS_PER_SESSION {
        streams.push(open(&harness, &cookie, None).await);
    }
    let fifth = refused(&harness, Some(&cookie)).await;
    fifth.assert_refused(StatusCode::TOO_MANY_REQUESTS, "too_many_streams");

    // The others are unaffected, and so is another session of the same person.
    harness.app.events.publish(Event::Devices);
    for stream in &mut streams {
        assert_eq!(stream.next().await.kind(), "devices");
    }
    let other = harness.login(&mut auth).await;
    let _other_stream = open(&harness, &other, None).await;

    // A closed stream gives its place back.
    streams.pop();
    let mut again = open(&harness, &cookie, None).await;
    refused(&harness, Some(&cookie)).await.assert_refused(StatusCode::TOO_MANY_REQUESTS, "too_many_streams");
    harness.app.events.publish(Event::Devices);
    assert_eq!(again.next().await.kind(), "devices");
}

#[tokio::test]
async fn a_stream_is_not_activity_and_ends_when_the_session_goes_idle() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let idle_ms = harness.app.config.session_idle_ms();

    harness.advance(idle_ms - MINUTE_MS);
    let mut stream = open(&harness, &cookie, None).await;
    harness.app.events.publish(Event::Devices);
    assert_eq!(stream.next().await.kind(), "devices");

    // Opening the stream and receiving on it did not count, so the session is
    // idle a minute after it would have been anyway.
    harness.advance(2 * MINUTE_MS);
    stream.assert_ends().await;
    let session = harness.get("/api/v1/session", &cookie).await;
    session.assert_refused(StatusCode::UNAUTHORIZED, "session_idle");
}

// ----- what the hub publishes

/// Reads one device as a page does, and returns its state.
async fn read(harness: &Harness, cookie: &str, target: &str) -> Value {
    let state = harness.get(&format!("/api/v1/state/{target}"), cookie).await;
    state.assert_ok();
    state.body["state"].clone()
}

#[tokio::test]
async fn an_action_run_is_published_in_its_phases_in_order() {
    let (harness, mut owner_auth, owner_cookie) = Harness::signed_in().await;
    let (_kid_auth, kid_cookie, _) = harness.add_family(&owner_cookie, &mut owner_auth, "Kid").await;
    // What a page last saw, so the run's own reading is a change.
    let was_on = read(&harness, &owner_cookie, "bulb1").await["on"].as_bool().unwrap();
    let mut owner = open(&harness, &owner_cookie, None).await;
    let mut kid = open(&harness, &kid_cookie, None).await;

    let run = harness.execute(&owner_cookie, "light.power", "bulb1", json!({ "on": !was_on })).await;
    assert_eq!(run.phase(), "confirmed", "{}", run.text);
    let run_id = run.body["run_id"].as_str().unwrap();

    let seen = owner.take(3).await;
    assert_eq!(Stream::kinds(&seen), ["run", "state", "run"], "{seen:?}");
    assert_eq!(
        seen[0].json(),
        json!({ "run_id": run_id, "action": "light.power", "target": "bulb1", "phase": "commanded" })
    );
    assert_eq!(seen[1].json()["target"], "bulb1");
    assert_eq!(seen[1].json()["state"]["on"], json!(!was_on), "the reading that confirmed it");
    assert_eq!(
        seen[2].json(),
        json!({ "run_id": run_id, "action": "light.power", "target": "bulb1", "phase": "confirmed" })
    );
    owner.assert_quiet().await;

    // The family member may see the light, and not the owner's run.
    let theirs = kid.next().await;
    assert_eq!(theirs.kind(), "state");
    assert_eq!(theirs.id(), seen[1].id());
    kid.assert_quiet().await;

    // Their own run reaches them, and the owner.
    let mut owner = open(&harness, &owner_cookie, None).await;
    let run = harness.execute(&kid_cookie, "light.power", "bulb1", json!({ "on": was_on })).await;
    assert_eq!(run.phase(), "confirmed", "{}", run.text);
    for stream in [&mut kid, &mut owner] {
        let seen = stream.take(3).await;
        assert_eq!(Stream::kinds(&seen), ["run", "state", "run"], "{seen:?}");
        assert_eq!(seen[0].json()["run_id"], run.body["run_id"]);
        assert_eq!(seen[2].json()["phase"], "confirmed");
    }
}

#[tokio::test]
async fn only_a_reading_that_differs_from_the_last_is_published() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    // The first reading after a start is what later ones are compared with.
    read(&harness, &cookie, "bulb2").await;
    let mut stream = open(&harness, &cookie, None).await;
    read(&harness, &cookie, "bulb2").await;
    stream.assert_quiet().await;

    // Someone used the switch on the wall.
    harness.devices.add_state("bulb2", State::Light { on: true, bri: 7, hue: 120, color: true });
    let shown = read(&harness, &cookie, "bulb2").await;
    let event = stream.next().await;
    assert_eq!(event.kind(), "state");
    assert_eq!(event.json(), json!({ "target": "bulb2", "state": shown }));
    read(&harness, &cookie, "bulb2").await;
    stream.assert_quiet().await;
}

#[tokio::test]
async fn a_rebuilt_index_goes_to_everyone_and_a_changed_instance_to_the_owner() {
    let (harness, mut owner_auth, owner_cookie) = Harness::signed_in().await;
    let (_kid_auth, kid_cookie, _) = harness.add_family(&owner_cookie, &mut owner_auth, "Kid").await;
    let mut owner = open(&harness, &owner_cookie, None).await;
    let mut kid = open(&harness, &kid_cookie, None).await;

    // A new name rebuilds the index, and everyone refetches the devices.
    let renamed = harness
        .send(
            Method::PUT,
            "/api/v1/devices/bulb1/name",
            Some(&owner_cookie),
            Some(ORIGIN),
            Some(json!({ "name": "Desk" })),
        )
        .await;
    renamed.assert_ok();
    for stream in [&mut owner, &mut kid] {
        let event = stream.next().await;
        assert_eq!((event.kind(), event.json()), ("devices", json!({})));
        stream.assert_quiet().await;
    }

    // An instance that can no longer be used is news for the owner only.
    harness.devices.set_disabled("wiz", "Switched off for the test.");
    harness.app.sync_directory().unwrap();
    let changed = owner.take(2).await;
    assert_eq!(Stream::kinds(&changed), ["plugin", "devices"], "{changed:?}");
    assert_eq!(changed[0].json()["instance"], "wiz");
    assert_eq!(changed[0].json()["readiness"], json!({ "status": "disabled", "reason": "Switched off for the test." }));
    assert!(changed[0].json()["health"]["status"].is_string());
    assert_eq!(kid.next().await.kind(), "devices");
    owner.assert_quiet().await;
    kid.assert_quiet().await;

    // Loaded again as it is, nothing about an instance is news.
    harness.app.sync_directory().unwrap();
    assert_eq!(owner.next().await.kind(), "devices");
    owner.assert_quiet().await;
}

/// Shorter than the periodic check of a stream's session, so a stream that ends this
/// soon was told.
const AT_ONCE: Duration = Duration::from_millis(1000);

/// Opens a stream and lets it settle into waiting, as one a page holds open does.
async fn waiting(harness: &Harness, cookie: &str) -> Stream {
    let mut stream = open(harness, cookie, None).await;
    stream.assert_quiet().await;
    stream
}

#[tokio::test]
async fn a_stream_ends_at_once_when_its_session_locks_signs_out_or_is_ended() {
    let (harness, mut auth, cookie) = Harness::signed_in().await;
    let mut stream = waiting(&harness, &cookie).await;
    harness.post("/api/v1/session/lock", &cookie, json!({})).await.assert_ok();
    stream.assert_ends_within(AT_ONCE).await;

    let cookie = harness.login(&mut auth).await;
    let mut stream = waiting(&harness, &cookie).await;
    harness.post("/api/v1/auth/logout", &cookie, json!({})).await.assert_ok();
    stream.assert_ends_within(AT_ONCE).await;

    // Another session of the same person, ended from this one.
    let cookie = harness.login(&mut auth).await;
    let other = harness.login(&mut auth).await;
    let (other_ref, _) = whoami(&harness, &other).await;
    let mut theirs = waiting(&harness, &other).await;
    let mut mine = waiting(&harness, &cookie).await;
    let path = format!("/api/v1/sessions/{other_ref}");
    harness.send(Method::DELETE, &path, Some(&cookie), Some(ORIGIN), None).await.assert_ok();
    theirs.assert_ends_within(AT_ONCE).await;
    mine.assert_quiet().await;
}

#[tokio::test]
async fn a_family_members_stream_ends_at_once_when_they_are_removed() {
    let (harness, mut owner_auth, owner_cookie) = Harness::signed_in().await;
    let (_kid_auth, kid_cookie, kid) = harness.add_family(&owner_cookie, &mut owner_auth, "Kid").await;
    let mut theirs = waiting(&harness, &kid_cookie).await;
    let removed = harness.critical(&owner_cookie, &mut owner_auth, "user.remove", &kid, json!({})).await;
    assert_eq!(removed.phase(), "confirmed", "{}", removed.text);
    theirs.assert_ends_within(AT_ONCE).await;
    refused(&harness, Some(&kid_cookie)).await.assert_refused(StatusCode::UNAUTHORIZED, "session_ended");
}
