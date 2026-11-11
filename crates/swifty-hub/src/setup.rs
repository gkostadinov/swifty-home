//! Setup sessions: the hub runs a plugin's setup flow for the web app
//! and the command line, keeps it in memory, and applies what it ends with.
//!
//! A flow produces steps that are data ([`SetupStep`]); whoever drives it renders a
//! step and sends back an input ([`SetupInput`]). The hub checks every input against
//! the step it answers before the plugin sees it, so a plugin never gets a field it
//! did not ask for, a number out of its range or a choice it did not offer.
//!
//! - A session is opened by `plugin.add` or `plugin.reconfigure` (both critical, so
//!   after a passkey step-up) or by the command line, which is the local
//!   administrator. It is bound to the session that opened it: nobody else can see or
//!   drive it. At most [`MAX_OPEN`] are open at once, one per instance, and one left
//!   alone for [`IDLE_MS`] is forgotten.
//! - A `wait` step is polled by the hub itself every `poll_ms` until it moves on or
//!   its `timeout_s` runs out; each time it moves on, the owning session gets a
//!   `setup` event. A client may also send `poll` to ask at once.
//! - Nothing is stored before `done`. Then, in one transaction, the hub checks the
//!   config with the plugin, writes the instance, upserts the devices its config
//!   names and adopts the ones that were chosen, moves `instances_rev`, and appends a
//!   `plugin.setup` row naming the secret slots it wrote, never their values. The
//!   secrets go to the secret store just before that transaction, and the instance
//!   is then connected again. The `done` step a client is sent carries the summary
//!   only.
//! - A plugin that panics or hangs inside its flow ends that one setup, never the hub.
//!
//! Values typed into a secret field reach the plugin and the secret store and
//! nothing else: they are not logged, audited or echoed, and if a plugin puts one
//! into a step's text anyway, it is replaced before the step is shown.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard as StdGuard};
use std::time::Duration;

use axum::http::StatusCode;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use swifty_plugin_api::{
    DeviceClass, DeviceDecl, Field, FieldType, Manifest, Plugin, Scalar, SetupContext, SetupFlow, SetupInput,
    SetupResult, SetupStep,
};
use tokio::sync::{Mutex, MutexGuard};

use crate::actions::Actor;
use crate::app::App;
use crate::audit::Event as AuditEvent;
use crate::crypto::random_id;
use crate::error::ApiError;
use crate::events::Event;
use crate::inventory;
use crate::runtime::index::assign_target;
use crate::runtime::{contained, InstanceSpec, Origin};
use crate::secrets::{is_instance_id, is_slot_name};

/// How many setups may be open at once, across the hub.
pub const MAX_OPEN: usize = 4;
/// How long a setup nobody touches stays open.
pub const IDLE_MS: i64 = 15 * 60_000;
/// The audit action of what a setup did.
pub const SETUP_ACTION: &str = "plugin.setup";
/// The longest one step of a plugin's flow may take, readings of every chosen device included.
const FLOW_DEADLINE: Duration = Duration::from_secs(120);
/// The bounds of how often a `wait` step is polled, whatever the plugin asks.
const POLL_MIN_MS: u64 = 250;
const POLL_MAX_MS: u64 = 60_000;
/// The longest text a field may hold when its kind sets no limit (a host, a URL).
const MAX_PLAIN: usize = 2048;
/// The longest secret a field may hold, as the secret store takes it.
const MAX_SECRET_BYTES: usize = 8192;
/// The longest summary of a finished setup that is kept.
const MAX_SUMMARY: usize = 300;
/// What stands in for a secret a plugin put into a step's text.
const REDACTED: &str = "<redacted>";

const PLUGIN_FAILED: &str = "The plugin's setup failed. Nothing was stored; start the setup again.";
const PLUGIN_SLOW: &str = "The plugin's setup did not answer in time. Nothing was stored; start the setup again.";
const WAITED_TOO_LONG: &str = "Nothing happened in time. Do what the step asks, then try again.";

/// What a setup is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    /// A new instance, written when the setup is done.
    Add,
    /// A stored instance, set up again from its stored config.
    Reconfigure,
}

/// Who opens a setup, and how: what its audit rows say.
#[derive(Debug, Clone)]
pub struct Opener {
    pub actor: Actor,
    /// `panel` or `palette` from the app, `cli` from the command line.
    pub channel: String,
    /// How opening it was approved: `passkey` after a step-up, `local` for the command line.
    pub step_up: String,
}

/// A setup as its owner sees it.
#[derive(Debug, Clone)]
pub struct SetupView {
    pub setup_id: String,
    pub plugin: String,
    pub plugin_name: String,
    pub instance: String,
    pub purpose: Purpose,
    /// The step to show. A finished setup's `Done` holds its summary and nothing else.
    pub step: SetupStep,
    /// With a `Failed` step that can be tried again: the step to show again, to which
    /// the same kind of input is sent.
    pub retry: Option<SetupStep>,
    /// Done, failed for good, or abandoned: no input is taken any more.
    pub finished: bool,
    /// When it is forgotten unless it is touched again.
    pub expires_at: i64,
}

impl SetupView {
    /// What the API answers with: the step as the plugin API serialises it, except
    /// that `done` carries its summary only.
    pub fn to_json(&self) -> Value {
        json!({
            "setup_id": self.setup_id,
            "plugin": self.plugin,
            "plugin_name": self.plugin_name,
            "instance": self.instance,
            "purpose": self.purpose,
            "step": step_json(&self.step),
            "retry": self.retry.as_ref().map(step_json),
            "finished": self.finished,
            "expires_at": self.expires_at,
        })
    }
}

/// A step as a client is sent it.
pub fn step_json(step: &SetupStep) -> Value {
    match step {
        SetupStep::Done(result) => json!({ "step": "done", "summary": result.summary }),
        other => serde_json::to_value(other).unwrap_or(Value::Null),
    }
}

/// What a finished setup's `Done` keeps: the summary.
fn summary_only(summary: String) -> SetupStep {
    SetupStep::Done(SetupResult { config: Value::Null, secrets: Vec::new(), adopt: Vec::new(), summary })
}

fn failed_for_good(reason: impl Into<String>) -> SetupStep {
    SetupStep::Failed { reason: reason.into(), retry: false }
}

// ----- errors

fn unknown_setup() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "unknown_setup",
        "There is no such setup here. It may have been left alone too long: start it again.",
    )
}

fn wrong_session() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "wrong_session", "This setup was started in another session.")
}

fn setup_finished() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "setup_finished",
        "This setup has finished. Start a new one to change anything.",
    )
}

fn invalid_input(message: impl Into<String>, field: Option<&str>) -> ApiError {
    let error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_input", message);
    match field {
        Some(field) => error.with_extra(json!({ "field": field })),
        None => error,
    }
}

