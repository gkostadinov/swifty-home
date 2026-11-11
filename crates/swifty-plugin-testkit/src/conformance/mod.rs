//! The conformance suite every plugin runs from its `tests/conformance.rs`:
//!
//! ```text
//! #[tokio::test]
//! async fn conformance() {
//!     let device = MockUdp::start(|_| None).await;
//!     let fixture = Fixture { config: json!({ "host": device.addr.to_string() }), device: Box::new(device), ..Fixture::empty() };
//!     swifty_plugin_testkit::conformance::run(&my_plugin::PLUGIN, fixture).await;
//! }
//! ```
//!
//! [`run`] panics with every problem it found. [`report`] runs the same checks
//! and hands the problems back, numbered by check, for a test that wants to look
//! at them.
//!
//! What it checks:
//!
//! 1. **Manifest** ([`manifest_problems`]): id and slot grammar; `version` is the
//!    crate version; `api == API_VERSION`; custom action ids start with the
//!    plugin id; no custom action is Read or SafeDirection; parameter schemas
//!    are well formed; vocabulary is lower-case words and not grammar.
//! 2. **Config** ([`config_problems`]): the fixture validates; a missing required
//!    field and an unknown field are refused with the field named; every
//!    `ConfigField` key parses and no other key is in the fixture; `migrate`
//!    leaves a current config alone.
//! 3. **Purity**: with traps on every loopback address in the fixture, and the
//!    fixture's device watching its own, `validate`, `readiness`, `devices` and
//!    `refusal` cause no connection and no datagram, do not panic, and give the
//!    same answer twice.
//! 4. **Dry-run**: a stand-in for a dry-run hub drives every catalog operation
//!    (the catalog's ranges and their edges, see [`corpus`]) at every declared
//!    device. It never calls `connect`. Every run is simulated or refused with a
//!    sentence, nothing panics, and nothing is reached. The declared devices are
//!    well formed.
//! 5. **Refusals agree**: once live, `refusal` gives the dry-run answer for every
//!    one of those operations and still sends nothing. An operation it accepts is
//!    not answered `Disabled` by the driver (a rule the driver knows belongs in
//!    `refusal`, so dry-run shows it); these are sent last of all, after check 11,
//!    because they can leave the device anywhere. A device that is not in the
//!    directory is refused by the driver on its own, and nothing is sent for it.
//! 6. **Exactly once**: each operation of the fixture is accepted with exactly one
//!    command exchange at the mock ([`MockDevice::exchanges`]), and a silent device
//!    is not asked twice. A plugin whose `refusal` turns down every operation of
//!    the catalog has nothing to send, so checks 6 and 8 hold for it with no
//!    operations in the fixture.
//! 7. **Timeouts**: against a device that never answers ([`MockDevice::set_silent`]),
//!    `command`, `read_state`, `discover` and `health` end within connect plus
//!    read plus a margin ([`time_limit`]), and errors are `Timeout` or `Unreachable`.
//! 8. **Confirmation**: after each operation of the fixture, the state read back
//!    satisfies `Op::is_confirmed_by`, or the custom action's `Expect`.
//! 9. **Secrets**: no secret value of the fixture or of a finished setup appears
//!    in an error, a `Debug` output, a `State`, a setup step, a log line captured
//!    from `tracing`, or a URL query the device saw.
//! 10. **Device text**: when the device reports a name with control characters
//!     and 10 kB of text ([`MockDevice::set_device_text`]), every string in what
//!     comes back is clean and at most [`MAX_DEVICE_TEXT`] characters. That
//!     includes the setup flow's steps: a string a step shows now that it did not
//!     show with a normal name came from the device.
//! 11. **Setup**: the fixture's inputs drive the flow to `Done`; the result
//!     validates; secrets are only in `secrets` and their slots are declared; a
//!     secret field is never prefilled; with `grant: None` nothing is contacted.
//!     The flow is begun as the fixture's instance set up again (`existing` is the
//!     fixture's config), so a flow whose endpoints come from config reaches the
//!     fixture's device and never a vendor's real service. When the fixture holds
//!     secrets the same inputs run once more with those slots already stored (the
//!     flow's `secrets_present`), as when an owner sets an integration up again:
//!     it still ends at `Done`, with the same rules.
//! 12. **Robustness**: malformed, truncated and oversized replies
//!     ([`MockDevice::set_fault`]) give an error, never a panic, and never a state.
//!
//! A check that needs something the fixture's device cannot do (go silent,
//! garble its replies, report a name) is listed under "not run" rather than
//! failed. [`Rigged`] adds those switches to any mock, and [`run_every_check`]
//! fails when anything was not run.

