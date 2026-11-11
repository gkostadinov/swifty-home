//! The guided setup, scripted against the mock Proxmox.

mod common;

use std::collections::BTreeMap;

use common::*;
use serde_json::{json, Value};
use swifty_plugin_api::{Capability, Plugin, Scalar, SetupInput, SetupResult, SetupStep};
use swifty_plugin_proxmox::PLUGIN;
use swifty_plugin_testkit::devices::{MockProxmox, PROXMOX_TOKEN};
use swifty_plugin_testkit::trap::Trap;

fn values(pairs: &[(&str, Scalar)]) -> SetupInput {
    let values: BTreeMap<String, Scalar> = pairs.iter().map(|(key, value)| (key.to_string(), value.clone())).collect();
    SetupInput::Values { values }
}

fn text(value: &str) -> Scalar {
    Scalar::Text(value.to_string())
}

fn chosen(ids: &[&str]) -> SetupInput {
    SetupInput::Chosen { ids: ids.iter().map(|id| id.to_string()).collect() }
}

/// What the form is filled in with for `url`.
fn form(url: &str, token: &str) -> SetupInput {
    values(&[("url", text(url)), ("node", text(NODE)), ("token", text(token)), ("public_ca", Scalar::Bool(false))])
}

fn option_ids(step: &SetupStep) -> Vec<String> {
    match step {
        SetupStep::Choose { options, .. } => options.iter().map(|option| option.id.clone()).collect(),
        other => panic!("not a choice: {other:?}"),
    }
}

fn done(step: SetupStep) -> SetupResult {
    match step {
        SetupStep::Done(result) => result,
        other => panic!("not done: {other:?}"),
    }
}

#[tokio::test]
async fn the_setup_reaches_done_against_the_mock() {
    let mock = MockProxmox::start(NODE).await;
    let ctx = setup_context(true, &[]);
    let mut flow = PLUGIN.setup().unwrap();

    let step = flow.begin(&ctx, None).await;
    assert!(matches!(&step, SetupStep::External { fields, .. } if fields.is_empty()), "{step:?}");
    let step = flow.advance(&ctx, values(&[])).await;
    let SetupStep::Form { fields, .. } = &step else { panic!("{step:?}") };
    let keys: Vec<&str> = fields.iter().map(|field| field.key.as_str()).collect();
    assert_eq!(keys, ["url", "node", "token", "public_ca", "ca"]);
    assert!(fields.iter().all(|field| field.key != "token" || field.value.is_none()));

    let step = flow.advance(&ctx, form(&mock.http.url, PROXMOX_TOKEN)).await;
    assert_eq!(option_ids(&step), ["vm100", "vm101", "vm102", "vm103"]);
    let step = flow.advance(&ctx, chosen(&["vm101", "vm102"])).await;
    assert_eq!(option_ids(&step), ["vm101", "vm102"]);
    let result = done(flow.advance(&ctx, chosen(&["vm102"])).await);

    PLUGIN.validate(&result.config).unwrap();
    assert_eq!(result.config["url"], json!(mock.http.url));
    assert_eq!(result.config["node"], json!(NODE));
    assert_eq!(result.config["console_targets"], json!([{ "vm": "vm102" }]));
    let slots: Vec<(&str, &str)> = result.secrets.iter().map(|(slot, value)| (slot.as_str(), value.expose())).collect();
    assert_eq!(slots, [("token", PROXMOX_TOKEN)]);
    let adopted: Vec<(&str, bool)> = result
        .adopt
        .iter()
        .map(|decl| (decl.local_id.as_str(), decl.capabilities.contains(&Capability::Console)))
        .collect();
    assert_eq!(adopted, [("vm101", false), ("vm102", true)]);
    assert!(result.summary.contains("2 VMs approved, 1 with a console"), "{}", result.summary);

    // The token went only into the secrets.
    for shown in [result.config.to_string(), result.summary.clone(), format!("{result:?}")] {
        assert!(!shown.contains(PROXMOX_TOKEN), "{shown}");
    }
    // One read of each chosen VM.
    let reads: Vec<String> =
        mock.http.requests().into_iter().filter(|r| r.path.ends_with("/status/current")).map(|r| r.path).collect();
    assert_eq!(reads, ["/api2/json/nodes/pve/qemu/101/status/current", "/api2/json/nodes/pve/qemu/102/status/current"]);
    assert!(mock.http.posts().is_empty(), "setting up changes nothing on the host");
}

#[tokio::test]
async fn the_setup_in_dry_run_says_live_mode_is_needed_and_reaches_nothing() {
    let trap = Trap::start().await;
    let ctx = setup_context(false, &[]);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    flow.advance(&ctx, values(&[])).await;
    let step = flow.advance(&ctx, form(&format!("http://{}", trap.addr), PROXMOX_TOKEN)).await;
    let SetupStep::Failed { reason, retry } = step else { panic!("{step:?}") };
    assert!(reason.contains("live"), "{reason}");
    assert!(!retry);
    assert_eq!(trap.traffic(), 0);
}