// ----- sessions

/// The shown state of a setup: what a client gets without waiting for the plugin.
struct Shown {
    step: SetupStep,
    retry: Option<SetupStep>,
}

/// The plugin's side of a setup, used by one caller at a time.
struct FlowState {
    /// `None` once the setup has finished, or when the plugin failed.
    flow: Option<Box<dyn SetupFlow>>,
    ctx: SetupContext,
    /// The last step that asked for something, offered again after a failure.
    last_asking: Option<SetupStep>,
    /// When the current wait began, by the hub's clock.
    wait_since: Option<i64>,
    /// What was typed into secret fields, to keep it out of every step shown.
    typed: Vec<String>,
}

struct Entry {
    id: String,
    opener: Opener,
    plugin: &'static dyn Plugin,
    instance: String,
    purpose: Purpose,
    touched: AtomicI64,
    finished: AtomicBool,
    /// Set first when it is abandoned, so a step on its way cannot apply anything.
    abandoned: AtomicBool,
    /// Moves whenever the step changes, so a poll that was waiting knows it is stale.
    generation: AtomicU64,
    shown: StdMutex<Shown>,
    state: Mutex<FlowState>,
}

impl Entry {
    fn shown(&self) -> StdGuard<'_, Shown> {
        self.shown.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn is_finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    fn view(&self) -> SetupView {
        let shown = self.shown();
        SetupView {
            setup_id: self.id.clone(),
            plugin: self.plugin.manifest().id.to_string(),
            plugin_name: self.plugin.manifest().name.to_string(),
            instance: self.instance.clone(),
            purpose: self.purpose,
            step: shown.step.clone(),
            retry: shown.retry.clone(),
            finished: self.is_finished(),
            expires_at: self.touched.load(Ordering::SeqCst) + IDLE_MS,
        }
    }

