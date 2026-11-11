//! The event stream that clients follow to refresh sooner: `GET /api/v1/events`, as
//! Server-Sent Events.
//!
//! A stream belongs to one signed-in, unlocked session, which may hold at most
//! [`MAX_STREAMS_PER_SESSION`] of them. It resumes from `Last-Event-ID` while the
//! bus still has everything after it, and otherwise starts with a `resync` event.
//! A comment every [`HEARTBEAT`] keeps proxies from closing it. It ends when the
//! session ends, locks or runs out of time: the session is checked before anything
//! is sent, at once when a session is locked or ended anywhere
//! ([`crate::events::EventBus::recheck_sessions`]), and every [`SESSION_CHECK`]
//! while nothing happens, which is what notices idle and absolute expiry.
//!
//! Opening or holding a stream is not activity. An open tab must still reach the
//! idle limit, so the session is looked up without being touched ([`Passive`]).

use std::collections::VecDeque;
use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event as Frame, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream;
use serde::Serialize;
use swifty_plugin_api::{Health, Readiness, State as DeviceState};
use tokio::sync::watch;
use tokio::time::{interval_at, Instant, Interval, MissedTickBehavior};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::guards::Passive;
use super::RouterState;
use crate::app::App;
use crate::error::ApiError;
use crate::events::{Published, StreamSlot, Viewer, MAX_STREAMS_PER_SESSION};
use crate::sessions;

/// How often an idle stream sends a comment line.
pub const HEARTBEAT: Duration = Duration::from_secs(20);
/// How often an idle stream checks that its session can still act.
pub const SESSION_CHECK: Duration = Duration::from_secs(2);

const LAST_EVENT_ID: &str = "last-event-id";

pub(crate) fn routes() -> OpenApiRouter<RouterState> {
    OpenApiRouter::new().routes(routes!(follow))
}

// ----- what each event's `data` is

/// `run`: an action run changed phase. For the person who started it, and the owner.
#[derive(Debug, Serialize, ToSchema)]
pub struct RunEvent {
    pub run_id: String,
    pub action: String,
    pub target: String,
    /// `commanded`, `confirmed`, `no_response`, `failed` or `simulated`.
    pub phase: String,
}

/// `state`: a read of a device returned something different from the last read of
/// it. For anyone who may see the device.
#[derive(Debug, Serialize, ToSchema)]
pub struct StateEvent {
    pub target: String,
    pub state: DeviceState,
}

/// `devices`, and `resync`: an empty object. `devices` means the device index was
/// rebuilt (for everyone); `resync` that this stream missed events. Fetch again.
#[derive(Debug, Serialize, ToSchema)]
pub struct DevicesEvent {}

/// `plugin`: an instance's readiness or health changed. For the owner.
#[derive(Debug, Serialize, ToSchema)]
pub struct PluginEvent {
    pub instance: String,
    pub readiness: Readiness,
    pub health: Health,
}

/// `setup`: a waiting setup step moved on. For the session that owns the setup,
/// which fetches the step with `GET /api/v1/setup/{setup_id}`.
#[derive(Debug, Serialize, ToSchema)]
pub struct SetupEvent {
    pub setup_id: String,
}

/// `assistant`: a Swifty job has news. For its person, who fetches the job with
/// `GET /api/v1/assistant/jobs/{job_id}`.
#[derive(Debug, Serialize, ToSchema)]
pub struct AssistantEvent {
    pub job_id: String,
}

