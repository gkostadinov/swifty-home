//! The event bus behind the event stream (`GET /api/v1/events`).
//!
//! Whatever changes in the hub is published here as a typed [`Event`]. The bus
//! numbers each one and keeps the last [`BUFFER`] of them, so a client that
//! reconnects with `Last-Event-ID` gets what it missed, and one that has been
//! away too long is told to refetch everything instead. Nobody is ever sent an
//! event they may not see: [`Viewer::may_receive`] is checked for every event and
//! every stream, at the moment it is sent.
//!
//! The bus holds no state of its own beyond that buffer. Publishing never blocks
//! on a slow client: a stream that falls more than the buffer behind is resynced.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::{json, Value};
use swifty_plugin_api::{Health, Readiness, State};
use tokio::sync::watch;

use crate::actions::Phase;
use crate::crypto::random_bytes;
use crate::policy::{self, Role};
use crate::runtime::index::DeviceIndex;

/// How many events the bus keeps for clients that resume.
pub const BUFFER: usize = 1024;
/// How many streams one session may hold open at once.
pub const MAX_STREAMS_PER_SESSION: usize = 4;

/// Something that changed. Each variant carries what decides who gets it, and
/// [`Event::data`] is the part a client is sent.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// An action run changed phase. For the person who started it, and the owner.
    Run {
        run_id: String,
        action: String,
        target: String,
        phase: Phase,
        /// The user id of the person who started the run. Not sent.
        started_by: String,
    },
    /// A read of a device returned something different from the last read of it.
    /// For anyone who may see the device.
    State { target: String, state: State },
    /// The device index was rebuilt. For everyone; the client refetches.
    Devices,
    /// An instance's readiness or health changed. For the owner.
    Plugin { instance: String, readiness: Readiness, health: Health },
    /// A waiting setup step moved on. For the session that owns the setup; the
    /// client fetches the step itself.
    Setup {
        setup_id: String,
        /// The session that started the setup. Not sent.
        session_ref: String,
    },
    /// A Swifty job has news. For its person; the client fetches the job.
    Assistant {
        job_id: String,
        /// The user id of the person the job belongs to. Not sent.
        user_id: String,
    },
}

/// Who an event is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recipients<'a> {
    Everyone,
    Owner,
    /// This user id, and the owner.
    PersonAndOwner(&'a str),
    /// This user id only.
    Person(&'a str),
    /// This session only.
    Session(&'a str),
    /// Anyone who may see this device.
    MaySee(&'a str),
}

impl Event {
    /// The SSE event name.
    pub fn kind(&self) -> &'static str {
        match self {
            Event::Run { .. } => "run",
            Event::State { .. } => "state",
            Event::Devices => "devices",
            Event::Plugin { .. } => "plugin",
            Event::Setup { .. } => "setup",
            Event::Assistant { .. } => "assistant",
        }
    }

    /// The JSON a client is sent. Who the event is for is never part of it.
    pub fn data(&self) -> Value {
        match self {
            Event::Run { run_id, action, target, phase, started_by: _ } => {
                json!({ "run_id": run_id, "action": action, "target": target, "phase": phase.as_str() })
            }
            Event::State { target, state } => json!({ "target": target, "state": state }),
            Event::Devices => json!({}),
            Event::Plugin { instance, readiness, health } => {
                json!({ "instance": instance, "readiness": readiness, "health": health })
            }
            Event::Setup { setup_id, session_ref: _ } => json!({ "setup_id": setup_id }),
            Event::Assistant { job_id, user_id: _ } => json!({ "job_id": job_id }),
        }
    }

    pub fn recipients(&self) -> Recipients<'_> {
        match self {
            Event::Run { started_by, .. } => Recipients::PersonAndOwner(started_by),
            Event::State { target, .. } => Recipients::MaySee(target),
            Event::Devices => Recipients::Everyone,
            Event::Plugin { .. } => Recipients::Owner,
            Event::Setup { session_ref, .. } => Recipients::Session(session_ref),
            Event::Assistant { user_id, .. } => Recipients::Person(user_id),
        }
    }
}

/// The session at the other end of a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    pub session_ref: String,
    pub user_id: String,
    pub role: Role,
}

