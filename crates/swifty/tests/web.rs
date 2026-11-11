//! The served page and its files, their Content-Security-Policy, and rules that hold
//! across the source tree.

mod common;

use std::collections::BTreeSet;

use axum::http::{header, Method, StatusCode};
use common::*;
use serde_json::json;
use swifty_hub::http::assets::{embedded, CONTENT_SECURITY_POLICY};

async fn page() -> Resp {
    Harness::new().send(Method::GET, "/", None, None, None).await
}

/// The shell at `/`.
fn shell() -> &'static str {
    embedded().into_iter().find(|(path, _)| path == "/").unwrap().1
}

/// Everything the page is made of, the shell and every module and stylesheet, one after
/// the other, so a rule about the page holds wherever its code now lives.
fn shipped() -> String {
    embedded().into_iter().map(|(_, text)| text).collect::<Vec<_>>().join("\n")
}

/// The embedded scripts, by the path they are served at.
fn scripts() -> Vec<(String, &'static str)> {
    embedded().into_iter().filter(|(path, _)| path.ends_with(".js")).collect()
}

#[tokio::test]
async fn the_console_is_served_at_the_root() {
    let response = page().await;
    response.assert_ok();
    assert!(response.text.contains("<title>Swifty Home</title>"));
    assert!(response.headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"));
}

#[tokio::test]
async fn the_policy_is_strict() {
    let response = page().await;
    let csp = response.headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
    assert!(csp.contains("default-src 'none'"));
    assert!(csp.contains("connect-src 'self'"));
    assert!(csp.contains("frame-ancestors 'none'"));
    assert!(csp.contains("base-uri 'none'"));
    assert!(csp.contains("form-action 'none'"));
    assert!(csp.contains("object-src 'none'"));
    let script_src = csp.split(';').find(|p| p.trim_start().starts_with("script-src")).unwrap();
    assert!(!script_src.contains("unsafe"), "{script_src}");
    assert!(!script_src.contains("http"), "{script_src}");
    let style_src = csp.split(';').find(|p| p.trim_start().starts_with("style-src ")).unwrap();
    assert!(!style_src.contains("unsafe"), "{style_src}");
    assert!(!csp.contains("googleapis"));
}

#[tokio::test]
async fn scripts_and_styles_come_only_from_this_origin() {
    // Nothing is inline any more, so there is nothing to hash: only the embedded files run.
    let response = page().await;
    let csp = response.headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
    let directive = |name: &str| csp.split(';').map(str::trim).find(|p| p.split(' ').next() == Some(name)).unwrap();
    assert_eq!(directive("script-src"), "script-src 'self'");
    assert_eq!(directive("style-src"), "style-src 'self'");
    assert!(!csp.contains("'sha256-"), "{csp}");
}

#[test]
fn no_inline_script_or_style_remains() {
    let html = shell();
    assert_eq!(html.matches("<script").count(), 1, "one script element");
    assert!(html.contains(r#"<script type="module" src="/app/main.js"></script>"#));
    assert!(!html.contains("<style"), "no style element");
    assert_eq!(html.matches("<link").count(), 1, "one stylesheet");
    assert!(html.contains(r#"<link rel="stylesheet" href="/app/css/app.css">"#));
}

#[test]
fn the_shipped_page_has_no_inline_event_handlers() {
    // The policy allows no inline script, so a handler in the markup, or in markup a
    // script writes, would silently do nothing.
    for (path, text) in embedded() {
        for handler in [" onclick=", " onload=", " onerror=", " onchange=", " oninput=", " onsubmit="] {
            assert!(!text.contains(handler), "found {handler} in {path}");
        }
    }
}

fn action_line<'a>(html: &'a str, name: &str) -> &'a str {
    let start = html.find(&format!("\n  {name}(")).unwrap_or_else(|| panic!("no action {name}"));
    let rest = &html[start + 1..];
    &rest[..rest.find('\n').unwrap()]
}

#[test]
fn every_device_control_gets_the_benefit_of_the_doubt() {
    let html = &shipped();
    for name in [
        "bulbOn",
        "bulbBri",
        "bulbHue",
        "lightsLevel",
        "roomLevel",
        "roomScene",
        "plug",
        "vol",
        "mute",
        "night",
        "tvPower",
        "tvInput",
        "sceneApply",
        "sceneMovie",
        "acPower",
        "acMode",
        "acTemp",
        "acFan",
    ] {
        assert!(action_line(html, name).contains("lean:"), "{name} shows nothing until the device answers");
    }
}

#[test]
fn machines_the_printer_pets_and_passkeys_keep_their_honest_state() {
    let html = &shipped();
    for name in ["vm", "host", "wg", "approve", "printPause", "printStart", "estop", "feed", "clean", "revoke"] {
        assert!(!action_line(html, name).contains("lean:"), "{name} must not show a value before it is true");
    }
}

#[test]
fn the_overlay_is_held_through_the_whole_life_of_a_command() {
    let html = &shipped();
    let body = |name: &str| {
        let start = html.find(&format!("function {name}(")).unwrap_or_else(|| panic!("no function {name}"));
        let rest = &html[start..];
        &rest[..rest.find("\n}").unwrap_or_else(|| rest.find('\n').unwrap())]
    };
    assert!(body("run").contains("wantSet(a);PENDING[a.key]=a;render()"), "set before the first paint");
    assert!(body("settled").contains("wantSettled(a)"), "kept after success");
    assert!(body("failAction").contains("wantClear(a)"), "dropped on a failure");
    assert!(body("refused").contains("wantClear(a)"), "dropped on a refusal");
    assert!(body("applyState").contains("wantApply()"), "laid back over each poll");
    assert!(html.contains("WANT_MS=10000"), "about ten seconds");
    assert!(!body("lean").contains("*0.5"), "the full target, not half way");
}

#[test]
fn home_and_the_wall_pick_a_room_and_the_pinned_tile_is_gone() {
    let html = &shipped();
    for id in ["id=\"roompick\"", "id=\"wall-roompick\""] {
        assert!(html.contains(id), "{id}");
    }
    assert_eq!(html.matches("role=\"radiogroup\"").count(), 2);
    assert!(html.contains("store.get('home-room',null)"), "remembered on this device");
    assert!(html.contains("wireTile($('#t-lights'),'lights',pickedRoom)"), "the Home tile follows the pick");
    assert!(html.contains("wireTile($('#w-lights'),'lights',pickedRoom)"), "the wall tile follows the pick");
    assert!(!html.contains("paintPins"), "the same room would be on Home twice");
    assert!(!html.contains("id=\"pins\""));
}

#[test]
fn sockets_are_shown_only_when_there_are_some() {
    let html = &shipped();
    assert!(html.contains("id=\"plugs\" role=\"group\" aria-label=\"Sockets\" hidden"));
    assert!(html.contains("pl.hidden=!shown.length"), "none at all, or none here and none without a room");
    assert!(html.contains("(p.reason||NR)"), "the existing reason text");
}

#[test]
fn air_conditioners_follow_the_room_can_be_arranged_and_are_shown_only_when_there_are_some() {
    let html = &shipped();
    assert!(html.contains("id=\"acs\" role=\"group\" aria-label=\"Air conditioning\" hidden"));
    assert!(html.contains("D.acs.filter(c=>inView(c.target))"), "it follows the picked room");
    assert!(html.contains("al.hidden=!acShown.length"), "none at all, or none here");
    assert!(html.contains("...D.acs.map(c=>[c.target,'snow'])"), "Arrange can move it");
    assert!(html.contains("${placeRow(target,'Tap to change')}"), "its sheet moves it too");
    assert!(html.contains("return D.plugs.concat(D.acs).filter(p=>p.target===a.key)"), "the overlay reaches it");
}

#[test]
fn the_page_only_offers_what_the_server_accepts_for_an_air_conditioner() {
    use swifty_hub::catalog::{find, AC_FANS, AC_MODES};
    use swifty_plugin_api::ParamKind;
    let html = &shipped();
    let start = html.find("const AC_MODES_SHOWN=").unwrap();
    let block = &html[start..start + html[start..].find('\n').unwrap()];
    for value in AC_MODES.iter().chain(AC_FANS) {
        assert!(block.contains(&format!("'{value}'")), "{value} is not offered");
    }
    let def = find("ac.temperature").unwrap();
    let ParamKind::Int { min, max } = def.params[0].kind else { panic!("no longer a number") };
    assert!(html.contains(&format!("AC_LOW={min},AC_HIGH={max}")), "the same limits as the catalog");
}

#[test]
fn every_button_on_the_remote_is_a_key_the_server_knows() {
    let html = &shipped();
    let keys: Vec<&str> = html.split("remoteKey('").skip(1).map(|rest| rest.split('\'').next().unwrap()).collect();
    assert_eq!(keys.len(), 14, "{keys:?}");
    for key in keys {
        assert!(swifty_hub::catalog::TV_KEYS.contains(&key), "{key}");
    }
}

/// One embedded file's text, by the path it is served at.
fn file(path: &str) -> &'static str {
    embedded().into_iter().find(|(p, _)| p == path).unwrap_or_else(|| panic!("{path} is not embedded")).1
}

#[test]
fn a_device_is_known_by_its_class_and_never_by_the_shape_of_its_target() {
    let html = &shipped();
    // What a device is comes from `/devices`: its class, then its capabilities.
    assert!(html.contains("const classOf=t=>{const d=DEVINFO[t];if(d&&d.class)return d.class;"), "the class");
    for class in ["light", "plug", "aircon"] {
        assert!(html.contains(&format!("targetsOf('{class}')")), "{class} by class");
    }
    assert!(html.contains("const capOf=(t,cap)=>capsOf(t).find(c=>c.cap===cap)||null;"), "the capabilities");
    // The old rules that read a device off its target are gone, with every target the page used to name.
    for shape in [
        "/^(wiz|hue)-/",
        "|hue-[1-9]",
        "/^(bulb|wiz-|hue-)/",
        "/^plug-",
        "/^ac-",
        "'sonos'",
        "'collector'",
        "D.sonos",
        "vm100",
        "vm101",
        "vm102",
        "vm103",
    ] {
        assert!(!html.contains(shape), "the page still reads a device off its target: {shape}");
    }
    // A light behind a bridge is a light like any other.
    assert!(!html.contains("read-only here"), "the bridge's lights can be controlled now");
}

#[test]
fn every_class_has_a_card_and_any_other_class_gets_the_generic_one() {
    let cards = file("/app/cards/index.js");
    for class in swifty_plugin_mock::CLASSES {
        let name = serde_json::to_value(class).unwrap();
        let name = name.as_str().unwrap();
        if name == "other" {
            continue;
        }
        assert!(cards.contains(&format!("\n  {name}:{{label:")), "no card for the class {name}");
    }
    assert!(cards.contains("const cardOf=cls=>CARDS[cls]||GENERIC;"), "an unknown class falls back");
    assert!(cards.contains("const GENERIC={label:'Other devices',one:'Device',icon:'tech',sheet:t=>'device:'+t};"));
    // The generic card draws the catalogue's actions for the device, each with a form from its parameters.
    let generic = file("/app/cards/generic.js");
    assert!(generic.contains("(d.targets||[]).includes(t)"), "every action the catalogue admits for it");
    for kind in ["boolean", "enum", "integer"] {
        assert!(generic.contains(&format!("s.type==='{kind}'")), "a parameter of type {kind}");
    }
    // Home shows a device of a class it has no card for.
    assert!(file("/app/pages/home.js").contains("Object.keys(DEVINFO).filter(t=>!hasCard(classOf(t))&&inView(t))"));
}

#[test]
fn the_setup_wizard_draws_every_step_and_field_the_contract_has() {
    use swifty_plugin_api::{FieldType, SetupResult, SetupStep};
    let setup = file("/app/pages/setup.js");
    let tag = |step: SetupStep| serde_json::to_value(step).unwrap()["step"].as_str().unwrap().to_string();
    let text = String::new;
    let steps = [
        SetupStep::Form { title: text(), text: text(), fields: Vec::new() },
        SetupStep::Wait { title: text(), text: text(), poll_ms: 1_000, timeout_s: 60 },
        SetupStep::Choose { title: text(), text: text(), options: Vec::new(), multiple: true, min: 1 },
        SetupStep::External { title: text(), text: text(), link: None, fields: Vec::new() },
        SetupStep::Done(SetupResult {
            config: serde_json::json!({}),
            secrets: Vec::new(),
            adopt: Vec::new(),
            summary: text(),
        }),
        SetupStep::Failed { reason: text(), retry: true },
    ];
    for step in steps {
        let name = tag(step);
        assert!(setup.contains(&format!("case '{name}':")), "the wizard does not draw a {name} step");
    }
    let kinds = [
        FieldType::Text { max_len: 8 },
        FieldType::Host,
        FieldType::Url,
        FieldType::HostPort,
        FieldType::Port,
        FieldType::Int { min: 0, max: 1 },
        FieldType::Bool,
        FieldType::Choice { options: Vec::new() },
        FieldType::Mac,
        FieldType::Secret,
        FieldType::Multiline { max_len: 8 },
    ];
    for kind in kinds {
        let name = serde_json::to_value(kind).unwrap()["type"].as_str().unwrap().to_string();
        let drawn = setup.contains(&format!("t==='{name}'")) || setup.contains(&format!("{name}:'Such as"));
        assert!(drawn || name == "text", "the wizard does not know a {name} field");
    }
    // A secret is never filled in, is emptied as soon as it is read, and is never kept for a second try.
    assert!(setup.contains("const pre=t==='secret'?'':"));
    assert!(setup.contains("for(const el of $$('[data-t=\"secret\"]',root))el.value='';"));
    assert!(setup.contains("if(t!=='secret')entered[f.key]=values[f.key]"));
    assert!(!setup.contains("type=\"password\""), "masked by style, so no browser offers to keep it");
    // Leaving a setup before it is over lets the hub drop it.
    assert!(setup.contains("api('DELETE','/setup/'+enc(id)"));
    assert!(setup.contains("close(){abandon()}"));
}

#[test]
fn integrations_are_added_set_up_again_and_removed_through_the_catalogue() {
    let page = file("/app/pages/integrations.js");
    for (action, target) in [("plugin.add", "'new'"), ("plugin.reconfigure", "id"), ("plugin.remove", "id")] {
        assert!(page.contains(&format!("ACT.integration('{action}',{target},")), "{action} on {target}");
    }
    // The integration action goes through the guards like any other: a tier from the catalogue, the passkey for tier 3.
    let catalog = file("/app/core/catalog.js");
    assert!(catalog.contains(
        "integration(id,target,params,label){return A({action:id,target,params:params||{},cls:'key',owner:true,"
    ));
    for word in ["Local", "Cloud", "Unofficial"] {
        assert!(page.contains(&format!("{word}</span>")), "the {word} badge");
    }
}

#[test]
fn notes_show_on_the_device_and_where_an_action_on_it_is_confirmed() {
    let guards = file("/app/core/guards.js");
    assert!(guards.contains("const notes=new Set(NOTES[a.target]||[]);"), "the owner's notes are marked");
    assert!(guards.contains("<small>Your note</small>"));
    assert!(guards.contains("await loadNotes(a.target)"), "fetched before the confirm is drawn");
    for card in ["vm", "host", "generic", "plug", "aircon", "printer", "camera"] {
        assert!(file(&format!("/app/cards/{card}.js")).contains("notesBlock("), "{card} shows its notes");
    }
}

#[test]
fn the_page_follows_the_event_stream_and_keeps_polling() {
    let events = file("/app/core/events.js");
    assert!(events.contains("new EventSource(apiUrl('/events'))"));
    for kind in ["run", "state", "devices", "plugin", "setup", "resync"] {
        assert!(events.contains(&format!("es.addEventListener('{kind}',")), "the page ignores {kind} events");
    }
    let lock = file("/app/ui/lock.js");
    assert!(lock.contains("startPolling();startEvents();"), "polling stays beside the stream");
    assert!(lock.contains("S.locked=true;stopEvents();"), "the stream closes with the lock");
    assert!(file("/app/core/actions.js").contains("await runWait(v.run_id,600)"), "a run event wakes its follower");
}

/// Words of one house that the page used to show: its computers, their hardware and its network.
/// The page takes every name from the API now; product names appear only as plugin ids in action ids.
/// The house's own names are not listed: a list of them would be the leak. The private denylist of
/// `cargo xtask check-public-tree --denylist` holds them and reads the page's files too.
const NOT_IN_THE_PAGE: &[&str] = &[
    "vmbr",
    "10 GbE",
    "Windows PC",
    "Ubuntu",
    "TrueNAS",
    "eARC",
    "Klipper",
    "Petkit",
    "Toshiba",
    "Sonos",
    "webOS",
    "WiZ",
    "Glimdock",
    "Proxmox",
    "WireGuard",
    "Philips",
    "Moonraker",
    "Plex",
    "P cores",
    "E cores",
    "gaming",
    "graphics card",
    "build runner",
    "Bluetooth",
    "10.0.0.",
];

#[test]
fn no_house_is_named_in_the_page() {
    for (path, text) in embedded() {
        for word in NOT_IN_THE_PAGE {
            assert!(!text.contains(word), "{path} names {word}");
        }
    }
}

#[test]
fn the_shipped_page_loads_nothing_from_anywhere_else() {
    assert!(shell().starts_with("<!doctype html>"));
    let html = &shipped();
    // a link tag reaches out before any policy applies: the one there is goes to this origin
    for (at, _) in html.match_indices("<link") {
        let tag = &html[at..at + html[at..].find('>').unwrap()];
        assert!(tag.contains(r#"href="/app/"#), "{tag}");
    }
    for (at, _) in html.match_indices("http") {
        let rest = &html[at..];
        assert!(
            rest.starts_with("http://www.w3.org/")
                || rest.starts_with("https:'")
                || rest.starts_with("http:") && !rest.starts_with("http://"),
            "unexpected address near: {}",
            &rest[..rest.len().min(40)]
        );
    }
}

#[test]
fn the_shipped_page_is_not_the_simulation() {
    let html = &shipped();
    for leftover in [
        "simulated in this prototype",
        "Show me a problem",
        "Preview as a family member",
        "function seeded(",
        "function cmdOutput(",
        "const VMS=[\n",
        "const PASSKEYS=[\n",
    ] {
        assert!(!html.contains(leftover), "still in the page: {leftover}");
    }
    // Used only for where the wall clock drifts to, so a screen does not burn in.
    assert_eq!(html.matches("Math.random").count(), 1);
    assert!(html.contains("const rnd=(a,b)=>a+Math.random()*(b-a);"));
}

/// The prefix the page puts before every request: the one constant in `core/api.js`.
fn api_base() -> &'static str {
    let api = embedded().into_iter().find(|(path, _)| path == "/app/core/api.js").unwrap().1;
    const NAMED: &str = "const API_BASE='";
    let start = api.find(NAMED).expect("core/api.js names the API prefix") + NAMED.len();
    &api[start..start + api[start..].find('\'').unwrap()]
}

#[test]
fn the_page_names_the_api_prefix_once() {
    assert_eq!(api_base(), swifty_hub::http::API_PREFIX);
    for (path, text) in embedded() {
        if path != "/app/core/api.js" {
            assert!(!text.contains("/api/"), "{path} names an API path of its own instead of going through api()");
        }
    }
}

#[tokio::test]
async fn every_route_the_page_names_exists() {
    let html = &shipped();
    let harness = Harness::new();
    let mut checked = 0;
    // Every call is `api('METHOD','/path'...)`, with the path below the prefix.
    for call in html.split("api('").skip(1) {
        let Some((method, rest)) = call.split_once('\'') else { continue };
        let Some(rest) = rest.strip_prefix(",'") else { continue };
        if method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) {
            continue;
        }
        let path = rest[..rest.find('\'').unwrap()].split('?').next().unwrap_or_default();
        let plain = path.bytes().all(|b| b.is_ascii_lowercase() || matches!(b, b'/' | b'-'));
        if !plain || !path.starts_with('/') || path.ends_with('/') {
            continue;
        }
        let path = format!("{}{path}", api_base());
        let response = harness.send(Method::GET, &path, None, None, None).await;
        assert_ne!(response.code(), "not_found", "the page calls {path}, which is not a route");
        checked += 1;
    }
    assert!(checked >= 20, "only found {checked} routes in the page");
}

/// Asserts that an answer of the hub has every field the page reads from it, by the hub's own names.
#[track_caller]
fn has(value: &serde_json::Value, keys: &[&str], what: &str) {
    for key in keys {
        assert!(value.get(*key).is_some(), "{what} has no `{key}`: {value}");
    }
}

fn setup_at(id: &str) -> String {
    format!("/api/v1/setup/{id}")
}

/// The targets the hub lists for one instance in `GET /plugins`, as the page counts and lists them.
fn devices_of(listed: &Resp, instance: &str) -> Vec<String> {
    let found = listed.body["instances"].as_array().unwrap().iter().find(|i| i["id"] == json!(instance));
    let Some(found) = found else { return Vec::new() };
    found["devices"].as_array().unwrap().iter().map(|d| d["target"].as_str().unwrap().to_string()).collect()
}

/// The requests the wizard sends, in the order and shapes the page sends them, answered by the real hub:
/// the lists of Integrations, the plugin's page, `plugin.add` with no id, each step, the devices that appear,
/// setting it up again, leaving a setup, and removing it. What the page reads by name is asserted here, so a
/// name that moves on either side fails this test and not a person's setup.
#[tokio::test]
async fn the_wizard_adds_sets_up_again_and_removes_the_mock_through_the_hub_in_the_shapes_the_page_reads() {
    let (harness, mut owner, cookie) = Harness::signed_in().await;

    // Integrations (`loadPlugins`): what each plugin is and reaches, and every instance with its devices.
    let listed = harness.get("/api/v1/plugins", &cookie).await;
    listed.assert_ok();
    let plugin = listed.body["plugins"].as_array().unwrap().iter().find(|p| p["id"] == json!("mock")).unwrap();
    has(
        plugin,
        &["name", "version", "summary", "reach", "network", "classes", "multi_instance", "can_add", "why_not"],
        "a plugin in the list",
    );
    assert_eq!(plugin["reach"]["reach"], json!("local"));
    assert_eq!(plugin["can_add"], json!(true));
    let instance = &listed.body["instances"][0];
    has(
        instance,
        &["id", "plugin", "plugin_name", "in_build", "enabled", "readiness", "unavailable", "health", "devices"],
        "an instance in the list",
    );
    assert!(instance["readiness"]["status"].is_string() && instance["health"]["status"].is_string());
    has(&instance["devices"][0], &["target", "name", "class", "pending"], "a device of an instance");
    assert!(devices_of(&listed, "mock").is_empty(), "the test house has no instance called mock");

    // The plugin's page (`loadPluginFull`): what its setup asks for and its own actions, with their tiers.
    let full = harness.get("/api/v1/plugins/mock", &cookie).await;
    full.assert_ok();
    has(&full.body, &["config", "secrets", "actions"], "a plugin's manifest");
    has(&full.body["secrets"][0], &["slot", "label"], "a secret slot");
    let action = &full.body["actions"][0];
    has(action, &["id", "label", "description", "tier", "tier_effective"], "a plugin's own action");
    assert!(
        ["read", "comfort", "consequential", "critical", "safe_direction"].contains(&action["tier"].as_str().unwrap())
    );
    assert!(action["tier_effective"].is_number() || action["tier_effective"] == json!("S"));

    // Set up: `plugin.add` on `new` with the plugin and no id. The hub picks the id, and the run carries the setup.
    let params = json!({ "plugin": "mock" });
    let prepared = harness.prepare(&cookie, "plugin.add", "new", params.clone()).await;
    prepared.assert_ok();
    has(&prepared.body, &["what", "consequences", "needs"], "the prepared add");
    assert_eq!(prepared.body["needs"], json!("passkey"));
    let confirmed = harness.critical(&cookie, &mut owner, "plugin.add", "new", params.clone()).await;
    confirmed.assert_ok();
    assert_eq!(confirmed.phase(), "confirmed");
    let opened = &confirmed.body["setup"];
    has(opened, &["setup_id", "plugin", "instance", "purpose", "step"], "the setup in the run's answer");
    assert_eq!((opened["purpose"].as_str(), opened["instance"].as_str()), (Some("add"), Some("mock")));
    let id = opened["setup_id"].as_str().unwrap().to_string();
    let step = &opened["step"];
    assert_eq!(step["step"], json!("choose"));
    has(step, &["title", "text", "options", "multiple", "min"], "a choose step");
    let options = step["options"].as_array().unwrap();
    for option in options {
        has(option, &["id", "label", "detail", "selected"], "a choice");
    }
    let all: Vec<&str> = options.iter().map(|o| o["id"].as_str().unwrap()).collect();
    assert!(all.len() > 3, "{all:?}");

    // Every answer is the whole view: `step` is drawn, and a page that reloads asks `GET` for the same.
    let kept: Vec<&str> = all[..all.len() - 1].to_vec();
    let form = harness.post(&setup_at(&id), &cookie, json!({ "input": "chosen", "ids": kept })).await;
    form.assert_ok();
    assert_eq!(form.body["step"]["step"], json!("form"));
    let field = &form.body["step"]["fields"][0];
    has(field, &["key", "label", "help", "kind", "required", "value"], "a field");
    assert_eq!(field["kind"], json!({ "type": "secret" }));
    assert_eq!(field["value"], json!(null), "a secret is never prefilled");
    let shown = harness.get(&setup_at(&id), &cookie).await;
    assert_eq!(shown.body["step"], form.body["step"]);

    // The access code is left empty, and the page leaves an empty field out of the values.
    let done = harness.post(&setup_at(&id), &cookie, json!({ "input": "values", "values": {} })).await;
    done.assert_ok();
    assert_eq!(done.body["finished"], json!(true));
    assert_eq!(done.body["step"]["step"], json!("done"));
    let summary = done.body["step"]["summary"].as_str().unwrap();
    assert!(summary.contains(&kept.len().to_string()) && summary.ends_with("added."), "{summary}");

    // The devices appear: in the instance's list, and in the devices the page reads for Home.
    let listed = harness.get("/api/v1/plugins", &cookie).await;
    let targets = devices_of(&listed, "mock");
    assert_eq!(targets.len(), kept.len(), "{}", listed.text);
    let everything = harness.get("/api/v1/devices", &cookie).await;
    for target in &targets {
        let device = everything.body["devices"].as_array().unwrap().iter().find(|d| d["target"] == json!(target));
        let device = device.unwrap_or_else(|| panic!("{target} is not in /devices"));
        assert_eq!((device["instance"].as_str(), device["plugin"].as_str()), (Some("mock"), Some("mock")));
    }

    // A second one of the same plugin, again with no id, gets the next free one. Leaving the wizard abandons it.
    let second = harness.critical(&cookie, &mut owner, "plugin.add", "new", params).await;
    assert_eq!(second.body["setup"]["instance"], json!("mock2"));
    let left = second.body["setup"]["setup_id"].as_str().unwrap().to_string();
    let abandoned = harness.send(Method::DELETE, &setup_at(&left), Some(&cookie), Some(ORIGIN), None).await;
    abandoned.assert_ok();
    assert_eq!(abandoned.body, json!({ "abandoned": true }));
    harness.get(&setup_at(&left), &cookie).await.assert_refused(StatusCode::NOT_FOUND, "unknown_setup");
    assert!(devices_of(&harness.get("/api/v1/plugins", &cookie).await, "mock2").is_empty());

    // Set up again: `plugin.reconfigure` on the instance. It starts from what is stored.
    let again = harness.critical(&cookie, &mut owner, "plugin.reconfigure", "mock", json!({})).await;
    again.assert_ok();
    let opened = &again.body["setup"];
    assert_eq!((opened["purpose"].as_str(), opened["instance"].as_str()), (Some("reconfigure"), Some("mock")));
    let selected: Vec<&str> = opened["step"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["selected"] == json!(true))
        .map(|o| o["id"].as_str().unwrap())
        .collect();
    assert_eq!(selected, kept, "the devices it has are ticked");
    let id = opened["setup_id"].as_str().unwrap().to_string();
    let two = &kept[..2];
    let form = harness.post(&setup_at(&id), &cookie, json!({ "input": "chosen", "ids": two })).await;
    assert_eq!(form.body["step"]["step"], json!("form"));
    const CODE: &str = "typed-in-the-wizard-4711";
    let typed = json!({ "input": "values", "values": { "access_code": CODE } });
    let done = harness.post(&setup_at(&id), &cookie, typed).await;
    assert_eq!(done.body["step"]["step"], json!("done"), "{}", done.text);
    assert!(!done.text.contains(CODE), "a secret is never sent back");
    let listed = harness.get("/api/v1/plugins", &cookie).await;
    assert_eq!(devices_of(&listed, "mock").len(), 2, "the two that were ticked");
    let stored = listed.body["instances"].as_array().unwrap().iter().find(|i| i["id"] == json!("mock")).unwrap();
    assert_eq!(stored["secrets"], json!(["access_code"]), "the slot is named, never the value");
    assert!(!listed.text.contains(CODE));

    // Remove: the confirmation lists what goes, devices first; then the instance and its devices are gone.
    let prepared = harness.prepare(&cookie, "plugin.remove", "mock", json!({})).await;
    prepared.assert_ok();
    has(&prepared.body, &["what", "consequences", "affected", "needs"], "the prepared removal");
    assert!(prepared.body["consequences"][0].as_str().unwrap().starts_with("Its 2 devices go:"), "{}", prepared.text);
    assert_eq!(prepared.body["affected"].as_array().unwrap().len(), 2);
    let removed = harness.critical(&cookie, &mut owner, "plugin.remove", "mock", json!({})).await;
    removed.assert_ok();
    assert_eq!(removed.phase(), "confirmed");
    assert!(removed.body.get("setup").is_none(), "a removal opens no setup");
    let listed = harness.get("/api/v1/plugins", &cookie).await;
    assert!(listed.body["instances"].as_array().unwrap().iter().all(|i| i["id"] != json!("mock")), "{}", listed.text);
    let everything = harness.get("/api/v1/devices", &cookie).await;
    assert!(everything.body["devices"].as_array().unwrap().iter().all(|d| d["instance"] != json!("mock")));
}

/// What the page reads of the house's scenes and of the owner's notes, and the two places it meets the hub's
/// words: the scenes it knows by id, and a note it recognises in a confirmation by the line itself.
#[tokio::test]
async fn the_page_reads_scenes_and_notes_by_the_names_the_hub_uses() {
    let (harness, _owner, cookie) = Harness::signed_in().await;

    let scenes = harness.get("/api/v1/scenes", &cookie).await;
    scenes.assert_ok();
    let named = file("/app/ui/scenes.js");
    for id in ["movie", "evening", "focus", "all_off"] {
        assert!(named.contains(&format!("'{id}'")), "the page does not name the {id} scene");
        let scene = scenes.body["scenes"].as_array().unwrap().iter().find(|s| s["id"] == json!(id));
        let scene = scene.unwrap_or_else(|| panic!("the hub has no {id} scene: {}", scenes.text));
        has(scene, &["name", "position", "percent", "hue", "overrides", "extras"], "a scene");
        assert!(scene["percent"].is_number() && scene["overrides"].is_array());
    }
    let evening = scenes.body["scenes"].as_array().unwrap().iter().find(|s| s["id"] == json!("evening")).unwrap();
    assert!(evening["overrides"]
        .as_array()
        .unwrap()
        .iter()
        .all(|o| o["target"].is_string() && o["percent"].is_number()));

    // A note is saved as `{notes: [...]}` and read back the same; the confirmation lists the line itself.
    let note = "The shared folders go offline while this is down, so tell the others first.";
    let path = "/api/v1/devices/vm101/notes";
    let saved = harness.send(Method::PUT, path, Some(&cookie), Some(ORIGIN), Some(json!({ "notes": [note] }))).await;
    saved.assert_ok();
    assert_eq!(saved.body["notes"], json!([note]));
    let read = harness.get(path, &cookie).await;
    assert_eq!(read.body["notes"], json!([note]));
    let prepared = harness.prepare(&cookie, "vm.stop", "vm101", json!({})).await;
    prepared.assert_ok();
    let lines: Vec<&str> =
        prepared.body["consequences"].as_array().unwrap().iter().filter_map(|l| l.as_str()).collect();
    assert!(lines.contains(&note), "the page marks the note by its text: {lines:?}");
}

/// The part of a source file from one marker to the next, so a rule is checked where it is meant to hold.
fn between<'a>(text: &'a str, from: &str, to: &str) -> &'a str {
    let start = text.find(from).unwrap_or_else(|| panic!("no `{from}`"));
    let rest = &text[start..];
    &rest[..rest.find(to).unwrap_or_else(|| panic!("no `{to}` after `{from}`"))]
}

