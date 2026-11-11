//! The setup flow, run step by step against mock sockets and a mock of Tuya's cloud.

mod common;

use common::*;
use serde_json::{json, Value};
use swifty_plugin_api::{Op, Plugin, Scalar, SetupFlow, SetupInput, SetupResult, SetupStep, State};
use swifty_plugin_testkit::devices::*;
use swifty_plugin_tuya::{TuyaSetup, PLUGIN};

const A: &str = "bf0123456789abcdef";
const B: &str = "bf9876543210fedcba";
/// The mock sockets' key as Tuya shows it: sixteen characters.
fn shown_key() -> &'static str {
    std::str::from_utf8(TUYA_KEY).unwrap()
}

fn done(step: SetupStep) -> SetupResult {
    match step {
        SetupStep::Done(result) => result,
        other => panic!("expected Done, got {other:?}"),
    }
}

fn failure(step: SetupStep) -> String {
    match step {
        SetupStep::Failed { reason, retry } => {
            assert!(retry, "a correctable failure offers the step again");
            reason
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

fn field_keys(step: &SetupStep) -> Vec<String> {
    match step {
        SetupStep::Form { fields, .. } => fields.iter().map(|field| field.key.clone()).collect(),
        other => panic!("expected a Form, got {other:?}"),
    }
}

fn option_ids(step: &SetupStep) -> Vec<(String, bool)> {
    match step {
        SetupStep::Choose { options, .. } => {
            options.iter().map(|option| (option.id.clone(), option.selected)).collect()
        }
        other => panic!("expected Choose, got {other:?}"),
    }
}

/// Nothing a step says, in JSON or in `Debug`, may hold a key or the access secret.
fn assert_nothing_secret_in(step: &SetupStep) {
    let text = format!("{} {step:?}", serde_json::to_string(step).unwrap());
    for secret in [shown_key(), TUYA_KEY_BASE64, ACCESS_SECRET] {
        assert!(!text.contains(secret), "{text}");
    }
}

fn manual_socket(plug: &MockTuya, local_key: &str, another: bool) -> SetupInput {
    values(&[
        ("id", text(&plug.id)),
        ("address", text("127.0.0.1")),
        ("port", Scalar::Int(i64::from(plug.port))),
        ("local_key", text(local_key)),
        ("name", text("")),
        ("usb_dp", text("")),
        ("another", Scalar::Bool(another)),
    ])
}

/// The scripted manual path, in dry-run: a form only, so it finishes and contacts nothing.
#[tokio::test]
async fn the_manual_path_finishes_in_dry_run_and_contacts_nothing() {
    let plug = MockTuya::start(A).await;
    let ctx = context(false, &[]);
    let mut flow = PLUGIN.setup().unwrap();
    let first = flow.begin(&ctx, None).await;
    assert_eq!(option_ids(&first), [("cloud".to_string(), true), ("manual".to_string(), false)]);
    let form = flow.advance(&ctx, chosen(&["manual"])).await;
    assert_eq!(field_keys(&form), ["id", "address", "port", "local_key", "name", "usb_dp", "another"]);
    let choose = flow.advance(&ctx, manual_socket(&plug, shown_key(), false)).await;
    assert_nothing_secret_in(&choose);
    assert_eq!(option_ids(&choose), [(A.to_string(), true)]);
    let result = done(flow.advance(&ctx, chosen(&[A])).await);

    PLUGIN.validate(&result.config).unwrap();
    assert_eq!(result.config, json!({ "plugs": [{ "id": A, "address": "127.0.0.1", "port": plug.port }] }));
    assert_eq!(result.secrets.len(), 1);
    assert_eq!(result.secrets[0].0, format!("local_key/{A}"));
    assert_eq!(result.secrets[0].1.expose(), TUYA_KEY_BASE64, "stored in base64, as the key files were");
    assert!(result.adopt.is_empty(), "sockets come from the config");
    assert_eq!(result.summary, "Saved 1 socket.");
    assert!(!serde_json::to_string(&result).unwrap().contains(TUYA_KEY_BASE64));
    assert!(!format!("{result:?}").contains(TUYA_KEY_BASE64));
    assert_eq!(plug.traffic(), 0, "dry-run reaches nothing");
}

/// Live: each socket is read once, and the result drives the socket through the plugin.
#[tokio::test]
async fn the_manual_path_reads_each_socket_and_the_result_works() {
    let (a, b) = (MockTuya::start(A).await, MockTuya::start(B).await);
    b.state.lock().unwrap().usb = Some(false);
    let ctx = context(true, &[]);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    flow.advance(&ctx, chosen(&["manual"])).await;
    let again = flow.advance(&ctx, manual_socket(&a, shown_key(), true)).await;
    assert_eq!(field_keys(&again)[0], "id", "the form again, for the next socket");
    let mut second = manual_socket(&b, TUYA_KEY_BASE64, false);
    if let SetupInput::Values { values } = &mut second {
        values.insert("name".to_string(), text("Heater"));
        values.insert("usb_dp".to_string(), Scalar::Int(7));
    }
    let choose = flow.advance(&ctx, second).await;
    assert_eq!(option_ids(&choose).len(), 2);
    let result = done(flow.advance(&ctx, chosen(&[A, B])).await);
    assert_eq!(result.summary, "Saved 2 sockets. 2 answered.");
    assert_eq!((a.traffic(), b.traffic()), (1, 1), "one read each");
    assert_eq!(result.config["plugs"][1]["name"], json!("Heater"));
    assert_eq!(result.config["plugs"][1]["usb_dp"], json!(7));

    let keys: Vec<(String, String)> =
        result.secrets.iter().map(|(slot, value)| (slot.clone(), value.expose().to_string())).collect();
    let rows = result.config["plugs"].as_array().unwrap().clone();
    let driver = driver(rows, keys);
    driver.command(&b.target(), &Op::PlugPower { on: true }).await.unwrap();
    driver.command(&format!("{}-usb", b.target()), &Op::PlugPower { on: true }).await.unwrap();
    assert_eq!(driver.read_state(&b.target()).await.unwrap(), State::Plug { on: true });
    assert_eq!(b.state.lock().unwrap().usb, Some(true));
}

#[tokio::test]
async fn a_wrong_key_or_address_stops_the_setup_with_a_reason_and_no_key() {
    let plug = MockTuya::start(A).await;
    let ctx = context(true, &[]);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    flow.advance(&ctx, chosen(&["manual"])).await;
    let wrong = "zyxwvutsrqponmlk";
    flow.advance(&ctx, manual_socket(&plug, wrong, false)).await;
    let reason = failure(flow.advance(&ctx, chosen(&[A])).await);
    assert!(reason.contains(&plug.target()) && reason.contains("Check its address and key"), "{reason}");
    assert!(!reason.contains(wrong) && !reason.contains("enl4d3Z1dHNycXBvbm1saw"), "{reason}");

    // The step is offered again: going back and entering the right key finishes.
    flow.advance(&ctx, SetupInput::Back).await;
    flow.advance(&ctx, chosen(&["manual"])).await;
    flow.advance(&ctx, manual_socket(&plug, shown_key(), false)).await;
    let result = done(flow.advance(&ctx, chosen(&[A])).await);
    assert_eq!(result.secrets[0].1.expose(), TUYA_KEY_BASE64, "the new key replaced the wrong one");

    // A form that cannot be right is refused before anything is contacted.
    let before = plug.traffic();
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    flow.advance(&ctx, chosen(&["manual"])).await;
    for (key, value, why) in [
        ("id", text("SHORT"), "device id"),
        ("address", text("192.0.2.300"), "address"),
        ("local_key", text("too short"), "local key"),
        ("local_key", text(""), "needs its local key"),
        ("usb_dp", Scalar::Int(1), "USB data point"),
        ("name", text("bell\u{7}name"), "name"),
        ("port", Scalar::Int(0), "port"),
    ] {
        let mut input = manual_socket(&plug, shown_key(), false);
        if let SetupInput::Values { values } = &mut input {
            values.insert(key.to_string(), value);
        }
        let reason = failure(flow.advance(&ctx, input).await);
        assert!(reason.to_lowercase().contains(&why.to_lowercase()), "{key}: {reason}");
        assert!(!reason.contains(shown_key()), "{reason}");
    }
    assert_eq!(plug.traffic(), before);
}

/// Setting an instance up again keeps stored keys unless new ones are given, and
/// only the new keys are handed back.
#[tokio::test]
async fn setting_up_again_keeps_stored_keys_and_can_drop_a_socket() {
    let (a, b) = (MockTuya::start(A).await, MockTuya::start(B).await);
    let existing = json!({ "plugs": [row(&a), row(&b)] });
    let a_slot = format!("local_key/{A}");
    let ctx = context(true, &[&a_slot, &format!("local_key/{B}")]);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, Some(&existing)).await;
    let form = flow.advance(&ctx, chosen(&["manual"])).await;
    match &form {
        SetupStep::Form { fields, .. } => {
            let key = fields.iter().find(|field| field.key == "local_key").unwrap();
            assert!(!key.required, "leave empty to keep");
            assert!(key.value.is_none(), "a secret is never prefilled");
        }
        other => panic!("{other:?}"),
    }
    // Move socket A to another port, keeping its stored key.
    let mut moved = manual_socket(&a, "", false);
    if let SetupInput::Values { values } = &mut moved {
        values.insert("port".to_string(), Scalar::Int(7000));
    }
    let choose = flow.advance(&ctx, moved).await;
    assert_eq!(option_ids(&choose), [(A.to_string(), true), (B.to_string(), true)]);
    let result = done(flow.advance(&ctx, chosen(&[A])).await);
    assert_eq!(result.config, json!({ "plugs": [{ "id": A, "address": "127.0.0.1", "port": 7000 }] }));
    assert!(result.secrets.is_empty(), "no new key, nothing to store");
    assert_eq!(a.traffic() + b.traffic(), 0, "a socket with a stored key cannot be read here");
}

fn cloud_form(region: &str, access_id: &str, access_secret: &str) -> SetupInput {
    values(&[("region", text(region)), ("access_id", text(access_id)), ("access_secret", text(access_secret))])
}

/// The scripted cloud path: sign in, list two pages, pick the sockets, place them, read them.
#[tokio::test]
async fn the_cloud_path_finds_the_keys_and_reaches_done() {
    let (a, b) = (MockTuya::start(A).await, MockTuya::start(B).await);
    b.state.lock().unwrap().usb = Some(false);
    let cloud = mock_cloud(CloudPlan {
        pages: vec![
            json!([
                listed(&A.to_uppercase(), "Desk", shown_key(), "cz"),
                { "id": "bfhub00000000000", "name": "Gateway", "category": "wg2", "online": true },
            ]),
            json!([
                listed(B, "Heater\u{1b}[31m", shown_key(), "pc"),
                listed("bf5555555555555555", "Lamp", shown_key(), "dj"),
            ]),
        ],
        refuse_list: None,
    })
    .await;
    let ctx = context(true, &[]);
    let mut flow = TuyaSetup::with_cloud(&cloud.url);
    flow.begin(&ctx, None).await;
    let form = flow.advance(&ctx, chosen(&["cloud"])).await;
    assert_eq!(field_keys(&form), ["region", "access_id", "access_secret"]);
    let choose = flow.advance(&ctx, cloud_form("eu", ACCESS_ID, ACCESS_SECRET)).await;
    assert_nothing_secret_in(&choose);
    assert_eq!(
        option_ids(&choose),
        [(A.to_string(), true), (B.to_string(), true), ("bf5555555555555555".to_string(), false)],
        "sockets and power strips are ticked; the gateway has no key and is left out"
    );
    match &choose {
        SetupStep::Choose { options, text, .. } => {
            assert_eq!(options[1].label, "Heater[31m", "device text is cleaned");
            assert!(text.contains("1 other device"), "{text}");
        }
        other => panic!("{other:?}"),
    }
    let places = flow.advance(&ctx, chosen(&[A, B])).await;
    assert_nothing_secret_in(&places);
    let mut expected = Vec::new();
    for id in [A, B] {
        expected.extend([format!("address_{id}"), format!("port_{id}"), format!("name_{id}"), format!("usb_dp_{id}")]);
    }
    assert_eq!(field_keys(&places), expected);
    let input = values(&[
        (&format!("address_{A}"), text("127.0.0.1")),
        (&format!("port_{A}"), Scalar::Int(i64::from(a.port))),
        (&format!("name_{A}"), text("Desk")),
        (&format!("address_{B}"), text("127.0.0.1")),
        (&format!("port_{B}"), text(&b.port.to_string())),
        (&format!("usb_dp_{B}"), Scalar::Int(7)),
    ]);
    let result = done(flow.advance(&ctx, input).await);
    assert_eq!(result.summary, "Saved 2 sockets. 2 answered.");
    assert_eq!((a.traffic(), b.traffic()), (1, 1), "one read each");
    assert_eq!(
        result.config,
        json!({ "plugs": [
            { "id": A, "address": "127.0.0.1", "port": a.port, "name": "Desk" },
            { "id": B, "address": "127.0.0.1", "port": b.port, "usb_dp": 7 },
        ] })
    );
    PLUGIN.validate(&result.config).unwrap();
    let slots: Vec<&str> = result.secrets.iter().map(|(slot, _)| slot.as_str()).collect();
    assert_eq!(slots, [format!("local_key/{A}"), format!("local_key/{B}")]);
    assert!(result.secrets.iter().all(|(_, value)| value.expose() == TUYA_KEY_BASE64));

    // The keys from the cloud drive the sockets, on the local network.
    let rows = result.config["plugs"].as_array().unwrap().clone();
    let keys = result.secrets.iter().map(|(slot, value)| (slot.clone(), value.expose().to_string())).collect();
    let driver = driver(rows, keys);
    driver.command(&a.target(), &Op::PlugPower { on: true }).await.unwrap();
    driver.command(&format!("{}-usb", b.target()), &Op::PlugPower { on: true }).await.unwrap();
    assert!(a.state.lock().unwrap().on);
    assert_eq!(b.state.lock().unwrap().usb, Some(true));

    // Three signed requests: the token and two pages. The secret never travelled.
    let requests = cloud.requests();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        let all = format!("{} {} {:?}", request.path, request.query, request.headers);
        assert!(!all.contains(ACCESS_SECRET), "{all}");
    }
}

/// Setting up again through the cloud: what is stored is prefilled, a stored socket
/// the cloud does not list is kept with its stored key, and only new keys come back.
#[tokio::test]
async fn the_cloud_path_keeps_what_it_does_not_list_and_prefills_what_is_stored() {
    let (a, c) = (MockTuya::start(A).await, MockTuya::start(B).await);
    let cloud =
        mock_cloud(CloudPlan { pages: vec![json!([listed(A, "Desk", shown_key(), "cz")])], refuse_list: None }).await;
    let mut stored = row(&a);
    stored["name"] = json!("Kettle");
    let existing = json!({ "plugs": [stored, row(&c)] });
    let ctx = context(true, &[&format!("local_key/{A}"), &format!("local_key/{B}")]);
    let mut flow = TuyaSetup::with_cloud(&cloud.url);
    flow.begin(&ctx, Some(&existing)).await;
    flow.advance(&ctx, chosen(&["cloud"])).await;
    let choose = flow.advance(&ctx, cloud_form("eu-w", ACCESS_ID, ACCESS_SECRET)).await;
    assert_eq!(option_ids(&choose), [(A.to_string(), true)]);
    let places = flow.advance(&ctx, chosen(&[A])).await;
    match &places {
        SetupStep::Form { fields, .. } => {
            let value = |key: String| fields.iter().find(|field| field.key == key).unwrap().value.clone();
            assert_eq!(value(format!("address_{A}")), Some(text("127.0.0.1")));
            assert_eq!(value(format!("port_{A}")), Some(Scalar::Int(i64::from(a.port))));
            assert_eq!(value(format!("name_{A}")), Some(text("Kettle")), "the stored name wins over the cloud's");
        }
        other => panic!("{other:?}"),
    }
    let input = values(&[
        (&format!("address_{A}"), text("127.0.0.1")),
        (&format!("port_{A}"), Scalar::Int(i64::from(a.port))),
        (&format!("name_{A}"), text("Kettle")),
    ]);
    let result = done(flow.advance(&ctx, input).await);
    assert_eq!(result.summary, "Saved 2 sockets. 1 answered.");
    assert_eq!(
        result.config["plugs"].as_array().unwrap().iter().map(|plug| plug["id"].clone()).collect::<Vec<_>>(),
        [json!(B), json!(A)],
        "the unlisted socket is kept"
    );
    let slots: Vec<&str> = result.secrets.iter().map(|(slot, _)| slot.as_str()).collect();
    assert_eq!(slots, [format!("local_key/{A}")]);
    assert_eq!((a.traffic(), c.traffic()), (1, 0), "only a socket with a new key is read");
}

#[tokio::test]
async fn the_cloud_path_explains_each_refusal_and_never_shows_the_secret() {
    let cloud = mock_cloud(CloudPlan { pages: vec![json!([])], refuse_list: None }).await;
    let ctx = context(true, &[]);

    let mut flow = TuyaSetup::with_cloud(&cloud.url);
    flow.begin(&ctx, None).await;
    flow.advance(&ctx, chosen(&["cloud"])).await;
    let wrong = "ffffffffffffffffffffffffffffffff";
    let reason = failure(flow.advance(&ctx, cloud_form("eu", ACCESS_ID, wrong)).await);
    assert!(reason.contains("refused the login (code 1004)"), "{reason}");
    assert!(!reason.contains(wrong) && !reason.contains("sign invalid"), "only fixed text and the code: {reason}");

    let reason = failure(flow.advance(&ctx, cloud_form("eu", ACCESS_ID, ACCESS_SECRET)).await);
    assert!(reason.contains("no device with a local key"), "{reason}");
    let reason = failure(flow.advance(&ctx, cloud_form("mars", ACCESS_ID, ACCESS_SECRET)).await);
    assert!(reason.contains("data centers"), "{reason}");
    let reason = failure(flow.advance(&ctx, cloud_form("eu", "has spaces", ACCESS_SECRET)).await);
    assert!(reason.contains("access id"), "{reason}");

    let refusing = mock_cloud(CloudPlan { pages: vec![], refuse_list: Some(28841105) }).await;
    let mut flow = TuyaSetup::with_cloud(&refusing.url);
    flow.begin(&ctx, None).await;
    flow.advance(&ctx, chosen(&["cloud"])).await;
    let reason = failure(flow.advance(&ctx, cloud_form("eu", ACCESS_ID, ACCESS_SECRET)).await);
    assert!(reason.contains("refused the device list (code 28841105)"), "{reason}");

    let silent = MockHttp::start(|_| (500, "no".to_string())).await;
    let mut flow = TuyaSetup::with_cloud(&silent.url);
    flow.begin(&ctx, None).await;
    flow.advance(&ctx, chosen(&["cloud"])).await;
    let reason = failure(flow.advance(&ctx, cloud_form("eu", ACCESS_ID, ACCESS_SECRET)).await);
    assert!(reason.contains("HTTP status 500"), "{reason}");

    let hanging = MockHttp::start(|_| (200, "{}".to_string())).await;
    hanging.hang.store(true, std::sync::atomic::Ordering::SeqCst);
    let mut flow = TuyaSetup::with_cloud(&hanging.url);
    flow.begin(&ctx, None).await;
    flow.advance(&ctx, chosen(&["cloud"])).await;
    let started = std::time::Instant::now();
    let reason = failure(flow.advance(&ctx, cloud_form("eu", ACCESS_ID, ACCESS_SECRET)).await);
    assert!(reason.contains("did not answer in time"), "{reason}");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[tokio::test]
async fn the_cloud_path_needs_a_live_hub_and_contacts_nothing_without_one() {
    let cloud = mock_cloud(CloudPlan { pages: vec![], refuse_list: None }).await;
    let ctx = context(false, &[]);
    let mut flow = TuyaSetup::with_cloud(&cloud.url);
    flow.begin(&ctx, None).await;
    let reason = failure(flow.advance(&ctx, chosen(&["cloud"])).await);
    assert!(reason.contains("live"), "{reason}");
    assert_eq!(cloud.traffic(), 0);
    // An answer that does not fit the step is refused, and Back starts over.
    let reason = failure(flow.advance(&ctx, values(&[("id", text(A))])).await);
    assert!(reason.contains("does not fit"), "{reason}");
    let start = flow.advance(&ctx, SetupInput::Back).await;
    assert_eq!(option_ids(&start).len(), 2);
    assert!(matches!(flow.advance(&ctx, chosen(&["something"])).await, SetupStep::Failed { .. }));
}

/// The flow as `Plugin::setup` returns it uses Tuya's real hosts; it is the same
/// code, so this only checks that the region decides the host it would call.
#[test]
fn every_region_is_a_declared_host() {
    let manifest = serde_json::to_value(&swifty_plugin_tuya::MANIFEST).unwrap();
    let declared: Vec<Value> = manifest["network"].as_array().unwrap().clone();
    for region in swifty_plugin_tuya::cloud::REGIONS {
        assert!(declared.contains(&json!({ "use": "cloud_host", "host": region.host })), "{}", region.code);
    }
}
