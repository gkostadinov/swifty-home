//! Answers that arrive a little at a time, and the page following them.
//!
//! An external agent with an HTTP API runs in two copies on this machine, on loopback
//! only. The owner's has every tool and asks for no approval. Everyone else's has web
//! search and nothing else, under an account of its own. Which one a turn goes to is
//! decided here from the signed-in role, and nothing the page sends can change it. A
//! spoken turn may look but not change anything, because a misheard word must not become
//! a command.
//!
//! The model itself also writes here when it is asked for prose rather than for a plan.
//!
//! A turn can outlast any web request, so it runs as a job. The page asks for the job
//! and the server answers as soon as something new has been written, which shows the
//! words as they come and works through any proxy. What a tool did is audited by name
//! only: its arguments can hold anything.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use reqwest::RequestBuilder;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use swifty_net::{Http, LiveGrant, Timeouts};
use swifty_plugin_api::DeviceError;
use tokio::sync::Notify;
use tokio::task::AbortHandle;

use crate::actions::Actor;
use crate::app::App;
use crate::config::{AgentConfig, ConfigError};
use crate::crypto::random_id;
use crate::error::ApiError;
use crate::policy::{Channel, Role};
use crate::voice;

const CONNECT_WAIT: Duration = Duration::from_secs(3);
/// The agent sends a heartbeat every few seconds while it works, so this long without
/// a word means the connection is gone. A long answer that keeps talking is fine.
const QUIET_LIMIT: Duration = Duration::from_secs(120);
/// A finished job is kept this long, for a page that is slow to look.
const JOB_KEPT: Duration = Duration::from_secs(600);
/// How long a request for news waits before answering that there is none. Well under
/// what the web proxy allows.
const LONG_WAIT: Duration = Duration::from_secs(20);
const MAX_ANSWER: usize = 64 * 1024;
const MAX_LINE: usize = 1024 * 1024;
const MAX_TOOLS: usize = 64;
const MAX_LABEL: usize = 200;
const MAX_NAME: usize = 40;
const ASK_TOOL: &str = "ask_user_question";
const STILL_BUSY: &str = "Still finishing your last request. Ask again when it is done.";

struct Tool {
    id: String,
    name: String,
    label: String,
    state: String,
}

enum Runner {
    Agent { owner_instance: bool, session: String },
    Model { task: Option<AbortHandle> },
}

struct Job {
    user: String,
    runner: Runner,
    text: String,
    tools: Vec<Tool>,
    error: Option<String>,
    /// What the agent has stopped to ask, until it is answered.
    question: Option<Value>,
    /// The agent said this chat is still busy with an earlier turn.
    refused_busy: bool,
    /// Goes up with every change, so the page can ask for what is newer than it has.
    version: u64,
    finished: Option<Instant>,
}

impl Job {
    fn view(&self) -> Value {
        let tools: Vec<Value> = self
            .tools
            .iter()
            .map(|tool| json!({ "id": tool.id, "name": tool.name, "label": tool.label, "state": tool.state }))
            .collect();
        json!({ "text": self.text, "tools": tools, "done": self.finished.is_some(), "error": self.error, "question": self.question, "version": self.version })
    }
}

/// The answers being written right now, and those just finished.
#[derive(Default)]
pub struct Jobs {
    jobs: Mutex<HashMap<String, Job>>,
    changed: Notify,
}

fn lost_job() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "unknown_job", "That answer is no longer here.")
}

