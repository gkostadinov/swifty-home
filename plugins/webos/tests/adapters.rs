//! The hub's webOS adapter tests, ported: the driver against its mock TV on loopback.

mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::{try_connect, tv_config, Instance};
use serde_json::json;
use swifty_plugin_api::{DeviceError, Op, State};
use swifty_plugin_testkit::devices::{MockTv, MockWol, POINTER_PATH, TV_KEY};
use swifty_plugin_webos::PERMISSIONS;

async fn webos() -> (MockTv, Instance) {
    let tv = MockTv::start().await;
    let adapter = Instance::connect(tv_config(&tv, None));
    (tv, adapter)
}

#[tokio::test]
async fn webos_presents_the_key_and_never_forces_pairing() {
    let (tv, adapter) = webos().await;
    adapter.read_state("tv").await.unwrap();
    let received = tv.received.lock().unwrap();
    let register = received.iter().find(|m| m["type"] == "register").unwrap();
    assert_eq!(register["payload"]["client-key"], json!(TV_KEY));
    assert_eq!(register["payload"]["forcePairing"], json!(false));
    assert_eq!(tv.prompts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn webos_never_connects_without_a_key_file() {
    let tv = MockTv::start().await;
    assert!(try_connect(tv_config(&tv, None), None).is_err(), "no key in the secret slot");
    assert_eq!(tv.traffic(), 0);
}

#[tokio::test]
async fn webos_hangs_up_on_a_pairing_prompt_and_stops_trying() {
    let (tv, adapter) = webos().await;
    tv.state.lock().unwrap().accepted_key = "some other key".to_string();

    let first = adapter.command("tv", &Op::TvPower { on: false }).await;
    let Err(DeviceError::Disabled(text)) = first else { panic!("{first:?}") };
    assert!(text.contains("Nothing here starts pairing"), "{text}");
    assert_eq!(tv.prompts.load(Ordering::SeqCst), 1);
    assert_eq!(tv.traffic(), 1);

    // No further prompts: every later call is refused before it connects.
    let again = adapter.command("tv", &Op::TvInput { input: "HDMI 2".into() }).await;
    assert!(matches!(again, Err(DeviceError::Disabled(_))));
    assert!(matches!(adapter.read_state("tv").await, Err(DeviceError::Disabled(_))));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(tv.traffic(), 1);
    assert_eq!(tv.prompts.load(Ordering::SeqCst), 1);
    assert!(tv.uris().is_empty(), "no request may follow a refused key");
}

#[tokio::test]
async fn webos_reads_power_and_input() {
    let (tv, adapter) = webos().await;
    assert_eq!(adapter.read_state("tv").await.unwrap(), State::Tv { on: true, input: "HDMI 1".to_string() });
    tv.state.lock().unwrap().app = "netflix".to_string();
    assert_eq!(adapter.read_state("tv").await.unwrap(), State::Tv { on: true, input: "netflix".to_string() });
    tv.state.lock().unwrap().power = "Screen Off".to_string();
    assert_eq!(adapter.read_state("tv").await.unwrap(), State::Tv { on: false, input: String::new() });
}

#[tokio::test]
async fn webos_input_launches_the_input_app() {
    let (tv, adapter) = webos().await;
    adapter.command("tv", &Op::TvInput { input: "HDMI 3".into() }).await.unwrap();
    assert_eq!(tv.state.lock().unwrap().app, "com.webos.app.hdmi3");
    assert_eq!(tv.uris(), ["ssap://system.launcher/launch"]);
    assert!(matches!(
        adapter.command("tv", &Op::TvInput { input: "HDMI 9".into() }).await,
        Err(DeviceError::Disabled(_))
    ));
}

#[tokio::test]
async fn webos_silence_after_turning_off_is_an_error_and_never_a_reading_of_off() {
    let (tv, adapter) = webos().await;
    adapter.command("tv", &Op::TvPower { on: false }).await.unwrap();
    assert_eq!(tv.uris(), ["ssap://system/turnOff"]);
    let result = adapter.read_state("tv").await;
    assert!(result.as_ref().unwrap_err().is_silence(), "{result:?}");
}

#[tokio::test]
async fn webos_a_tv_that_says_it_is_off_does_read_as_off() {
    let (tv, adapter) = webos().await;
    tv.state.lock().unwrap().power = "Screen Off".to_string();
    let state = adapter.read_state("tv").await.unwrap();
    assert!(Op::TvPower { on: false }.is_confirmed_by(&state, None));
}

#[tokio::test]
async fn webos_a_hung_tv_is_an_error_and_never_a_reading_of_off() {
    let (tv, adapter) = webos().await;
    tv.hang.store(true, Ordering::SeqCst);
    let result = tokio::time::timeout(OUTER_LIMIT, adapter.read_state("tv")).await.unwrap();
    assert_eq!(result, Err(DeviceError::Timeout));
}

#[tokio::test]
async fn webos_connections_wait_their_turn_so_a_stale_key_prompts_once() {
    let (tv, adapter) = webos().await;
    tv.state.lock().unwrap().accepted_key = "some other key".to_string();
    let (a, b, c) = tokio::join!(adapter.read_state("tv"), adapter.read_state("tv"), adapter.read_state("tv"));
    for result in [a, b, c] {
        assert!(matches!(result, Err(DeviceError::Disabled(_))), "{result:?}");
    }
    assert_eq!(tv.prompts.load(Ordering::SeqCst), 1);
    assert_eq!(tv.traffic(), 1);
}

#[tokio::test]
async fn webos_is_left_alone_by_polls_until_its_key_has_been_accepted() {
    let (tv, adapter) = webos().await;
    assert!(adapter.poll_refusal().unwrap().contains("first time"));
    assert_eq!(tv.traffic(), 0, "asking must open nothing");
    adapter.read_state("tv").await.unwrap();
    assert_eq!(adapter.poll_refusal(), None);
}

#[tokio::test]
async fn webos_power_on_needs_wake_on_lan() {
    let (tv, adapter) = webos().await;
    let on = Op::TvPower { on: true };
    assert!(adapter.refusal("tv", &on).unwrap().contains("wol_mac"));
    assert!(matches!(adapter.command("tv", &on).await, Err(DeviceError::Disabled(_))));
    assert_eq!(tv.traffic(), 0);
}

#[tokio::test]
async fn webos_wake_on_lan_sends_one_magic_packet() {
    let tv = MockTv::start().await;
    let wol = MockWol::start(tv.state.clone()).await;
    tv.state.lock().unwrap().power = "Off".to_string();
    let adapter = Instance::connect(tv_config(&tv, Some(wol.addr.to_string())));
    assert_eq!(adapter.refusal("tv", &Op::TvPower { on: true }), None);
    adapter.command("tv", &Op::TvPower { on: true }).await.unwrap();
    for _ in 0..100 {
        if wol.traffic() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let packets = wol.packets.lock().unwrap();
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0].len(), 102);
    assert_eq!(&packets[0][..6], &[0xFF; 6]);
    assert_eq!(&packets[0][6..12], &[0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
}

#[tokio::test]
async fn webos_device_errors_are_refusals() {
    let (tv, adapter) = webos().await;
    tv.state.lock().unwrap().refuse_uri = Some("ssap://system.launcher/launch".to_string());
    let result = adapter.command("tv", &Op::TvInput { input: "HDMI 2".into() }).await;
    let Err(DeviceError::Rejected(text)) = result else { panic!("{result:?}") };
    assert!(text.contains("401 insufficient permissions"), "{text}");
}

#[tokio::test]
async fn webos_firmware_without_the_power_call_still_reads_as_on() {
    let (tv, adapter) = webos().await;
    tv.state.lock().unwrap().refuse_uri = Some("ssap://com.webos.service.tvpower/power/getPowerState".to_string());
    assert_eq!(adapter.read_state("tv").await.unwrap(), State::Tv { on: true, input: "HDMI 1".to_string() });
}

fn press(key: &str) -> Op {
    Op::TvKey { key: key.to_string() }
}

fn tv_port(tv: &MockTv) -> String {
    tv.url.rsplit(':').next().unwrap().to_string()
}

/// The socket closes before this returns, but the mock may still be reading.
async fn frames(tv: &MockTv, count: usize) -> Vec<String> {
    for _ in 0..200 {
        if tv.frames.lock().unwrap().len() >= count {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tv.frames.lock().unwrap().clone()
}

async fn webos_on_tls() -> (MockTv, Instance) {
    let tv = MockTv::start_tls().await;
    let mut config = tv_config(&tv, None);
    config["cert_sha256"] = json!(tv.pin);
    let adapter = Instance::connect(config);
    (tv, adapter)
}

#[tokio::test]
async fn webos_each_key_goes_out_as_its_own_text_frame() {
    let (tv, adapter) = webos().await;
    let table = [
        ("up", "UP"),
        ("down", "DOWN"),
        ("left", "LEFT"),
        ("right", "RIGHT"),
        ("ok", "ENTER"),
        ("back", "BACK"),
        ("home", "HOME"),
        ("exit", "EXIT"),
        ("volume_up", "VOLUMEUP"),
        ("volume_down", "VOLUMEDOWN"),
        ("mute", "MUTE"),
        ("channel_up", "CHANNELUP"),
        ("channel_down", "CHANNELDOWN"),
        ("play", "PLAY"),
        ("pause", "PAUSE"),
    ];
    // The hub's catalog has these 15 keys. The plugin cannot name the hub, so the
    // count is the catalog's, written out.
    assert_eq!(table.len(), 15);
    assert_eq!(table.len(), swifty_plugin_webos::BUTTONS.len());
    for (sent, (key, button)) in table.iter().enumerate() {
        adapter.command("tv", &press(key)).await.unwrap();
        let seen = frames(&tv, sent + 1).await;
        assert_eq!(seen.len(), sent + 1, "{key}");
        assert_eq!(seen[sent], format!("type:button\nname:{button}\n\n"), "{key}");
    }
    let asked = tv.uris().iter().filter(|uri| uri.ends_with("networkinput/getPointerInputSocket")).count();
    assert_eq!(asked, table.len(), "the socket is asked for on the SSAP connection, once a press");
}

#[tokio::test]
async fn webos_a_key_that_is_not_on_the_remote_reaches_nothing() {
    let (tv, adapter) = webos().await;
    for bad in ["power", "UP", "", "up\nname:POWER"] {
        assert!(adapter.refusal("tv", &press(bad)).is_some(), "{bad:?}");
        assert!(matches!(adapter.command("tv", &press(bad)).await, Err(DeviceError::Disabled(_))), "{bad:?}");
    }
    assert_eq!(tv.traffic(), 0);
}

#[tokio::test]
async fn webos_will_not_follow_a_socket_path_anywhere_else() {
    let (tv, adapter) = webos().await;
    let other = MockTv::start().await;
    let port = tv_port(&tv);
    let cases = [
        format!("{}{POINTER_PATH}", other.url),
        format!("ws://192.0.2.1:{port}{POINTER_PATH}"),
        format!("ws://evil.test:{port}{POINTER_PATH}"),
        format!("http://127.0.0.1:{port}{POINTER_PATH}"),
        format!("ws://user:x@evil.test:{port}{POINTER_PATH}"),
        POINTER_PATH.to_string(),
        "not an address".to_string(),
    ];
    for reported in cases {
        tv.state.lock().unwrap().socket_path = reported.clone();
        let error = adapter.command("tv", &press("up")).await.unwrap_err();
        assert!(
            matches!(&error, DeviceError::Rejected(text) if text.contains("somewhere else")),
            "{reported}: {error:?}"
        );
    }
    assert_eq!(other.traffic(), 0, "a reply sent this somewhere else");
    assert_eq!(tv.pointer_connections.load(Ordering::SeqCst), 0);
    assert!(tv.frames.lock().unwrap().is_empty());
}

#[tokio::test]
async fn webos_the_pointer_socket_is_always_tls_and_the_pin_on_a_secure_tv() {
    let (tv, adapter) = webos_on_tls().await;
    let port = tv_port(&tv);
    let cases = [
        format!("wss://127.0.0.1:{port}{POINTER_PATH}"),
        format!("ws://127.0.0.1:{port}{POINTER_PATH}"),
        format!("ws://127.0.0.1:3000{POINTER_PATH}"),
    ];
    for (sent, reported) in cases.iter().enumerate() {
        tv.state.lock().unwrap().socket_path = reported.clone();
        adapter.command("tv", &press("ok")).await.unwrap_or_else(|e| panic!("{reported}: {e:?}"));
        assert_eq!(frames(&tv, sent + 1).await.len(), sent + 1, "{reported}");
    }
    // This listener only speaks TLS, so each of those frames came over it.
    assert_eq!(tv.pointer_connections.load(Ordering::SeqCst), cases.len());

    for refused in [format!("ws://127.0.0.1:3002{POINTER_PATH}"), format!("wss://127.0.0.1:3000{POINTER_PATH}")] {
        tv.state.lock().unwrap().socket_path = refused.clone();
        assert!(matches!(adapter.command("tv", &press("ok")).await, Err(DeviceError::Rejected(_))), "{refused}");
    }
    assert_eq!(tv.pointer_connections.load(Ordering::SeqCst), cases.len());
}

#[tokio::test]
async fn webos_a_key_on_a_tv_that_is_off_or_gone_is_the_usual_silence() {
    let (tv, adapter) = webos().await;
    tv.state.lock().unwrap().power = "Off".to_string();
    assert!(adapter.command("tv", &press("up")).await.unwrap_err().is_silence());
    assert!(tv.frames.lock().unwrap().is_empty());
}

#[tokio::test]
async fn webos_a_tv_that_refuses_the_pointer_socket_is_a_refusal() {
    let (tv, adapter) = webos().await;
    tv.state.lock().unwrap().refuse_uri =
        Some("ssap://com.webos.service.networkinput/getPointerInputSocket".to_string());
    assert!(matches!(adapter.command("tv", &press("up")).await, Err(DeviceError::Rejected(_))));
    assert_eq!(tv.pointer_connections.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn webos_presses_reach_the_tv_in_the_order_they_came() {
    let (tv, adapter) = webos().await;
    tokio::join!(
        async { adapter.command("tv", &press("up")).await.unwrap() },
        async { adapter.command("tv", &press("down")).await.unwrap() },
        async { adapter.command("tv", &press("left")).await.unwrap() },
    );
    let names: Vec<String> = frames(&tv, 3).await.iter().map(|f| f.lines().nth(1).unwrap().to_string()).collect();
    assert_eq!(names, ["name:UP", "name:DOWN", "name:LEFT"]);
}

#[tokio::test]
async fn webos_the_pairing_manifest_asks_for_the_remote() {
    let (tv, adapter) = webos().await;
    adapter.read_state("tv").await.unwrap();
    let register = tv.received.lock().unwrap().iter().find(|m| m["type"] == "register").cloned().unwrap();
    for needed in ["CONTROL_AUDIO", "CONTROL_INPUT_MEDIA_PLAYBACK", "CONTROL_INPUT_JOYSTICK"] {
        assert!(
            register["payload"]["manifest"]["permissions"].as_array().unwrap().contains(&json!(needed)),
            "{needed}"
        );
    }
}

/// The pairing happens in the setup flow. What it asks the TV for must be exactly
/// what the driver presents, or the key it gets would not cover what the driver does.
#[tokio::test]
async fn the_setup_asks_for_exactly_what_the_service_presents() {
    use std::collections::BTreeMap;
    use swifty_plugin_api::{Plugin, Scalar, SetupContext, SetupInput};

    let tv = common::PairingTv::start(common::Prompt::Ignored).await;
    let ctx = SetupContext {
        instance_id: "webos".to_string(),
        grant: Some(common::grant()),
        timeouts: common::timeouts(),
        secrets_present: Default::default(),
    };
    let mut flow = swifty_plugin_webos::PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    let values = BTreeMap::from([("url".to_string(), Scalar::Text(tv.ws.url.clone()))]);
    flow.advance(&ctx, SetupInput::Values { values }).await;
    flow.advance(&ctx, SetupInput::Poll).await;
    let pairing = tv.registrations().into_iter().next().expect("the setup registered");
    let asked: Vec<String> = serde_json::from_value(pairing["payload"]["manifest"]["permissions"].clone()).unwrap();
    assert_eq!(asked, PERMISSIONS);

    let (presenting_tv, adapter) = webos().await;
    adapter.read_state("tv").await.unwrap();
    let presented = presenting_tv.received.lock().unwrap().iter().find(|m| m["type"] == "register").cloned().unwrap();
    assert_eq!(presented["payload"]["manifest"], pairing["payload"]["manifest"]);
}

#[tokio::test]
async fn webos_refuses_operations_that_are_not_for_a_tv_without_connecting() {
    let (tv, adapter) = webos().await;
    assert!(adapter.refusal("tv", &Op::Play).is_some());
    assert!(matches!(adapter.command("tv", &Op::Play).await, Err(DeviceError::Disabled(_))));
    assert_eq!(tv.traffic(), 0);
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

/// The webOS part of the hub's test of the same name.
#[tokio::test]
async fn a_device_that_never_answers_times_out_and_is_asked_once() {
    let tv = MockTv::start().await;
    tv.hang.store(true, Ordering::SeqCst);
    let adapter = Instance::connect(tv_config(&tv, None));
    assert_gives_up("webos", adapter.command("tv", &Op::TvPower { on: false })).await;
    assert_eq!(tv.traffic(), 1);
}

// ----- new with the plugin

#[tokio::test]
async fn a_device_other_than_the_tv_is_refused_without_connecting() {
    let (tv, adapter) = webos().await;
    assert!(adapter.refusal("sonos", &Op::TvPower { on: false }).is_some());
    assert!(matches!(adapter.command("sonos", &Op::TvPower { on: false }).await, Err(DeviceError::Disabled(_))));
    assert!(matches!(adapter.read_state("sonos").await, Err(DeviceError::Disabled(_))));
    assert!(adapter.refusal("tv", &Op::TvInput { input: "HDMI 9".into() }).is_some(), "refused as live refuses it");
    assert_eq!(tv.traffic(), 0);
}

#[tokio::test]
async fn a_refused_key_shows_in_health_and_in_poll_refusals() {
    use swifty_plugin_api::HealthStatus;
    let (tv, adapter) = webos().await;
    tv.state.lock().unwrap().accepted_key = "some other key".to_string();
    assert!(adapter.read_state("tv").await.is_err());
    let health = adapter.driver.health().await;
    assert_eq!(health.status, HealthStatus::Down);
    assert!(adapter.poll_refusal().unwrap().contains("did not accept"));
}
