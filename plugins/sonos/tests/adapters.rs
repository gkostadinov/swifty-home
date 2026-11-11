//! The hub's Sonos adapter tests, ported: the driver against its mock speakers on loopback.

mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::Instance;
use serde_json::json;
use swifty_plugin_api::{DeviceError, Op, State};
use swifty_plugin_testkit::devices::MockSonos;

async fn sonos() -> (MockSonos, MockSonos, Instance) {
    let arc = MockSonos::start(true).await;
    let sub = MockSonos::start(false).await;
    let config = json!({ "speakers": [
        { "id": "sonos", "url": arc.http.url },
        { "id": "sub", "url": sub.http.url, "read_only": true },
    ]});
    (arc, sub, Instance::connect(config))
}

#[tokio::test]
async fn sonos_volume_and_mute_use_rendering_control() {
    let (arc, _sub, adapter) = sonos().await;
    adapter.command("sonos", &Op::Volume { level: 41 }).await.unwrap();
    adapter.command("sonos", &Op::Mute { muted: true }).await.unwrap();

    let requests = arc.http.requests();
    assert_eq!(requests[0].path, "/MediaRenderer/RenderingControl/Control");
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].header("soapaction"), "\"urn:schemas-upnp-org:service:RenderingControl:1#SetVolume\"");
    assert!(requests[0].header("content-type").starts_with("text/xml"));
    assert!(requests[0].body.contains("<DesiredVolume>41</DesiredVolume>"));
    assert!(requests[0].body.contains("<Channel>Master</Channel>"));
    assert!(requests[1].body.contains("<DesiredMute>1</DesiredMute>"));
    assert_eq!(arc.state.lock().unwrap().volume, 41);
    assert!(arc.state.lock().unwrap().muted);
}

#[tokio::test]
async fn sonos_play_and_pause_use_av_transport() {
    let (arc, _sub, adapter) = sonos().await;
    adapter.command("sonos", &Op::Pause).await.unwrap();
    assert_eq!(arc.state.lock().unwrap().transport, "PAUSED_PLAYBACK");
    adapter.command("sonos", &Op::Play).await.unwrap();
    assert_eq!(arc.state.lock().unwrap().transport, "PLAYING");
    let requests = arc.http.requests();
    assert!(requests.iter().all(|r| r.path == "/MediaRenderer/AVTransport/Control"));
    assert!(requests[1].body.contains("<Speed>1</Speed>"));
}

#[tokio::test]
async fn sonos_state_is_read_from_the_speaker() {
    let (arc, _sub, adapter) = sonos().await;
    arc.state.lock().unwrap().volume = 33;
    assert_eq!(
        adapter.read_state("sonos").await.unwrap(),
        State::Speaker { vol: 33, mute: false, playing: true, source: "TV".to_string(), night: Some(false) }
    );
    arc.state.lock().unwrap().uri = "x-sonos-spotify:abc".to_string();
    arc.state.lock().unwrap().transport = "PAUSED_PLAYBACK".to_string();
    let state = adapter.read_state("sonos").await.unwrap();
    assert_eq!(
        state,
        State::Speaker { vol: 33, mute: false, playing: false, source: "Music".to_string(), night: Some(false) }
    );
}

#[tokio::test]
async fn sonos_night_mode_goes_out_as_set_eq_and_reads_back() {
    let (arc, _sub, adapter) = sonos().await;
    adapter.command("sonos", &Op::NightMode { on: true }).await.unwrap();
    let requests = arc.http.requests();
    assert_eq!(requests[0].path, "/MediaRenderer/RenderingControl/Control");
    assert_eq!(requests[0].header("soapaction"), "\"urn:schemas-upnp-org:service:RenderingControl:1#SetEQ\"");
    assert!(requests[0]
        .body
        .contains("<InstanceID>0</InstanceID><EQType>NightMode</EQType><DesiredValue>1</DesiredValue>"));
    assert!(arc.state.lock().unwrap().night);

    let state = adapter.read_state("sonos").await.unwrap();
    assert!(matches!(state, State::Speaker { night: Some(true), .. }), "{state:?}");
    assert!(Op::NightMode { on: true }.is_confirmed_by(&state, None));
    assert!(!Op::NightMode { on: false }.is_confirmed_by(&state, None));

    adapter.command("sonos", &Op::NightMode { on: false }).await.unwrap();
    assert!(arc.http.requests().iter().any(|r| r.body.contains("<DesiredValue>0</DesiredValue>")));
    assert!(!arc.state.lock().unwrap().night);
    let state = adapter.read_state("sonos").await.unwrap();
    assert!(Op::NightMode { on: false }.is_confirmed_by(&state, None));
}