impl Jobs {
    fn map(&self) -> MutexGuard<'_, HashMap<String, Job>> {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// One answer at a time for each person.
    fn begin(&self, user: &str, runner: Runner) -> Result<String, ApiError> {
        let mut jobs = self.map();
        jobs.retain(|_, job| job.finished.is_none_or(|at| at.elapsed() < JOB_KEPT));
        if jobs.values().any(|job| job.user == user && job.finished.is_none()) {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "assistant_busy",
                "Swifty is still answering. Try again in a moment.",
            ));
        }
        let id = random_id();
        jobs.insert(
            id.clone(),
            Job {
                user: user.to_string(),
                runner,
                text: String::new(),
                tools: Vec::new(),
                error: None,
                question: None,
                refused_busy: false,
                version: 0,
                finished: None,
            },
        );
        Ok(id)
    }

    fn change(&self, id: &str, change: impl FnOnce(&mut Job)) {
        if let Some(job) = self.map().get_mut(id) {
            change(job);
            job.version += 1;
        }
        self.changed.notify_waiters();
    }

    /// The person's answer still being written, if there is one.
    fn active_for(&self, user: &str) -> Option<String> {
        self.map().iter().find(|(_, job)| job.user == user && job.finished.is_none()).map(|(id, _)| id.clone())
    }

    fn take_busy(&self, id: &str) -> bool {
        self.map().get_mut(id).is_some_and(|job| std::mem::take(&mut job.refused_busy))
    }

    fn set_text(&self, id: &str, text: &str) {
        self.change(id, |job| {
            job.text.clear();
            job.text.push_str(text);
        });
    }

    fn finish(&self, id: &str, problem: Option<&str>) {
        self.change(id, |job| {
            if let Some(problem) = problem {
                job.error.get_or_insert_with(|| problem.to_string());
            }
            job.question = None;
            job.finished = Some(Instant::now());
        });
    }

    /// What has been written so far. Given the version the page already has, this waits
    /// until there is something newer, or a while has passed.
    pub async fn view(&self, user: &str, id: &str, seen: Option<u64>) -> Result<Value, ApiError> {
        let until = tokio::time::Instant::now() + LONG_WAIT;
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            // From here on no change can be missed, even one made before the wait starts.
            changed.as_mut().enable();
            {
                let jobs = self.map();
                let job = jobs.get(id).filter(|job| job.user == user).ok_or_else(lost_job)?;
                if seen != Some(job.version) || job.finished.is_some() {
                    return Ok(job.view());
                }
            }
            if tokio::time::timeout_at(until, changed).await.is_err() {
                let jobs = self.map();
                let job = jobs.get(id).filter(|job| job.user == user).ok_or_else(lost_job)?;
                return Ok(job.view());
            }
        }
    }
}

pub struct Agent {
    owner: Http,
    guest: Http,
    connection: String,
    model: String,
    /// The most one answer may take, however busy it stays.
    turn_limit: Duration,
    /// The start of each chat's session name, `[assistant.agent] session_prefix`.
    session_prefix: String,
    /// The header that tells the agent which client is asking, and what this hub
    /// says in it.
    client_header: String,
    client_name: String,
}

fn clip(text: &str, most: usize) -> String {
    text.chars().take(most).collect()
}

/// What a person sees for a tool. Only the owner sees what it was given.
fn tool_label(role: Role, tool: &Value) -> String {
    let verb = tool["verb"].as_str().unwrap_or("Working");
    if role != Role::Owner {
        return if tool["name"] == "web_search" { "Searching the web".to_string() } else { clip(verb, MAX_LABEL) };
    }
    match tool["preview"].as_str().filter(|preview| !preview.is_empty()) {
        Some(preview) => clip(&format!("{verb}: {preview}"), MAX_LABEL),
        None => clip(verb, MAX_LABEL),
    }
}

fn settled(state: &str) -> bool {
    matches!(state, "done" | "error" | "denied")
}

