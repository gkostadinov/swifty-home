//! The driver against the test kit's mock of Petkit's cloud on loopback. These are the
//! hub's Petkit tests from the hub's former `crates/swifty/tests/adapters.rs`, under the same names.

mod common;

use std::sync::atomic::Ordering;

use common::*;
use serde_json::json;
use swifty_plugin_api::{DeviceError, Driver, Op, Plugin, Readiness, State};
use swifty_plugin_petkit::protocol::LOGIN_SECRET_FIELD;
use swifty_plugin_petkit::{PetkitDriver, LOGIN_SLOT, PLUGIN};
use swifty_plugin_testkit::devices::*;

async fn petkit(commands: bool) -> (MockPetkit, PetkitDriver) {
    let mock = MockPetkit::start().await;
    let adapter = driver(&petkit_config(&mock, commands));
    (mock, adapter)
}

fn cloud(mock: &MockPetkit) -> std::sync::MutexGuard<'_, PetkitCloud> {
    mock.cloud.lock().unwrap()
}

/// 16:00 UTC on 2025-11-10, which is 18:00 two hours east.
const FEED_AT: i64 = 1_762_790_400;

#[tokio::test]
async fn petkit_logs_in_once_as_the_app_does_and_keeps_the_session() {
    let (mock, adapter) = petkit(false).await;
    cloud(&mock).feed_times = vec![1_762_760_000, FEED_AT];
    adapter.read_state("feeder").await.unwrap();
    adapter.read_state("litter").await.unwrap();
    assert_eq!(cloud(&mock).login_attempts, 1);

    let login = &mock.calls("/gw/user/login")[0];
    assert_eq!(login.method, "POST");
    let form = form_pairs(&login.body);
    assert_eq!(form["username"], PETKIT_ACCOUNT);
    assert_eq!(form[LOGIN_SECRET_FIELD], PETKIT_PASSWORD_MD5);
    assert_eq!((form["oldVersion"].as_str(), form["encrypt"].as_str(), form["region"].as_str()), ("13.2.1", "1", "fr"));
    assert!(form["client"].contains("'timezoneId': 'Europe/Paris', 'timezone': '2.0'"), "{}", form["client"]);
    assert_eq!(login.header("user-agent"), "okhttp/3.14.9");
    assert_eq!(login.header("x-client"), "android(16.1;23127PN0CG)");
    assert_eq!(login.header("x-timezone"), "2.0");
    assert_eq!(login.header("content-type"), "application/x-www-form-urlencoded");
    assert_eq!(login.header("f-session"), "", "no session exists yet");

    let detail = &mock.calls("/gw/d4/device_detail")[0];
    assert_eq!((detail.method.as_str(), detail.query.as_str()), ("POST", "id=4001"));
    assert!(detail.header("f-session").starts_with(PETKIT_SESSION));
    assert_eq!(detail.header("f-session"), detail.header("x-session"));
    assert_eq!(detail.header("x-timezoneid"), "Europe/Paris");
}

#[tokio::test]
async fn petkit_reads_the_feeder_and_the_litter_box() {
    let (mock, adapter) = petkit(false).await;
    assert_eq!(adapter.read_state("feeder").await.unwrap(), State::Feeder { last: None, portions: 0 });
    cloud(&mock).feed_times = vec![1_762_760_000, FEED_AT];
    cloud(&mock).cleanings = vec![1_762_700_000, FEED_AT];
    // A kept reading is not refreshed inside poll_secs, so ask a fresh adapter.
    let adapter = driver(&petkit_config(&mock, false));
    assert_eq!(
        adapter.read_state("feeder").await.unwrap(),
        State::Feeder { last: Some("18:00".to_string()), portions: 2 }
    );
    assert_eq!(
        adapter.read_state("litter").await.unwrap(),
        State::Litter { last: Some("18:00".to_string()), busy: false, cycles: 2 }
    );
    let records = &mock.calls("/gw/t4/getDeviceRecord")[0];
    assert!(
        form_pairs(&records.query)["date"].len() == 8 && records.query.contains("deviceId=4002"),
        "{}",
        records.query
    );
    assert!(matches!(adapter.read_state("bulb1").await, Err(DeviceError::Disabled(_))));
}

#[tokio::test]
async fn petkit_readings_are_kept_between_polls() {
    let (mock, adapter) = petkit(false).await;
    let first = adapter.read_state("feeder").await.unwrap();
    let asked = mock.http.requests().len();
    assert_eq!(adapter.read_state("feeder").await.unwrap(), first);
    assert_eq!(mock.http.requests().len(), asked, "a second reading inside poll_secs must not reach Petkit");
}

