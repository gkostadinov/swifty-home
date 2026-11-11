//! Checks 3 and 4: the pure calls, and a dry-run hub driving every operation.

use std::collections::{BTreeSet, HashSet};

use serde_json::Value;
use swifty_plugin_api::{DeviceDecl, DeviceRecord, InstanceView, Op, Plugin, Readiness};

use super::text::is_clean;
use super::watch::Watch;
use super::{caught, corpus, op_text, settle, Fixture, Observed, Report};

/// What the pure calls are given: what a hub with this fixture would hand over.
pub(super) struct View<'a> {
    pub(super) instance: &'a str,
    pub(super) config: &'a Value,
    pub(super) slots: &'a BTreeSet<String>,
    pub(super) adopted: &'a [DeviceRecord],
}

impl View<'_> {
    pub(super) fn view(&self) -> InstanceView<'_> {
        InstanceView { id: self.instance, config: self.config, secrets_present: self.slots, adopted: self.adopted }
    }
}

/// What the dry-run stand-in saw, for the live checks to compare with.
pub(super) struct DryRun {
    /// Whether the instance said it is ready.
    pub(super) ready: bool,
    /// The devices it declared.
    pub(super) decls: Vec<DeviceDecl>,
    /// Each device, operation and what `refusal` said.
    pub(super) runs: Vec<(String, Op, Option<String>)>,
}

/// Check 3.
pub(super) async fn purity(
    plugin: &dyn Plugin,
    fixture: &Fixture,
    view: &View<'_>,
    watch: &Watch,
    report: &mut Report,
    observed: &mut Observed,
) {
    let traffic = || fixture.device.traffic() + watch.traffic();
    let before = traffic();
    let instance = view.view();
    let twice = |name: &str, report: &mut Report, first: Result<String, String>, second: Result<String, String>| match (
        first, second,
    ) {
        (Err(panic), _) | (_, Err(panic)) => report.fail(3, format!("{name} panicked: {panic}")),
        (Ok(a), Ok(b)) if a != b => {
            report.fail(3, format!("{name} gave two answers to the same question: {a} and then {b}"))
        }
        _ => {}
    };
    match caught(|| plugin.validate(view.config)) {
        Err(panic) => report.fail(3, format!("validate panicked: {panic}")),
        Ok(Err(problem)) => observed.add("validate's refusal of the fixture", problem.to_string()),
        Ok(Ok(())) => {}
    }
    let readiness = caught(|| plugin.readiness(&instance));
    if let Ok(readiness) = &readiness {
        observed.debug("readiness", readiness);
    }
    twice(
        "readiness",
        report,
        readiness.map(|r| format!("{r:?}")),
        caught(|| format!("{:?}", plugin.readiness(&instance))),
    );
    let devices = caught(|| plugin.devices(&instance));
    if let Ok(decls) = &devices {
        observed.debug("devices", decls);
    }
    twice("devices", report, devices.map(|d| format!("{d:?}")), caught(|| format!("{:?}", plugin.devices(&instance))));
    for (device, op) in &fixture.ops {
        let first = caught(|| plugin.refusal(&instance, device, op));
        if let Ok(Some(text)) = &first {
            observed.add(format!("refusal of {} on {device}", op_text(op)), text.clone());
        }
        let second = caught(|| plugin.refusal(&instance, device, op));
        twice(
            &format!("refusal of {} on `{device}`", op_text(op)),
            report,
            first.map(|r| format!("{r:?}")),
            second.map(|r| format!("{r:?}")),
        );
    }
    settle().await;
    let reached = traffic().saturating_sub(before);
    if reached != 0 {
        report.fail(
            3,
            format!(
                "validate, readiness, devices or refusal reached the network {reached} time(s) (the fixture's device{}); \
                 they are pure and must not open a socket",
                if watch.addrs.is_empty() { String::new() } else { format!(" or a trap on {:?}", watch.addrs) }
            ),
        );
    }
}