    /// Ends it: no input is taken any more, and a waiting poll stops.
    fn finish(&self) {
        self.finished.store(true, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    fn audit_event(&self, app: &App, result: &str, params: Value) -> AuditEvent {
        let actor = &self.opener.actor;
        AuditEvent {
            ts: app.now(),
            action: SETUP_ACTION.to_string(),
            target: self.instance.clone(),
            params,
            credential_ref: actor.credential_id.clone(),
            session_ref: actor.session_ref.clone(),
            user_ref: actor.user_id.clone(),
            device: actor.device.clone(),
            channel: self.opener.channel.clone(),
            step_up: self.opener.step_up.clone(),
            result: result.to_string(),
            rtt_ms: None,
            run_id: None,
        }
    }

    /// One audit row about this setup, outside any other transaction.
    fn audit(&self, app: &App, result: &str) {
        let event =
            self.audit_event(app, result, json!({ "instance": self.instance, "plugin": self.plugin.manifest().id }));
        if let Err(error) = app.audit(event) {
            tracing::error!(setup = %self.id, message = %error.message, "could not record what a setup did");
        }
    }
}

/// Every open setup. Lives in the app; the CLI's app has its own.
#[derive(Default)]
pub struct SetupSessions {
    open: StdMutex<BTreeMap<String, Arc<Entry>>>,
}

impl SetupSessions {
    fn open(&self) -> StdGuard<'_, BTreeMap<String, Arc<Entry>>> {
        self.open.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Forgets every setup nobody touched for [`IDLE_MS`]. A waiting poll stops at its next turn.
    fn prune(&self, app: &App) {
        let now = app.now();
        self.open().retain(|_, entry| {
            let keep = now - entry.touched.load(Ordering::SeqCst) <= IDLE_MS;
            if !keep {
                entry.abandoned.store(true, Ordering::SeqCst);
                entry.finish();
            }
            keep
        });
    }

    /// How many setups are open and not finished.
    pub fn open_count(&self, app: &App) -> usize {
        self.prune(app);
        self.open().values().filter(|entry| !entry.is_finished()).count()
    }

    /// The setup open for this instance, if one is.
    pub fn open_for(&self, app: &App, instance: &str) -> Option<String> {
        self.prune(app);
        self.open()
            .values()
            .find(|entry| entry.instance == instance && !entry.is_finished())
            .map(|entry| entry.id.clone())
    }

    /// Why a setup of this instance could not be opened now, if it could not: too
    /// many are open, or one is open for the same instance.
    pub fn refusal(&self, app: &App, instance: Option<&str>) -> Option<ApiError> {
        if let Some(open) = instance.and_then(|instance| self.open_for(app, instance)) {
            return Some(
                ApiError::new(
                    StatusCode::CONFLICT,
                    "setup_open",
                    "This integration is being set up already. Finish or abandon that setup first.",
                )
                .with_extra(json!({ "setup_id": open })),
            );
        }
        if self.open_count(app) >= MAX_OPEN {
            return Some(ApiError::new(
                StatusCode::CONFLICT,
                "setup_busy",
                format!("{MAX_OPEN} setups are open already. Finish or abandon one first."),
            ));
        }
        None
    }

    /// Opens the setup of a new instance of `plugin`, under `instance` or, when that
    /// is left out, the plugin's id or the next free one. Returns its first step.
    pub async fn begin(
        &self,
        app: &App,
        opener: &Opener,
        plugin: &str,
        instance: Option<&str>,
    ) -> Result<SetupView, ApiError> {
        self.prune(app);
        let plugin = addable(app, plugin)?;
        let instance = match instance {
            Some(given) => {
                check_new_instance_id(app, given)?;
                given.to_string()
            }
            None => self.free_instance_id(app, plugin.manifest())?,
        };
        let ctx = SetupContext {
            instance_id: instance.clone(),
            grant: app.registry.grant(),
            timeouts: app.registry.timeouts_for(plugin),
            // Whatever a removed instance of the same id left is not this one's.
            secrets_present: BTreeSet::new(),
        };
        self.start(app, opener, plugin, instance, Purpose::Add, ctx, None).await
    }

    /// Opens the setup of a stored instance again, from its stored config.
    pub async fn begin_again(&self, app: &App, opener: &Opener, instance: &str) -> Result<SetupView, ApiError> {
        self.prune(app);
        let spec = app
            .store
            .with(inventory::instance_specs)?
            .into_iter()
            .find(|spec| spec.id == instance)
            .ok_or_else(unknown_instance)?;
        let plugin = app.registry.plugin(&spec.plugin).ok_or_else(|| plugin_missing(&spec.plugin))?;
        let existing = current_config(plugin, &spec);
        let ctx = SetupContext {
            instance_id: spec.id.clone(),
            grant: app.registry.grant(),
            timeouts: app.registry.timeouts_for(plugin),
            secrets_present: app.registry.secret_store().slots(&spec.id).unwrap_or_default(),
        };
        self.start(app, opener, plugin, spec.id, Purpose::Reconfigure, ctx, Some(existing)).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn start(
        &self,
        app: &App,
        opener: &Opener,
        plugin: &'static dyn Plugin,
        instance: String,
        purpose: Purpose,
        ctx: SetupContext,
        existing: Option<Value>,
    ) -> Result<SetupView, ApiError> {
        let Some(flow) = contained(|| plugin.setup()).ok().flatten() else {
            return Err(no_setup(plugin));
        };
        let entry = Arc::new(Entry {
            id: random_id(),
            opener: opener.clone(),
            plugin,
            instance,
            purpose,
            touched: AtomicI64::new(app.now()),
            finished: AtomicBool::new(false),
            abandoned: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            shown: StdMutex::new(Shown { step: failed_for_good("The setup is starting."), retry: None }),
            state: Mutex::new(FlowState {
                flow: Some(flow),
                ctx,
                last_asking: None,
                wait_since: None,
                typed: Vec::new(),
            }),
        });
        {
            // Checked and taken under one lock, so two at once cannot both get the last place.
            let mut open = self.open();
            let busy = open.values().filter(|other| !other.is_finished());
            if let Some(other) = busy.clone().find(|other| other.instance == entry.instance) {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "setup_open",
                    "This integration is being set up already. Finish or abandon that setup first.",
                )
                .with_extra(json!({ "setup_id": other.id })));
            }
            if busy.count() >= MAX_OPEN {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "setup_busy",
                    format!("{MAX_OPEN} setups are open already. Finish or abandon one first."),
                ));
            }
            open.insert(entry.id.clone(), entry.clone());
        }
        entry.audit(app, "opened");
        let (task_app, task_entry) = (app.clone(), entry.clone());
        // In a task of its own, so the first step is taken even if the caller goes away.
        tokio::spawn(async move { drive(&task_app, &task_entry, Call::Begin(existing)).await })
            .await
            .map_err(|_| ApiError::internal("a setup did not start"))?
    }

    fn entry_for(&self, app: &App, by: &Actor, id: &str) -> Result<Arc<Entry>, ApiError> {
        self.prune(app);
        let entry = self.open().get(id).cloned().ok_or_else(unknown_setup)?;
        if entry.opener.actor.session_ref != by.session_ref {
            // Recorded under the instance it is for: the id itself was the client's.
            let mut event = entry.audit_event(app, "refused: wrong_session", Value::Null);
            event.credential_ref = by.credential_id.clone();
            event.session_ref = by.session_ref.clone();
            event.user_ref = by.user_id.clone();
            event.device = by.device.clone();
            app.audit(event)?;
            return Err(wrong_session());
        }
        entry.touched.store(app.now(), Ordering::SeqCst);
        Ok(entry)
    }

    /// The setup as it stands, for the session that opened it.
    pub fn view(&self, app: &App, by: &Actor, id: &str) -> Result<SetupView, ApiError> {
        Ok(self.entry_for(app, by, id)?.view())
    }

    /// Sends an input to the step the setup shows, and returns the next step.
    pub async fn advance(&self, app: &App, by: &Actor, id: &str, input: SetupInput) -> Result<SetupView, ApiError> {
        let entry = self.entry_for(app, by, id)?;
        if entry.is_finished() {
            return Err(setup_finished());
        }
        let task_app = app.clone();
        // In a task of its own: a step that reaches `done` is applied even if the client goes away.
        tokio::spawn(async move { drive(&task_app, &entry, Call::Advance(input)).await })
            .await
            .map_err(|_| ApiError::internal("a setup step did not finish"))?
    }

    /// Ends a setup. One that was still open leaves nothing behind: nothing was stored
    /// yet, and what it held goes with it. Says whether it was still open.
    pub async fn abandon(&self, app: &App, by: &Actor, id: &str) -> Result<bool, ApiError> {
        let entry = self.entry_for(app, by, id)?;
        let was_open = Self::close(app, &entry).await;
        self.open().remove(id);
        Ok(was_open)
    }

    /// Ends every setup of an instance: when the instance itself goes.
    pub async fn abandon_instance(&self, app: &App, instance: &str) {
        let entries: Vec<Arc<Entry>> =
            self.open().values().filter(|entry| entry.instance == instance).cloned().collect();
        for entry in entries {
            Self::close(app, &entry).await;
            self.open().remove(&entry.id);
        }
    }

    async fn close(app: &App, entry: &Entry) -> bool {
        entry.abandoned.store(true, Ordering::SeqCst);
        // Waits for a step on its way: it sees `abandoned` and applies nothing.
        let mut state = entry.state.lock().await;
        let was_open = !entry.is_finished();
        state.flow = None;
        state.typed.clear();
        entry.finish();
        if was_open {
            entry.audit(app, "abandoned");
        }
        was_open
    }

    /// The first free instance id for a plugin: its id made to fit, then with 2, 3, ...
    fn free_instance_id(&self, app: &App, manifest: &Manifest) -> Result<String, ApiError> {
        let mut base: String = manifest.id.chars().filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit()).collect();
        if !base.starts_with(|c: char| c.is_ascii_lowercase()) {
            base.insert(0, 'i');
        }
        base.truncate(16);
        let taken = |id: &str| -> Result<bool, ApiError> {
            Ok(id == "new"
                || app.store.with(|conn| inventory::instance_exists(conn, id))?
                || self.open().values().any(|entry| entry.instance == id && !entry.is_finished()))
        };
        if !taken(&base)? {
            return Ok(base);
        }
        for n in 2..1000u32 {
            let suffix = n.to_string();
            let mut id = base.clone();
            id.truncate(16 - suffix.len());
            id.push_str(&suffix);
            if !taken(&id)? {
                return Ok(id);
            }
        }
        Err(ApiError::new(StatusCode::CONFLICT, "instance_exists", "Give the new integration an id of its own."))
    }
}

// ----- checks before a setup opens

fn unknown_instance() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "unknown_instance", "There is no such integration here.")
}

/// Why an instance of a plugin this build lacks cannot be set up.
pub fn plugin_missing(plugin: &str) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "plugin_not_in_build",
        format!("This build of Swifty Home has no plugin {plugin}, so this integration cannot be set up here."),
    )
}

/// Why a plugin with no setup flow cannot be added this way.
pub fn no_setup(plugin: &dyn Plugin) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "no_setup",
        format!(
            "{} has no guided setup. Its settings can be written with `swifty config import`.",
            plugin.manifest().name
        ),
    )
}