#[tokio::test]
async fn petkit_feed_sends_the_amount_for_today_and_is_confirmed_when_the_count_moves() {
    let (mock, adapter) = petkit(true).await;
    let before = adapter.read_state("feeder").await.unwrap();
    let op = Op::PetFeed { grams: 20 };
    assert!(!op.is_confirmed_by(&before, Some(&before)), "nothing has been fed yet");
    adapter.command("feeder", &op).await.unwrap();

    let sent = mock.calls("/gw/d4/saveDailyFeed");
    assert_eq!(sent.len(), 1);
    let form = form_pairs(&sent[0].body);
    assert_eq!(
        (form["amount"].as_str(), form["deviceId"].as_str(), form["time"].as_str(), form["name"].as_str()),
        ("20", "4001", "-1", "")
    );
    assert_eq!(form["day"].len(), 8);
    assert_eq!(cloud(&mock).fed_grams, vec![20]);

    // The kept reading no longer holds, so the next one goes to Petkit and sees the feed.
    let after = adapter.read_state("feeder").await.unwrap();
    assert!(matches!(after, State::Feeder { portions: 1, .. }), "{after:?}");
    assert!(op.is_confirmed_by(&after, Some(&before)));
    assert_eq!(mock.calls("/gw/d4/device_detail").len(), 2);
}

