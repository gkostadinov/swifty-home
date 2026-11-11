//! Checks 5 to 8, 10 and 12: the plugin with a live grant, against the fixture's device.

use std::sync::Arc;
use std::time::Duration;

use swifty_plugin_api::net::LiveGrant;
use swifty_plugin_api::{CommandAck, DeviceError, Driver, Expect, LiveContext, Op, Plugin, Secrets, State};

use super::pure::{DryRun, View};
use super::text::{hostile, is_clean, preview, strings};
use super::{
    caught, directory_records, fixed_directory, guarded, op_text, settle, time_limit, Fault, Fixture, Observed,
    Outcome, Report, TIMEOUTS,
};

/// The longest the suite waits for a call against a device that answers.
const NORMAL_LIMIT: Duration = Duration::from_secs(10);

/// How long `discover` is given to listen.
const DISCOVERY_WINDOW: Duration = Duration::from_millis(300);

/// How often check 8 reads a device back before it gives up, and how long it
/// waits between reads, as the engine polls until the confirmation timeout.
const CONFIRM_READS: usize = 5;
const CONFIRM_PAUSE: Duration = Duration::from_millis(100);

/// A local id no fixture has.
const GHOST: &str = "conformance-unknown-device";

/// Builds the driver as a live hub would. A failure is check 5's problem.
pub(super) fn connect(
    plugin: &dyn Plugin,
    fixture: &Fixture,
    dry: &DryRun,
    report: &mut Report,
    observed: &mut Observed,
) -> Option<Arc<dyn Driver>> {
    let instance = plugin.manifest().id;
    let Some(grant) = LiveGrant::when_live(true) else {
        report.fail(5, "no live grant could be made");
        return None;
    };
    let ctx = LiveContext {
        instance_id: instance.to_string(),
        config: fixture.config.clone(),
        secrets: Secrets::of(fixture.secrets.clone()),
        grant,
        timeouts: TIMEOUTS,
        directory: fixed_directory(directory_records(instance, &dry.decls, &fixture.adopted)),
    };
    match caught(|| plugin.connect(ctx)) {
        Ok(Ok(driver)) => Some(driver),
        Ok(Err(error)) => {
            observed.add("connect's error", error.to_string());
            observed.debug("connect's error", &error);
            report.fail(5, format!("connect failed with the fixture's config and secrets: {error}"));
            None
        }
        Err(panic) => {
            report.fail(5, format!("connect panicked: {panic}"));
            None
        }
    }
}

/// The plugin, live.
pub(super) struct Live<'a> {
    pub(super) plugin: &'static dyn Plugin,
    pub(super) fixture: &'a Fixture,
    pub(super) view: &'a View<'a>,
    pub(super) driver: Arc<dyn Driver>,
    pub(super) dry: &'a DryRun,
}

