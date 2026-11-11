//! People and roles: the one owner, single-use invitations, what family can and
//! cannot do on the server, who is recorded in the audit log, and what each person
//! keeps for themselves.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{json, Value};
use swifty_hub::users::INVITATION_TTL_MS;
use swifty_plugin_mock::Behavior;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;

async fn owner_and_family() -> (Harness, SoftPasskey, String, SoftPasskey, String, String) {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let (family, family_cookie, family_id) = harness.add_family(&cookie, &mut owner, "Alice").await;
    (harness, owner, cookie, family, family_cookie, family_id)
}

#[tokio::test]
async fn first_enrollment_makes_the_owner() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let me = harness.get("/api/v1/me", &cookie).await;
    me.assert_ok();
    assert_eq!(me.body["session"]["role"], json!("owner"));
    let users = harness.get("/api/v1/users", &cookie).await;
    let rows = users.body["users"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["role"], json!("owner"));
    assert_eq!(rows[0]["passkeys"], json!(1));
}

#[tokio::test]
async fn an_invitation_takes_the_owner_and_a_step_up() {
    let (harness, _owner, cookie, _family, family_cookie, _id) = owner_and_family().await;
    harness
        .execute(&cookie, "user.invite", "new", json!({ "name": "Bob" }))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "step_up_required");
    let prepared = harness.prepare(&cookie, "user.invite", "new", json!({ "name": "Bob" })).await;
    prepared.assert_ok();
    assert_eq!(prepared.body["needs"], json!("passkey"));
    harness
        .prepare(&family_cookie, "user.invite", "new", json!({ "name": "Bob" }))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "owner_only");
}

#[tokio::test]
async fn a_family_member_joins_with_their_own_passkey() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let (_family, family_cookie, family_id) = harness.add_family(&cookie, &mut owner, "Alice").await;
    let me = harness.get("/api/v1/me", &family_cookie).await;
    assert_eq!(me.body["session"]["role"], json!("family"));
    assert_eq!(me.body["session"]["user_name"], json!("Alice"));
    assert_eq!(me.body["session"]["user_id"], json!(family_id));
    let users = harness.get("/api/v1/users", &cookie).await;
    assert_eq!(users.body["users"].as_array().unwrap().len(), 2);
    let passkeys = harness.get("/api/v1/passkeys", &cookie).await;
    let owners: Vec<&Value> =
        passkeys.body["passkeys"].as_array().unwrap().iter().filter(|row| row["user_id"] == json!(family_id)).collect();
    assert_eq!(owners.len(), 1, "the passkey belongs to the person who joined");
    assert!(harness.audit_has("joined"));
}

#[tokio::test]
async fn the_token_exists_only_in_the_one_response() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let token = harness.invite(&cookie, &mut owner, "Alice").await;
    for table in ["invitations", "audit_log", "action_runs", "prepared_actions"] {
        assert!(!harness.dump(table).contains(&token), "{table} holds the token");
    }
    let runs = harness.get("/api/v1/actions/runs", &cookie).await;
    assert!(!runs.text.contains(&token));
}

#[tokio::test]
async fn an_invitation_works_exactly_once() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let token = harness.invite(&cookie, &mut owner, "Alice").await;
    let first = harness.join_start(&token, "One").await;
    let second = harness.join_start(&token, "Two").await;
    first.assert_ok();
    second.assert_ok();

    let mut a = authenticator();
    harness.join_finish(&mut a, &first).await.assert_ok();
    let mut b = authenticator();
    harness.join_finish(&mut b, &second).await.assert_refused(StatusCode::FORBIDDEN, "invalid_invitation");
    harness
        .post_public("/api/v1/auth/invite/start", json!({ "token": token, "name": "Three" }))
        .await
        .assert_refused(StatusCode::FORBIDDEN, "invalid_invitation");

    let users = harness.get("/api/v1/users", &cookie).await;
    assert_eq!(users.body["users"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn a_cancelled_prompt_does_not_use_up_the_invitation() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let token = harness.invite(&cookie, &mut owner, "Alice").await;
    harness.join_start(&token, "Abandoned").await.assert_ok();
    let again = harness.join_start(&token, "Phone").await;
    let mut auth = authenticator();
    harness.join_finish(&mut auth, &again).await.assert_ok();
}

#[tokio::test]
async fn a_ceremony_cannot_be_finished_twice() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let token = harness.invite(&cookie, &mut owner, "Alice").await;
    let started = harness.join_start(&token, "One").await;
    let mut auth = authenticator();
    let credential = register(&mut auth, &started.body["options"], origin_url());
    let body = json!({ "ceremony_id": started.body["ceremony_id"], "credential": credential });
    harness.post_public("/api/v1/auth/invite/finish", body.clone()).await.assert_ok();
    harness
        .post_public("/api/v1/auth/invite/finish", body)
        .await
        .assert_refused(StatusCode::CONFLICT, "challenge_used");
}