/// Follows what changes, as Server-Sent Events.
///
/// Each event has an `id`, a name in `event` and JSON in `data`:
///
/// ```text
/// id: 1042
/// event: run
/// data: {"run_id":"...","action":"light.power","target":"bulb1","phase":"confirmed"}
/// ```
///
/// | `event` | `data` | Sent when, and to whom |
/// | --- | --- | --- |
/// | `run` | `RunEvent` | an action run changes phase: the person who started it, and the owner |
/// | `state` | `StateEvent` | a read of a device differs from the last one: anyone who may see it |
/// | `devices` | `DevicesEvent` (`{}`) | the device index was rebuilt: everyone, who fetch again |
/// | `plugin` | `PluginEvent` | an instance's readiness or health changed: the owner |
/// | `setup` | `SetupEvent` | a waiting setup step moved on: the session that owns it |
/// | `assistant` | `AssistantEvent` | a Swifty job has news: its person |
/// | `resync` | `{}` | the `Last-Event-ID` sent is older than the hub keeps: fetch everything |
///
/// Send `Last-Event-ID` to resume: the hub keeps the last 1024 events. A comment line
/// every 20 seconds keeps the connection open. The stream ends when the session ends
/// or locks. At most four streams per session (`429 too_many_streams`). Holding a
/// stream open is not activity: the session still reaches its idle limit.
#[utoipa::path(
    get,
    path = "/events",
    tag = "events",
    security(("session" = [])),
    params(("Last-Event-ID" = Option<String>, Header, description = "The id of the last event received, to resume after it.")),
    responses(
        (status = 200, description = "The stream, until the session ends or locks.", content_type = "text/event-stream", body = String),
        (status = 429, description = "This session already has four streams open (`too_many_streams`).", body = super::openapi::ErrorEnvelope),
    )
)]
async fn follow(State(app): State<App>, Passive(principal): Passive, headers: HeaderMap) -> Result<Response, ApiError> {
    let viewer = Viewer {
        session_ref: principal.session_ref.clone(),
        user_id: principal.person.user_id.clone(),
        role: principal.person.role,
    };
    let slot = app.events.open_stream(&viewer.session_ref).ok_or_else(|| {
        ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_streams",
            format!("This session already has {MAX_STREAMS_PER_SESSION} event streams open."),
        )
    })?;
    // Before reading the bus, so nothing published in between is missed.
    let changes = app.events.subscribe();
    // Marked as changed, so the stream checks its session once more as it starts:
    // a lock written after the lookup above and before this is not missed.
    let mut sessions = app.events.session_changes();
    sessions.mark_changed();
    let resume_from =
        headers.get(LAST_EVENT_ID).map(|value| value.to_str().ok().and_then(|id| app.events.parse_id(id)));
    let mut follower = Follower {
        cursor: app.events.latest(),
        app,
        viewer,
        _slot: slot,
        changes,
        sessions,
        pending: VecDeque::new(),
        announced: None,
        heartbeat: ticker(HEARTBEAT),
        check: ticker(SESSION_CHECK),
    };
    match resume_from {
        // A new client starts from now.
        None => {}
        Some(Some(seq)) => {
            follower.cursor = seq;
            follower.announced = Some(seq);
            follower.catch_up();
        }
        // An id this hub did not hand out, or one from before it restarted.
        Some(None) => follower.resync(),
    }
    let mut response = Sse::new(stream::unfold(follower, Follower::next)).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    Ok(response)
}

fn ticker(period: Duration) -> Interval {
    let mut interval = interval_at(Instant::now() + period, period);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    interval
}

/// One open stream.
struct Follower {
    app: App,
    viewer: Viewer,
    _slot: StreamSlot,
    changes: watch::Receiver<u64>,
    /// Changes when a session was locked or ended somewhere.
    sessions: watch::Receiver<u64>,
    /// The sequence number of the last event this stream has dealt with, sent or not.
    cursor: u64,
    /// Frames ready to go out.
    pending: VecDeque<Frame>,
    /// The last id the client was given, which is what it resumes from.
    announced: Option<u64>,
    heartbeat: Interval,
    check: Interval,
}

impl Follower {
    async fn next(mut self) -> Option<(Result<Frame, Infallible>, Self)> {
        loop {
            if let Some(frame) = self.pending.pop_front() {
                return Some((Ok(frame), self));
            }
            tokio::select! {
                changed = self.changes.changed() => {
                    if changed.is_err() {
                        return None;
                    }
                    self.catch_up();
                    // Nothing goes out to a session that has ended or locked since
                    // the last check.
                    if !self.pending.is_empty() && !self.may_continue() {
                        return None;
                    }
                }
                changed = self.sessions.changed() => {
                    if changed.is_err() || !self.may_continue() {
                        return None;
                    }
                }
                _ = self.heartbeat.tick() => {
                    let frame = self.heartbeat_frame();
                    self.pending.push_back(frame);
                }
                _ = self.check.tick() => {
                    if !self.may_continue() {
                        return None;
                    }
                }
            }
        }
    }