/// The plugin, if a new instance of it can be added.
pub fn addable(app: &App, plugin: &str) -> Result<&'static dyn Plugin, ApiError> {
    let found = app.registry.plugin(plugin).ok_or_else(|| {
        ApiError::new(StatusCode::NOT_FOUND, "unknown_plugin", "This build of Swifty Home has no such plugin.")
    })?;
    if !found.manifest().multi_instance {
        let existing = app.store.with(inventory::instance_specs)?.into_iter().find(|spec| spec.plugin == plugin);
        if let Some(existing) = existing {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "instance_exists",
                format!(
                    "{} is set up already as {}, and one is all it takes. Set that one up again instead.",
                    found.manifest().name,
                    existing.id
                ),
            )
            .with_extra(json!({ "instance": existing.id })));
        }
    }
    Ok(found)
}

/// Checks an id given for a new instance.
pub fn check_new_instance_id(app: &App, id: &str) -> Result<(), ApiError> {
    if !is_instance_id(id) || id == "new" {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_params",
            "instance must be a lower-case letter, then up to 15 lower-case letters and digits, and not \"new\".",
        ));
    }
    if app.store.with(|conn| inventory::instance_exists(conn, id))? {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "instance_exists",
            format!("There is an integration called {id} already. Choose another id, or set that one up again."),
        )
        .with_extra(json!({ "instance": id })));
    }
    Ok(())
}

/// The stored config brought up to the plugin's config version, as setting it up
/// again starts from.
fn current_config(plugin: &dyn Plugin, spec: &InstanceSpec) -> Value {
    if spec.config_version >= plugin.manifest().config_version {
        return spec.config.clone();
    }
    match contained(|| plugin.migrate(spec.config_version, spec.config.clone())) {
        Ok(Ok(config)) => config,
        _ => spec.config.clone(),
    }
}

// ----- driving a flow

enum Call {
    Begin(Option<Value>),
    Advance(SetupInput),
}

/// Runs one call of a flow in a task of its own, under a deadline: a panic or a hang
/// is that setup's failure and nothing else's. The flow comes back unless it failed.
async fn call_flow(
    mut flow: Box<dyn SetupFlow>,
    ctx: SetupContext,
    call: Call,
) -> (Option<Box<dyn SetupFlow>>, SetupStep) {
    let task = tokio::spawn(async move {
        let step = match call {
            Call::Begin(existing) => flow.begin(&ctx, existing.as_ref()).await,
            Call::Advance(input) => flow.advance(&ctx, input).await,
        };
        (flow, step)
    });
    let abort = task.abort_handle();
    match tokio::time::timeout(FLOW_DEADLINE, task).await {
        Ok(Ok((flow, step))) => (Some(flow), step),
        Ok(Err(_)) => {
            tracing::error!("a plugin's setup flow panicked");
            (None, failed_for_good(PLUGIN_FAILED))
        }
        Err(_) => {
            abort.abort();
            tracing::warn!("a plugin's setup flow did not answer in time");
            (None, failed_for_good(PLUGIN_SLOW))
        }
    }
}

fn copy_of(ctx: &SetupContext) -> SetupContext {
    SetupContext {
        instance_id: ctx.instance_id.clone(),
        grant: ctx.grant,
        timeouts: ctx.timeouts,
        secrets_present: ctx.secrets_present.clone(),
    }
}

/// Takes one step of a setup: checks the input against the step shown, asks the
/// plugin, and settles what it answered.
async fn drive(app: &App, entry: &Arc<Entry>, call: Call) -> Result<SetupView, ApiError> {
    let mut state = entry.state.lock().await;
    if entry.is_finished() {
        return match call {
            Call::Begin(_) => Ok(entry.view()),
            Call::Advance(_) => Err(setup_finished()),
        };
    }
    let call = match call {
        Call::Advance(input) => {
            let (input, typed) = {
                let shown = entry.shown();
                checked_input(&shown, input)?
            };
            state.typed.extend(typed);
            Call::Advance(input)
        }
        begin => begin,
    };
    take_step(app, entry, &mut state, call).await;
    Ok(entry.view())
}

async fn take_step(app: &App, entry: &Arc<Entry>, state: &mut MutexGuard<'_, FlowState>, call: Call) {
    let step = match state.flow.take() {
        Some(flow) => {
            let (flow, step) = call_flow(flow, copy_of(&state.ctx), call).await;
            state.flow = flow;
            step
        }
        None => failed_for_good(PLUGIN_FAILED),
    };
    settle(app, entry, state, step);
}

/// Shows what the plugin answered, and acts on it: a wait is polled, `done` is
/// applied, a failure for good ends the setup.
fn settle(app: &App, entry: &Arc<Entry>, state: &mut FlowState, step: SetupStep) {
    let step = cleaned(step, &state.typed);
    match step {
        SetupStep::Done(result) => {
            state.flow = None;
            let abandoned = entry.abandoned.load(Ordering::SeqCst);
            let shown = if abandoned {
                failed_for_good("This setup was abandoned. Nothing was stored.")
            } else {
                match apply(app, entry, result) {
                    Ok(summary) => summary_only(summary),
                    Err(reason) => {
                        entry.audit(app, "failed: not_stored");
                        failed_for_good(scrubbed(&reason, &state.typed))
                    }
                }
            };
            state.typed.clear();
            *entry.shown() = Shown { step: shown, retry: None };
            entry.finish();
        }
        SetupStep::Failed { reason, retry: false } => {
            state.flow = None;
            state.typed.clear();
            *entry.shown() = Shown { step: failed_for_good(reason), retry: None };
            entry.finish();
            entry.audit(app, "failed");
        }
        SetupStep::Failed { reason, retry: true } => {
            let retry = state.last_asking.clone();
            *entry.shown() = Shown { step: SetupStep::Failed { reason, retry: true }, retry };
            state.wait_since = None;
            entry.generation.fetch_add(1, Ordering::SeqCst);
        }
        SetupStep::Wait { poll_ms, timeout_s, .. } => {
            let still_waiting = matches!(entry.shown().step, SetupStep::Wait { .. });
            state.last_asking = Some(step.clone());
            *entry.shown() = Shown { step, retry: None };
            if !still_waiting {
                state.wait_since = Some(app.now());
                let generation = entry.generation.fetch_add(1, Ordering::SeqCst) + 1;
                poll_while_waiting(app.clone(), entry.clone(), generation, poll_ms, timeout_s);
            }
        }
        asking => {
            state.last_asking = Some(asking.clone());
            state.wait_since = None;
            *entry.shown() = Shown { step: asking, retry: None };
            entry.generation.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// Polls a `wait` step every `poll_ms` until it moves on, the setup ends, or its
/// `timeout_s` runs out by the hub's clock. Tells the owning session each time it
/// moves on.
fn poll_while_waiting(app: App, entry: Arc<Entry>, generation: u64, poll_ms: u32, timeout_s: u32) {
    let every = Duration::from_millis(u64::from(poll_ms).clamp(POLL_MIN_MS, POLL_MAX_MS));
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(every).await;
            if entry.is_finished() || entry.generation() != generation {
                return;
            }
            let mut state = entry.state.lock().await;
            if entry.is_finished() || entry.generation() != generation {
                return;
            }
            let waited = app.now() - state.wait_since.unwrap_or_else(|| app.now());
            if waited >= i64::from(timeout_s) * 1000 {
                let retry = state.last_asking.clone();
                *entry.shown() =
                    Shown { step: SetupStep::Failed { reason: WAITED_TOO_LONG.to_string(), retry: true }, retry };
                state.wait_since = None;
                entry.generation.fetch_add(1, Ordering::SeqCst);
                drop(state);
                announce(&app, &entry);
                return;
            }
            take_step(&app, &entry, &mut state, Call::Advance(SetupInput::Poll)).await;
            drop(state);
            if entry.is_finished() || entry.generation() != generation {
                announce(&app, &entry);
                return;
            }
        }
    });
}