#[tokio::test]
async fn an_expired_or_cancelled_or_unknown_invitation_is_refused() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let token = harness.invite(&cookie, &mut owner, "Alice").await;
    harness.advance(INVITATION_TTL_MS);
    harness.join_start(&token, "Late").await.assert_refused(StatusCode::FORBIDDEN, "invalid_invitation");

    harness.join_start("not-a-token", "Guess").await.assert_refused(StatusCode::FORBIDDEN, "invalid_invitation");

    let cookie = harness.login(&mut owner).await;
    let token = harness.invite(&cookie, &mut owner, "Bob").await;
    let list = harness.get("/api/v1/users", &cookie).await;
    let id = list.body["invitations"][0]["id"].as_str().unwrap().to_string();
    let started = harness.join_start(&token, "Before").await;
    harness
        .send(Method::DELETE, &format!("/api/v1/invitations/{id}"), Some(&cookie), Some(ORIGIN), None)
        .await
        .assert_ok();
    harness.join_start(&token, "After").await.assert_refused(StatusCode::FORBIDDEN, "invalid_invitation");
    let mut auth = authenticator();
    harness.join_finish(&mut auth, &started).await.assert_refused(StatusCode::FORBIDDEN, "invalid_invitation");
}

#[tokio::test]
async fn an_invitation_ceremony_outlives_neither_its_invitation_nor_five_minutes() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let token = harness.invite(&cookie, &mut owner, "Alice").await;
    let started = harness.join_start(&token, "One").await;
    harness.advance(swifty_hub::passkeys::INVITE_CEREMONY_TTL_MS);
    let mut auth = authenticator();
    harness.join_finish(&mut auth, &started).await.assert_refused(StatusCode::GONE, "challenge_expired");
}

// ----- what family can do

#[tokio::test]
async fn family_can_use_lights_music_tv_pets_and_camera() {
    let (harness, _owner, _cookie, _family, family_cookie, _id) = owner_and_family().await;
    for (action, target, params) in [
        ("light.power", "bulb1", json!({ "on": false })),
        ("light.brightness", "bulb2", json!({ "percent": 40 })),
        ("lights.power", "all_bulbs", json!({ "on": true })),
        ("scene.apply", "evening", json!({})),
        ("speaker.volume", "sonos", json!({ "level": 20 })),
        ("speaker.mute", "sonos", json!({ "muted": true })),
        ("tv.input", "tv", json!({ "input": "HDMI 2" })),
        ("state.read", "camera", json!({})),
        ("state.read", "feeder", json!({})),
    ] {
        let done = harness.execute(&family_cookie, action, target, params).await;
        assert_eq!(done.phase(), "confirmed", "{action}: {}", done.text);
    }
    for (action, target, params) in [
        ("tv.power", "tv", json!({ "on": false })),
        ("pets.feed", "feeder", json!({ "grams": 10 })),
        ("pets.clean", "litter", json!({})),
        ("scene.movie", "movie", json!({})),
    ] {
        let prepared = harness.prepare(&family_cookie, action, target, params).await;
        prepared.assert_ok();
        let done = harness.confirm_tap(&family_cookie, &prepared).await;
        assert_eq!(done.phase(), "confirmed", "{action}: {}", done.text);
    }
}