#[tokio::test]
async fn a_speaker_that_refuses_get_eq_still_reads_and_has_no_night_mode() {
    let (arc, _sub, adapter) = sonos().await;
    arc.state.lock().unwrap().eq_refused = true;
    let state = adapter.read_state("sonos").await.unwrap();
    assert_eq!(state, State::Speaker { vol: 24, mute: false, playing: true, source: "TV".to_string(), night: None });
    assert!(arc.actions().contains(&"GetEQ".to_string()));
    let shown = serde_json::to_value(&state).unwrap();
    assert!(shown.get("night").is_none(), "{shown}");
    assert!(!Op::NightMode { on: false }.is_confirmed_by(&state, None), "unknown is not off");
}

#[tokio::test]
async fn night_mode_is_for_the_arc_and_never_the_sub() {
    let (_arc, sub, adapter) = sonos().await;
    let op = Op::NightMode { on: true };
    assert!(adapter.refusal("sub", &op).is_some());
    assert!(matches!(adapter.command("sub", &op).await, Err(DeviceError::Disabled(_))));
    assert_eq!(sub.http.traffic(), 0);
    assert!(adapter.refusal("sonos", &op).is_none());
}

#[tokio::test]
async fn sonos_cap_only_ever_lowers_the_volume() {
    let (arc, _sub, adapter) = sonos().await;
    arc.state.lock().unwrap().volume = 12;
    adapter.command("sonos", &Op::VolumeCap { max: 30 }).await.unwrap();
    assert_eq!(arc.actions(), ["GetVolume"]);
    assert_eq!(arc.state.lock().unwrap().volume, 12);

    arc.state.lock().unwrap().volume = 70;
    adapter.command("sonos", &Op::VolumeCap { max: 30 }).await.unwrap();
    assert_eq!(arc.state.lock().unwrap().volume, 30);
}

#[tokio::test]
async fn sonos_faults_are_refusals() {
    let (arc, _sub, adapter) = sonos().await;
    arc.state.lock().unwrap().fault = Some(701);
    match adapter.command("sonos", &Op::Play).await {
        Err(DeviceError::Rejected(text)) => assert!(text.contains("701"), "{text}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_sub_is_read_only_and_never_asked_for_av_transport() {
    let (_arc, sub, adapter) = sonos().await;
    sub.state.lock().unwrap().volume = 15;
    assert_eq!(
        adapter.read_state("sub").await.unwrap(),
        State::Speaker { vol: 15, mute: false, playing: false, source: "Sub".to_string(), night: None }
    );
    assert!(sub.http.requests().iter().all(|r| r.path == "/MediaRenderer/RenderingControl/Control"));
    let before = sub.http.requests().len();

    for op in [Op::Volume { level: 1 }, Op::Mute { muted: true }, Op::Play, Op::Pause] {
        assert!(adapter.refusal("sub", &op).is_some());
        assert!(matches!(adapter.command("sub", &op).await, Err(DeviceError::Disabled(_))));
    }
    assert_eq!(sub.http.requests().len(), before, "a refused command must reach nothing");
}

// ----- every adapter has a read timeout, and none retries

/// Generous, but far below the 120 seconds a hung mock would take.
const OUTER_LIMIT: Duration = Duration::from_secs(5);

async fn assert_gives_up<T: std::fmt::Debug>(
    name: &str,
    call: impl std::future::Future<Output = Result<T, DeviceError>>,
) {
    let started = Instant::now();
    let result = tokio::time::timeout(OUTER_LIMIT, call).await.unwrap_or_else(|_| panic!("{name} never gave up"));
    assert!(started.elapsed() < OUTER_LIMIT, "{name}");
    assert!(matches!(&result, Err(DeviceError::Timeout)), "{name}: {result:?}");
}

/// The Sonos part of the hub's test of the same name.
#[tokio::test]
async fn a_device_that_never_answers_times_out_and_is_asked_once() {
    let arc = MockSonos::start(true).await;
    arc.http.hang.store(true, Ordering::SeqCst);
    let adapter = Instance::connect(json!({ "speakers": [{ "id": "sonos", "url": arc.http.url }] }));
    assert_gives_up("sonos", adapter.command("sonos", &Op::Volume { level: 5 })).await;
    assert_eq!(arc.http.requests().len(), 1);
}

// ----- new with the plugin

#[tokio::test]
async fn a_device_the_config_does_not_name_is_refused_without_a_request() {
    let (arc, sub, adapter) = sonos().await;
    let op = Op::Volume { level: 5 };
    assert!(adapter.refusal("kitchen", &op).unwrap().contains("not a speaker"));
    assert!(matches!(adapter.command("kitchen", &op).await, Err(DeviceError::Disabled(_))));
    assert!(matches!(adapter.read_state("kitchen").await, Err(DeviceError::Disabled(_))));
    assert!(adapter.refusal("sonos", &Op::TvPower { on: true }).is_some());
    assert!(matches!(adapter.command("sonos", &Op::TvPower { on: true }).await, Err(DeviceError::Disabled(_))));
    assert_eq!(arc.http.traffic() + sub.http.traffic(), 0);
}
