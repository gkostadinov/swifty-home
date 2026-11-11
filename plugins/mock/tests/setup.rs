//! The setup flow, driven by scripted inputs: Choose, the access code, Done.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::{connect, timeouts, with_view};
use serde_json::{json, Value};
use swifty_plugin_api::net::LiveGrant;
use swifty_plugin_api::{Op, Plugin, Readiness, Scalar, SetupContext, SetupInput, SetupResult, SetupStep};
use swifty_plugin_mock::{DEMO_DEVICES, PLUGIN};

const CANARY: &str = "canary-7f3a-not-for-config";

fn ctx(live: bool, slots: &[&str]) -> SetupContext {
    SetupContext {
        instance_id: "mock".to_string(),
        grant: LiveGrant::when_live(live),
        timeouts: timeouts(),
        secrets_present: slots.iter().map(|slot| slot.to_string()).collect(),
    }
}

fn chosen(ids: &[&str]) -> SetupInput {
    SetupInput::Chosen { ids: ids.iter().map(|id| id.to_string()).collect() }
}

fn code(value: &str) -> SetupInput {
    SetupInput::Values { values: BTreeMap::from([("access_code".to_string(), Scalar::Text(value.to_string()))]) }
}

fn done(step: SetupStep) -> SetupResult {
    match step {
        SetupStep::Done(result) => result,
        other => panic!("expected Done, got {other:?}"),
    }
}

fn ids(config: &Value) -> Vec<String> {
    config["devices"].as_array().unwrap().iter().map(|row| row["id"].as_str().unwrap().to_string()).collect()
}

#[tokio::test]
async fn the_scripted_setup_reaches_done_in_dry_run_and_live() {
    for live in [false, true] {
        let ctx = ctx(live, &[]);
        let mut flow = PLUGIN.setup().expect("the mock has a setup flow");
        let SetupStep::Choose { options, multiple, min, .. } = flow.begin(&ctx, None).await else {
            panic!("the first step is Choose");
        };
        assert!(multiple && min == 1);
        assert_eq!(options.len(), DEMO_DEVICES.len());
        assert!(options.iter().all(|option| option.selected), "a new instance starts with every demo device");
        let SetupStep::Form { fields, .. } = flow.advance(&ctx, chosen(&["lamp", "tv", "gadget"])).await else {
            panic!("then the access code");
        };
        assert_eq!(fields.len(), 1);
        assert!(fields[0].value.is_none() && !fields[0].required);

        let result = done(flow.advance(&ctx, code(CANARY)).await);
        PLUGIN.validate(&result.config).unwrap();
        assert_eq!(ids(&result.config), ["lamp", "tv", "gadget"]);
        assert_eq!(result.summary, "3 demo devices added.");
        assert!(result.adopt.is_empty());
        assert_eq!(result.secrets.len(), 1);
        assert_eq!((result.secrets[0].0.as_str(), result.secrets[0].1.expose()), ("access_code", CANARY));
        // The secret is in `secrets` and nowhere else.
        let shown = format!("{} {result:?} {}", result.config, serde_json::to_string(&result).unwrap());
        assert!(!shown.contains(CANARY), "{shown}");
        // And the finished flow does not start over.
        assert!(matches!(flow.advance(&ctx, chosen(&["lamp"])).await, SetupStep::Failed { retry: false, .. }));
    }
}

#[tokio::test]
async fn the_result_runs_as_an_instance() {
    let ctx = ctx(true, &[]);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    flow.advance(&ctx, chosen(&["plug"])).await;
    let result = done(flow.advance(&ctx, SetupInput::Values { values: BTreeMap::new() }).await);
    assert!(result.secrets.is_empty(), "an empty form stores no code");
    assert_eq!(result.summary, "1 demo device added.");
    let readiness = with_view("mock", &result.config, &[], &[], |view| PLUGIN.readiness(view));
    assert_eq!(readiness, Readiness::Ready);
    let driver = connect(&PLUGIN, "mock", &result.config);
    driver.command("plug", &Op::PlugPower { on: true }).await.unwrap();
    assert!(Op::PlugPower { on: true }.is_confirmed_by(&driver.read_state("plug").await.unwrap(), None));
}

#[tokio::test]
async fn wrong_answers_offer_the_step_again_and_back_goes_back() {
    let ctx = ctx(false, &[]);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    for input in [chosen(&[]), chosen(&["lamp", "toaster"]), code("x"), SetupInput::Values { values: BTreeMap::new() }]
    {
        assert!(matches!(flow.advance(&ctx, input).await, SetupStep::Failed { retry: true, .. }));
    }
    assert!(matches!(flow.advance(&ctx, SetupInput::Poll).await, SetupStep::Choose { .. }));
    assert!(matches!(flow.advance(&ctx, chosen(&["lamp"])).await, SetupStep::Form { .. }));
    let refused = [
        SetupInput::Values { values: BTreeMap::from([("access_code".to_string(), Scalar::Int(4))]) },
        code(&"x".repeat(129)),
        code("line\nbreak"),
        SetupInput::Values { values: BTreeMap::from([("colour".to_string(), Scalar::Text("red".into()))]) },
        chosen(&["lamp"]),
    ];
    for input in refused {
        assert!(matches!(flow.advance(&ctx, input).await, SetupStep::Failed { retry: true, .. }));
    }
    assert!(matches!(flow.advance(&ctx, SetupInput::Back).await, SetupStep::Choose { .. }));
    assert!(matches!(flow.advance(&ctx, chosen(&["ceiling"])).await, SetupStep::Form { .. }));
    let result = done(flow.advance(&ctx, code("")).await);
    assert_eq!(ids(&result.config), ["ceiling"]);
    assert!(result.secrets.is_empty());
}

#[tokio::test]
async fn setting_up_again_keeps_what_it_does_not_ask_about() {
    let existing = json!({
        "devices": [
            { "id": "lamp", "class": "light", "name": "Floor lamp" },
            { "id": "fridge", "class": "plug", "name": "Fridge" },
        ],
        "require_code": true,
    });
    let ctx = ctx(false, &["access_code"]);
    let mut flow = PLUGIN.setup().unwrap();
    let SetupStep::Choose { options, .. } = flow.begin(&ctx, Some(&existing)).await else { panic!("Choose") };
    let selected: BTreeSet<&str> = options.iter().filter(|o| o.selected).map(|o| o.id.as_str()).collect();
    assert_eq!(selected, BTreeSet::from(["lamp"]), "what is there now starts picked");
    let SetupStep::Form { text, .. } = flow.advance(&ctx, chosen(&["lamp", "speaker"])).await else { panic!("Form") };
    assert!(text.contains("Leave it empty to keep"), "{text}");
    let result = done(flow.advance(&ctx, code("")).await);
    assert_eq!(ids(&result.config), ["fridge", "lamp", "speaker"]);
    assert_eq!(result.config["require_code"], json!(true));
    let readiness = with_view("mock", &result.config, &["access_code"], &[], |view| PLUGIN.readiness(view));
    assert_eq!(readiness, Readiness::Ready, "the stored code still counts");
}