#[test]
fn the_threads_are_grouped_by_the_computer_pinned_to_them_from_the_snapshot_and_from_nothing_else() {
    let data = file("/app/core/hostdata.js");
    let monitor = file("/app/pages/monitor.js");
    let store = file("/app/core/store.js");
    // What the collector sends: each guest's pinned threads, the kind of each thread, and the guests by id.
    for read in
        ["snapshot.guests", "g.cpu_affinity", "g.cpu_count", "host.cpu_core_kinds", "'performance'", "'efficiency'"]
    {
        assert!(data.contains(read), "hostdata.js does not read {read}");
    }
    // A null list means the computer may run on any thread; a list the collector could not read is not guessed.
    assert!(data.contains("g.cpu_affinity===null"), "null is a computer that runs anywhere");
    assert!(data.contains("let affinity;"), "an unread configuration stays undefined");
    assert!(monitor.contains("g.affinity===null&&g.running"), "only a running computer is said to run on any thread");
    // The chart groups by what the data says.
    for rule in ["ownersByThread(T.cores.length,T.guests)", "runsOf(owners.map(", "runsOf(kinds.map(", "T.coreKinds"] {
        assert!(monitor.contains(rule), "the thread chart does not use {rule}");
    }
    assert!(
        store.contains("T.guests=guestsOf(c)") && store.contains("T.coreKinds=T.cores?kindsOf(c,T.cores.length):null")
    );
    // A computer is named as this hub names the one with the same VM number, else as the collector does.
    assert!(monitor.contains("String(x.vmid)===String(g.id)") && monitor.contains("||g.name||"), "the names");
    // Nothing about a house: no thread count, no layout, no list of computers, no fixed colours per name.
    let chart = between(monitor, "function drawCores", "function wireCores");
    for house in ["=32", "!==32", "<16", "<8)", "CORE_SLOT", "new Array(32)", "P cores", "E cores"] {
        assert!(!chart.contains(house), "the thread chart carries {house}");
        assert!(!data.contains(house), "hostdata.js carries {house}");
    }
    assert!(!data.contains("vm1") && !monitor.contains("vm1"), "no computer is named in code");
    // The colours are slots by rank among the computers listed, from tokens that exist in all three places.
    assert!(data.contains("g.slot=i%PALETTE+1"));
    let tokens = file("/app/css/tokens.css");
    for slot in 1..=8 {
        assert_eq!(
            tokens.matches(&format!("--core-{slot}:")).count(),
            3,
            "--core-{slot} in light, dark and the dark override"
        );
    }
}