impl Viewer {
    /// Whether this session may be sent this event. `index` is the device index
    /// as it is now, so a device's audience is the one it has when the event goes out.
    pub fn may_receive(&self, event: &Event, index: &DeviceIndex) -> bool {
        let owner = self.role == Role::Owner;
        match event.recipients() {
            Recipients::Everyone => true,
            Recipients::Owner => owner,
            Recipients::PersonAndOwner(user_id) => owner || self.user_id == user_id,
            Recipients::Person(user_id) => self.user_id == user_id,
            Recipients::Session(session_ref) => self.session_ref == session_ref,
            Recipients::MaySee(target) => policy::may_see(index, self.role, target),
        }
    }
}

/// An event with its place in the sequence.
#[derive(Debug, PartialEq)]
pub struct Published {
    pub seq: u64,
    pub event: Event,
}

/// The events after a client's position are no longer all in the buffer, or the
/// position was never handed out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap;

struct Ring {
    events: VecDeque<Arc<Published>>,
    /// The sequence number of the last event published, 0 before the first.
    last: u64,
}

type StreamCounts = Arc<Mutex<HashMap<String, usize>>>;

pub struct EventBus {
    /// Random per start of the hub. Sequence numbers start again at 1 on every
    /// start, so an id from before a restart must not be mistaken for a new one.
    boot: String,
    capacity: usize,
    ring: Mutex<Ring>,
    /// The last sequence number, for waking streams.
    latest: watch::Sender<u64>,
    /// Moves whenever a session was locked or ended, so every stream checks its
    /// own at once instead of at its next periodic check.
    sessions: watch::Sender<u64>,
    streams: StreamCounts,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    pub fn new() -> Self {
        Self::with_capacity(BUFFER)
    }

    fn with_capacity(capacity: usize) -> Self {
        Self {
            boot: hex::encode(random_bytes::<4>()),
            capacity,
            ring: Mutex::new(Ring { events: VecDeque::with_capacity(capacity), last: 0 }),
            latest: watch::Sender::new(0),
            sessions: watch::Sender::new(0),
            streams: StreamCounts::default(),
        }
    }

    /// Tells every open stream to check its session now. Call it once a lock, a
    /// sign-out, an ended session, a revoked passkey or a removed person has been
    /// written, so a stream of that session ends at once. Idle and absolute
    /// expiry announce nothing; the streams' own periodic check finds those.
    pub fn recheck_sessions(&self) {
        self.sessions.send_modify(|changes| *changes = changes.wrapping_add(1));
    }

    /// A receiver that changes whenever [`EventBus::recheck_sessions`] is called.
    pub fn session_changes(&self) -> watch::Receiver<u64> {
        self.sessions.subscribe()
    }

    fn ring(&self) -> MutexGuard<'_, Ring> {
        self.ring.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Numbers the event, keeps it, and wakes every stream. Returns its sequence number.
    pub fn publish(&self, event: Event) -> u64 {
        let mut ring = self.ring();
        ring.last += 1;
        let seq = ring.last;
        if ring.events.len() == self.capacity {
            ring.events.pop_front();
        }
        ring.events.push_back(Arc::new(Published { seq, event }));
        // Inside the lock, so the value only ever moves forward.
        self.latest.send_replace(seq);
        seq
    }

    /// The sequence number of the last event published, 0 before the first.
    pub fn latest(&self) -> u64 {
        self.ring().last
    }