const OWNER_ONLY_ACTIONS: &[(&str, &str, &str)] = &[
    ("vm.start", "vm100", "prepare"),
    ("vm.stop", "vm100", "prepare"),
    ("vm.restart", "vm100", "prepare"),
    ("vm.force_stop", "vm100", "prepare"),
    ("host.reboot", "host", "prepare"),
    ("host.shutdown", "host", "prepare"),
    ("console.open", "vm102", "prepare"),
    ("passkey.register", "new", "prepare"),
    ("passkey.revoke", "someone", "prepare"),
    ("machine.approve", "vm200", "prepare"),
    ("machine.remove", "vm103", "prepare"),
    ("proxmox.add_host", "new", "prepare"),
    ("agent.pair", "new", "prepare"),
    ("user.invite", "new", "prepare"),
    ("user.remove", "someone", "prepare"),
    ("print.start", "printer", "prepare"),
    ("print.pause", "printer", "prepare"),
    ("router.wireguard_restart", "router", "prepare"),
    ("print.estop", "printer", "execute"),
    ("state.read", "vm100", "execute"),
    ("state.read", "host", "execute"),
    ("state.read", "all_vms", "execute"),
    ("state.read", "collector", "execute"),
    ("state.read", "printer", "execute"),
    ("state.read", "router", "execute"),
];

#[tokio::test]
async fn family_is_refused_everything_else_on_the_server() {
    let (harness, _owner, _cookie, _family, family_cookie, _id) = owner_and_family().await;
    for (action, target, route) in OWNER_ONLY_ACTIONS {
        let refused = match *route {
            "prepare" => harness.prepare(&family_cookie, action, target, json!({})).await,
            _ => harness.execute(&family_cookie, action, target, json!({})).await,
        };
        refused.assert_refused(StatusCode::FORBIDDEN, "owner_only");
    }
    assert_eq!(harness.devices.total_commands(), 0);
    assert_eq!(harness.devices.total_reads(), 0);
    assert!(harness.audit_count("refused: owner_only") >= OWNER_ONLY_ACTIONS.len());
}

#[tokio::test]
async fn a_prepared_action_belongs_to_the_session_that_prepared_it() {
    let (harness, _owner, cookie, _family, family_cookie, _id) = owner_and_family().await;
    let prepared = harness.prepare(&cookie, "vm.start", "vm100", json!({})).await;
    prepared.assert_ok();
    harness.confirm_tap(&family_cookie, &prepared).await.assert_refused(StatusCode::FORBIDDEN, "wrong_session");
    assert_eq!(harness.devices.total_commands(), 0);
}

const OWNER_ONLY_ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/v1/audit"),
    ("GET", "/api/v1/audit/verify"),
    ("GET", "/api/v1/passkeys"),
    ("PATCH", "/api/v1/passkeys/x"),
    ("POST", "/api/v1/passkeys/register/finish"),
    ("GET", "/api/v1/users"),
    ("DELETE", "/api/v1/invitations/x"),
    ("POST", "/api/v1/rooms"),
    ("PATCH", "/api/v1/rooms/x"),
    ("DELETE", "/api/v1/rooms/x"),
    ("PUT", "/api/v1/rooms/order"),
    ("PUT", "/api/v1/rooms/x/devices"),
    ("POST", "/api/v1/rooms/x/scenes"),
    ("DELETE", "/api/v1/rooms/x/scenes/y"),
    ("PUT", "/api/v1/devices/bulb1/name"),
    ("GET", "/api/v1/lights"),
    ("POST", "/api/v1/lights"),
    ("POST", "/api/v1/lights/discover"),
    ("POST", "/api/v1/lights/hue/pair"),
    ("PUT", "/api/v1/lights/x/address"),
    ("GET", "/api/v1/machines"),
    ("POST", "/api/v1/machines/scan"),
    ("GET", "/api/v1/history/host"),
    ("GET", "/api/v1/history/vm100"),
    ("GET", "/api/v1/history/collector"),
    ("GET", "/api/v1/health"),
    ("GET", "/api/v1/console/connect"),
];

#[tokio::test]
async fn family_is_refused_the_owner_routes_before_anything_is_read() {
    let (harness, _owner, _cookie, _family, family_cookie, _id) = owner_and_family().await;
    for (method, path) in OWNER_ONLY_ROUTES {
        let method: Method = method.parse().unwrap();
        let body = (method != Method::GET && method != Method::DELETE).then(|| json!({}));
        let refused = harness.send(method, path, Some(&family_cookie), Some(ORIGIN), body).await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN, "{path}: {}", refused.text);
        assert_eq!(refused.code(), "owner_only", "{path}: {}", refused.text);
    }
    assert_eq!(harness.devices.total_reads(), 0);
    let log = harness.dump("audit_log");
    assert!(log.contains("access.owner_only"));
    assert!(log.contains("/api/v1/audit"), "the route is named from the router's table");
}