/// `[a-z0-9][a-z0-9_-]{0,63}`: a local id or a target.
pub(super) fn is_local_id(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && text.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Check 4. Returns what it saw, for check 5.
pub(super) async fn dry_run(
    plugin: &dyn Plugin,
    fixture: &Fixture,
    view: &View<'_>,
    watch: &Watch,
    report: &mut Report,
    observed: &mut Observed,
) -> DryRun {
    let traffic = || fixture.device.traffic() + watch.traffic();
    let before = traffic();
    let instance = view.view();
    let manifest = plugin.manifest();
    let mut dry = DryRun { ready: false, decls: Vec::new(), runs: Vec::new() };

    let readiness = match caught(|| plugin.readiness(&instance)) {
        Ok(readiness) => readiness,
        Err(panic) => {
            report.fail(4, format!("readiness panicked: {panic}"));
            return dry;
        }
    };
    match &readiness {
        Readiness::Ready => dry.ready = true,
        Readiness::NeedsSetup(reason) | Readiness::Disabled(reason) => {
            report.fail(4, format!("the fixture's instance is not ready ({readiness:?}); give it what it needs"));
            if reason.trim().is_empty() {
                report.fail(4, "readiness gives no reason");
            }
        }
    }
    dry.decls = match caught(|| plugin.devices(&instance)) {
        Ok(decls) => decls,
        Err(panic) => {
            report.fail(4, format!("devices panicked: {panic}"));
            Vec::new()
        }
    };
    decl_problems(manifest.classes, &dry.decls, report);
    if dry.decls.is_empty() {
        report.skip(4, "the fixture's instance declares no device, so no operation was driven");
    }

    if dry.ready {
        for decl in &dry.decls {
            for op in corpus::ops_for(manifest, decl) {
                let answer = match caught(|| plugin.refusal(&instance, &decl.local_id, &op)) {
                    Ok(answer) => answer,
                    Err(panic) => {
                        report.fail(4, format!("refusal of {} on `{}` panicked: {panic}", op_text(&op), decl.local_id));
                        continue;
                    }
                };
                if let Some(text) = &answer {
                    observed.add(format!("refusal of {} on {}", op_text(&op), decl.local_id), text.clone());
                    if text.trim().is_empty() {
                        report.fail(
                            4,
                            format!(
                                "refusal of {} on `{}` gives no reason; a refusal is a sentence a person can act on",
                                op_text(&op),
                                decl.local_id
                            ),
                        );
                    }
                }
                dry.runs.push((decl.local_id.clone(), op, answer));
            }
        }
        // An operation on a device the instance does not have is refused, too.
        let ghost = "conformance-unknown-device";
        let op = dry.runs.first().map(|(_, op, _)| op.clone()).unwrap_or(Op::LightPower { on: true });
        match caught(|| plugin.refusal(&instance, ghost, &op)) {
            Err(panic) => report.fail(4, format!("refusal of a device the instance does not have panicked: {panic}")),
            Ok(None) => report
                .fail(4, format!("refusal accepts {} on `{ghost}`, a device the instance does not have", op_text(&op))),
            Ok(Some(_)) => {}
        }
    }

    settle().await;
    let reached = traffic().saturating_sub(before);
    if reached != 0 {
        report.fail(4, format!("the dry-run hub reached the network {reached} time(s); dry-run must reach nothing"));
    }
    dry
}

/// The declared devices are well formed.
fn decl_problems(classes: &[swifty_plugin_api::DeviceClass], decls: &[DeviceDecl], report: &mut Report) {
    let mut seen = HashSet::new();
    for decl in decls {
        let id = &decl.local_id;
        if !is_local_id(id) {
            report.fail(4, format!("the local id `{id}` must match [a-z0-9][a-z0-9_-]{{0,63}}"));
        }
        if !seen.insert(id.clone()) {
            report.fail(4, format!("the local id `{id}` is declared twice"));
        }
        if let Some(hint) = &decl.target_hint {
            if !is_local_id(hint) {
                report.fail(4, format!("the target hint `{hint}` of `{id}` must match [a-z0-9][a-z0-9_-]{{0,63}}"));
            }
        }
        if !classes.contains(&decl.class) {
            report.fail(4, format!("`{id}` is of a class the manifest does not offer"));
        }
        if decl.name.trim().is_empty() || !is_clean(&decl.name) {
            report.fail(
                4,
                format!(
                    "`{id}` needs a name with no control characters, of at most {} characters",
                    super::MAX_DEVICE_TEXT
                ),
            );
        }
    }
}