    /// Every event after `seq`, oldest first. A [`Gap`] when some of them have
    /// already left the buffer, or when `seq` is ahead of the bus.
    pub fn since(&self, seq: u64) -> Result<Vec<Arc<Published>>, Gap> {
        let ring = self.ring();
        if seq > ring.last {
            return Err(Gap);
        }
        let oldest = ring.events.front().map_or(ring.last + 1, |published| published.seq);
        if seq + 1 < oldest {
            return Err(Gap);
        }
        Ok(ring.events.iter().filter(|published| published.seq > seq).cloned().collect())
    }

    /// A receiver that changes whenever an event is published. Take it before
    /// asking [`EventBus::since`], so nothing published in between is missed.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.latest.subscribe()
    }

    /// The `id:` a client is sent for this sequence number.
    pub fn id(&self, seq: u64) -> String {
        format!("{}-{seq}", self.boot)
    }

    /// The sequence number in an id this bus handed out. `None` for anything
    /// else, including an id from before the hub restarted.
    pub fn parse_id(&self, id: &str) -> Option<u64> {
        let (boot, seq) = id.trim().split_once('-')?;
        if boot != self.boot || seq.is_empty() || !seq.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        seq.parse().ok()
    }

    /// Takes one of the session's stream places, or `None` when all
    /// [`MAX_STREAMS_PER_SESSION`] are taken. The place is given back when the
    /// slot is dropped.
    pub fn open_stream(&self, session_ref: &str) -> Option<StreamSlot> {
        let mut streams = self.streams.lock().unwrap_or_else(PoisonError::into_inner);
        let open = streams.entry(session_ref.to_string()).or_insert(0);
        if *open >= MAX_STREAMS_PER_SESSION {
            return None;
        }
        *open += 1;
        Some(StreamSlot { streams: self.streams.clone(), session_ref: session_ref.to_string() })
    }

    /// How many streams this session has open.
    pub fn open_streams(&self, session_ref: &str) -> usize {
        self.streams.lock().unwrap_or_else(PoisonError::into_inner).get(session_ref).copied().unwrap_or(0)
    }
}

