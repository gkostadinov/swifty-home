//! Check 11: the setup flow, scripted. Also the setup's part of check 10, the text
//! a flow shows while the device reports a hostile name.

use std::collections::BTreeSet;
use std::time::Duration;

use swifty_plugin_api::net::LiveGrant;
use swifty_plugin_api::{FieldType, Plugin, SetupContext, SetupFlow, SetupInput, SetupStep};

use super::text::{hostile, is_clean, preview, strings};
use super::{guarded, settle, Fixture, Observed, Outcome, Report, MAX_DEVICE_TEXT, TIMEOUTS};

/// The longest one step of a scripted flow may take.
const STEP_LIMIT: Duration = Duration::from_secs(10);

/// Runs the flow from `begin` through the inputs, stopping at `Done` or `Failed`.
///
/// The flow is begun as the fixture's instance set up again: `existing` is the
/// fixture's config. A flow whose endpoints come from config (a vendor cloud's
/// URL) then reaches the fixture's device, never the real service. Begun with
/// nothing, such a flow would talk to the vendor. `stored` is the slots the
/// instance already holds a secret in.
async fn drive(
    mut flow: Box<dyn SetupFlow>,
    instance: String,
    grant: Option<LiveGrant>,
    existing: serde_json::Value,
    stored: BTreeSet<String>,
    inputs: Vec<SetupInput>,
) -> Outcome<Vec<SetupStep>> {
    let limit = STEP_LIMIT * (inputs.len() as u32 + 1);
    guarded(limit, async move {
        let ctx = SetupContext { instance_id: instance, grant, timeouts: TIMEOUTS, secrets_present: stored };
        let mut steps = vec![flow.begin(&ctx, Some(&existing)).await];
        for input in inputs {
            if matches!(steps.last(), Some(SetupStep::Done(_) | SetupStep::Failed { .. })) {
                break;
            }
            steps.push(flow.advance(&ctx, input).await);
        }
        steps
    })
    .await
}

/// The kind of a step, for a problem's sentence.
fn kind(step: &SetupStep) -> String {
    match step {
        SetupStep::Form { title, .. } => format!("the form \"{title}\""),
        SetupStep::Wait { title, .. } => format!("the wait \"{title}\""),
        SetupStep::Choose { title, .. } => format!("the choice \"{title}\""),
        SetupStep::External { title, .. } => format!("the step \"{title}\""),
        SetupStep::Done(_) => "Done".to_string(),
        SetupStep::Failed { reason, .. } => format!("Failed (\"{reason}\")"),
    }
}

/// Whether `slot` is one the manifest declares: by name, or `name/{key}` for a row.
fn declared(slots: &[swifty_plugin_api::SecretSlot], slot: &str) -> bool {
    slots.iter().any(|declared| {
        declared.slot == slot
            || declared.slot.split_once('/').is_some_and(|(name, _)| {
                slot.strip_prefix(name).and_then(|rest| rest.strip_prefix('/')).is_some_and(|key| !key.is_empty())
            })
    })
}

/// Runs check 11. Returns the secrets a finished setup handed over, for check 9.
pub(super) async fn check(
    plugin: &'static dyn Plugin,
    fixture: &Fixture,
    report: &mut Report,
    observed: &mut Observed,
) -> Vec<(String, String)> {
    let manifest = plugin.manifest();
    let instance = manifest.id.to_string();
    if plugin.setup().is_none() {
        if fixture.setup.is_empty() {
            report.skip(11, "the plugin has no setup flow");
        } else {
            report.fail(11, "the fixture has setup inputs, but the plugin has no setup flow");
        }
        return Vec::new();
    }

    // Without a grant, as a dry-run hub runs it: nothing may be contacted.
    let device = &fixture.device;
    settle().await;
    let before = device.traffic();
    let Some(flow) = plugin.setup() else { return Vec::new() };
    let nothing_stored = BTreeSet::new();
    let dry = drive(flow, instance, None, fixture.config.clone(), nothing_stored.clone(), fixture.setup.clone()).await;
    match dry {
        Outcome::Done(steps, _) => {
            for step in &steps {
                observed.debug("a setup step", step);
                observed.json("a setup step", step);
            }
        }
        Outcome::Panicked(panic) => report.fail(11, format!("the setup flow panicked with grant: None: {panic}")),
        Outcome::Hung => report.fail(11, "the setup flow did not end with grant: None"),
    }
    settle().await;
    let reached = device.traffic().saturating_sub(before);
    if reached != 0 {
        report.fail(
            11,
            format!(
                "with grant: None the setup flow reached the device {reached} time(s); without a grant it returns Failed \
                 saying live mode is needed"
            ),
        );
    }

    // With a grant, the scripted inputs reach Done: a first setup, nothing stored yet.
    let handed = scripted(plugin, fixture, report, observed, nothing_stored, "").await;

    // And again with every secret the fixture holds already stored, as when the owner sets an
    // integration up again: the flow keeps what is stored, and still ends at Done.
    if !fixture.secrets.is_empty() {
        let label = " when set up again with its secrets stored";
        scripted(plugin, fixture, report, observed, fixture.slots(), label).await;
    }
    handed
}