    /// Queues every event after the cursor that this session may see, or a
    /// `resync` when some of them have left the buffer.
    fn catch_up(&mut self) {
        let Ok(published) = self.app.events.since(self.cursor) else {
            return self.resync();
        };
        let index = self.app.registry.index();
        for event in published {
            self.cursor = event.seq;
            if self.viewer.may_receive(&event.event, &index) {
                self.pending.push_back(self.frame(&event));
                self.announced = Some(event.seq);
            }
        }
    }

    /// Tells the client it has missed events and must refetch everything, and
    /// carries on from now.
    fn resync(&mut self) {
        self.cursor = self.app.events.latest();
        self.pending.clear();
        self.pending.push_back(Frame::default().event("resync").id(self.app.events.id(self.cursor)).data("{}"));
        self.announced = Some(self.cursor);
    }

    fn frame(&self, published: &Published) -> Frame {
        Frame::default()
            .event(published.event.kind())
            .id(self.app.events.id(published.seq))
            .data(published.event.data().to_string())
    }

    /// A comment. When events this session may not see have gone by, it also
    /// moves the client's `Last-Event-ID` past them, without firing anything, so
    /// a later resume does not start from too far back.
    fn heartbeat_frame(&mut self) -> Frame {
        let frame = Frame::default().comment("");
        if self.announced == Some(self.cursor) {
            return frame;
        }
        self.announced = Some(self.cursor);
        frame.id(self.app.events.id(self.cursor))
    }

    /// Whether the session is still signed in, unlocked and within its limits.
    /// Checking does not touch it.
    fn may_continue(&self) -> bool {
        let app = &self.app;
        match app.store.with(|conn| sessions::is_live(conn, app.now(), app.limits(), &self.viewer.session_ref)) {
            Ok(live) => live,
            Err(error) => {
                tracing::warn!(%error, "closed an event stream: its session could not be checked");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use swifty_plugin_api::{Examples, HealthStatus};

    use super::*;
    use crate::actions::Phase;
    use crate::events::Event;

    fn data<T: Serialize>(typed: T) -> serde_json::Value {
        serde_json::to_value(typed).unwrap()
    }

    /// What the document says each event carries is what the bus sends.
    #[test]
    fn each_event_carries_what_the_document_says() {
        let text = |s: &str| s.to_string();
        let run = Event::Run {
            run_id: text("r1"),
            action: text("light.power"),
            target: text("bulb1"),
            phase: Phase::Confirmed,
            started_by: text("u1"),
        };
        let typed = RunEvent {
            run_id: text("r1"),
            action: text("light.power"),
            target: text("bulb1"),
            phase: text("confirmed"),
        };
        assert_eq!(run.data(), data(typed));
        for state in DeviceState::examples() {
            let event = Event::State { target: text("bulb1"), state: state.clone() };
            assert_eq!(event.data(), data(StateEvent { target: text("bulb1"), state }));
        }
        assert_eq!(Event::Devices.data(), data(DevicesEvent {}));
        for readiness in Readiness::examples() {
            let health = Health { status: HealthStatus::Degraded, detail: Some(text("1 of 2 answer.")) };
            let event = Event::Plugin { instance: text("hue"), readiness: readiness.clone(), health: health.clone() };
            assert_eq!(event.data(), data(PluginEvent { instance: text("hue"), readiness, health }));
        }
        let setup = Event::Setup { setup_id: text("s1"), session_ref: text("ref") };
        assert_eq!(setup.data(), data(SetupEvent { setup_id: text("s1") }));
        let job = Event::Assistant { job_id: text("j1"), user_id: text("u1") };
        assert_eq!(job.data(), data(AssistantEvent { job_id: text("j1") }));
    }
}