mod config;
pub mod corpus;
mod live;
mod logs;
mod manifest;
mod mocks;
mod pure;
mod secrets;
mod setup;
mod text;
mod watch;

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use config::config_problems;
pub use manifest::{is_grammar_word, manifest_problems, GRAMMAR_WORDS};
pub use mocks::{Rigged, Switches};
pub use text::{HOSTILE_TEXT, MAX_DEVICE_TEXT};

use swifty_plugin_api::net::Timeouts;
use swifty_plugin_api::{DeviceRecord, Op, Plugin, SetupInput};
use tracing::instrument::WithSubscriber;

/// A loopback stand-in for the plugin's device, as the suite sees it.
///
/// Only [`traffic`](MockDevice::traffic) is required. The other methods switch
/// on what checks 6, 7, 9, 10 and 12 need; a mock that cannot do one says so by
/// returning `false`, and the check that needs it is listed as not run.
pub trait MockDevice: Send + Sync {
    /// Every connection or datagram that has reached it.
    fn traffic(&self) -> usize;

    /// Command exchanges it has seen: one per datagram, request or frame that
    /// asks it to do something. Check 6 wants exactly one per operation. The
    /// default is [`traffic`](MockDevice::traffic), which is right for a device
    /// that takes one datagram or one connection per command.
    fn exchanges(&self) -> usize {
        self.traffic()
    }

    /// While `true`, it hears everything and answers nothing. Returns whether
    /// it can.
    fn set_silent(&self, _silent: bool) -> bool {
        false
    }

    /// While `Some`, every reply is spoiled this way ([`Fault::apply`]). Returns
    /// whether it can spoil its replies this way.
    fn set_fault(&self, _fault: Option<Fault>) -> bool {
        false
    }

    /// While `Some`, every name or label it reports (in discovery and in its
    /// state) is this text. Returns whether it reports any text.
    fn set_device_text(&self, _text: Option<&str>) -> bool {
        false
    }

    /// The path and query of every URL it was asked for, for check 9. Empty for
    /// a device that has no URLs.
    fn urls(&self) -> Vec<String> {
        Vec::new()
    }
}

/// A way to spoil a reply, for check 12.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// The first quarter of the reply, then bytes that are no text at all.
    Malformed,
    /// The first half of the reply.
    Truncated,
    /// The reply, then [`OVERSIZE`] bytes of `x`.
    Oversized,
}

/// How many bytes [`Fault::Oversized`] adds: more than any device reply the
/// socket helpers accept, and still one UDP datagram.
pub const OVERSIZE: usize = 60_000;

impl Fault {
    /// Every fault, in the order the suite tries them.
    pub const ALL: [Fault; 3] = [Fault::Malformed, Fault::Truncated, Fault::Oversized];

    /// `reply`, spoiled.
    pub fn apply(self, reply: &[u8]) -> Vec<u8> {
        match self {
            Fault::Malformed => {
                let keep = reply.len() / 4;
                let mut spoiled = reply[..keep].to_vec();
                spoiled.resize(reply.len().max(8), 0xFF);
                spoiled
            }
            Fault::Truncated => reply[..reply.len() / 2].to_vec(),
            Fault::Oversized => {
                let mut spoiled = reply.to_vec();
                spoiled.resize(reply.len() + OVERSIZE, b'x');
                spoiled
            }
        }
    }

    /// `reply`, spoiled, for a device that speaks text. A malformed reply has
    /// replacement characters where the bytes were not text.
    pub fn apply_text(self, reply: &str) -> String {
        String::from_utf8_lossy(&self.apply(reply.as_bytes())).into_owned()
    }

    fn name(self) -> &'static str {
        match self {
            Fault::Malformed => "malformed",
            Fault::Truncated => "truncated",
            Fault::Oversized => "oversized",
        }
    }
}