/// Applies one line of the agent's stream. Returns a tool that has just finished. A
/// turn taken over again repeats its tools as they are, and those are not new.
fn apply(jobs: &Jobs, id: &str, role: Role, line: &[u8], resumed: bool) -> Option<(String, String)> {
    let frame: Value = serde_json::from_slice(line).ok()?;
    let mut finished = None;
    match frame["type"].as_str() {
        Some("resync") => {
            jobs.change(id, |job| job.text = clip(frame["content"].as_str().unwrap_or_default(), MAX_ANSWER))
        }
        Some("error")
            if frame["code"] == "turn-active"
                || frame["message"].as_str().is_some_and(|text| text.starts_with("This chat is still working")) =>
        {
            jobs.change(id, |job| job.refused_busy = true);
        }
        Some("delta") => jobs.change(id, |job| {
            if job.text.len() < MAX_ANSWER {
                job.text.push_str(frame["text"].as_str().unwrap_or_default());
            }
        }),
        // The question itself is shown as a card, not as a tool with its arguments.
        Some("agent-tool") if frame["tool"]["name"] == ASK_TOOL => {}
        Some("agent-question") => jobs
            .change(id, |job| job.question = Some(json!({ "call_id": frame["callId"], "items": frame["questions"] }))),
        Some("agent-question-end") => jobs.change(id, |job| {
            if job.question.as_ref().is_some_and(|question| question["call_id"] == frame["callId"]) {
                job.question = None;
            }
        }),
        Some("agent-tool") => jobs.change(id, |job| {
            let tool = &frame["tool"];
            let tool_id = tool["id"].as_str().unwrap_or_default();
            let state = tool["state"].as_str().unwrap_or("running").to_string();
            let known = job.tools.iter().position(|known| known.id == tool_id);
            let was_settled = known.is_some_and(|at| settled(&job.tools[at].state));
            if settled(&state) && !was_settled && !(resumed && known.is_none()) {
                finished = Some((tool["name"].as_str().unwrap_or("tool").to_string(), state.clone()));
            }
            match known {
                Some(at) => {
                    job.tools[at].state = state;
                    job.tools[at].label = tool_label(role, tool);
                }
                None if job.tools.len() < MAX_TOOLS => {
                    let name = clip(tool["name"].as_str().unwrap_or("tool"), MAX_NAME);
                    job.tools.push(Tool { id: tool_id.to_string(), name, label: tool_label(role, tool), state });
                }
                None => {}
            }
        }),
        Some("error") => jobs.change(id, |job| {
            job.error = Some(clip(frame["message"].as_str().unwrap_or("Swifty hit a problem."), MAX_LABEL))
        }),
        _ => {}
    }
    finished
}

impl Agent {
    pub fn new(grant: LiveGrant, config: &AgentConfig) -> Result<Self, ConfigError> {
        let timeouts = Timeouts { connect: CONNECT_WAIT, read: QUIET_LIMIT };
        let client = |base: &str| {
            Http::new(grant, &format!("{}/", base.trim_end_matches('/')), timeouts, None)
                .map_err(|why| ConfigError::Invalid(format!("assistant.agent: {why}")))
        };
        Ok(Self {
            owner: client(&config.owner_url)?,
            guest: client(&config.guest_url)?,
            connection: config.connection.clone(),
            model: config.model.clone(),
            turn_limit: Duration::from_secs(config.max_turn_secs),
            session_prefix: config.session_prefix.clone(),
            client_header: config.client_header.clone(),
            client_name: config.client_name.clone(),
        })
    }

    /// The agent's session for one chat: `<session_prefix>-<user>-<chat>`. The
    /// agent keeps its sessions by this name, so an install keeps its prefix.
    fn session(&self, user: &str, chat: &str) -> String {
        format!("{}-{user}-{chat}", self.session_prefix)
    }

    /// Tells the agent which client is asking, in the header the configuration names.
    fn tagged(&self, request: RequestBuilder) -> RequestBuilder {
        request.header(self.client_header.as_str(), self.client_name.as_str())
    }

    fn instance(&self, owner: bool) -> &Http {
        if owner {
            &self.owner
        } else {
            &self.guest
        }
    }

    async fn halt(&self, owner_instance: bool, session: &str) -> Result<(), DeviceError> {
        let http = self.instance(owner_instance);
        let request = self.tagged(http.post(http.url("api/chat/stop")?)).json(&json!({ "sessionId": session }));
        http.send(request).await.map(|_| ())
    }
}