/// Tells the session that owns a setup that its step moved on.
fn announce(app: &App, entry: &Entry) {
    app.events
        .publish(Event::Setup { setup_id: entry.id.clone(), session_ref: entry.opener.actor.session_ref.clone() });
}

// ----- what goes out

/// A step as it may be shown: no prefill in a secret field, nothing typed into a
/// secret field anywhere in its text, and a summary of a sensible length.
fn cleaned(step: SetupStep, typed: &[String]) -> SetupStep {
    let fields = |fields: Vec<Field>| -> Vec<Field> {
        fields
            .into_iter()
            .map(|mut field| {
                if matches!(field.kind, FieldType::Secret) {
                    field.value = None;
                }
                field.label = scrubbed(&field.label, typed);
                field.help = scrubbed(&field.help, typed);
                if let Some(Scalar::Text(text)) = &field.value {
                    field.value = Some(Scalar::Text(scrubbed(text, typed)));
                }
                field
            })
            .collect()
    };
    let clean = |text: String| scrubbed(&text, typed);
    match step {
        SetupStep::Form { title, text, fields: given } => {
            SetupStep::Form { title: clean(title), text: clean(text), fields: fields(given) }
        }
        SetupStep::External { title, text, link, fields: given } => {
            SetupStep::External { title: clean(title), text: clean(text), link, fields: fields(given) }
        }
        SetupStep::Wait { title, text, poll_ms, timeout_s } => {
            SetupStep::Wait { title: clean(title), text: clean(text), poll_ms, timeout_s }
        }
        SetupStep::Choose { title, text, options, multiple, min } => SetupStep::Choose {
            title: clean(title),
            text: clean(text),
            options: options
                .into_iter()
                .map(|mut choice| {
                    choice.label = clean(choice.label);
                    choice.detail = clean(choice.detail);
                    choice
                })
                .collect(),
            multiple,
            min,
        },
        SetupStep::Failed { reason, retry } => SetupStep::Failed { reason: clean(reason), retry },
        SetupStep::Done(mut result) => {
            let summary: String = clean(result.summary).chars().filter(|c| !c.is_control()).take(MAX_SUMMARY).collect();
            result.summary = if summary.trim().is_empty() { "Set up.".to_string() } else { summary };
            SetupStep::Done(result)
        }
    }
}

/// `text` with every secret typed in this setup replaced. Very short values are
/// left, as they would match ordinary words.
fn scrubbed(text: &str, typed: &[String]) -> String {
    typed.iter().filter(|value| value.chars().count() >= 4).fold(text.to_string(), |text, value| {
        if text.contains(value.as_str()) {
            text.replace(value.as_str(), REDACTED)
        } else {
            text
        }
    })
}

// ----- what comes in

/// Checks an input against the step it answers, and returns it with numbers and
/// truth values in their own types, plus what was typed into secret fields.
fn checked_input(shown: &Shown, input: SetupInput) -> Result<(SetupInput, Vec<String>), ApiError> {
    let answering = match &shown.step {
        SetupStep::Done(_) | SetupStep::Failed { retry: false, .. } => return Err(setup_finished()),
        SetupStep::Failed { retry: true, .. } => shown.retry.as_ref(),
        step => Some(step),
    };
    if matches!(input, SetupInput::Back) {
        return Ok((input, Vec::new()));
    }
    let Some(step) = answering else {
        return Err(invalid_input("This step can only go back.", None));
    };
    match (step, input) {
        (SetupStep::Form { fields, .. } | SetupStep::External { fields, .. }, SetupInput::Values { values }) => {
            let (values, typed) = checked_values(fields, values)?;
            Ok((SetupInput::Values { values }, typed))
        }
        (SetupStep::Choose { options, multiple, min, .. }, SetupInput::Chosen { ids }) => {
            let mut chosen: Vec<String> = Vec::new();
            for id in ids {
                if !options.iter().any(|option| option.id == id) {
                    return Err(invalid_input("That is not one of the choices offered.", None));
                }
                if !chosen.contains(&id) {
                    chosen.push(id);
                }
            }
            if chosen.len() < *min as usize {
                return Err(invalid_input(
                    if *min == 1 { "Pick at least one.".to_string() } else { format!("Pick at least {min}.") },
                    None,
                ));
            }
            if !multiple && chosen.len() > 1 {
                return Err(invalid_input("Pick one.", None));
            }
            Ok((SetupInput::Chosen { ids: chosen }, Vec::new()))
        }
        (SetupStep::Wait { .. }, SetupInput::Poll) => Ok((SetupInput::Poll, Vec::new())),
        (step, _) => {
            let expected = match step {
                SetupStep::Form { .. } | SetupStep::External { .. } => "the form's values",
                SetupStep::Choose { .. } => "what was chosen",
                SetupStep::Wait { .. } => "a poll",
                _ => "nothing but going back",
            };
            Err(invalid_input(format!("This step takes {expected}, or going back."), None))
        }
    }
}

fn checked_values(
    fields: &[Field],
    values: BTreeMap<String, Scalar>,
) -> Result<(BTreeMap<String, Scalar>, Vec<String>), ApiError> {
    if let Some(key) = values.keys().find(|key| !fields.iter().any(|field| &field.key == *key)) {
        return Err(invalid_input("The form has no such field.", Some(key)));
    }
    let mut out = BTreeMap::new();
    let mut typed = Vec::new();
    for field in fields {
        let value = match values.get(&field.key) {
            None => None,
            Some(value) => checked_value(field, value)?,
        };
        // A required field left empty is not given at all.
        let value =
            value.filter(|value| !(field.required && matches!(value, Scalar::Text(text) if text.trim().is_empty())));
        match value {
            None if field.required => {
                return Err(invalid_input(format!("{} is required.", field.label), Some(&field.key)));
            }
            None => {}
            Some(value) => {
                if let (FieldType::Secret, Scalar::Text(text)) = (&field.kind, &value) {
                    typed.push(text.clone());
                }
                out.insert(field.key.clone(), value);
            }
        }
    }
    Ok((out, typed))
}