/// A device that is never there. For a fixture whose checks need none.
struct Nothing;

impl MockDevice for Nothing {
    fn traffic(&self) -> usize {
        0
    }
}

/// What the suite runs a plugin against.
pub struct Fixture {
    /// A config that passes validate() and points at `device`.
    pub config: serde_json::Value,
    /// Slot and value. The suite adds a canary to each value's content checks.
    pub secrets: Vec<(String, String)>,
    /// Devices adopted for the instance.
    pub adopted: Vec<DeviceRecord>,
    /// The loopback mock, with its traffic counters.
    pub device: Box<dyn MockDevice>,
    /// Each supported operation to exercise, by local id.
    pub ops: Vec<(String, Op)>,
    /// Inputs that drive the setup flow to Done against `device`.
    pub setup: Vec<SetupInput>,
}

impl Fixture {
    /// An empty object as config, no secrets, devices, ops or setup inputs, and
    /// no device. Fill in what the plugin needs with struct update syntax.
    pub fn empty() -> Self {
        Self {
            config: serde_json::json!({}),
            secrets: Vec::new(),
            adopted: Vec::new(),
            device: Box::new(Nothing),
            ops: Vec::new(),
            setup: Vec::new(),
        }
    }

    /// The slots the fixture fills, as a pure call sees them.
    fn slots(&self) -> BTreeSet<String> {
        self.secrets.iter().map(|(slot, _)| slot.clone()).collect()
    }
}

/// What the live checks give the driver: short, so a silent device is quick.
pub const TIMEOUTS: Timeouts = Timeouts { connect: Duration::from_millis(500), read: Duration::from_millis(500) };

/// What a call may take beyond connect plus read before check 7 calls it too slow.
pub const MARGIN: Duration = Duration::from_millis(1_500);

/// The longest a call against a silent device may take: connect plus read plus
/// [`MARGIN`].
pub fn time_limit() -> Duration {
    TIMEOUTS.connect + TIMEOUTS.read + MARGIN
}

/// One thing a check found wrong, or could not look at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// The number of the check, 1 to 12, as `docs/plugins/testing.md` lists them.
    pub check: u8,
    /// One sentence.
    pub text: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[check {} {}] {}", self.check, check_name(self.check), self.text)
    }
}

/// The short name of each check.
pub fn check_name(check: u8) -> &'static str {
    match check {
        1 => "manifest",
        2 => "config",
        3 => "purity",
        4 => "dry-run",
        5 => "refusals agree",
        6 => "exactly once",
        7 => "timeouts",
        8 => "confirmation",
        9 => "secrets",
        10 => "device text",
        11 => "setup",
        12 => "robustness",
        _ => "other",
    }
}

/// What a run of the suite found.
#[derive(Debug, Default, Clone)]
pub struct Report {
    /// What is wrong. The plugin conforms when this is empty.
    pub problems: Vec<Problem>,
    /// What could not be checked with this fixture, and why.
    pub not_run: Vec<Problem>,
}

impl Report {
    /// Whether nothing is wrong.
    pub fn passed(&self) -> bool {
        self.problems.is_empty()
    }

    /// Whether this check found something wrong.
    pub fn failed(&self, check: u8) -> bool {
        self.problems.iter().any(|problem| problem.check == check)
    }

    /// The checks that found something wrong, in order.
    pub fn failed_checks(&self) -> BTreeSet<u8> {
        self.problems.iter().map(|problem| problem.check).collect()
    }

    fn fail(&mut self, check: u8, text: impl Into<String>) {
        self.problems.push(Problem { check, text: text.into() });
    }

    fn skip(&mut self, check: u8, text: impl Into<String>) {
        self.not_run.push(Problem { check, text: text.into() });
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for problem in &self.problems {
            writeln!(f, "- {problem}")?;
        }
        if !self.not_run.is_empty() {
            writeln!(f, "not run:")?;
            for problem in &self.not_run {
                writeln!(f, "- {problem}")?;
            }
        }
        Ok(())
    }
}

/// Everything the plugin said during the run, for check 9 to search.
#[derive(Default)]
struct Observed {
    /// Where it was seen, and the text.
    texts: Vec<(String, String)>,
}