/// Stops an answer that its own writer asked to stop.
pub async fn stop(app: &App, user: &str, id: &str) -> Result<Value, ApiError> {
    let assistant = app.assistant.as_ref().ok_or_else(lost_job)?;
    let agent_turn = {
        let jobs = assistant.jobs().map();
        let job = jobs.get(id).filter(|job| job.user == user).ok_or_else(lost_job)?;
        match &job.runner {
            Runner::Agent { owner_instance, session } => Some((*owner_instance, session.clone())),
            Runner::Model { task } => {
                if let Some(task) = task {
                    task.abort();
                }
                None
            }
        }
    };
    match agent_turn {
        Some((owner_instance, session)) => {
            let unavailable =
                || ApiError::new(StatusCode::BAD_GATEWAY, "agent_unavailable", "Swifty could not be stopped.");
            let agent = assistant.agent().ok_or_else(lost_job)?;
            agent.halt(owner_instance, &session).await.map_err(|_| unavailable())?;
        }
        None => assistant.jobs().finish(id, None),
    }
    Ok(json!({ "stopped": true }))
}

#[derive(Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AnswerItem {
    pub id: String,
    #[serde(default)]
    pub selected: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuestionAnswer {
    pub call_id: String,
    #[serde(default)]
    pub answers: Vec<AnswerItem>,
    #[serde(default)]
    pub skip: bool,
}

/// Passes on what the person chose. Audited as having happened, without the answer.
pub async fn answer(app: &App, actor: &Actor, id: &str, reply: QuestionAnswer) -> Result<Value, ApiError> {
    let gone = || {
        ApiError::new(
            StatusCode::CONFLICT,
            "question_gone",
            "That question was already answered, or Swifty has gone on.",
        )
    };
    let assistant = app.assistant.as_ref().ok_or_else(lost_job)?;
    let (owner_instance, session) = {
        let jobs = assistant.jobs().map();
        let job = jobs.get(id).filter(|job| job.user == actor.user_id).ok_or_else(lost_job)?;
        if job.question.as_ref().is_none_or(|question| question["call_id"] != reply.call_id.as_str()) {
            return Err(gone());
        }
        match &job.runner {
            Runner::Agent { owner_instance, session } => (*owner_instance, session.clone()),
            Runner::Model { .. } => return Err(gone()),
        }
    };
    let unavailable =
        || ApiError::new(StatusCode::BAD_GATEWAY, "agent_unavailable", "Swifty did not take that answer.");
    let agent = assistant.agent().ok_or_else(lost_job)?;
    let http = agent.instance(owner_instance);
    let body = json!({ "sessionId": session, "callId": reply.call_id, "answers": reply.answers, "skip": reply.skip });
    let request = agent.tagged(http.post(http.url("api/chat/answer").map_err(|_| unavailable())?)).json(&body);
    let sent = http.send(request).await.map_err(|_| unavailable())?;
    if sent.status == StatusCode::CONFLICT {
        return Err(gone());
    }
    if !sent.status.is_success() {
        tracing::warn!(status = %sent.status, "the agent refused an answer");
        return Err(unavailable());
    }
    assistant.jobs().change(id, |job| job.question = None);
    voice::audit_on(
        app,
        actor,
        Channel::Assistant,
        "agent.question",
        "-",
        Value::Null,
        if reply.skip { "skipped" } else { "answered" },
    )?;
    Ok(json!({ "answered": true }))
}

/// Starts an agent turn and hands back the job the page follows.
pub fn start(app: &App, actor: &Actor, chat: &str, message: String, spoken: bool) -> Result<Value, ApiError> {
    let Some((assistant, agent)) =
        app.assistant.as_ref().and_then(|assistant| assistant.agent().map(|agent| (assistant, agent)))
    else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "agent_not_configured",
            "Swifty's agent is not set up here yet.",
        ));
    };
    let owner_instance = actor.role == Role::Owner;
    let session = agent.session(&actor.user_id, chat);
    let id = assistant.jobs().begin(&actor.user_id, Runner::Agent { owner_instance, session: session.clone() })?;
    let mode = if owner_instance && !spoken { "bypass" } else { "plan" };
    voice::audit_on(app, actor, Channel::Assistant, "assistant.agent", mode, Value::Null, "started")?;
    let (app, actor, job) = (app.clone(), actor.clone(), id.clone());
    tokio::spawn(async move { run(app, actor, job, session, message, mode).await });
    Ok(json!({ "job": id }))
}