#[tokio::test]
async fn nobody_without_a_session_reaches_the_new_routes() {
    let harness = Harness::with(|_| {});
    for (method, path) in OWNER_ONLY_ROUTES.iter().chain([("GET", "/api/v1/me"), ("GET", "/api/v1/rooms")].iter()) {
        let method: Method = method.parse().unwrap();
        let refused = harness.send(method, path, None, Some(ORIGIN), Some(json!({}))).await;
        assert_eq!(refused.status, StatusCode::UNAUTHORIZED, "{path}");
    }
}

#[tokio::test]
async fn a_new_route_still_needs_the_right_origin() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    for path in ["/api/v1/rooms", "/api/v1/machines/scan", "/api/v1/lights/discover"] {
        let refused = harness.send(Method::POST, path, Some(&cookie), Some(EVIL_ORIGIN), Some(json!({}))).await;
        refused.assert_refused(StatusCode::FORBIDDEN, "bad_origin");
    }
}

#[tokio::test]
async fn family_only_sees_what_they_can_use() {
    let (harness, _owner, cookie, _family, family_cookie, _id) = owner_and_family().await;
    let devices = harness.get("/api/v1/devices", &family_cookie).await;
    let targets: Vec<&str> =
        devices.body["devices"].as_array().unwrap().iter().map(|d| d["target"].as_str().unwrap()).collect();
    for hidden in ["vm100", "host", "router", "collector", "printer", "plex"] {
        assert!(!targets.contains(&hidden), "{hidden} was listed");
    }
    for shown in ["bulb1", "sonos", "tv", "feeder", "camera"] {
        assert!(targets.contains(&shown), "{shown} was not listed");
    }
    let state = harness.get("/api/v1/state", &family_cookie).await;
    assert!(state.body["devices"].get("vm100").is_none());
    assert!(state.body["devices"].get("bulb1").is_some());
    harness.get("/api/v1/state/vm100", &family_cookie).await.assert_refused(StatusCode::FORBIDDEN, "owner_only");

    let catalog = harness.get("/api/v1/catalog", &family_cookie).await;
    let actions: Vec<&str> =
        catalog.body["actions"].as_array().unwrap().iter().map(|a| a["id"].as_str().unwrap()).collect();
    for hidden in ["vm.start", "host.reboot", "console.open", "passkey.register", "print.start", "user.invite"] {
        assert!(!actions.contains(&hidden), "{hidden} was listed");
    }
    for shown in ["light.power", "room.power", "tv.power", "pets.feed", "state.read"] {
        assert!(actions.contains(&shown), "{shown} was not listed");
    }
    let read = catalog.body["actions"].as_array().unwrap().iter().find(|a| a["id"] == json!("state.read")).unwrap();
    assert!(!read["targets"].as_array().unwrap().contains(&json!("vm100")));

    let owner_catalog = harness.get("/api/v1/catalog", &cookie).await;
    assert!(owner_catalog.text.contains("vm.start"));
}