/// One field's value, in its own type. `None` for an empty one where emptiness
/// means "not given"; an empty text field stays, so "leave empty to keep" works.
fn checked_value(field: &Field, value: &Scalar) -> Result<Option<Scalar>, ApiError> {
    let label = field.label.as_str();
    let wrong = |what: String| Err(invalid_input(format!("{label} {what}"), Some(&field.key)));
    let one_line = |text: &str, max: usize| text.chars().count() <= max && !text.chars().any(char::is_control);
    let number = |value: &Scalar| -> Option<Option<i64>> {
        match value {
            Scalar::Int(number) => Some(Some(*number)),
            Scalar::Text(text) if text.trim().is_empty() => Some(None),
            Scalar::Text(text) => text.trim().parse().ok().map(Some),
            Scalar::Bool(_) => None,
        }
    };
    match (&field.kind, value) {
        (FieldType::Text { max_len }, Scalar::Text(text)) => {
            if !one_line(text, *max_len) {
                return wrong(format!("must be at most {max_len} characters, on one line."));
            }
            Ok(Some(value.clone()))
        }
        (FieldType::Host | FieldType::Url | FieldType::HostPort | FieldType::Mac, Scalar::Text(text)) => {
            if !one_line(text, MAX_PLAIN) {
                return wrong("must be on one line.".to_string());
            }
            Ok(Some(Scalar::Text(text.trim().to_string())))
        }
        (FieldType::Secret, Scalar::Text(text)) => {
            if text.len() > MAX_SECRET_BYTES || text.chars().any(char::is_control) {
                return wrong("must be on one line, and not that long.".to_string());
            }
            Ok(Some(value.clone()))
        }
        (FieldType::Multiline { max_len }, Scalar::Text(text)) => {
            let allowed = |c: char| !c.is_control() || matches!(c, '\n' | '\r' | '\t');
            if text.chars().count() > *max_len || !text.chars().all(allowed) {
                return wrong(format!("must be at most {max_len} characters."));
            }
            Ok(Some(value.clone()))
        }
        (FieldType::Port, _) => match number(value) {
            Some(None) => Ok(None),
            Some(Some(port)) if (1..=65535).contains(&port) => Ok(Some(Scalar::Int(port))),
            _ => wrong("must be a port, from 1 to 65535.".to_string()),
        },
        (FieldType::Int { min, max }, _) => match number(value) {
            Some(None) => Ok(None),
            Some(Some(n)) if (*min..=*max).contains(&n) => Ok(Some(Scalar::Int(n))),
            _ => wrong(format!("must be a whole number from {min} to {max}.")),
        },
        (FieldType::Bool, Scalar::Bool(_)) => Ok(Some(value.clone())),
        (FieldType::Bool, Scalar::Text(text)) => match text.as_str() {
            "true" => Ok(Some(Scalar::Bool(true))),
            "false" => Ok(Some(Scalar::Bool(false))),
            _ => wrong("must be true or false.".to_string()),
        },
        (FieldType::Choice { options }, Scalar::Text(text)) => {
            if text.is_empty() {
                Ok(None)
            } else if options.contains(text) {
                Ok(Some(value.clone()))
            } else {
                wrong(format!("must be one of: {}.", options.join(", ")))
            }
        }
        _ => wrong("is not that kind of value.".to_string()),
    }
}

// ----- applying a finished setup

/// Whether the plugin declares this slot: as it is, or as a row of a `name/{id}` slot.
fn declares_slot(manifest: &Manifest, slot: &str) -> bool {
    is_slot_name(slot)
        && manifest.secrets.iter().any(|declared| match declared.slot.split_once('/') {
            Some((base, _)) => slot.split_once('/').is_some_and(|(given, row)| given == base && !row.is_empty()),
            None => declared.slot == slot,
        })
}

/// `[a-z0-9][a-z0-9_-]{0,63}`: what a local id may be.
pub fn is_local_id(text: &str) -> bool {
    let bytes = text.as_bytes();
    (1..=64).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

/// A target no stored device has: the plugin's hint, or `<instance>-<local id>` and a
/// number. Refused rather than made up when the local id is too long to build one.
pub fn free_target(conn: &Connection, instance: &str, decl: &DeviceDecl) -> Result<String, ApiError> {
    let failed = RefCell::new(None);
    let taken = |target: &str| match inventory::device_exists(conn, target) {
        Ok(exists) => exists,
        Err(error) => {
            failed.borrow_mut().get_or_insert(error);
            true
        }
    };
    let hinted = decl.target_hint.as_deref().is_some_and(|hint| crate::runtime::index::is_free_target(&taken, hint));
    if !hinted && instance.len() + 1 + decl.local_id.len() > 58 {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_device",
            "The device's id is too long to give it a name here.",
        ));
    }
    let target = assign_target(&taken, instance, decl);
    match failed.into_inner() {
        Some(error) => Err(error.into()),
        None => Ok(target),
    }
}