impl Observed {
    fn add(&mut self, place: impl Into<String>, text: impl Into<String>) {
        self.texts.push((place.into(), text.into()));
    }

    /// A value's `Debug` and, when it has one, its JSON.
    fn debug(&mut self, place: &str, value: &impl fmt::Debug) {
        self.add(format!("the Debug output of {place}"), format!("{value:?}"));
    }

    fn json(&mut self, place: &str, value: &impl serde::Serialize) {
        if let Ok(json) = serde_json::to_string(value) {
            self.add(format!("the JSON of {place}"), json);
        }
    }

    fn error(&mut self, place: &str, error: &swifty_plugin_api::DeviceError) {
        self.add(format!("the error of {place}"), error.to_string());
        self.debug(&format!("the error of {place}"), error);
    }
}

/// How a call to the plugin ended.
enum Outcome<T> {
    /// It returned, after this long.
    Done(T, Duration),
    /// It panicked, with this message.
    Panicked(String),
    /// It was still running when the suite stopped waiting.
    Hung,
}

/// The text of a panic.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| text.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic without a message".to_string())
}

/// Runs `call` in its own task, as the hub does, under `limit`.
async fn guarded<T, F>(limit: Duration, call: F) -> Outcome<T>
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    let started = Instant::now();
    let mut task = tokio::spawn(call.with_current_subscriber());
    match tokio::time::timeout(limit, &mut task).await {
        Err(_) => {
            task.abort();
            Outcome::Hung
        }
        Ok(Err(error)) if error.is_panic() => Outcome::Panicked(panic_text(&*error.into_panic())),
        Ok(Err(_)) => Outcome::Hung,
        Ok(Ok(value)) => Outcome::Done(value, started.elapsed()),
    }
}

/// Runs a pure call, catching a panic.
fn caught<T>(call: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(call)).map_err(|payload| panic_text(&*payload))
}

/// Waits for packets in flight to reach the counters.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(60)).await;
}

/// The operation, as a reader sees it in a problem.
fn op_text(op: &Op) -> String {
    serde_json::to_string(op).unwrap_or_else(|_| format!("{op:?}"))
}

/// Runs every check and hands back what it found. Checks the plugin against
/// its fixture; it never panics because of the plugin.
pub async fn report(plugin: &'static dyn Plugin, fixture: Fixture) -> Report {
    let capture = logs::Capture::new();
    let dispatch = capture.dispatch();
    let mut report = all_checks(plugin, fixture, &capture).with_subscriber(dispatch).await;
    report.problems.sort_by_key(|problem| problem.check);
    report.not_run.sort_by_key(|problem| problem.check);
    report
}

/// Runs the suite and panics with every problem it found, one per line.
pub async fn run(plugin: &'static dyn Plugin, fixture: Fixture) {
    let report = report(plugin, fixture).await;
    assert!(report.passed(), "plugin `{}` fails conformance:\n{report}", plugin.manifest().id);
}

/// As [`run`], and it also panics when a check could not run with this
/// fixture: every one of the twelve checks must have looked at the plugin.
/// The first-party plugins use this one.
pub async fn run_every_check(plugin: &'static dyn Plugin, fixture: Fixture) {
    let report = report(plugin, fixture).await;
    assert!(report.passed(), "plugin `{}` fails conformance:\n{report}", plugin.manifest().id);
    assert!(
        report.not_run.is_empty(),
        "plugin `{}` passes, but its fixture leaves checks out:\n{report}",
        plugin.manifest().id
    );
}