/// Reads a turn's frames into its job until the turn ends.
async fn follow(
    app: &App,
    actor: &Actor,
    jobs: &Jobs,
    id: &str,
    http: &Http,
    request: RequestBuilder,
    resumed: bool,
) -> Result<StatusCode, DeviceError> {
    let mut pending: Vec<u8> = Vec::new();
    http.stream(request, |chunk| {
        pending.extend_from_slice(chunk);
        while let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            if let Some((tool, state)) = apply(jobs, id, actor.role, &line, resumed) {
                let _ = voice::audit_on(app, actor, Channel::Assistant, "agent.tool", &tool, Value::Null, &state);
            }
        }
        if pending.len() > MAX_LINE {
            pending.clear();
        }
    })
    .await
}

/// Takes over a turn that is running with nobody watching it.
async fn take_over(
    app: &App,
    actor: &Actor,
    jobs: &Jobs,
    id: &str,
    agent: &Agent,
    owner_instance: bool,
    session: &str,
) -> Result<StatusCode, DeviceError> {
    let http = agent.instance(owner_instance);
    let request = agent
        .tagged(http.post(http.url("api/chat/attach")?))
        .json(&json!({ "sessionId": session }))
        .timeout(agent.turn_limit);
    follow(app, actor, jobs, id, http, request, true).await
}

async fn conclude(
    agent: &Agent,
    owner_instance: bool,
    session: &str,
    result: Result<StatusCode, DeviceError>,
) -> Option<&'static str> {
    match result {
        Ok(status) if status.is_success() => None,
        Ok(StatusCode::NOT_FOUND) => Some("That answer has already finished. Ask again."),
        Ok(status) => {
            tracing::warn!(%status, "the agent refused a turn");
            Some("Swifty's agent refused that.")
        }
        Err(DeviceError::Timeout) => {
            // Walking away would leave it running with nobody watching.
            let _ = agent.halt(owner_instance, session).await;
            Some("Swifty went quiet or ran past its time limit, so it was stopped.")
        }
        Err(_) => Some("Swifty's agent is not answering."),
    }
}