#[tokio::test]
async fn family_sees_only_their_own_runs_and_sessions() {
    let (harness, _owner, cookie, _family, family_cookie, family_id) = owner_and_family().await;
    let theirs = harness.execute(&family_cookie, "light.power", "bulb1", json!({ "on": false })).await;
    let mine = harness.execute(&cookie, "light.power", "bulb2", json!({ "on": false })).await;
    let (theirs, mine) =
        (theirs.body["run_id"].as_str().unwrap().to_string(), mine.body["run_id"].as_str().unwrap().to_string());

    let list = harness.get("/api/v1/actions/runs", &family_cookie).await;
    let ids: Vec<&str> = list.body["runs"].as_array().unwrap().iter().map(|r| r["run_id"].as_str().unwrap()).collect();
    assert_eq!(ids, [theirs.as_str()]);
    harness.get(&format!("/api/v1/actions/runs/{theirs}"), &family_cookie).await.assert_ok();
    harness
        .get(&format!("/api/v1/actions/runs/{mine}"), &family_cookie)
        .await
        .assert_refused(StatusCode::NOT_FOUND, "unknown_run");
    harness.get(&format!("/api/v1/actions/runs/{theirs}"), &cookie).await.assert_ok();

    let sessions = harness.get("/api/v1/sessions", &family_cookie).await;
    let rows = sessions.body["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["user_id"], json!(family_id));
    let owner_sessions = harness.get("/api/v1/sessions", &cookie).await;
    assert_eq!(owner_sessions.body["sessions"].as_array().unwrap().len(), 2);
    let owner_ref =
        owner_sessions.body["sessions"].as_array().unwrap().iter().find(|row| row["role"] == json!("owner")).unwrap()
            ["session_ref"]
            .as_str()
            .unwrap()
            .to_string();
    harness
        .send(Method::DELETE, &format!("/api/v1/sessions/{owner_ref}"), Some(&family_cookie), Some(ORIGIN), None)
        .await
        .assert_refused(StatusCode::NOT_FOUND, "unknown_session");
    harness.get("/api/v1/session", &cookie).await.assert_ok();
}

#[tokio::test]
async fn family_cannot_tell_which_computers_exist() {
    let (harness, _owner, _cookie, _family, family_cookie, family_id) = owner_and_family().await;
    harness
        .app
        .store
        .with(|conn| {
            let seen = swifty_plugin_api::SeenMachine { host: "primary".to_string(), vmid: 150, name: "x".to_string() };
            swifty_hub::inventory::record_seen(conn, 0, "proxmox", &[seen])
        })
        .unwrap();
    let targets = ["vm100", "vm101", "host", "router", "vm150", "vm999", "no-such-thing"];
    let mut answers = Vec::new();
    for target in targets {
        let refused = harness.get(&format!("/api/v1/state/{target}"), &family_cookie).await;
        refused.assert_refused(StatusCode::FORBIDDEN, "owner_only");
        answers.push(refused.text);
    }
    assert!(answers.windows(2).all(|pair| pair[0] == pair[1]), "{answers:?}");
    assert_eq!(harness.devices.total_reads(), 0);

    let rows = harness
        .app
        .store
        .with(|conn| {
            let mut statement =
                conn.prepare("SELECT target, user_ref FROM audit_log WHERE action = 'access.owner_only'")?;
            let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap();
    assert_eq!(rows.len(), targets.len());
    assert!(rows.iter().all(|(route, user)| route == "/api/v1/state/{target}" && *user == family_id), "{rows:?}");

    // What family may see still answers as it did. A target that merely looks like a
    // light is not one: the index does not show it to family, so it is refused like
    // any other target they cannot see.
    harness.get("/api/v1/state/bulb1", &family_cookie).await.assert_ok();
    harness
        .get("/api/v1/state/wiz-a8bb50aaaaaa", &family_cookie)
        .await
        .assert_refused(StatusCode::FORBIDDEN, "owner_only");
}

#[tokio::test]
async fn a_request_joined_to_someone_elses_run_is_recorded_and_can_be_followed() {
    let (harness, mut owner, cookie) = Harness::signed_in_with(|c| {
        c.timing.sync_wait_ms = 5;
        c.timing.confirm_timeout_ms = Some(600);
    })
    .await;
    let (_family, family_cookie, family_id) = harness.add_family(&cookie, &mut owner, "Alice").await;
    let (_other, other_cookie, _other_id) = harness.add_family(&cookie, &mut owner, "Bob").await;
    harness.devices.set_behavior("bulb1", Behavior::AcceptsButIgnores);

    let first = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    assert_eq!(first.phase(), "commanded", "{}", first.text);
    let joined = harness.execute(&family_cookie, "light.power", "bulb1", json!({ "on": false })).await;
    assert_eq!(joined.status, StatusCode::ACCEPTED, "{}", joined.text);
    assert_eq!(joined.body["duplicate"], json!(true));
    assert_eq!(joined.body["run_id"], first.body["run_id"]);
    assert_eq!(harness.devices.command_count("bulb1"), 1, "one command reaches the device");

    let run_id = first.body["run_id"].as_str().unwrap();
    let rows = harness
        .app
        .store
        .with(|conn| {
            let mut statement = conn.prepare(
                "SELECT action, target, run_id, user_ref FROM audit_log WHERE result = 'joined_existing_run'",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap();
    assert_eq!(rows, [("light.power".to_string(), "bulb1".to_string(), run_id.to_string(), family_id)]);

    let path = format!("/api/v1/actions/runs/{run_id}");
    harness.get(&path, &family_cookie).await.assert_ok();
    harness.get(&path, &other_cookie).await.assert_refused(StatusCode::NOT_FOUND, "unknown_run");

    // Asking twice yourself is not joining someone else's.
    let again = harness.execute(&cookie, "light.power", "bulb1", json!({ "on": false })).await;
    assert_eq!(again.body["duplicate"], json!(true));
    assert_eq!(harness.audit_count("joined_existing_run"), 1);
    let report = harness.get("/api/v1/audit/verify", &cookie).await;
    report.assert_ok();
}

#[tokio::test]
async fn an_ended_session_says_who_ended_it() {
    let (harness, mut owner, cookie, mut family, family_cookie, _id) = owner_and_family().await;
    let end_reason = |session_ref: &str| -> String {
        harness
            .app
            .store
            .with(|conn| {
                conn.query_row("SELECT end_reason FROM sessions WHERE ref = ?1", [session_ref], |row| row.get(0))
            })
            .unwrap()
    };
    let current = |list: &Resp| -> String {
        list.body["sessions"].as_array().unwrap().iter().find(|row| row["current"] == json!(true)).unwrap()
            ["session_ref"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let end = |session_ref: String, cookie: String| {
        let harness = &harness;
        async move {
            let path = format!("/api/v1/sessions/{session_ref}");
            harness.send(Method::DELETE, &path, Some(&cookie), Some(ORIGIN), None).await.assert_ok();
        }
    };

    let theirs = current(&harness.get("/api/v1/sessions", &family_cookie).await);
    end(theirs.clone(), family_cookie.clone()).await;
    assert_eq!(end_reason(&theirs), "ended_by_user");

    let second = harness.login(&mut family).await;
    let theirs = current(&harness.get("/api/v1/sessions", &second).await);
    end(theirs.clone(), cookie.clone()).await;
    assert_eq!(end_reason(&theirs), "ended_by_owner");

    let spare = harness.login(&mut owner).await;
    let mine = current(&harness.get("/api/v1/sessions", &spare).await);
    end(mine.clone(), cookie.clone()).await;
    assert_eq!(end_reason(&mine), "ended_by_user", "the owner ended a session of their own");
}

#[tokio::test]
async fn a_step_up_only_offers_your_own_passkeys() {
    let (harness, _owner, cookie, _family, _family_cookie, _id) = owner_and_family().await;
    let prepared = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    let started = harness.step_up_start(&cookie, &prepared).await;
    started.assert_ok();
    let allowed = started.body["options"]["publicKey"]["allowCredentials"].as_array().unwrap();
    assert_eq!(allowed.len(), 1, "only the owner's passkey may answer");
}

#[tokio::test]
async fn a_family_passkey_cannot_answer_the_owners_step_up() {
    let (harness, _owner, cookie, mut family, _family_cookie, _id) = owner_and_family().await;
    let prepared = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    let started = harness.step_up_start(&cookie, &prepared).await;

    // Sign the bound challenge with the family passkey, as if it had been offered.
    let mut options = started.body["options"].clone();
    let login = harness.login_start().await;
    options["publicKey"]["allowCredentials"] = login.body["options"]["publicKey"]["allowCredentials"].clone();
    let assertion = assert_with(&mut family, &options, origin_url());
    harness
        .confirm_assertion(&cookie, &prepared, &assertion)
        .await
        .assert_refused(StatusCode::UNAUTHORIZED, "assertion_rejected");
    assert_eq!(harness.devices.total_commands(), 0);
}

// ----- ending someone's access

#[tokio::test]
async fn removing_a_family_member_ends_everything_at_once() {
    let (harness, mut owner, cookie, mut family, family_cookie, family_id) = owner_and_family().await;
    // A sign-in that was already under way when they were removed.
    let under_way = harness.login_start().await;
    let done = harness.critical(&cookie, &mut owner, "user.remove", &family_id, json!({})).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    harness.get("/api/v1/session", &family_cookie).await.assert_refused(StatusCode::UNAUTHORIZED, "session_ended");
    let users = harness.get("/api/v1/users", &cookie).await;
    assert_eq!(users.body["users"].as_array().unwrap().len(), 1);

    // Their passkey is no longer offered, and is not accepted either.
    let started = harness.login_start().await;
    let allowed = started.body["options"]["publicKey"]["allowCredentials"].as_array().unwrap();
    assert_eq!(allowed.len(), 1);
    let assertion = assert_with(&mut family, &under_way.body["options"], origin_url());
    harness
        .login_submit(&under_way.body["ceremony_id"], &assertion, None)
        .await
        .assert_refused(StatusCode::UNAUTHORIZED, "credential_not_active");
}

#[tokio::test]
async fn the_owner_cannot_be_removed_as_family() {
    let (harness, _owner, cookie, _family, _family_cookie, _id) = owner_and_family().await;
    let me = harness.get("/api/v1/me", &cookie).await;
    let owner_id = me.body["session"]["user_id"].as_str().unwrap().to_string();
    harness
        .prepare(&cookie, "user.remove", &owner_id, json!({}))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_target");
}

#[tokio::test]
async fn a_family_passkey_does_not_count_towards_the_owners_last_passkey() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;
    let mut recovery = authenticator();
    let first = harness.get("/api/v1/passkeys", &cookie).await.body["passkeys"][0]["id"].as_str().unwrap().to_string();
    harness.register_passkey(&cookie, &mut owner, &mut recovery, "Recovery", true).await;
    harness.add_family(&cookie, &mut owner, "Alice").await;

    let from_recovery = harness.login(&mut recovery).await;
    harness
        .prepare(&from_recovery, "passkey.revoke", &first, json!({}))
        .await
        .assert_refused(StatusCode::CONFLICT, "last_usable_credential");
}

#[tokio::test]
async fn the_owner_can_take_away_a_family_members_only_passkey() {
    let (harness, mut owner, cookie, _family, family_cookie, family_id) = owner_and_family().await;
    let passkeys = harness.get("/api/v1/passkeys", &cookie).await;
    let id =
        passkeys.body["passkeys"].as_array().unwrap().iter().find(|row| row["user_id"] == json!(family_id)).unwrap()
            ["id"]
            .as_str()
            .unwrap()
            .to_string();
    let done = harness.critical(&cookie, &mut owner, "passkey.revoke", &id, json!({})).await;
    assert_eq!(done.phase(), "confirmed", "{}", done.text);
    harness.get("/api/v1/session", &family_cookie).await.assert_refused(StatusCode::UNAUTHORIZED, "session_ended");
}

// ----- the audit log

fn audit_rows(harness: &Harness) -> Vec<(String, String, String)> {
    harness
        .app
        .store
        .with(|conn| {
            let mut statement = conn.prepare("SELECT action, result, user_ref FROM audit_log ORDER BY seq")?;
            let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap()
}

#[tokio::test]
async fn the_audit_log_records_who_did_it() {
    let (harness, mut owner, cookie, _family, family_cookie, family_id) = owner_and_family().await;
    let owner_id = harness.get("/api/v1/me", &cookie).await.body["session"]["user_id"].as_str().unwrap().to_string();
    harness.execute(&family_cookie, "light.power", "bulb1", json!({ "on": false })).await.assert_ok();
    harness.critical(&cookie, &mut owner, "vm.stop", "vm101", json!({})).await.assert_ok();
    harness.prepare(&family_cookie, "vm.start", "vm100", json!({})).await;

    let rows = audit_rows(&harness);
    assert!(rows.iter().any(|(a, r, u)| a == "light.power" && r == "commanded" && *u == family_id));
    assert!(rows.iter().any(|(a, r, u)| a == "light.power" && r == "confirmed" && *u == family_id));
    assert!(rows.iter().any(|(a, r, u)| a == "vm.stop" && r == "confirmed" && *u == owner_id));
    assert!(rows.iter().any(|(a, r, u)| a == "vm.start" && r == "refused: owner_only" && *u == family_id));
    assert!(rows.iter().any(|(a, _, u)| a == "auth.login" && *u == family_id));
    assert!(rows.iter().any(|(a, _, u)| a == "passkey.enroll" && *u == owner_id));
    assert!(rows.iter().any(|(a, r, u)| a == "user.join" && r == "joined" && *u == family_id));
    assert!(rows.iter().all(|(_, _, u)| !u.is_empty()), "every new row names someone or says nobody");

    let report = harness.get("/api/v1/audit/verify", &cookie).await;
    assert_eq!(report.body["ok"], json!(true), "{}", report.text);
    let read = harness.get("/api/v1/audit", &cookie).await;
    assert!(read.body["rows"][0].get("user_ref").is_some());
}

// ----- what each person keeps

#[tokio::test]
async fn favourites_and_pins_belong_to_one_person() {
    let (harness, _owner, cookie, _family, family_cookie, _id) = owner_and_family().await;
    let room = harness.post("/api/v1/rooms", &cookie, json!({ "name": "Kitchen" })).await;
    let other = harness.post("/api/v1/rooms", &cookie, json!({ "name": "Hall" })).await;
    let (room, other) = (room.body["room"]["id"].as_str().unwrap(), other.body["room"]["id"].as_str().unwrap());

    let put = |cookie: String, path: &'static str, body: Value| {
        let harness = &harness;
        async move { harness.send(Method::PUT, path, Some(&cookie), Some(ORIGIN), Some(body)).await }
    };
    put(cookie.clone(), "/api/v1/me/favourites", json!({ "targets": ["vm100", "bulb1", "evening"] })).await.assert_ok();
    put(family_cookie.clone(), "/api/v1/me/favourites", json!({ "targets": ["sonos"] })).await.assert_ok();
    put(cookie.clone(), "/api/v1/me/pinned-rooms", json!({ "rooms": [other, room] })).await.assert_ok();
    put(family_cookie.clone(), "/api/v1/me/pinned-rooms", json!({ "rooms": [room] })).await.assert_ok();

    let mine = harness.get("/api/v1/me", &cookie).await;
    assert_eq!(mine.body["preferences"]["favourites"], json!(["vm100", "bulb1", "evening"]));
    assert_eq!(mine.body["preferences"]["pinned_rooms"], json!([other, room]));
    let theirs = harness.get("/api/v1/me", &family_cookie).await;
    assert_eq!(theirs.body["preferences"]["favourites"], json!(["sonos"]));
    assert_eq!(theirs.body["preferences"]["pinned_rooms"], json!([room]));

    // A deleted room stops being pinned for everybody.
    harness.send(Method::DELETE, &format!("/api/v1/rooms/{room}"), Some(&cookie), Some(ORIGIN), None).await.assert_ok();
    let theirs = harness.get("/api/v1/me", &family_cookie).await;
    assert_eq!(theirs.body["preferences"]["pinned_rooms"], json!([]));
}

#[tokio::test]
async fn a_favourite_has_to_be_something_you_can_use() {
    let (harness, _owner, cookie, _family, family_cookie, _id) = owner_and_family().await;
    for (who, targets) in [
        (&family_cookie, json!(["vm100"])),
        (&family_cookie, json!(["host"])),
        (&cookie, json!(["not-a-device"])),
        (&cookie, json!(["bulb1", "bulb1"])),
    ] {
        harness
            .send(Method::PUT, "/api/v1/me/favourites", Some(who), Some(ORIGIN), Some(json!({ "targets": targets })))
            .await
            .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_favourites");
    }
    harness
        .send(Method::PUT, "/api/v1/me/pinned-rooms", Some(&cookie), Some(ORIGIN), Some(json!({ "rooms": ["nope"] })))
        .await
        .assert_refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_pins");
}

// ----- voice follows the same rules

#[tokio::test]
async fn voice_gives_family_no_more_than_the_panel_does() {
    let speech = FakeSpeech::start().await;
    let config = speech.config();
    let (harness, mut owner, cookie) = Harness::signed_in_with(|c| c.speech = Some(config)).await;
    let (_family, family_cookie, _id) = harness.add_family(&cookie, &mut owner, "Alice").await;

    speech.say("stop alpha");
    let refused = post_voice(&harness, &family_cookie).await;
    assert_eq!(refused.body["kind"], json!("refused"), "{}", refused.text);
    assert_eq!(refused.body["code"], json!("owner_only"));

    speech.say("stop the vm");
    let choices = post_voice(&harness, &family_cookie).await;
    assert_eq!(choices.body["kind"], json!("choices"));
    assert_eq!(choices.body["choices"], json!([]), "family is not offered what they cannot run");

    speech.say("turn off bulb 1");
    let done = post_voice(&harness, &family_cookie).await;
    assert_eq!(done.body["kind"], json!("executed"), "{}", done.text);
    assert_eq!(harness.devices.total_commands(), 1);
}

#[tokio::test]
async fn enrollment_tokens_still_make_only_the_first_person() {
    let (harness, _owner, _cookie, _family, _family_cookie, _id) = owner_and_family().await;
    let error = swifty_hub::passkeys::Passkeys::issue_enroll_token(&harness.app.store, harness.now()).unwrap_err();
    assert_eq!(error.code, "already_enrolled");
}