/// One scripted run with a grant, judged: it ends at `Done`, the result is sound and no secret leaks.
/// `label` says which run it is in a problem's sentence. Returns the secrets the run handed over.
async fn scripted(
    plugin: &'static dyn Plugin,
    fixture: &Fixture,
    report: &mut Report,
    observed: &mut Observed,
    stored: BTreeSet<String>,
    label: &str,
) -> Vec<(String, String)> {
    let manifest = plugin.manifest();
    let Some(flow) = plugin.setup() else { return Vec::new() };
    let instance = manifest.id.to_string();
    let grant = LiveGrant::when_live(true);
    let steps = match drive(flow, instance, grant, fixture.config.clone(), stored, fixture.setup.clone()).await {
        Outcome::Done(steps, _) => steps,
        Outcome::Panicked(panic) => {
            report.fail(11, format!("the setup flow panicked{label}: {panic}"));
            return Vec::new();
        }
        Outcome::Hung => {
            report.fail(11, format!("a step of the setup flow{label} took longer than {STEP_LIMIT:?}"));
            return Vec::new();
        }
    };
    for step in &steps {
        observed.debug("a setup step", step);
        observed.json("a setup step", step);
        let fields = match step {
            SetupStep::Form { fields, .. } | SetupStep::External { fields, .. } => fields.as_slice(),
            _ => &[],
        };
        for field in fields {
            if matches!(field.kind, FieldType::Secret) && field.value.is_some() {
                report.fail(
                    11,
                    format!(
                        "{} prefills the secret field `{}`{label}; a secret is never shown again",
                        kind(step),
                        field.key
                    ),
                );
            }
        }
    }
    let Some(last) = steps.last() else { return Vec::new() };
    let SetupStep::Done(result) = last else {
        report.fail(11, format!("the fixture's setup inputs end{label} at {}, not Done", kind(last)));
        return Vec::new();
    };

    if let Err(problem) = plugin.validate(&result.config) {
        report.fail(11, format!("the finished setup's config does not validate{label}: {problem}"));
    }
    for (slot, _) in &result.secrets {
        if !declared(manifest.secrets, slot) {
            report.fail(
                11,
                format!(
                    "the finished setup{label} stores the secret slot `{slot}`, which the manifest does not declare"
                ),
            );
        }
    }
    let handed: Vec<(String, String)> =
        result.secrets.iter().map(|(slot, secret)| (slot.clone(), secret.expose().to_string())).collect();
    let config = serde_json::to_string(&result.config).unwrap_or_default();
    let adopt = serde_json::to_string(&result.adopt).unwrap_or_default();
    let mut leaks = BTreeSet::new();
    for (slot, value) in handed.iter().chain(fixture.secrets.iter()) {
        if value.chars().count() < 4 {
            continue;
        }
        for (place, text) in [("config", &config), ("devices to adopt", &adopt), ("summary", &result.summary)] {
            if text.contains(value.as_str()) {
                leaks.insert(format!(
                    "the secret for `{slot}` appears in the finished setup's {place}{label}; secrets go only in `secrets`"
                ));
            }
        }
        for step in &steps[..steps.len() - 1] {
            if serde_json::to_string(step).unwrap_or_default().contains(value.as_str()) {
                leaks.insert(format!(
                    "the secret for `{slot}` appears in {}{label}; a step never shows a secret",
                    kind(step)
                ));
            }
        }
    }
    for leak in leaks {
        report.fail(11, leak);
    }
    handed
}