impl Live<'_> {
    async fn command(&self, device: &str, op: &Op, limit: Duration) -> Outcome<Result<CommandAck, DeviceError>> {
        let (driver, device, op) = (self.driver.clone(), device.to_string(), op.clone());
        guarded(limit, async move { driver.command(&device, &op).await }).await
    }

    async fn read(&self, device: &str, limit: Duration) -> Outcome<Result<State, DeviceError>> {
        let (driver, device) = (self.driver.clone(), device.to_string());
        guarded(limit, async move { driver.read_state(&device).await }).await
    }

    async fn discover(&self, limit: Duration) -> Outcome<Result<Vec<swifty_plugin_api::DeviceDecl>, DeviceError>> {
        let driver = self.driver.clone();
        guarded(limit, async move { driver.discover(DISCOVERY_WINDOW).await }).await
    }

    /// The devices the checks read: those of the fixture's operations, then the declared ones.
    fn devices(&self) -> Vec<String> {
        let mut devices: Vec<String> = Vec::new();
        let ids = self.fixture.ops.iter().map(|(device, _)| device).chain(self.dry.decls.iter().map(|d| &d.local_id));
        for id in ids {
            if !devices.contains(id) {
                devices.push(id.clone());
            }
        }
        devices
    }

    /// Records what a call gave back, for check 9.
    fn note<T: std::fmt::Debug>(&self, observed: &mut Observed, place: &str, result: &Result<T, DeviceError>) {
        match result {
            Ok(value) => observed.debug(place, value),
            Err(error) => observed.error(place, error),
        }
    }

    fn note_state(&self, observed: &mut Observed, place: &str, result: &Result<State, DeviceError>) {
        self.note(observed, place, result);
        if let Ok(state) = result {
            observed.json(place, state);
        }
    }

    /// Check 5, the half that leaves the device as it is: `refusal` gives the
    /// dry-run answer once live and sends nothing, and the driver refuses a
    /// device that is not in its directory on its own. Hands back what
    /// `refusal` accepts, for [`Live::accepted_ops_are_taken`].
    pub(super) async fn refusals_agree(&self, report: &mut Report, observed: &mut Observed) -> Vec<(String, Op)> {
        let device = &self.fixture.device;
        let instance = self.view.view();
        let before = device.traffic();
        let mut accepted = Vec::new();
        for (local, op, dry) in &self.dry.runs {
            match caught(|| self.plugin.refusal(&instance, local, op)) {
                Err(panic) => {
                    report.fail(5, format!("refusal of {} on `{local}` panicked once live: {panic}", op_text(op)))
                }
                Ok(live) if live != *dry => report.fail(
                    5,
                    format!("refusal of {} on `{local}` says {dry:?} in dry-run but {live:?} live", op_text(op)),
                ),
                Ok(None) => accepted.push((local.clone(), op.clone())),
                Ok(Some(_)) => {}
            }
        }
        settle().await;
        let reached = device.traffic().saturating_sub(before);
        if reached != 0 {
            report.fail(
                5,
                format!("refusal reached the device {reached} time(s) once the plugin was live; it stays pure"),
            );
        }

        // A device that is not in the directory is refused by the driver itself.
        let op = self.dry.runs.first().map(|(_, op, _)| op.clone()).unwrap_or(Op::LightPower { on: true });
        settle().await;
        let before = device.traffic();
        match self.command(GHOST, &op, NORMAL_LIMIT).await {
            Outcome::Done(Ok(_), _) => report
                .fail(5, format!("the driver accepts {} for `{GHOST}`, which is not in its directory", op_text(&op))),
            Outcome::Done(Err(error), _) => observed.error("a command to a device not in the directory", &error),
            Outcome::Panicked(panic) => {
                report.fail(5, format!("a command to a device not in the directory panicked: {panic}"))
            }
            Outcome::Hung => report.fail(5, "a command to a device not in the directory did not end"),
        }
        match self.read(GHOST, NORMAL_LIMIT).await {
            Outcome::Done(Ok(state), _) => {
                report.fail(5, format!("read_state of `{GHOST}`, which is not in the directory, gives {state:?}"))
            }
            Outcome::Done(Err(error), _) => observed.error("a read of a device not in the directory", &error),
            Outcome::Panicked(panic) => {
                report.fail(5, format!("a read of a device not in the directory panicked: {panic}"))
            }
            Outcome::Hung => report.fail(5, "a read of a device not in the directory did not end"),
        }
        settle().await;
        let reached = device.traffic().saturating_sub(before);
        if reached != 0 {
            report.fail(
                5,
                format!("the driver reached the device {reached} time(s) for `{GHOST}`, which is not in its directory"),
            );
        }
        accepted
    }

    /// Check 5, the half that changes the device, run last: every operation
    /// `refusal` accepts is sent, and the driver must not answer `Disabled`, a
    /// rule only the driver knows. `Rejected`, `Timeout` and `Unreachable`
    /// depend on the state the device is in by now, so they are not looked at.
    pub(super) async fn accepted_ops_are_taken(
        &self,
        accepted: Vec<(String, Op)>,
        report: &mut Report,
        observed: &mut Observed,
    ) {
        for (local, op) in &accepted {
            let place = format!("command {} on {local}", op_text(op));
            match self.command(local, op, NORMAL_LIMIT).await {
                Outcome::Done(Err(DeviceError::Disabled(text)), _) => {
                    observed.add(format!("the error of {place}"), text.clone());
                    report.fail(
                        5,
                        format!(
                            "refusal accepts {} on `{local}`, but the driver answers Disabled ({text}); put that rule in \
                             refusal, so dry-run refuses it too",
                            op_text(op)
                        ),
                    );
                }
                Outcome::Done(result, _) => self.note(observed, &place, &result),
                Outcome::Panicked(panic) => report.fail(5, format!("{place} panicked: {panic}")),
                Outcome::Hung => report.fail(5, format!("{place} did not end within {NORMAL_LIMIT:?}")),
            }
        }
    }

    /// The `Expect` of a custom operation, or why there is none.
    fn expect_of(&self, op: &Op) -> Result<Option<Expect>, String> {
        let Op::Custom { action, .. } = op else { return Ok(None) };
        self.plugin
            .manifest()
            .actions
            .iter()
            .find(|declared| declared.id == action)
            .map(|declared| Some(declared.expect))
            .ok_or_else(|| format!("the fixture's operation names `{action}`, which the manifest does not declare"))
    }

    /// Checks 6 (the half with a device that answers) and 8.
    pub(super) async fn exactly_once_and_confirmed(&self, report: &mut Report, observed: &mut Observed) {
        let device = &self.fixture.device;
        if self.fixture.ops.is_empty() {
            let accepted = self.dry.runs.iter().filter(|(_, _, answer)| answer.is_none()).count();
            if accepted > 0 || self.dry.decls.is_empty() {
                let why = if self.dry.decls.is_empty() {
                    "the fixture lists no operations, and its instance declares no device".to_string()
                } else {
                    format!("the fixture lists no operations, though refusal accepts {accepted} of the catalog's")
                };
                report.skip(6, why.clone());
                report.skip(8, why);
            }
            // Otherwise refusal turned down every operation the catalog has for
            // every device: nothing can be sent twice or left unconfirmed.
            return;
        }
        for (local, op) in &self.fixture.ops {
            let place = format!("command {} on {local}", op_text(op));
            let expect = match self.expect_of(op) {
                Ok(expect) => expect,
                Err(text) => {
                    report.fail(8, text);
                    continue;
                }
            };
            let needs_baseline = op.needs_baseline() || expect.is_some_and(|expect| expect.needs_baseline());
            let mut baseline = None;
            if needs_baseline {
                match self.read(local, NORMAL_LIMIT).await {
                    Outcome::Done(result, _) => {
                        self.note_state(observed, &format!("read_state of {local}"), &result);
                        match result {
                            Ok(state) => baseline = Some(state),
                            Err(error) => {
                                report.fail(8, format!("the read of `{local}` before {} failed: {error}", op_text(op)))
                            }
                        }
                    }
                    Outcome::Panicked(panic) => report.fail(8, format!("read_state of `{local}` panicked: {panic}")),
                    Outcome::Hung => {
                        report.fail(8, format!("read_state of `{local}` did not end within {NORMAL_LIMIT:?}"))
                    }
                }
            }

            let before = device.exchanges();
            match self.command(local, op, NORMAL_LIMIT).await {
                Outcome::Done(Ok(_), _) => {
                    settle().await;
                    let exchanges = device.exchanges().saturating_sub(before);
                    if exchanges != 1 {
                        report.fail(
                            6,
                            format!(
                                "{} on `{local}` made {exchanges} command exchanges at the device; a command is sent exactly once",
                                op_text(op)
                            ),
                        );
                    }
                }
                Outcome::Done(Err(error), _) => {
                    observed.error(&place, &error);
                    report.fail(
                        6,
                        format!("{place} failed against the fixture's device ({error}); every operation of the fixture must be one it accepts"),
                    );
                    continue;
                }
                Outcome::Panicked(panic) => {
                    report.fail(6, format!("{place} panicked: {panic}"));
                    continue;
                }
                Outcome::Hung => {
                    report.fail(6, format!("{place} did not end within {NORMAL_LIMIT:?}"));
                    continue;
                }
            }

            if op.cannot_be_read_back() || matches!(expect, Some(Expect::Unreadable)) {
                continue;
            }
            let mut last = String::from("nothing");
            let mut confirmed = false;
            for attempt in 0..CONFIRM_READS {
                if attempt > 0 {
                    tokio::time::sleep(CONFIRM_PAUSE).await;
                }
                match self.read(local, NORMAL_LIMIT).await {
                    Outcome::Done(result, _) => {
                        self.note_state(observed, &format!("read_state of {local}"), &result);
                        match result {
                            Ok(state) => {
                                let met = match (&expect, op) {
                                    (Some(expect), Op::Custom { args, .. }) => {
                                        expect.is_met(args, &state, baseline.as_ref())
                                    }
                                    _ => op.is_confirmed_by(&state, baseline.as_ref()),
                                };
                                if met {
                                    confirmed = true;
                                    break;
                                }
                                last = format!("{state:?}");
                            }
                            Err(error) => last = format!("the error {error}"),
                        }
                    }
                    Outcome::Panicked(panic) => {
                        last = format!("a panic ({panic})");
                        break;
                    }
                    Outcome::Hung => last = "no answer".to_string(),
                }
            }
            if !confirmed {
                report.fail(
                    8,
                    format!(
                        "after {} on `{local}`, read_state gives {last}, which does not confirm it; report what the \
                         device says now",
                        op_text(op)
                    ),
                );
            }
        }
    }

    /// Check 10.
    pub(super) async fn device_text(&self, report: &mut Report, observed: &mut Observed) {
        let device = &self.fixture.device;
        let text = hostile();
        if !device.set_device_text(Some(&text)) {
            report.skip(10, "the fixture's device reports no text (MockDevice::set_device_text)");
            return;
        }
        let mut seen: Vec<(String, String)> = Vec::new();
        for local in self.devices() {
            let place = format!("read_state of {local}");
            match self.read(&local, NORMAL_LIMIT).await {
                Outcome::Done(result, _) => {
                    self.note_state(observed, &place, &result);
                    match result {
                        Ok(state) => {
                            if let Ok(json) = serde_json::to_value(&state) {
                                strings(&json, &format!("the state of `{local}`"), &mut seen);
                            }
                        }
                        Err(error) => seen.push((format!("the error of {place}"), error.to_string())),
                    }
                }
                Outcome::Panicked(panic) => report.fail(10, format!("{place} panicked on a long name: {panic}")),
                Outcome::Hung => report.fail(10, format!("{place} did not end on a long name")),
            }
        }
        match self.discover(NORMAL_LIMIT).await {
            Outcome::Done(result, _) => {
                self.note(observed, "discover", &result);
                match result {
                    Ok(decls) => {
                        for decl in decls {
                            let at = format!("the discovered device `{}`", decl.local_id);
                            seen.push((format!("{at}'s name"), decl.name.clone()));
                            seen.push((format!("{at}'s local id"), decl.local_id.clone()));
                            if let Some(hint) = &decl.target_hint {
                                seen.push((format!("{at}'s target hint"), hint.clone()));
                            }
                            strings(&decl.attrs, &format!("{at}'s attrs"), &mut seen);
                        }
                    }
                    Err(error) => seen.push(("the error of discover".to_string(), error.to_string())),
                }
            }
            Outcome::Panicked(panic) => report.fail(10, format!("discover panicked on a long name: {panic}")),
            Outcome::Hung => report.fail(10, "discover did not end on a long name"),
        }
        device.set_device_text(None);
        for (place, value) in seen {
            if !is_clean(&value) {
                report.fail(
                    10,
                    format!(
                        "{place} is {}; pass device text through net::device_text (no control characters, at most {} \
                         characters)",
                        preview(&value),
                        super::MAX_DEVICE_TEXT
                    ),
                );
            }
        }
    }

    /// Check 7, and check 6's other half: silence is not retried.
    pub(super) async fn silence(&self, report: &mut Report, observed: &mut Observed) {
        let device = &self.fixture.device;
        if !device.set_silent(true) {
            let why = "the fixture's device cannot go silent (MockDevice::set_silent)";
            report.skip(6, format!("{why}, so retries were not looked for"));
            report.skip(7, why);
            return;
        }
        let limit = time_limit();
        let hard = limit * 3;
        let slow = |what: &str, elapsed: Duration, limit: Duration| {
            format!(
                "{what} took {elapsed:?} against a device that never answers; the limit is {limit:?} (connect {:?} + \
                 read {:?} + margin); use the Timeouts in LiveContext",
                TIMEOUTS.connect, TIMEOUTS.read
            )
        };
        for (local, op) in &self.fixture.ops {
            let place = format!("command {} on {local}", op_text(op));
            let before = device.exchanges();
            match self.command(local, op, hard).await {
                Outcome::Done(result, elapsed) => {
                    if elapsed > limit {
                        report.fail(7, slow(&place, elapsed, limit));
                    }
                    self.note(observed, &place, &result);
                    match result {
                        Ok(_) => report.fail(7, format!("{place} was acknowledged by a device that never answers")),
                        Err(error) if !error.is_silence() => report.fail(
                            7,
                            format!("{place} ended with {error:?} against a silent device; silence is Timeout or Unreachable"),
                        ),
                        Err(_) => {}
                    }
                }
                Outcome::Panicked(panic) => {
                    report.fail(7, format!("{place} panicked against a silent device: {panic}"))
                }
                Outcome::Hung => report.fail(7, format!("{place} did not end within {hard:?} against a silent device")),
            }
            settle().await;
            let sent = device.exchanges().saturating_sub(before);
            if sent > 1 {
                report.fail(
                    6,
                    format!("{place} was sent {sent} times to a device that did not answer; never retry a command, the engine decides"),
                );
            }
        }
        for local in self.devices() {
            let place = format!("read_state of {local}");
            match self.read(&local, hard).await {
                Outcome::Done(result, elapsed) => {
                    if elapsed > limit {
                        report.fail(7, slow(&place, elapsed, limit));
                    }
                    self.note_state(observed, &place, &result);
                    match result {
                        Ok(state) => report.fail(7, format!("{place} gives {state:?} from a device that never answers")),
                        Err(error) if !error.is_silence() => report.fail(
                            7,
                            format!("{place} ended with {error:?} against a silent device; silence is Timeout or Unreachable"),
                        ),
                        Err(_) => {}
                    }
                }
                Outcome::Panicked(panic) => {
                    report.fail(7, format!("{place} panicked against a silent device: {panic}"))
                }
                Outcome::Hung => report.fail(7, format!("{place} did not end within {hard:?} against a silent device")),
            }
        }
        let driver = self.driver.clone();
        match guarded(hard, async move { driver.health().await }).await {
            Outcome::Done(health, elapsed) => {
                observed.debug("health", &health);
                if elapsed > limit {
                    report.fail(7, slow("health", elapsed, limit));
                }
            }
            Outcome::Panicked(panic) => report.fail(7, format!("health panicked against a silent device: {panic}")),
            Outcome::Hung => report.fail(7, format!("health did not end within {hard:?} against a silent device")),
        }
        match self.discover(hard + DISCOVERY_WINDOW).await {
            Outcome::Done(result, elapsed) => {
                self.note(observed, "discover", &result);
                if elapsed > limit + DISCOVERY_WINDOW {
                    report.fail(7, slow("discover", elapsed, limit + DISCOVERY_WINDOW));
                }
            }
            Outcome::Panicked(panic) => report.fail(7, format!("discover panicked against a silent device: {panic}")),
            Outcome::Hung => report.fail(7, format!("discover did not end within {hard:?} against a silent device")),
        }
        device.set_silent(false);
        settle().await;
    }

    /// Check 12.
    pub(super) async fn robustness(&self, report: &mut Report, observed: &mut Observed) {
        let device = &self.fixture.device;
        let hard = time_limit() * 3;
        for fault in Fault::ALL {
            let name = fault.name();
            if !device.set_fault(Some(fault)) {
                report.skip(12, format!("the fixture's device cannot send {name} replies (MockDevice::set_fault)"));
                continue;
            }
            for (local, op) in &self.fixture.ops {
                let place = format!("command {} on {local}", op_text(op));
                match self.command(local, op, hard).await {
                    Outcome::Done(result, _) => self.note(observed, &place, &result),
                    Outcome::Panicked(panic) => report.fail(12, format!("{place} panicked on a {name} reply: {panic}")),
                    Outcome::Hung => report.fail(12, format!("{place} did not end on a {name} reply")),
                }
            }
            for local in self.devices() {
                let place = format!("read_state of {local}");
                match self.read(&local, hard).await {
                    Outcome::Done(Ok(state), _) => {
                        observed.debug(&place, &state);
                        report.fail(12, format!("{place} made {state:?} out of a {name} reply; that is an error"));
                    }
                    Outcome::Done(Err(error), _) => observed.error(&place, &error),
                    Outcome::Panicked(panic) => report.fail(12, format!("{place} panicked on a {name} reply: {panic}")),
                    Outcome::Hung => report.fail(12, format!("{place} did not end on a {name} reply")),
                }
            }
            match self.discover(hard + DISCOVERY_WINDOW).await {
                Outcome::Done(result, _) => self.note(observed, "discover", &result),
                Outcome::Panicked(panic) => report.fail(12, format!("discover panicked on a {name} reply: {panic}")),
                Outcome::Hung => report.fail(12, format!("discover did not end on a {name} reply")),
            }
            device.set_fault(None);
        }
    }
}