#[test]
fn memory_is_split_by_computer_from_the_hosts_own_figures() {
    let data = file("/app/core/hostdata.js");
    let usage = file("/app/cards/hostusage.js");
    for read in
        ["g.mem_used_bytes", "g.mem_host_bytes", "g.mem_assigned_bytes", "g.mem_total_bytes", "host.mem_total_bytes"]
    {
        assert!(data.contains(read) || file("/app/core/store.js").contains(read), "the page does not read {read}");
    }
    // A computer's share is the host memory it takes, else what it uses; the host's own use is the rest of what is used.
    assert!(data.contains("g.memHost!=null?g.memHost:g.memUsed"));
    assert!(data.contains("Math.max(0,hostUsed-held)"));
    // One bar and one legend, in the colours of the thread chart, and nothing drawn when there is nothing to split.
    assert!(usage.contains("memoryParts(T.memTotal,T.memUsed,T.guests)"));
    assert!(usage.contains("if(!m)return '';"));
    assert!(usage.contains("var(--core-${p.guest.slot})") && usage.contains("var(--core-${g.slot})"));
    assert!(usage.contains(r#"class="stack" role="img""#), "the bar has words for a screen reader");
    assert!(file("/app/cards/host.js").contains("${ramHtml()}"), "the host card shows it");
}

#[test]
fn the_host_card_shows_the_energy_and_its_cost_and_the_owner_sets_the_tariff_through_the_api() {
    let store = file("/app/core/store.js");
    let usage = file("/app/cards/hostusage.js");
    let host = file("/app/cards/host.js");
    // Both requests go through api(), below the prefix, and the page names no path of its own.
    assert!(store.contains("api('GET','/energy',undefined,{quiet:true})"));
    assert!(store.contains("api('PUT','/energy/tariff',body)"));
    assert!(!usage.contains("fetch(") && !usage.contains("/api/"), "the card goes through the store");
    assert!(usage.contains("saveEnergyTariff(got.body)"));
    // The controls: a price, a currency code, a night price with its hours behind a switch, summer hours behind
    // another, and a save, for the owner and only when there is energy to price.
    for control in [
        "field('en-day'",
        "field('en-cur'",
        "sw('en-two'",
        "field('en-night'",
        "hoursField('Night hours','en-ns','en-ne'",
        "timeField(a,",
        "timeField(b,",
        "sw('en-summer'",
        "hoursField('Night hours in summer time','en-ss','en-se'",
        r#"id="en-save""#,
        "isOwner()&&(T.energy||ENERGY.tariff)",
    ] {
        assert!(usage.contains(control), "no {control}");
    }
    assert!(usage.contains(r#"type="time""#), "the hours are times of day");
    // One price all day until night is cheaper; the first price says which it is.
    assert!(usage.contains("two?'Day price per kWh':'Price per kWh'"));
    assert!(usage.contains("field('en-day',dayLabel(two)") && usage.contains("'en-daylabel'"));
    // Labelled as the setup wizard labels its fields, under one heading, with examples as placeholders.
    assert!(usage.contains(r#"<label class="sfield" for="${id}"><span class="sl""#));
    assert!(usage.contains("Electricity tariff") && !usage.contains("What does a kWh cost?"));
    assert!(usage.contains(r#"placeholder="e.g. 0.31""#) && usage.contains(r#"placeholder="e.g. EUR""#));
    assert!(
        usage.contains("priceFrom($('#en-day',box).value)") && usage.contains("currencyFrom($('#en-cur',box).value)")
    );
    // The zone the hours are read in: the tariff's own, else this device's.
    assert!(usage.contains("Intl.DateTimeFormat().resolvedOptions().timeZone"));
    assert!(usage.contains("time_zone:(ENERGY.tariff&&ENERGY.tariff.zone)||ZONE"));
    // What it says: the counter and its date, the scope in words, and the cost only when there is a price.
    assert!(usage.contains("'Energy since '+fmtDay(e.since)"));
    assert!(usage.contains("CPU and GPU, not the whole machine at the wall"));
    assert!(usage.contains("if(t){"), "no tariff, no cost");
    assert!(usage.contains("'By day'") && usage.contains("'At night'"), "a two-rate tariff shows the split");
    assert!(usage.contains("moneyText(led.cost"), "money has its code and two decimals");
    assert!(file("/app/core/hostdata.js").contains("x.toFixed(2)+' '+currency"));
    assert!(host.contains("${energyHtml()}") && host.contains("wireEnergy(root)") && host.contains("paintUsage(root)"));
    // A refresh repaints the readings and never the fields being typed in.
    assert!(usage.contains("!box.contains(document.activeElement)"));
}

#[tokio::test]
async fn the_page_reads_the_energy_answers_by_the_names_the_hub_and_the_collector_use() {
    let (harness, _owner, cookie) = Harness::signed_in().await;
    harness.devices.set_collector(json!({
        "energy": { "since": 1_794_380_000, "total_kwh": 4.0, "scope": "components",
                    "domains": [{ "id": "cpu-package", "label": "CPU package", "kwh": 4.0, "watts": 30.0 }] },
    }));
    swifty_hub::history::sample_collector(&harness.app).await.unwrap();
    let tariff = json!({ "currency": "EUR", "day_price_per_kwh": 0.5, "night_price_per_kwh": 0.2,
                        "night": { "start": "22:00", "end": "06:00" }, "time_zone": "UTC" });
    let set = harness.send(Method::PUT, "/api/v1/energy/tariff", Some(&cookie), Some(ORIGIN), Some(tariff)).await;
    set.assert_ok();
    let read = harness.get("/api/v1/energy", &cookie).await;
    assert_eq!(read.body, set.body, "the answer to the PUT is the answer to the GET");
    has(&read.body, &["tariff", "targets"], "the energy answer");
    has(
        &read.body["tariff"],
        &[
            "currency",
            "day_price_per_kwh",
            "night_price_per_kwh",
            "night",
            "summer_night",
            "time_zone",
            "set_at",
            "band_now",
        ],
        "the tariff",
    );
    let target = &read.body["targets"][0];
    has(
        target,
        &[
            "target",
            "kwh",
            "since",
            "cost",
            "cost_day",
            "cost_night",
            "kwh_day",
            "kwh_night",
            "priced_since",
            "backfilled_kwh",
            "updated_at",
        ],
        "an energy target",
    );

    // The page reads every one of those names, and the collector's.
    let store = file("/app/core/store.js");
    for name in [
        "r.tariff",
        "t.currency",
        "t.day_price_per_kwh",
        "t.night_price_per_kwh",
        "t.night",
        "t.summer_night",
        "t.time_zone",
        "t.band_now",
        "r.targets",
        "x.target",
    ] {
        assert!(store.contains(name), "store.js does not read {name}");
    }
    let usage = file("/app/cards/hostusage.js");
    for name in ["led.cost", "led.cost_day", "led.cost_night", "led.kwh_day", "led.kwh_night", "led.backfilled_kwh"] {
        assert!(usage.contains(name), "hostusage.js does not read {name}");
    }
    let data = file("/app/core/hostdata.js");
    for name in
        ["e.total_kwh", "e.since", "e.scope", "e.domains", "snapshot.energy_watts", "d.label", "d.watts", "d.kwh"]
    {
        assert!(data.contains(name), "hostdata.js does not read {name}");
    }
    // The only unit the page converts is the collector's: its `since` is seconds, the API's are milliseconds.
    assert!(data.contains("e.since*1000"));
    assert_eq!(target["since"], json!(1_794_380_000_000_i64));
}

/// `swifty init` prints `<origin>/#enroll=<token>`. The page reads it as it reads `#invite=`: straight to making
/// the passkey with that token, asking only for a name, and the hash cleared once the passkey is made.
#[tokio::test]
async fn the_enrollment_link_goes_straight_to_the_first_passkey() {
    let lock = file("/app/ui/lock.js");
    assert!(lock.contains("location.hash.match(/^#(invite|enroll)=(.+)$/)"), "both links are read from the hash");
    assert!(lock.contains("showJoin(decodeURIComponent(link[2]),link[1])"), "the token goes to the passkey form");
    // The enrollment form sends the token and the name to the hub's own enrollment routes, then the credential.
    assert!(lock.contains("api('POST','/auth/enroll/start',body,{quiet:true})"));
    assert!(lock.contains("api('POST','/auth/enroll/finish',body,{quiet:true})"));
    assert!(
        lock.contains("L.start({token,name})") && lock.contains("L.finish({ceremony_id:st.ceremony_id,credential})")
    );
    // The invitation link still works the same way, and the hash is cleared once the passkey exists.
    assert!(lock.contains("api('POST','/auth/invite/start',body,{quiet:true})"));
    assert!(lock.contains("history.replaceState(null,'',location.pathname)"));

    // The token the link carries survives the hash untouched, and the hub takes it once, with a name.
    let harness = Harness::new();
    let token = harness.enroll_token();
    assert!(token.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')), "{token}");
    let mut auth = authenticator();
    harness.enroll_with(&mut auth, &token, "This laptop").await.assert_ok();
    let again = harness.post_public("/api/v1/auth/enroll/start", json!({ "token": token, "name": "Again" })).await;
    assert!(!again.status.is_success(), "a token is good once: {}", again.text);
}

/// The page asks for what the hub sends, and nothing is left of what it only guessed while the routes were missing.
#[test]
fn the_page_reads_the_hubs_answers_by_the_hubs_names_and_guesses_no_more() {
    let integrations = file("/app/pages/integrations.js");
    let setup = file("/app/pages/setup.js");
    let store = file("/app/core/store.js");
    // The run's answer: the setup is `setup`, with its own id, plugin, instance and purpose.
    for read in ["v.setup.setup_id", "s.setup_id", "s.purpose==='reconfigure'", "s.instance"] {
        assert!(integrations.contains(read), "integrations.js does not read {read}");
    }
    // The list and the manifest: the plugin's page reads the manifest, the tiers come as a word and a number.
    for read in [
        "loadPluginFull(id)",
        "tier_effective",
        "TIER_OF",
        "can_add",
        "why_not",
        "plugin_name",
        "in_build",
        "d.pending",
    ] {
        assert!(integrations.contains(read), "integrations.js does not read {read}");
    }
    assert!(store.contains("api('GET','/plugins/'+enc(id)"), "the manifest is a request of its own");
    // Saving a scene is announced as `devices`, so the page reads the scenes again then; the audit actions the
    // hub added have words in the history.
    assert!(file("/app/core/events.js").contains("await loadScenes();"));
    for action in [
        "scene.save",
        "device.notes",
        "device.adopt",
        "device.remove",
        "device.discover",
        "plugin.setup",
        "energy.price",
    ] {
        assert!(store.contains(&format!("'{action}':")), "the history has no words for {action}");
    }
    // The id of a new integration is the hub's rule: a letter, then up to 15 letters and digits.
    assert!(integrations.contains("/^[a-z][a-z0-9]{0,15}$/"));
    // A step that can be tried again comes with the step to draw once more, and a wait is asked again with a poll.
    for read in ["const retryOf=", "SETUP.retry", "send({input:'poll'})", "{stopTimers();SETUP.waitKey=''}"] {
        assert!(setup.contains(read), "setup.js does not read {read}");
    }
    // What the page guessed before the routes existed is gone, and so is every tolerance for a missing route.
    let everything = shipped();
    for guess in [
        "r.setup_id||(r.setup&&r.setup.id)",
        "v.result&&typeof v.result==='object'",
        "applied_tier",
        "typeof i.devices==='number'",
        "p.instances||[]",
        "e.status===404?''",
        "a-z0-9-]{1,31}",
        "cannot list its integrations yet",
        "cannot run setups yet",
        "phase==='simulated'?'Dry-run: nothing was removed.'",
    ] {
        assert!(!everything.contains(guess), "the page still carries a guess: {guess}");
    }
}

/// Every relative module or stylesheet a file names, as the path it is served at.
fn references(path: &str, text: &str) -> Vec<String> {
    let dir = &path[..path.rfind('/').unwrap()];
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let named = if line.starts_with("import ") && line.ends_with("';") {
            line[..line.len() - 2].rsplit('\'').next()
        } else if let Some(rest) = line.strip_prefix("@import url('") {
            rest.split('\'').next()
        } else {
            None
        };
        if let Some(name) = named {
            let mut parts: Vec<&str> = dir.split('/').collect();
            for piece in name.split('/') {
                match piece {
                    "." => {}
                    ".." => {
                        parts.pop();
                    }
                    other => parts.push(other),
                }
            }
            out.push(parts.join("/"));
        }
    }
    out
}

#[test]
fn everything_the_page_loads_is_embedded() {
    let served: BTreeSet<String> = embedded().into_iter().map(|(path, _)| path).collect();
    let mut wanted = vec!["/app/main.js".to_string(), "/app/css/app.css".to_string()];
    for (path, text) in embedded() {
        wanted.extend(references(&path, text));
    }
    assert!(wanted.len() > 60, "only {} references found", wanted.len());
    for path in &wanted {
        assert!(served.contains(path), "{path} is loaded but not embedded");
    }
    // and nothing is embedded that nothing loads
    for path in served.iter().filter(|p| p.as_str() != "/") {
        assert!(wanted.contains(path), "{path} is embedded but nothing loads it");
    }
}

#[test]
fn every_file_under_web_app_is_embedded() {
    fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                let rel = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
                out.insert(format!("/app/{rel}"));
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/app");
    let mut on_disk = BTreeSet::new();
    walk(&root, &root, &mut on_disk);
    let served: BTreeSet<String> = embedded().into_iter().map(|(path, _)| path).filter(|p| p != "/").collect();
    assert_eq!(on_disk, served);
    // and each embedded copy is the file as it is on disk
    for (path, text) in embedded().into_iter().filter(|(p, _)| p != "/") {
        let disk = std::fs::read_to_string(root.join(&path["/app/".len()..])).unwrap();
        assert_eq!(disk, text, "{path}");
    }
}

#[test]
fn the_script_is_split_into_modules() {
    // The page was one strict function. Modules are strict by themselves, and a leftover
    // wrapper or a second copy of the old script would show up here.
    for (path, text) in scripts() {
        assert!(!text.contains("'use strict'"), "{path}");
        assert!(!text.contains("})();\n</script>"), "{path}");
    }
    assert!(scripts().len() >= 30, "only {} modules", scripts().len());
}

#[tokio::test]
async fn every_embedded_file_is_served_with_the_policy() {
    let harness = Harness::new();
    for (path, text) in embedded() {
        let response = harness.send(Method::GET, &path, None, None, None).await;
        assert_eq!(response.status, StatusCode::OK, "{path}");
        assert_eq!(response.text, text, "{path}");
        assert_eq!(response.headers[header::CONTENT_SECURITY_POLICY], CONTENT_SECURITY_POLICY, "{path}");
        assert_eq!(response.headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff", "{path}");
        let kind = response.headers[header::CONTENT_TYPE].to_str().unwrap();
        let expected = match path.rsplit('.').next() {
            Some("js") => "text/javascript; charset=utf-8",
            Some("css") => "text/css; charset=utf-8",
            _ => "text/html; charset=utf-8",
        };
        assert_eq!(kind, expected, "{path}");
        let etag = response.headers[header::ETAG].to_str().unwrap().to_string();
        assert!(etag.starts_with('"') && etag.ends_with('"') && etag.len() == 34, "{path}: {etag}");
        let request = axum::http::Request::builder().uri(path.as_str()).header(header::IF_NONE_MATCH, etag.as_str());
        let again = harness.dispatch(request.body(axum::body::Body::empty()).unwrap()).await;
        assert_eq!(again.status, StatusCode::NOT_MODIFIED, "{path}");
        assert!(again.text.is_empty(), "{path}");
        assert_eq!(again.headers[header::ETAG], etag.as_str(), "{path}");
        assert_eq!(again.headers[header::CONTENT_SECURITY_POLICY], CONTENT_SECURITY_POLICY, "{path}");
    }
}

#[tokio::test]
async fn an_unknown_file_is_not_found() {
    let harness = Harness::new();
    for path in ["/app/nope.js", "/app/", "/app/../index.html", "/index.html", "/app/core", "/app/core/"] {
        let response = harness.send(Method::GET, path, None, None, None).await;
        assert_eq!(response.status, StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn hardening_headers_are_on_every_response() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    // The page's own files are kept and asked for again with their ETag; nothing
    // else is kept at all.
    let responses = [
        ("no-store", harness.get("/api/v1/catalog", &cookie).await),
        ("no-cache", harness.send(Method::GET, "/", None, None, None).await),
        ("no-cache", harness.send(Method::GET, "/app/main.js", None, None, None).await),
        ("no-store", harness.send(Method::GET, "/api/v1/nope", None, None, None).await),
        ("no-store", harness.send(Method::GET, "/api/catalog", None, None, None).await),
        ("no-store", harness.send(Method::GET, "/healthz", None, None, None).await),
    ];
    for (cache, response) in responses {
        assert_eq!(response.headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(response.headers[header::X_FRAME_OPTIONS], "DENY");
        assert_eq!(response.headers[header::REFERRER_POLICY], "no-referrer");
        assert_eq!(response.headers[header::CACHE_CONTROL], cache);
        assert!(response.headers.contains_key(header::STRICT_TRANSPORT_SECURITY));
        assert_eq!(response.headers["x-swifty-api"], "1");
    }
}

#[tokio::test]
async fn oversized_bodies_are_refused() {
    let (harness, _auth, cookie) = Harness::signed_in().await;
    let response = harness
        .post(
            "/api/v1/actions/execute",
            &cookie,
            json!({ "action": "light.power", "target": "bulb1", "params": { "on": true, "x": "y".repeat(70_000) } }),
        )
        .await;
    assert_eq!(response.status, axum::http::StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(harness.devices.total_commands(), 0);
}

/// Crates in the workspace that only tests use. They are not shipped.
const TEST_ONLY_CRATES: &[&str] = &["swifty-plugin-testkit"];

/// The source of everything that ships: `src/` of every crate under `crates/`
/// and `plugins/` at the workspace root. Tests are not part of it.
fn source_files() -> Vec<std::path::PathBuf> {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut out = Vec::new();
    for group in ["crates", "plugins"] {
        let Ok(crates) = std::fs::read_dir(root.join(group)) else { continue };
        for entry in crates {
            let path = entry.unwrap().path();
            let test_only = path.file_name().is_some_and(|name| TEST_ONLY_CRATES.iter().any(|t| name == *t));
            if !test_only && path.join("src").is_dir() {
                walk(&path.join("src"), &mut out);
            }
        }
    }
    for needed in ["swifty-hub", "swifty"] {
        let src = root.join("crates").join(needed).join("src");
        assert!(out.iter().any(|path| path.starts_with(&src)), "nothing found in {}", src.display());
    }
    out
}

#[test]
fn nothing_reads_configuration_or_secrets_from_the_environment() {
    for path in source_files() {
        let text = std::fs::read_to_string(&path).unwrap();
        for needle in ["env::var", "env::{", "std::env", "from_default_env", "option_env!"] {
            assert!(!text.contains(needle), "{} uses {needle}", path.display());
        }
    }
}

#[test]
fn there_is_no_shell_or_process_execution() {
    for path in source_files() {
        let text = std::fs::read_to_string(&path).unwrap();
        for needle in [
            "process::Command",
            "process::{",
            "Command::new",
            "tokio::process",
            "execv",
            "posix_spawn",
            "CreateProcess",
        ] {
            assert!(!text.contains(needle), "{} uses {needle}", path.display());
        }
    }
}

/// The two plugins whose vendors call the account's sign-in secret by that word, in
/// the field of their own sign-in and so in the label of the setup's field.
const LABELLED_BY_THE_VENDOR: &[&str] = &["plugins/toshiba/src/", "plugins/petkit/src/"];

/// A field label: a constant whose whole value is the word, and nothing else on its
/// line, such as `pub const LABEL: &str = "Password";`.
fn is_field_label(line: &str) -> bool {
    let line = line.trim();
    let Some(rest) = line.strip_prefix("pub const ").or_else(|| line.strip_prefix("pub(crate) const ")) else {
        return false;
    };
    rest.split_once(": &str = ").is_some_and(|(name, value)| {
        name.bytes().all(|b| b.is_ascii_uppercase() || b == b'_') && value.eq_ignore_ascii_case("\"password\";")
    })
}

#[test]
fn there_are_no_passwords() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut labels = 0;
    for path in source_files() {
        let name = path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
        let labelled = LABELLED_BY_THE_VENDOR.iter().any(|plugin| name.starts_with(plugin));
        let text = std::fs::read_to_string(&path).unwrap();
        let hits: Vec<&str> = text
            .lines()
            .filter(|line| {
                let line = line.to_lowercase();
                line.contains("password") && !line.contains("there are no passwords")
            })
            .filter(|line| !(labelled && is_field_label(line)))
            .collect();
        labels += text.lines().filter(|line| labelled && is_field_label(line)).count();
        assert!(hits.is_empty(), "{name}: {hits:?}");
    }
    // The allowance is for those labels and nothing more: each plugin names the
    // vendor's field and labels the setup's.
    assert_eq!(labels, 4, "the field labels of the two plugins");
}

#[test]
fn a_field_label_is_the_word_alone() {
    assert!(is_field_label(r#"pub const SIGN_IN_SECRET_KEY: &str = "Password";"#));
    assert!(is_field_label(r#"    pub(crate) const SECRET_LABEL: &str = "password";"#));
    for line in [
        r#"pub const SECRET_LABEL: &str = "Password please";"#,
        r#"pub const SECRET_LABEL: &str = "password"; // and more"#,
        r#"let password = "x";"#,
        r#"pub fn password() -> &'static str { "password" }"#,
        r#"pub const LABEL: &str = concat!("pass", "word", "x");"#,
        r#"pub const label: &str = "password";"#,
    ] {
        assert!(!is_field_label(line), "{line}");
    }
}

/// The pixels of `key:-Npx` (or `key:max(Npx`) in the first rule that starts with `selector{`.
fn css_px(css: &str, selector: &str, key: &str) -> i64 {
    let rule = css.split(&format!("{selector}{{")).nth(1).unwrap_or_else(|| panic!("no rule `{selector}`"));
    let rule = &rule[..rule.find('}').unwrap()];
    let value = rule.split(&format!("{key}:")).nth(1).unwrap_or_else(|| panic!("`{selector}` has no `{key}`"));
    let digits: String =
        value.trim_start_matches("max(").trim_start_matches('-').chars().take_while(char::is_ascii_digit).collect();
    digits.parse().unwrap()
}

#[test]
fn the_headers_fade_stays_inside_the_page_so_a_phone_never_scrolls_sideways() {
    // The fade behind the sticky header reaches past the header on both sides. If it reaches
    // further than the page's side padding, the page grows wider than the screen.
    let css = shipped();
    let gutter = css_px(&css, ".wrap", "padding-left").min(css_px(&css, ".wrap", "padding-right"));
    for side in ["left", "right"] {
        assert!(css_px(&css, ".top::before", side) <= gutter, "the header's fade reaches past the page on the {side}");
    }
}