#[tokio::test]
async fn a_wrong_token_fails_with_a_reason_and_can_be_tried_again() {
    let mock = MockProxmox::start(NODE).await;
    let ctx = setup_context(true, &[]);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    flow.advance(&ctx, values(&[])).await;
    let step = flow.advance(&ctx, form(&mock.http.url, "swifty@pve!home=not-the-right-one")).await;
    let SetupStep::Failed { reason, retry } = step else { panic!("{step:?}") };
    assert!(retry);
    assert!(reason.contains("token"), "{reason}");
    assert!(!reason.contains("not-the-right-one"), "{reason}");

    let step = flow.advance(&ctx, SetupInput::Back).await;
    let SetupStep::Form { fields, .. } = &step else { panic!("{step:?}") };
    let url = fields.iter().find(|field| field.key == "url").unwrap();
    assert_eq!(url.value, Some(text(&mock.http.url)), "what was entered is offered again");
    assert!(fields.iter().find(|field| field.key == "token").unwrap().value.is_none());
    let step = flow.advance(&ctx, form(&mock.http.url, PROXMOX_TOKEN)).await;
    assert_eq!(option_ids(&step).len(), 4);
}

#[tokio::test]
async fn the_form_says_what_is_wrong_and_contacts_nothing() {
    let trap = Trap::start().await;
    let ctx = setup_context(true, &[]);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    flow.advance(&ctx, values(&[])).await;
    let cases = [
        (form("http://192.0.2.10:8006", PROXMOX_TOKEN), "url"),
        (form(&format!("http://{}", trap.addr), "just-a-value"), "user@realm!tokenid=uuid"),
        (form(&format!("http://{}", trap.addr), ""), "token is needed"),
        (form(&format!("https://{}", trap.addr), PROXMOX_TOKEN), "CA certificate"),
    ];
    for (input, needle) in cases {
        let step = flow.advance(&ctx, input).await;
        let SetupStep::Form { text, fields, .. } = &step else { panic!("{step:?}") };
        assert!(text.contains(needle), "{needle}: {text}");
        assert!(fields.iter().all(|field| field.key != "token" || field.value.is_none()));
    }
    let mut bad_ca = BTreeMap::new();
    bad_ca.insert("url".to_string(), text(&format!("https://{}", trap.addr)));
    bad_ca.insert("node".to_string(), text(NODE));
    bad_ca.insert("token".to_string(), text(PROXMOX_TOKEN));
    bad_ca.insert("ca".to_string(), text("not a certificate"));
    let step = flow.advance(&ctx, SetupInput::Values { values: bad_ca }).await;
    assert!(matches!(&step, SetupStep::Form { text, .. } if text.contains("PEM")), "{step:?}");
    assert_eq!(trap.traffic(), 0);
}

#[tokio::test]
async fn a_vm_that_cannot_be_read_fails_the_setup() {
    let mock = MockProxmox::start(NODE).await;
    // Listed, but its status cannot be read.
    mock.node.lock().unwrap().listed = vec![(150, "broken".to_string(), false)];
    let ctx = setup_context(true, &[]);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    flow.advance(&ctx, values(&[])).await;
    flow.advance(&ctx, form(&mock.http.url, PROXMOX_TOKEN)).await;
    let step = flow.advance(&ctx, chosen(&["vm150"])).await;
    let SetupStep::Failed { reason, retry } = step else { panic!("{step:?}") };
    assert!(retry && reason.contains("vm150"), "{reason}");
    let step = flow.advance(&ctx, chosen(&["vm999"])).await;
    assert!(matches!(step, SetupStep::Failed { retry: true, .. }));
    let step = flow.advance(&ctx, SetupInput::Back).await;
    assert_eq!(option_ids(&step).len(), 5);
    let step = flow.advance(&ctx, chosen(&["vm999"])).await;
    assert!(matches!(&step, SetupStep::Failed { reason, .. } if reason.contains("vm999")), "{step:?}");
}

#[tokio::test]
async fn setting_up_again_with_the_stored_token_kept_changes_only_the_config() {
    let trap = Trap::start().await;
    let files = tempfile::tempdir().unwrap();
    let ctx = setup_context(true, &["token"]);
    let existing: Value = json!({
        "url": format!("http://{}", trap.addr),
        "node": "old",
        "console_targets": [{ "vm": "vm102" }],
        "host_files_dir": files.path(),
    });
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, Some(&existing)).await;
    let step = flow.advance(&ctx, values(&[])).await;
    let SetupStep::Form { fields, .. } = &step else { panic!("{step:?}") };
    let node = fields.iter().find(|field| field.key == "node").unwrap();
    assert_eq!(node.value, Some(text("old")));
    let token = fields.iter().find(|field| field.key == "token").unwrap();
    assert!(!token.required && token.help.contains("keep"));

    let result = done(flow.advance(&ctx, form(&format!("http://{}", trap.addr), "")).await);
    PLUGIN.validate(&result.config).unwrap();
    assert_eq!(result.config["node"], json!(NODE));
    assert_eq!(result.config["console_targets"], existing["console_targets"]);
    assert_eq!(result.config["host_files_dir"], existing["host_files_dir"]);
    assert!(result.secrets.is_empty());
    assert!(result.adopt.is_empty());
    assert_eq!(trap.traffic(), 0, "a kept token cannot be used here, so nothing is asked");
}