#[tokio::test]
async fn petkit_clean_starts_the_cycle_and_the_box_reads_busy() {
    let (mock, adapter) = petkit(true).await;
    let before = adapter.read_state("litter").await.unwrap();
    assert_eq!(before, State::Litter { last: None, busy: false, cycles: 0 });
    adapter.command("litter", &Op::LitterClean).await.unwrap();

    let sent = mock.calls("/gw/t4/controlDevice");
    assert_eq!(sent.len(), 1);
    let form = form_pairs(&sent[0].body);
    assert_eq!(
        (form["id"].as_str(), form["kv"].as_str(), form["type"].as_str()),
        ("4002", r#"{"start_action":0}"#, "start")
    );

    let after = adapter.read_state("litter").await.unwrap();
    assert!(matches!(after, State::Litter { busy: true, .. }), "{after:?}");
    assert!(Op::LitterClean.is_confirmed_by(&after, Some(&before)));
}

#[tokio::test]
async fn petkit_commands_are_off_until_the_config_turns_them_on() {
    let (mock, adapter) = petkit(false).await;
    let config = petkit_config(&mock, false);
    for (target, op) in [("feeder", Op::PetFeed { grams: 10 }), ("litter", Op::LitterClean)] {
        assert!(refusal(&config, target, &op).unwrap().contains("switched off"));
        assert!(matches!(adapter.command(target, &op).await, Err(DeviceError::Disabled(_))));
    }
    assert_eq!(mock.http.traffic(), 0, "a refused command opens nothing");
}

#[tokio::test]
async fn petkit_d4_refuses_amounts_it_cannot_dispense_without_asking_petkit() {
    let (mock, adapter) = petkit(true).await;
    let config = petkit_config(&mock, true);
    for grams in [5u8, 15, 25, 51] {
        let op = Op::PetFeed { grams };
        assert!(refusal(&config, "feeder", &op).unwrap().contains("10, 20, 30, 40 or 50"), "{grams}");
        assert!(matches!(adapter.command("feeder", &op).await, Err(DeviceError::Disabled(_))), "{grams}");
    }
    assert!(refusal(&config, "feeder", &Op::LitterClean).is_some());
    assert!(refusal(&config, "litter", &Op::PetFeed { grams: 10 }).is_some());
    assert_eq!(mock.http.traffic(), 0);
}

#[tokio::test]
async fn petkit_finds_devices_that_are_only_shared_to_the_account() {
    let (mock, adapter) = petkit(false).await;
    cloud(&mock).shared = true;
    assert!(matches!(adapter.read_state("feeder").await.unwrap(), State::Feeder { .. }));
    assert!(matches!(adapter.read_state("litter").await.unwrap(), State::Litter { .. }));
    let roster = mock.calls("/gw/discovery/device_roster_v2");
    assert_eq!(roster.len(), 1, "the devices are looked for once");
    let form = form_pairs(&roster[0].body);
    assert_eq!(form["groupId"], "77");
    assert_eq!(form["day"].len(), 8);
}

#[tokio::test]
async fn petkit_a_missing_device_is_disabled_alone() {
    let (mock, adapter) = petkit(true).await;
    cloud(&mock).no_litter_box = true;
    assert!(matches!(adapter.read_state("feeder").await.unwrap(), State::Feeder { .. }));
    for result in
        [adapter.read_state("litter").await.map(|_| ()), adapter.command("litter", &Op::LitterClean).await.map(|_| ())]
    {
        assert!(matches!(&result, Err(DeviceError::Disabled(text)) if text.contains("No Petkit T4")), "{result:?}");
    }
    assert!(mock.calls("/gw/t4/controlDevice").is_empty());
}

#[tokio::test]
async fn petkit_a_configured_device_id_wins_over_the_account_listing() {
    let mock = MockPetkit::start().await;
    let mut config = petkit_config(&mock, false);
    config["feeder_id"] = json!(9_999);
    let adapter = driver(&config);
    let error = adapter.read_state("feeder").await.unwrap_err();
    assert!(mock.calls("/gw/d4/device_detail")[0].query == "id=9999");
    assert!(error.to_string().contains("does not know this device"), "{error}");
    assert!(!error.to_string().contains("Petkit's own words"));
}

#[tokio::test]
async fn petkit_a_session_signed_out_is_replaced_for_a_read_but_a_command_is_not_resent() {
    let (mock, adapter) = petkit(true).await;
    adapter.read_state("feeder").await.unwrap();
    assert_eq!(cloud(&mock).login_attempts, 1);

    // Someone signs in elsewhere: Petkit keeps one session per account.
    cloud(&mock).session = "signed-in-on-the-phone".to_string();
    assert!(matches!(adapter.read_state("litter").await.unwrap(), State::Litter { .. }));
    assert_eq!(cloud(&mock).login_attempts, 2, "a read signs in again and asks once more");

    cloud(&mock).session = "signed-in-on-the-phone-again".to_string();
    let error = adapter.command("feeder", &Op::PetFeed { grams: 10 }).await.unwrap_err();
    assert!(matches!(&error, DeviceError::Rejected(text) if text.contains("nothing was sent")), "{error:?}");
    assert_eq!(mock.calls("/gw/d4/saveDailyFeed").len(), 1, "the command went out once and is not repeated");
    assert_eq!(cloud(&mock).applied, 0, "and Petkit did not act on it");

    adapter.command("feeder", &Op::PetFeed { grams: 10 }).await.unwrap();
    assert_eq!(mock.calls("/gw/d4/saveDailyFeed").len(), 2);
    let carried_out = cloud(&mock).clone();
    assert_eq!((carried_out.applied, carried_out.login_attempts), (1, 3));
}

#[tokio::test]
async fn petkit_a_refused_login_is_never_repeated() {
    let (mock, adapter) = petkit(true).await;
    cloud(&mock).refuse_login = true;
    let first = adapter.read_state("feeder").await.unwrap_err();
    assert!(
        matches!(&first, DeviceError::Rejected(text) if text.contains("setup again to fix the login")),
        "{first:?}"
    );
    let second = adapter.read_state("litter").await.unwrap_err();
    let third = adapter.command("feeder", &Op::PetFeed { grams: 10 }).await.unwrap_err();
    assert_eq!((&second, &third), (&first, &first));
    assert_eq!(cloud(&mock).login_attempts, 1, "a wrong secret is tried once, so the account is never locked out");
    cloud(&mock).refuse_login = false;
    assert!(adapter.read_state("litter").await.is_err(), "it stays stopped until the instance is loaded again");
    assert_eq!(cloud(&mock).login_attempts, 1);
}

#[tokio::test]
async fn petkit_a_failed_login_is_not_tried_again_at_once() {
    let (mock, adapter) = petkit(false).await;
    cloud(&mock).login_server_error = true;
    let first = adapter.read_state("feeder").await.unwrap_err();
    assert!(first.is_silence(), "{first:?}");
    let second = adapter.read_state("litter").await.unwrap_err();
    assert!(second.is_silence() && second.to_string().contains("not being tried again"), "{second:?}");
    assert_eq!(
        cloud(&mock).login_attempts,
        1,
        "the engine reads every 250 ms, and Petkit must not be asked that often"
    );
}

#[tokio::test]
async fn petkit_never_logs_in_to_a_server_petkit_did_not_name() {
    let (mock, adapter) = petkit(false).await;
    cloud(&mock).gateway = Some("https://evil.example/latest/".to_string());
    let error = adapter.read_state("feeder").await.unwrap_err();
    assert!(matches!(&error, DeviceError::Rejected(text) if text.contains("will not log in to")), "{error:?}");
    assert_eq!(mock.http.requests().len(), 1, "only the list of servers was asked");
    assert!(mock.calls("/gw/user/login").is_empty());
}

#[tokio::test]
async fn petkit_a_feed_that_gets_no_answer_is_silence_and_is_not_sent_again() {
    let (mock, adapter) = petkit(true).await;
    adapter.read_state("feeder").await.unwrap();
    mock.http.hang_posts.store(true, Ordering::SeqCst);
    let error = adapter.command("feeder", &Op::PetFeed { grams: 10 }).await.unwrap_err();
    assert!(error.is_silence(), "{error:?}");
    assert_eq!(mock.calls("/gw/d4/saveDailyFeed").len(), 1);
}

#[tokio::test]
async fn petkit_nothing_it_says_carries_the_login_the_session_or_petkits_own_words() {
    let mut said = Vec::new();
    {
        let (mock, adapter) = petkit(true).await;
        cloud(&mock).refuse_login = true;
        said.push(adapter.read_state("feeder").await.unwrap_err().to_string());
    }
    {
        let (mock, adapter) = petkit(true).await;
        cloud(&mock).login_server_error = true;
        said.push(adapter.read_state("feeder").await.unwrap_err().to_string());
    }
    {
        let (mock, adapter) = petkit(true).await;
        adapter.read_state("feeder").await.unwrap();
        cloud(&mock).session = "other".to_string();
        said.push(adapter.command("feeder", &Op::PetFeed { grams: 10 }).await.unwrap_err().to_string());
        cloud(&mock).no_litter_box = true;
        said.push(adapter.read_state("litter").await.unwrap_err().to_string());
    }
    assert!(said.len() == 4 && said.iter().all(|text| !text.is_empty()));
    for text in said {
        for secret in [PETKIT_ACCOUNT, PETKIT_PASSWORD_MD5, PETKIT_SESSION, "Petkit's own words", "evil.example"] {
            assert!(!text.contains(secret), "{secret} in: {text}");
        }
    }
}

#[tokio::test]
async fn petkit_needs_a_login_file_in_the_right_shape() {
    let mock = MockPetkit::start().await;
    let good = petkit_config(&mock, false);
    assert!(connect(good.clone(), Some(&login())).is_ok());
    for text in ["no-colon-here", "me@example.com:not-a-hash", ":5f4dcc3b5aa765d61d8327deb882cf99"] {
        let Err(error) = connect(good.clone(), Some(text)) else { panic!("{text} was accepted") };
        assert!(
            !error.0.contains("5f4dcc3b5aa765d61d8327deb882cf99") && !error.0.contains("me@example.com"),
            "{error}"
        );
    }
    assert!(connect(good, None).is_err());
    assert_eq!(mock.http.traffic(), 0, "building the adapter reaches nothing");
}

#[tokio::test]
async fn petkit_in_dry_run_predicts_what_live_would_refuse_and_reaches_nothing() {
    // Dry-run never builds a driver: the hub has only the plugin's pure calls.
    let mock = MockPetkit::start().await;
    let config = petkit_config(&mock, true);
    assert_eq!(readiness(&config, &[LOGIN_SLOT]), Readiness::Ready);
    assert!(refusal(&config, "feeder", &Op::PetFeed { grams: 15 }).is_some());
    assert_eq!(refusal(&config, "feeder", &Op::PetFeed { grams: 20 }), None);
    assert_eq!(refusal(&config, "litter", &Op::LitterClean), None);
    let devices = pure(&config, &[LOGIN_SLOT], |view| PLUGIN.devices(view));
    let ids: Vec<&str> = devices.iter().map(|device| device.local_id.as_str()).collect();
    assert_eq!(ids, ["feeder", "litter"]);

    let off = petkit_config(&mock, false);
    assert!(refusal(&off, "litter", &Op::LitterClean).unwrap().contains("switched off"));
    assert_eq!(mock.http.traffic(), 0, "the pure calls reach nothing");
}

#[tokio::test]
async fn petkit_live_without_a_readable_login_is_disabled_and_says_why() {
    let mock = MockPetkit::start().await;
    let config = petkit_config(&mock, true);
    let Readiness::NeedsSetup(reason) = readiness(&config, &[]) else { panic!("it should need the login") };
    assert!(!reason.is_empty());
    let Err(error) = connect(config.clone(), None) else { panic!("it should not build") };
    assert!(!error.0.is_empty());

    assert!(connect(config, Some(&login())).is_ok());
    assert_eq!(mock.http.traffic(), 0);

    let Readiness::Disabled(reason) = readiness(&json!({}), &[LOGIN_SLOT]) else { panic!("it should be disabled") };
    assert!(reason.contains("region"), "{reason}");
}