async fn run(app: App, actor: Actor, id: String, session: String, message: String, mode: &'static str) {
    let Some(assistant) = app.assistant.as_ref() else {
        return;
    };
    let Some(agent) = assistant.agent() else {
        return;
    };
    let jobs = assistant.jobs();
    let owner_instance = actor.role == Role::Owner;
    let http = agent.instance(owner_instance);
    let body = json!({
        "message": message,
        "sessionId": session,
        "mode": mode,
        "remoteModel": { "connectionId": agent.connection, "modelId": agent.model },
    });
    let Ok(url) = http.url("api/chat/stream") else {
        jobs.finish(&id, Some("Swifty's agent address is not valid."));
        return;
    };
    let request = agent.tagged(http.post(url)).json(&body).timeout(agent.turn_limit);
    let mut result = follow(&app, &actor, jobs, &id, http, request, false).await;
    if jobs.take_busy(&id) {
        // An earlier answer in this chat is still being written, by a turn nobody was
        // watching. Show that one, with Stop, rather than an error.
        jobs.change(&id, |job| {
            job.tools.push(Tool {
                id: "busy".to_string(),
                name: "note".to_string(),
                label: STILL_BUSY.to_string(),
                state: "info".to_string(),
            })
        });
        result = take_over(&app, &actor, jobs, &id, agent, owner_instance, &session).await;
    }
    let problem = conclude(agent, owner_instance, &session, result).await;
    jobs.finish(&id, problem);
    let _ = voice::audit_on(
        &app,
        &actor,
        Channel::Assistant,
        "assistant.agent",
        "-",
        Value::Null,
        if problem.is_some() { "failed" } else { "finished" },
    );
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ResumeRequest {
    pub chat: String,
}

/// The person's answer still being written: the one this service is following, or
/// one the agent is still writing after this service restarted. Starts nothing new.
pub async fn resume(app: &App, actor: &Actor, chat: &str) -> Result<Value, ApiError> {
    if chat.len() > 64 || !chat.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_chat",
            "That conversation cannot be followed.",
        ));
    }
    let nothing = Ok(json!({ "job": null }));
    let Some(assistant) = app.assistant.as_ref() else {
        return nothing;
    };
    if let Some(id) = assistant.jobs().active_for(&actor.user_id) {
        return Ok(json!({ "job": id }));
    }
    let Some(agent) = assistant.agent() else {
        return nothing;
    };
    let owner_instance = actor.role == Role::Owner;
    let session = agent.session(&actor.user_id, chat);
    let http = agent.instance(owner_instance);
    let Ok(url) = http.url("api/chat/active") else {
        return nothing;
    };
    let Ok(reply) = http.send(agent.tagged(http.get(url))).await else {
        return nothing;
    };
    let running = reply.json().ok().and_then(|value| {
        value["turns"].as_array().map(|turns| turns.iter().any(|turn| turn["chatId"] == session.as_str()))
    });
    if running != Some(true) {
        return nothing;
    }
    let id = assistant.jobs().begin(&actor.user_id, Runner::Agent { owner_instance, session: session.clone() })?;
    let (owned, who, job) = (app.clone(), actor.clone(), id.clone());
    tokio::spawn(async move {
        let Some(assistant) = owned.assistant.as_ref() else {
            return;
        };
        let Some(agent) = assistant.agent() else {
            return;
        };
        let jobs = assistant.jobs();
        let result = take_over(&owned, &who, jobs, &job, agent, owner_instance, &session).await;
        let problem = conclude(agent, owner_instance, &session, result).await;
        jobs.finish(&job, problem);
    });
    Ok(json!({ "job": id }))
}

/// Asks the model for prose, and hands back the job the page follows.
pub fn start_model(app: &App, actor: &Actor, messages: Vec<Value>) -> Result<Value, ApiError> {
    let Some(assistant) = app.assistant.as_ref() else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "assistant_not_configured",
            "Swifty is not set up here yet.",
        ));
    };
    let id = assistant.jobs().begin(&actor.user_id, Runner::Model { task: None })?;
    let (owned, who, job) = (app.clone(), actor.clone(), id.clone());
    let task = tokio::spawn(async move { write(owned, who, job, messages).await });
    if let Some(job) = assistant.jobs().map().get_mut(&id) {
        job.runner = Runner::Model { task: Some(task.abort_handle()) };
    }
    Ok(json!({ "job": id }))
}

async fn write(app: App, actor: Actor, id: String, messages: Vec<Value>) {
    let Some(assistant) = app.assistant.as_ref() else {
        return;
    };
    let jobs = assistant.jobs();
    match assistant.stream_text(&messages, |text| jobs.set_text(&id, text)).await {
        Ok(answer) => {
            jobs.set_text(&id, &answer);
            jobs.finish(&id, None);
            let _ = voice::audit_on(&app, &actor, Channel::Assistant, "assistant", "-", Value::Null, "answered");
        }
        Err(problem) => jobs.finish(&id, Some(&problem.message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_owner_sees_what_a_tool_was_given() {
        let tool = json!({ "name": "web_search", "verb": "Searching", "preview": "some private words" });
        assert_eq!(tool_label(Role::Family, &tool), "Searching the web");
        assert_eq!(tool_label(Role::Owner, &tool), "Searching: some private words");
        let shell = json!({ "name": "bash", "verb": "Running a command", "preview": "whoami" });
        assert_eq!(tool_label(Role::Family, &shell), "Running a command");
        assert_eq!(tool_label(Role::Owner, &shell), "Running a command: whoami");
    }
}
