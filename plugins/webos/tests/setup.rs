//! The pairing setup, scripted against mock TVs.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use common::{PairingTv, Prompt};
use serde_json::{json, Value};
use swifty_plugin_api::{Plugin, Scalar, SetupContext, SetupFlow, SetupInput, SetupStep};
use swifty_plugin_testkit::devices::{MockTv, TV_KEY};
use swifty_plugin_testkit::trap::Trap;
use swifty_plugin_webos::{fingerprint, WebosSetup, CLIENT_KEY, PERMISSIONS, PLUGIN, TRUST};

fn context(live: bool) -> SetupContext {
    SetupContext {
        instance_id: "webos".to_string(),
        grant: live.then(common::grant),
        timeouts: common::timeouts(),
        secrets_present: BTreeSet::new(),
    }
}

fn values(pairs: &[(&str, &str)]) -> SetupInput {
    let values: BTreeMap<String, Scalar> =
        pairs.iter().map(|(key, value)| (key.to_string(), Scalar::Text(value.to_string()))).collect();
    SetupInput::Values { values }
}

/// Polls as the hub does until the flow moves on from waiting, or gives up.
async fn poll_until_moved(flow: &mut dyn SetupFlow, ctx: &SetupContext) -> SetupStep {
    for _ in 0..40 {
        let step = flow.advance(ctx, SetupInput::Poll).await;
        if !matches!(step, SetupStep::Wait { .. }) {
            return step;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the flow never moved on from waiting");
}

#[tokio::test]
async fn the_scripted_setup_pairs_the_tv_and_reaches_done() {
    let tv = PairingTv::start(Prompt::Accepted).await;
    let ctx = context(true);
    let mut flow = PLUGIN.setup().unwrap();

    let SetupStep::Form { fields, .. } = flow.begin(&ctx, None).await else { panic!("no form") };
    let keys: Vec<&str> = fields.iter().map(|field| field.key.as_str()).collect();
    assert_eq!(keys, ["url", "wol_mac", "wol_broadcast"], "the certificate pin is learned, never typed");

    let input = values(&[("url", &tv.ws.url), ("wol_mac", "aa:bb:cc:dd:ee:ff")]);
    let step = flow.advance(&ctx, input).await;
    let SetupStep::Wait { poll_ms, timeout_s, .. } = step else { panic!("{step:?}") };
    assert!(poll_ms > 0 && timeout_s >= 60);

    let SetupStep::Done(result) = poll_until_moved(flow.as_mut(), &ctx).await else { panic!("not done") };
    assert_eq!(result.config, json!({ "url": tv.ws.url, "wol_mac": "aa:bb:cc:dd:ee:ff" }));
    PLUGIN.validate(&result.config).unwrap();
    assert_eq!(result.secrets.len(), 1);
    assert_eq!(result.secrets[0].0, CLIENT_KEY);
    assert_eq!(result.secrets[0].1.expose(), TV_KEY);
    assert_eq!(result.summary, "Paired. The TV is on HDMI 2.");
    for shown in [result.config.to_string(), result.summary.clone(), format!("{result:?}")] {
        assert!(!shown.contains(TV_KEY), "the key shows in {shown}");
    }

    // One registration without a key, then the read that presents the new one.
    let registrations = tv.registrations();
    assert_eq!(registrations.len(), 2);
    assert!(registrations[0]["payload"].get("client-key").is_none());
    assert_eq!(registrations[0]["payload"]["forcePairing"], json!(true));
    assert_eq!(registrations[0]["payload"]["pairingType"], json!("PROMPT"));
    let asked: Vec<String> =
        serde_json::from_value(registrations[0]["payload"]["manifest"]["permissions"].clone()).unwrap();
    assert_eq!(asked, PERMISSIONS);
    assert_eq!(registrations[1]["payload"]["client-key"], json!(TV_KEY));
    assert_eq!(registrations[1]["payload"]["forcePairing"], json!(false));
    assert_eq!(tv.prompts.load(std::sync::atomic::Ordering::SeqCst), 1, "one prompt, for the pairing only");
    let uris: Vec<Value> = tv.received().iter().filter_map(|m| m.get("uri").cloned()).collect();
    assert!(uris.iter().all(|uri| uri.as_str().unwrap().contains("get")), "only reads: {uris:?}");
}

#[tokio::test]
async fn a_prompt_nobody_accepts_ends_the_wait_with_a_reason() {
    let tv = PairingTv::start(Prompt::Ignored).await;
    let ctx = context(true);
    let mut flow = WebosSetup::waiting_at_most(Duration::from_millis(300));
    flow.begin(&ctx, None).await;
    assert!(matches!(flow.advance(&ctx, values(&[("url", &tv.ws.url)])).await, SetupStep::Wait { .. }));
    let step = poll_until_moved(&mut flow, &ctx).await;
    let SetupStep::Failed { reason, retry: true } = step else { panic!("{step:?}") };
    assert!(reason.contains("nobody accepted"), "{reason}");
    assert_eq!(tv.registrations().len(), 1, "asked once, and never again on its own");
}

#[tokio::test]
async fn a_declined_prompt_fails_and_says_so() {
    let tv = PairingTv::start(Prompt::Declined).await;
    let ctx = context(true);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    flow.advance(&ctx, values(&[("url", &tv.ws.url)])).await;
    let SetupStep::Failed { reason, .. } = poll_until_moved(flow.as_mut(), &ctx).await else { panic!("not failed") };
    assert!(reason.contains("refused") && reason.contains("denied by the user"), "{reason}");
}

#[tokio::test]
async fn a_key_that_cannot_be_stored_is_not_stored() {
    let tv = PairingTv::start(Prompt::BadKey).await;
    let ctx = context(true);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    flow.advance(&ctx, values(&[("url", &tv.ws.url)])).await;
    let step = poll_until_moved(flow.as_mut(), &ctx).await;
    assert!(matches!(&step, SetupStep::Failed { reason, .. } if reason.contains("cannot be used")), "{step:?}");
}

#[tokio::test]
async fn pairing_needs_live_mode_and_contacts_nothing_in_dry_run() {
    let tv = Trap::start().await;
    let ctx = context(false);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    let step = flow.advance(&ctx, values(&[("url", &format!("ws://{}", tv.addr))])).await;
    let SetupStep::Failed { reason, retry: false } = step else { panic!("{step:?}") };
    assert!(reason.contains("live"), "{reason}");
    assert_eq!(tv.traffic(), 0);
}

fn trust() -> SetupInput {
    SetupInput::Chosen { ids: vec![TRUST.to_string()] }
}

#[tokio::test]
async fn pairing_on_wss_trusts_the_certificate_the_person_confirms() {
    // The form's address is all it takes: the TV's certificate is learned and shown.
    let tv = MockTv::start_tls().await;
    let pin = tv.pin.clone().unwrap();
    let ctx = context(true);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    let step = flow.advance(&ctx, values(&[("url", &tv.url)])).await;
    let SetupStep::Choose { text, options, min: 1, multiple: false, .. } = step else { panic!("{step:?}") };
    assert_eq!(options.len(), 1);
    assert_eq!((options[0].id.as_str(), options[0].detail.clone()), (TRUST, fingerprint(&pin)));
    assert!(text.contains(&fingerprint(&pin)), "{text}");
    assert_eq!(tv.traffic(), 1, "one connection, to learn the certificate");
    assert!(tv.received.lock().unwrap().is_empty(), "nothing but the handshake was sent");
    assert_eq!(tv.prompts.load(std::sync::atomic::Ordering::SeqCst), 0);

    // Trusted, it is pinned and the TV raises its prompt.
    assert!(matches!(flow.advance(&ctx, trust()).await, SetupStep::Wait { .. }));
    assert!(matches!(flow.advance(&ctx, SetupInput::Poll).await, SetupStep::Wait { .. }));
    assert_eq!(tv.prompts.load(std::sync::atomic::Ordering::SeqCst), 1);
    let register = tv.received.lock().unwrap()[0].clone();
    assert!(register["payload"].get("client-key").is_none());

    // A certificate nobody trusted is never sent a registration.
    let other = MockTv::start_tls().await;
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    assert!(matches!(flow.advance(&ctx, values(&[("url", &other.url)])).await, SetupStep::Choose { .. }));
    let step = flow.advance(&ctx, SetupInput::Chosen { ids: Vec::new() }).await;
    assert!(matches!(&step, SetupStep::Failed { reason, .. } if reason.contains("not trusted")), "{step:?}");
    assert!(other.received.lock().unwrap().is_empty());
    assert_eq!(other.prompts.load(std::sync::atomic::Ordering::SeqCst), 0);
    // The question stays, and can still be answered.
    assert!(matches!(flow.advance(&ctx, trust()).await, SetupStep::Wait { .. }));
    assert!(matches!(flow.advance(&ctx, SetupInput::Poll).await, SetupStep::Wait { .. }));
    assert_eq!(other.prompts.load(std::sync::atomic::Ordering::SeqCst), 1, "now the registration");

    // In dry-run nothing is learned: the TV is not contacted at all.
    let before = other.traffic();
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&context(false), None).await;
    let step = flow.advance(&context(false), values(&[("url", &other.url)])).await;
    assert!(matches!(&step, SetupStep::Failed { reason, retry: false } if reason.contains("live")), "{step:?}");
    assert_eq!(other.traffic(), before, "dry-run contacted the TV");
}

#[tokio::test]
async fn setting_a_tv_up_again_asks_again_only_when_its_certificate_changed() {
    let tv = MockTv::start_tls().await;
    let pin = tv.pin.clone().unwrap();
    let ctx = context(true);

    // The certificate stored for it: nothing to confirm.
    let stored = json!({ "url": tv.url, "cert_sha256": pin.to_ascii_uppercase() });
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, Some(&stored)).await;
    assert!(matches!(flow.advance(&ctx, values(&[("url", &tv.url)])).await, SetupStep::Wait { .. }));

    // Another certificate at that address is shown, with a warning, and sent nothing.
    let other = MockTv::start_tls().await;
    let stored = json!({ "url": other.url, "cert_sha256": pin });
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, Some(&stored)).await;
    let step = flow.advance(&ctx, values(&[("url", &other.url)])).await;
    let SetupStep::Choose { text, options, .. } = step else { panic!("{step:?}") };
    assert!(text.contains("not the certificate stored"), "{text}");
    assert_eq!(options[0].detail, fingerprint(other.pin.as_deref().unwrap()));
    assert!(other.received.lock().unwrap().is_empty());
}