async fn all_checks(plugin: &'static dyn Plugin, fixture: Fixture, capture: &logs::Capture) -> Report {
    let mut report = Report::default();
    let mut observed = Observed::default();
    let manifest = plugin.manifest();
    let slots = fixture.slots();
    let instance = manifest.id.to_string();

    // Checks 1 and 2, then 3 and 4: nothing here may reach the network.
    let watch = watch::Watch::start(&fixture).await;
    let before = fixture.device.traffic() + watch.traffic();
    for text in manifest_problems(manifest, crate_version_under_test().as_deref()) {
        report.fail(1, text);
    }
    let config_ok = plugin.validate(&fixture.config).is_ok();
    for text in config_problems(plugin, &fixture.config) {
        report.fail(2, text);
    }
    settle().await;
    let traffic = (fixture.device.traffic() + watch.traffic()).saturating_sub(before);
    if traffic != 0 {
        report
            .fail(3, format!("validate or migrate reached the network {traffic} time(s); they must not open a socket"));
    }
    let view = pure::View { instance: &instance, config: &fixture.config, slots: &slots, adopted: &fixture.adopted };
    pure::purity(plugin, &fixture, &view, &watch, &mut report, &mut observed).await;
    let dry = pure::dry_run(plugin, &fixture, &view, &watch, &mut report, &mut observed).await;
    drop(watch);

    // Checks 5 to 10 and 12 need the plugin live. The reads of check 10 and the
    // fixture's operations go first, against the device as the fixture set it
    // up, then check 11's setup; the whole corpus, which can leave the device
    // anywhere (shut down, stopped), goes last.
    let live_ready = config_ok && dry.ready;
    let mut live = None;
    if !live_ready {
        for check in [5, 6, 7, 8, 10, 12] {
            report.skip(check, "the fixture's instance is not valid and ready, so it was not connected");
        }
    } else {
        match live::connect(plugin, &fixture, &dry, &mut report, &mut observed) {
            Some(driver) => live = Some(live::Live { plugin, fixture: &fixture, view: &view, driver, dry: &dry }),
            None => {
                for check in [6, 7, 8, 10, 12] {
                    report.skip(check, "connect failed");
                }
            }
        }
    }
    let mut accepted = Vec::new();
    if let Some(live) = &live {
        accepted = live.refusals_agree(&mut report, &mut observed).await;
        live.device_text(&mut report, &mut observed).await;
        live.exactly_once_and_confirmed(&mut report, &mut observed).await;
        live.silence(&mut report, &mut observed).await;
        live.robustness(&mut report, &mut observed).await;
    }

    // Check 11 runs its own flows, with and without a grant.
    let setup_secrets = setup::check(plugin, &fixture, &mut report, &mut observed).await;
    if live.is_some() {
        setup::device_text(plugin, &fixture, &mut report).await;
    }

    if let Some(live) = &live {
        live.accepted_ops_are_taken(accepted, &mut report, &mut observed).await;
    }

    // Check 9 looks at everything seen so far.
    let mut canaries: Vec<(String, String)> = fixture.secrets.clone();
    canaries.extend(setup_secrets);
    secrets::check(&canaries, &observed, &capture.text(), &fixture.device.urls(), &mut report);
    report
}

/// The version of the crate under test, as cargo states it when it runs that
/// crate's tests. `None` outside cargo.
fn crate_version_under_test() -> Option<String> {
    std::env::var("CARGO_PKG_VERSION").ok().filter(|version| !version.is_empty())
}

/// The fixture's devices and the instance's directory, as the hub would keep them.
fn directory_records(
    instance: &str,
    decls: &[swifty_plugin_api::DeviceDecl],
    adopted: &[DeviceRecord],
) -> Vec<DeviceRecord> {
    let mut records: Vec<DeviceRecord> = decls
        .iter()
        .map(|decl| DeviceRecord {
            target: decl.target_hint.clone().unwrap_or_else(|| format!("{instance}-{}", decl.local_id)),
            local_id: decl.local_id.clone(),
            class: decl.class,
            name: decl.name.clone(),
            attrs: decl.attrs.clone(),
        })
        .collect();
    for record in adopted {
        if !records.iter().any(|known| known.local_id == record.local_id) {
            records.push(record.clone());
        }
    }
    records
}

/// A directory that does not change.
struct FixedDirectory(Vec<DeviceRecord>);

impl swifty_plugin_api::Directory for FixedDirectory {
    fn get(&self, local_id: &str) -> Option<DeviceRecord> {
        self.0.iter().find(|record| record.local_id == local_id).cloned()
    }

    fn all(&self) -> Vec<DeviceRecord> {
        self.0.clone()
    }
}

fn fixed_directory(records: Vec<DeviceRecord>) -> Arc<dyn swifty_plugin_api::Directory> {
    Arc::new(FixedDirectory(records))
}