/// Every string of every step the fixture's inputs reach, with a grant, against the device as it is
/// now. Empty when the plugin has no flow or the fixture no inputs. A panic or a hang is check 10's
/// problem. `what` says which run it is.
pub(super) async fn step_text(
    plugin: &'static dyn Plugin,
    fixture: &Fixture,
    report: &mut Report,
    what: &str,
) -> Vec<(String, String)> {
    let Some(flow) = plugin.setup() else { return Vec::new() };
    if fixture.setup.is_empty() {
        return Vec::new();
    }
    let instance = plugin.manifest().id.to_string();
    let grant = LiveGrant::when_live(true);
    let inputs = fixture.setup.clone();
    match drive(flow, instance, grant, fixture.config.clone(), BTreeSet::new(), inputs).await {
        Outcome::Done(steps, _) => {
            let mut seen = Vec::new();
            for (number, step) in steps.iter().enumerate() {
                if let Ok(json) = serde_json::to_value(step) {
                    strings(&json, &format!("step {} of the setup flow ({what})", number + 1), &mut seen);
                }
            }
            seen
        }
        Outcome::Panicked(panic) => {
            report.fail(10, format!("the setup flow panicked on a long device name: {panic}"));
            Vec::new()
        }
        Outcome::Hung => {
            report.fail(10, "the setup flow did not end on a long device name");
            Vec::new()
        }
    }
}

/// The setup's part of check 10, run after check 11 because a flow can change what the device holds
/// (a sign-in replaces another). The flow runs once with the device's normal names and once with the
/// hostile one: a string a step shows now that it did not show before was supplied by the device, and
/// must be clean. What the flow always says (its titles, its help) may be as long as it likes.
/// Nothing is said when the device cannot report text: check 10 has listed that as not run.
pub(super) async fn device_text(plugin: &'static dyn Plugin, fixture: &Fixture, report: &mut Report) {
    let device = &fixture.device;
    if plugin.setup().is_none() || fixture.setup.is_empty() || !device.set_device_text(Some(&hostile())) {
        return;
    }
    device.set_device_text(None);
    let normal = step_text(plugin, fixture, report, "a normal name").await;
    device.set_device_text(Some(&hostile()));
    let long = step_text(plugin, fixture, report, "a long name").await;
    device.set_device_text(None);
    device_text_problems(&normal, &long, report);
}

fn device_text_problems(normal: &[(String, String)], hostile: &[(String, String)], report: &mut Report) {
    let known: BTreeSet<&str> = normal.iter().map(|(_, text)| text.as_str()).collect();
    for (place, text) in hostile {
        if !known.contains(text.as_str()) && !is_clean(text) {
            report.fail(
                10,
                format!(
                    "{place} is {}; pass device text through net::device_text (no control characters, at most {} \
                     characters)",
                    preview(text),
                    MAX_DEVICE_TEXT
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use swifty_plugin_api::SecretSlot;

    #[test]
    fn row_slots_are_declared_by_their_pattern() {
        let slots = [
            SecretSlot { slot: "app_key", label: "k", help: "h", required: true },
            SecretSlot { slot: "local_key/{id}", label: "k", help: "h", required: true },
        ];
        assert!(declared(&slots, "app_key"));
        assert!(declared(&slots, "local_key/plug1"));
        assert!(!declared(&slots, "local_key/"));
        assert!(!declared(&slots, "token"));
    }

    #[test]
    fn only_text_that_the_device_changed_is_held_to_the_yardstick() {
        let long = "w".repeat(MAX_DEVICE_TEXT + 1);
        let normal = vec![("a".to_string(), long.clone()), ("b".to_string(), "Lamp".to_string())];
        let mut report = Report::default();
        // What the flow always says may be long; a name that is the same in both runs is not the device's.
        device_text_problems(
            &normal,
            &[("a".to_string(), long.clone()), ("b".to_string(), "Lamp".to_string())],
            &mut report,
        );
        assert!(report.passed(), "{report}");
        // A name that came from the device, with a control character, or too long, fails check 10.
        let hostile = vec![("c".to_string(), "Bad\u{1b}[2J".to_string()), ("d".to_string(), "x".repeat(400))];
        device_text_problems(&normal, &hostile, &mut report);
        assert_eq!(report.problems.len(), 2, "{report}");
        assert!(report.failed(10));
        // A clean name that the device changed is fine.
        let mut again = Report::default();
        device_text_problems(&normal, &[("e".to_string(), "Hostile name".to_string())], &mut again);
        assert!(again.passed(), "{again}");
    }
}