#[tokio::test]
async fn going_back_hangs_up_and_keeps_what_was_typed() {
    let tv = PairingTv::start(Prompt::Ignored).await;
    let ctx = context(true);
    let mut flow = PLUGIN.setup().unwrap();
    flow.begin(&ctx, None).await;
    flow.advance(&ctx, values(&[("url", &tv.ws.url)])).await;
    let SetupStep::Form { fields, .. } = flow.advance(&ctx, SetupInput::Back).await else { panic!("no form") };
    assert_eq!(fields[0].value, Some(Scalar::Text(tv.ws.url.clone())));
    let step = flow.advance(&ctx, SetupInput::Poll).await;
    assert!(matches!(step, SetupStep::Failed { .. }), "nothing is waiting any more: {step:?}");
}

#[tokio::test]
async fn setting_up_again_prefills_the_form_but_never_a_secret() {
    let existing =
        json!({ "url": "wss://192.0.2.40:3001", "cert_sha256": "a".repeat(64), "wol_mac": "02:00:00:00:00:01" });
    let ctx = SetupContext { secrets_present: BTreeSet::from([CLIENT_KEY.to_string()]), ..context(false) };
    let mut flow = PLUGIN.setup().unwrap();
    let SetupStep::Form { fields, .. } = flow.begin(&ctx, Some(&existing)).await else { panic!("no form") };
    let shown: BTreeMap<&str, Option<Scalar>> = fields.iter().map(|f| (f.key.as_str(), f.value.clone())).collect();
    assert_eq!(shown["url"], Some(Scalar::Text("wss://192.0.2.40:3001".to_string())));
    assert_eq!(shown["wol_mac"], Some(Scalar::Text("02:00:00:00:00:01".to_string())));
    assert_eq!(shown["wol_broadcast"], None);
    assert!(fields.iter().all(|field| field.key != CLIENT_KEY));
}