/// Stores a device as adopted and active, under the target the hub gives it. A
/// light keeps what a discovery would have stored with it, so the light routes know
/// it. One stored already is left as it is, except that one waiting for approval is
/// approved: whoever adopts it here has passed the same step-up. Returns its target.
pub fn adopt_device(
    conn: &Connection,
    now: i64,
    instance: &str,
    decl: &DeviceDecl,
    by: &str,
) -> Result<String, ApiError> {
    let decl = if decl.class == DeviceClass::Light { inventory::light_candidate(decl) } else { decl.clone() };
    let stored: Option<(String, String)> = conn
        .query_row(
            "SELECT target, status FROM devices WHERE instance_id = ?1 AND local_id = ?2",
            params![instance, decl.local_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((target, status)) = stored {
        if status == "pending" {
            conn.execute(
                "UPDATE devices SET status = 'active', adopted_at = ?1, adopted_by = ?2 WHERE target = ?3",
                params![now, by, target],
            )?;
        }
        return Ok(target);
    }
    let target = free_target(conn, instance, &decl)?;
    let name: String = decl.name.chars().filter(|c| !c.is_control()).take(64).collect();
    let name = if name.trim().is_empty() { decl.local_id.clone() } else { name.trim().to_string() };
    let class = serde_json::to_value(decl.class)?;
    conn.execute(
        "INSERT INTO devices (target, instance_id, local_id, class, name, attrs, origin, status,
                              first_seen, adopted_at, adopted_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'adopted', 'active', ?7, ?7, ?8)",
        params![
            target,
            instance,
            decl.local_id,
            class.as_str().unwrap_or_default(),
            name,
            decl.attrs.to_string(),
            now,
            by
        ],
    )?;
    Ok(target)
}

/// Stores what a finished setup handed back: checks it, writes the secrets, then
/// in one transaction the instance, its devices, `instances_rev` and the audit row;
/// then connects the instance again. Returns the summary, or why nothing was stored.
fn apply(app: &App, entry: &Entry, result: SetupResult) -> Result<String, String> {
    let plugin = entry.plugin;
    let manifest = plugin.manifest();
    match contained(|| plugin.validate(&result.config)) {
        Err(_) => return Err(PLUGIN_FAILED.to_string()),
        Ok(Err(problem)) => {
            return Err(format!("The plugin's own checks refused the settings ({problem}). Nothing was stored."));
        }
        Ok(Ok(())) => {}
    }
    if let Some((slot, _)) = result.secrets.iter().find(|(slot, _)| !declares_slot(manifest, slot)) {
        return Err(format!(
            "The plugin handed back a credential for {slot}, a slot it does not declare. Nothing was stored."
        ));
    }
    if let Some(decl) =
        result.adopt.iter().find(|decl| !is_local_id(&decl.local_id) || !manifest.classes.contains(&decl.class))
    {
        let _ = decl;
        return Err("The plugin handed back a device it cannot have. Nothing was stored.".to_string());
    }
    let instance = entry.instance.as_str();
    let now = app.now();
    let store = app.registry.secret_store();
    if entry.purpose == Purpose::Add {
        match app.store.with(|conn| inventory::instance_exists(conn, instance)) {
            Ok(false) => {}
            Ok(true) => {
                return Err(format!("An integration called {instance} was added meanwhile. Nothing was stored."))
            }
            Err(error) => return Err(stored_nothing(&ApiError::from(error))),
        }
        // Whatever a removed instance of the same id left is not this one's.
        if let Err(error) = store.remove_instance(instance) {
            return Err(format!(
                "The secret store could not be cleared for it ({:?}). Nothing was stored.",
                error.kind()
            ));
        }
    }
    let mut written: Vec<String> = Vec::new();
    for (slot, value) in &result.secrets {
        if value.expose().is_empty() {
            continue;
        }
        if let Err(error) = store.write(instance, slot, value) {
            if entry.purpose == Purpose::Add {
                let _ = store.remove_instance(instance);
            }
            return Err(format!(
                "The credential {slot} could not be stored ({:?}). Nothing else was stored.",
                error.kind()
            ));
        }
        written.push(slot.clone());
    }
    let committed = app.store.tx(|tx| -> Result<(), ApiError> {
        store_instance(app, tx, entry, &result, now)?;
        let devices: i64 = tx.query_row(
            "SELECT COUNT(*) FROM devices WHERE instance_id = ?1 AND status = 'active'",
            params![instance],
            |row| row.get(0),
        )?;
        inventory::bump_instances_rev(tx)?;
        let params = json!({
            "instance": instance,
            "plugin": manifest.id,
            "version": manifest.version,
            "secrets": written,
            "devices": devices,
        });
        app.append_audit(tx, &entry.audit_event(app, "done", params))?;
        Ok(())
    });
    if let Err(error) = committed {
        if entry.purpose == Purpose::Add {
            let _ = store.remove_instance(instance);
        }
        return Err(stored_nothing(&error));
    }
    if let Err(error) = reload(app, instance) {
        tracing::error!(%instance, message = %error.message, "a set-up instance could not be loaded");
    }
    Ok(result.summary)
}

fn stored_nothing(error: &ApiError) -> String {
    tracing::error!(message = %error.message, "a finished setup could not be stored");
    "The settings could not be stored. Nothing was stored; try the setup again.".to_string()
}

/// The instance row, the devices it adopts and the devices its config names.
fn store_instance(app: &App, tx: &Connection, entry: &Entry, result: &SetupResult, now: i64) -> Result<(), ApiError> {
    let manifest = entry.plugin.manifest();
    let instance = entry.instance.as_str();
    let by = entry.opener.actor.user_id.as_str();
    let config = result.config.to_string();
    let enabled = match entry.purpose {
        Purpose::Add => {
            tx.execute(
                "INSERT INTO plugin_instances (id, plugin, config, config_version, enabled, created_at, created_by, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?5)",
                params![instance, manifest.id, config, manifest.config_version, now, by],
            )?;
            true
        }
        Purpose::Reconfigure => {
            // `updated_at` always moves forward: a hub in another process builds the
            // instance anew when it sees it move, since a secret may have changed in place.
            let changed = tx.execute(
                "UPDATE plugin_instances SET config = ?2, config_version = ?3, updated_at = MAX(updated_at + 1, ?4)
                 WHERE id = ?1 AND plugin = ?5",
                params![instance, config, manifest.config_version, now, manifest.id],
            )?;
            if changed != 1 {
                return Err(ApiError::new(StatusCode::CONFLICT, "unknown_instance", "The integration is gone."));
            }
            tx.query_row("SELECT enabled FROM plugin_instances WHERE id = ?1", params![instance], |row| {
                row.get::<_, i64>(0)
            })? != 0
        }
    };
    for decl in &result.adopt {
        adopt_device(tx, now, instance, decl, by)?;
    }
    let spec = InstanceSpec {
        id: instance.to_string(),
        plugin: manifest.id.to_string(),
        config: result.config.clone(),
        config_version: manifest.config_version,
        enabled,
        updated_at: now,
    };
    let adopted: Vec<_> = inventory::stored_devices(tx)?
        .into_iter()
        .filter(|device| device.instance == instance && device.origin == Origin::Adopted && !device.pending)
        .map(|device| device.record())
        .collect();
    if let Some(declared) = app.registry.declared(&spec, &adopted) {
        inventory::store_config_devices(tx, now, instance, &declared)?;
    }
    Ok(())
}

/// Loads every instance again, building this one anew: its secrets may have changed
/// in the same slots.
pub fn reload(app: &App, instance: &str) -> Result<(), ApiError> {
    let (specs, devices) = app.store.with(|conn| -> rusqlite::Result<_> {
        Ok((inventory::instance_specs(conn)?, inventory::stored_devices(conn)?))
    })?;
    app.registry.reload_instance(instance, specs, devices);
    Ok(())
}

#[cfg(test)]
mod tests {
    use swifty_plugin_api::Choice;

    use super::*;

    fn field(key: &str, kind: FieldType, required: bool) -> Field {
        Field { key: key.to_string(), label: key.to_uppercase(), help: String::new(), kind, required, value: None }
    }

    fn form(fields: Vec<Field>) -> Shown {
        Shown { step: SetupStep::Form { title: "t".into(), text: "x".into(), fields }, retry: None }
    }

    fn values(pairs: &[(&str, Scalar)]) -> SetupInput {
        SetupInput::Values { values: pairs.iter().map(|(key, value)| (key.to_string(), value.clone())).collect() }
    }

    fn text(value: &str) -> Scalar {
        Scalar::Text(value.to_string())
    }

    #[test]
    fn a_form_takes_only_its_own_fields_in_their_own_types() {
        let shown = form(vec![
            field("address", FieldType::Host, true),
            field("port", FieldType::Port, false),
            field("level", FieldType::Int { min: 1, max: 5 }, false),
            field("on", FieldType::Bool, false),
            field("region", FieldType::Choice { options: vec!["eu".into(), "us".into()] }, false),
            field("code", FieldType::Secret, false),
        ]);
        let given = values(&[
            ("address", text(" 192.0.2.10 ")),
            ("port", text("8080")),
            ("level", Scalar::Int(3)),
            ("on", text("true")),
            ("region", text("eu")),
            ("code", text("hunter-two")),
        ]);
        let (SetupInput::Values { values: checked }, typed) = checked_input(&shown, given).unwrap() else { panic!() };
        assert_eq!(checked["address"], text("192.0.2.10"));
        assert_eq!(checked["port"], Scalar::Int(8080));
        assert_eq!(checked["on"], Scalar::Bool(true));
        assert_eq!(typed, ["hunter-two"], "what was typed into a secret field is kept to scrub it");

        for (bad, field) in [
            (values(&[("address", text("192.0.2.10")), ("extra", text("x"))]), "extra"),
            (values(&[]), "address"),
            (values(&[("address", text(""))]), "address"),
            (values(&[("address", text("a\nb"))]), "address"),
            (values(&[("address", text("192.0.2.10")), ("port", Scalar::Int(70000))]), "port"),
            (values(&[("address", text("192.0.2.10")), ("level", Scalar::Int(6))]), "level"),
            (values(&[("address", text("192.0.2.10")), ("on", text("yes"))]), "on"),
            (values(&[("address", text("192.0.2.10")), ("region", text("mars"))]), "region"),
            (values(&[("address", text("192.0.2.10")), ("code", text("two\nlines"))]), "code"),
        ] {
            let error = checked_input(&shown, bad).unwrap_err();
            assert_eq!(error.code, "invalid_input");
            assert_eq!(error.extra.unwrap()["field"], json!(field));
            assert!(!error.message.contains("hunter"), "{}", error.message);
        }
        assert!(checked_input(&shown, SetupInput::Poll).is_err(), "a form takes no poll");
        assert!(checked_input(&shown, SetupInput::Back).is_ok(), "every step can go back");
    }

    #[test]
    fn a_choice_is_one_of_those_offered_and_as_many_as_asked() {
        let choice = |id: &str| Choice { id: id.into(), label: id.into(), detail: String::new(), selected: false };
        let shown = Shown {
            step: SetupStep::Choose {
                title: "t".into(),
                text: "x".into(),
                options: vec![choice("a"), choice("b")],
                multiple: false,
                min: 1,
            },
            retry: None,
        };
        let chosen = |ids: &[&str]| SetupInput::Chosen { ids: ids.iter().map(|id| id.to_string()).collect() };
        assert!(checked_input(&shown, chosen(&["a"])).is_ok());
        let (SetupInput::Chosen { ids }, _) = checked_input(&shown, chosen(&["b", "b"])).unwrap() else { panic!() };
        assert_eq!(ids, ["b"], "a repeat counts once");
        for bad in [chosen(&[]), chosen(&["a", "b"]), chosen(&["c"])] {
            assert_eq!(checked_input(&shown, bad).unwrap_err().code, "invalid_input");
        }
    }

    #[test]
    fn a_failure_offers_its_step_again_and_a_final_one_takes_nothing() {
        let wait = SetupStep::Wait { title: "t".into(), text: "x".into(), poll_ms: 1000, timeout_s: 10 };
        let again = Shown { step: SetupStep::Failed { reason: "r".into(), retry: true }, retry: Some(wait) };
        assert!(checked_input(&again, SetupInput::Poll).is_ok());
        assert!(checked_input(&again, values(&[])).is_err());
        let nothing = Shown { step: SetupStep::Failed { reason: "r".into(), retry: true }, retry: None };
        assert!(checked_input(&nothing, SetupInput::Back).is_ok());
        assert!(checked_input(&nothing, SetupInput::Poll).is_err());
        let over = Shown { step: failed_for_good("r"), retry: None };
        assert_eq!(checked_input(&over, SetupInput::Back).unwrap_err().code, "setup_finished");
        let done = Shown { step: summary_only("ok".into()), retry: None };
        assert_eq!(checked_input(&done, SetupInput::Poll).unwrap_err().code, "setup_finished");
    }

    #[test]
    fn nothing_typed_into_a_secret_field_is_shown_again() {
        let typed = vec!["s3cret-value".to_string(), "abc".to_string()];
        let mut prefilled = field("code", FieldType::Secret, false);
        prefilled.value = Some(text("stored-value"));
        let step = SetupStep::Form {
            title: "Wrong key s3cret-value".into(),
            text: "abc stays: too short to tell from a word".into(),
            fields: vec![prefilled],
        };
        let SetupStep::Form { title, text: body, fields } = cleaned(step, &typed) else { panic!() };
        assert_eq!(title, "Wrong key <redacted>");
        assert!(body.starts_with("abc"));
        assert_eq!(fields[0].value, None, "a secret field is never prefilled");
        let done = cleaned(
            SetupStep::Done(SetupResult {
                config: json!({ "x": 1 }),
                secrets: vec![("code".into(), "s3cret-value".to_string().into())],
                adopt: Vec::new(),
                summary: "Paired with s3cret-value.\n".into(),
            }),
            &typed,
        );
        let SetupStep::Done(result) = &done else { panic!() };
        assert_eq!(result.summary, "Paired with <redacted>.");
        assert_eq!(
            step_json(&summary_only(result.summary.clone())),
            json!({ "step": "done", "summary": "Paired with <redacted>." })
        );
    }

    #[test]
    fn only_declared_slots_and_well_formed_devices_are_taken() {
        let manifest = swifty_plugin_mock::MANIFEST.clone();
        assert!(declares_slot(&manifest, "access_code"));
        assert!(!declares_slot(&manifest, "other"));
        assert!(!declares_slot(&manifest, "access_code/x"), "not a row slot");
        let rows = Manifest {
            secrets: &[swifty_plugin_api::SecretSlot { slot: "local_key/{id}", label: "k", help: "h", required: true }],
            ..manifest
        };
        assert!(declares_slot(&rows, "local_key/plug-1"));
        assert!(!declares_slot(&rows, "local_key"));
        assert!(!declares_slot(&rows, "local_key/Bad Id"));
        for good in ["a", "hue-1", "wiz-a8bb50aabbcc", "vm100", "0x"] {
            assert!(is_local_id(good), "{good}");
        }
        for bad in ["", "-a", "A", "a b", "a/b", &"x".repeat(65)] {
            assert!(!is_local_id(bad), "{bad}");
        }
    }
}