/// One open stream's place in its session's allowance.
pub struct StreamSlot {
    streams: StreamCounts,
    session_ref: String,
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        let mut streams = self.streams.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(open) = streams.get_mut(&self.session_ref) {
            *open = open.saturating_sub(1);
            if *open == 0 {
                streams.remove(&self.session_ref);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use swifty_plugin_api::HealthStatus;

    use super::*;
    use crate::runtime::index;

    fn viewer(role: Role, user_id: &str, session_ref: &str) -> Viewer {
        Viewer { session_ref: session_ref.to_string(), user_id: user_id.to_string(), role }
    }

    fn run(by: &str) -> Event {
        Event::Run {
            run_id: "r1".into(),
            action: "light.power".into(),
            target: "bulb1".into(),
            phase: Phase::Confirmed,
            started_by: by.into(),
        }
    }

    #[test]
    fn events_are_numbered_from_one_and_kept_in_order() {
        let bus = EventBus::new();
        assert_eq!(bus.latest(), 0);
        assert_eq!(bus.since(0).unwrap().len(), 0);
        assert_eq!(bus.publish(Event::Devices), 1);
        assert_eq!(bus.publish(run("u1")), 2);
        let all: Vec<u64> = bus.since(0).unwrap().iter().map(|p| p.seq).collect();
        assert_eq!(all, [1, 2]);
        let after_first: Vec<u64> = bus.since(1).unwrap().iter().map(|p| p.seq).collect();
        assert_eq!(after_first, [2]);
        assert!(bus.since(2).unwrap().is_empty());
        assert_eq!(bus.since(3), Err(Gap), "a position ahead of the bus was never handed out");
    }

    #[test]
    fn the_buffer_keeps_the_last_events_only() {
        let bus = EventBus::with_capacity(3);
        for _ in 0..5 {
            bus.publish(Event::Devices);
        }
        assert_eq!(bus.since(1), Err(Gap), "event 2 has left the buffer");
        let kept: Vec<u64> = bus.since(2).unwrap().iter().map(|p| p.seq).collect();
        assert_eq!(kept, [3, 4, 5]);
        assert_eq!(bus.since(5).unwrap().len(), 0);
        assert_eq!(EventBus::new().capacity, 1024);
    }

    #[test]
    fn ids_belong_to_one_start_of_the_hub() {
        let bus = EventBus::new();
        let id = bus.id(42);
        assert_eq!(bus.parse_id(&id), Some(42));
        assert_eq!(bus.parse_id(&format!(" {id} ")), Some(42));
        let other = EventBus::new();
        assert_ne!(bus.boot, other.boot);
        assert_eq!(other.parse_id(&id), None, "an id from another start is not resumed");
        for garbage in ["", "42", "-42", &format!("{}-", bus.boot), &format!("{}-+4", bus.boot), "x-y-z"] {
            assert_eq!(bus.parse_id(garbage), None, "{garbage:?}");
        }
    }

    #[test]
    fn a_session_has_four_stream_places() {
        let bus = EventBus::new();
        let slots: Vec<StreamSlot> = (0..4).map(|_| bus.open_stream("s1").unwrap()).collect();
        assert!(bus.open_stream("s1").is_none());
        assert!(bus.open_stream("s2").is_some(), "another session has its own places");
        assert_eq!(bus.open_streams("s1"), 4);
        drop(slots);
        assert_eq!(bus.open_streams("s1"), 0);
        assert!(bus.streams.lock().unwrap().is_empty(), "nothing is kept for a session with no streams");
        let _again = bus.open_stream("s1").unwrap();
        assert_eq!(bus.open_streams("s1"), 1);
    }

    #[test]
    fn each_event_reaches_only_its_recipients() {
        let index = index::legacy_seeded();
        let owner = viewer(Role::Owner, "owner", "s-owner");
        let family = viewer(Role::Family, "kid", "s-kid");
        let state = |target: &str| Event::State { target: target.into(), state: State::Plug { on: true } };
        let plugin = Event::Plugin {
            instance: "wiz".into(),
            readiness: Readiness::Ready,
            health: Health { status: HealthStatus::Ok, detail: None },
        };
        let setup = |session: &str| Event::Setup { setup_id: "x".into(), session_ref: session.into() };
        let job = |user: &str| Event::Assistant { job_id: "j".into(), user_id: user.into() };

        assert!(index.family_may_see("bulb1") && !index.family_may_see("vm100"));
        let cases = [
            (Event::Devices, true, true),
            (plugin, true, false),
            (run("owner"), true, false),
            (run("kid"), true, true),
            (state("bulb1"), true, true),
            (state("vm100"), true, false),
            (state("not-a-device"), true, false),
            (setup("s-owner"), true, false),
            (setup("s-kid"), false, true),
            (job("owner"), true, false),
            (job("kid"), false, true),
        ];
        for (event, to_owner, to_family) in cases {
            assert_eq!(owner.may_receive(&event, &index), to_owner, "owner, {event:?}");
            assert_eq!(family.may_receive(&event, &index), to_family, "family, {event:?}");
        }
    }

    #[test]
    fn what_a_client_is_sent_never_names_the_recipients() {
        let events = [
            run("user-secret-id"),
            Event::Setup { setup_id: "x".into(), session_ref: "session-secret-ref".into() },
            Event::Assistant { job_id: "j".into(), user_id: "user-secret-id".into() },
        ];
        for event in events {
            let data = event.data().to_string();
            assert!(!data.contains("secret"), "{data}");
        }
        assert_eq!(
            run("u").data(),
            json!({ "run_id": "r1", "action": "light.power", "target": "bulb1", "phase": "confirmed" })
        );
        assert_eq!(Event::Devices.data(), json!({}));
        assert_eq!(Event::State { target: "plug1".into(), state: State::Plug { on: false } }.kind(), "state");
    }
}
