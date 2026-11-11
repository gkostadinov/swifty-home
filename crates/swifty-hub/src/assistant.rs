//! Swifty: quick chats with the home's own model, over its OpenAI-compatible API.
//!
//! The model only ever proposes. Anything it asks for takes the path a spoken command
//! takes, so the person's role, the action's tier and every confirmation still apply,
//! and only the actions listed here can be proposed at all. Machines, the printer,
//! passkeys and people are not among them.
//!
//! Device requests come here and go to the model through its API. Everything else goes to
//! the external agent (see `agent.rs`) when it is set up. Without it a search is declined
//! plainly, because the model alone would answer from memory and name a source it never
//! read: over the API nothing carries out its web_search tool.
//!
//! Actions are taken only on a turn where the person's own words are about the home,
//! and those turns carry none of the model's earlier replies, so nothing it picked up
//! elsewhere can talk it into switching something.
//!
//! What Swifty remembers is in `memory.rs`. Every turn is given the person's notes and
//! the house's. "Remember that ..." and "forget ..." are handled here without a model,
//! and on a turn about the home the model may note something lasting from the person's
//! own words, which the page shows with Undo. A command the parsers missed and the
//! model worked out into one everyday action is learned, so it runs at once next time,
//! and each command that needed the model is noted for the weekly review. Nothing else
//! anyone says is written down, and a key never appears in a reply or a log.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex as StdMutex, PoisonError};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use reqwest::header::AUTHORIZATION;
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{json, Value};
use swifty_net::{Http, LiveGrant, Reply, Timeouts};
use swifty_plugin_api::{DeviceClass, DeviceError, Secret};
use tokio::sync::Mutex;

use crate::actions::Actor;
use crate::agent::{Agent, Jobs};
use crate::app::App;
use crate::catalog;
use crate::config::{read_secret_file, AssistantConfig, ConfigError};
use crate::error::ApiError;
use crate::memory::{self, Memory, Outcome, Said};
use crate::policy::{self, Channel, Role};
use crate::rooms;
use crate::runtime::index::DeviceIndex;
use crate::voice::{self, Candidate, Lexicon, Resolution};

const MAX_MESSAGES: usize = 12;
const MAX_TEXT: usize = 2_000;
const MAX_CONTEXT: usize = 3_000;
const MAX_ACTIONS: usize = 4;
const MAX_SAY: usize = 4_000;
const MAX_REPLY_BODY: usize = 1024 * 1024;
const HISTORY_SENT: usize = 8;
const MAX_STREAM_LINE: usize = 1024 * 1024;
const NO_SEARCH: &str = "I can't search the web from here yet, so I won't guess.";
const CONNECT_WAIT: Duration = Duration::from_secs(3);
const OUTSIDE_WAIT: Duration = Duration::from_secs(8);
/// Less than this left and the model cannot finish, so it is not asked.
const LEAST_LEFT: Duration = Duration::from_secs(2);
/// The most notes the model may keep on one turn.
const MAX_NOTED: usize = 2;
/// How soon after a learned phrase ran a "no" takes it back.
const TAKE_BACK_WITHIN: Duration = Duration::from_secs(120);
/// What a "no" sounds like, in the parser's normal form.
const TAKE_BACKS: &[&str] = &[
    "no",
    "nope",
    "wrong",
    "not that",
    "undo",
    "that is wrong",
    "thats wrong",
    "that was wrong",
    "not what i meant",
    "thats not what i meant",
    "i didnt mean that",
    "i did not mean that",
];

/// Everything the model may propose, with the shape of each. ROOM, LIGHT, SOCKET, AIRCON,
/// SPEAKER, TV, FEEDER and LITTER are ids from the lists in its instructions.
const ACTIONS: &[(&str, &str)] = &[
    ("room.power", r#"target=ROOM params {"on":true}"#),
    ("room.brightness", r#"target=ROOM params {"percent":40}"#),
    ("room.scene", r#"target=ROOM params {"scene":"NAME"}"#),
    ("lights.power", r#"target=all_bulbs params {"on":false}"#),
    ("lights.brightness", r#"target=all_bulbs params {"percent":40}"#),
    ("scene.apply", "target=evening or focus or all_off params {}"),
    ("scene.movie", "target=movie params {}"),
    ("light.power", r#"target=LIGHT params {"on":true}"#),
    ("light.brightness", r#"target=LIGHT params {"percent":40}"#),
    ("light.hue", r#"target=LIGHT params {"hue":30}"#),
    ("plug.power", r#"target=SOCKET params {"on":true}"#),
    ("ac.power", r#"target=AIRCON params {"on":true}"#),
    ("ac.mode", r#"target=AIRCON params {"mode":"cool"}"#),
    ("ac.temperature", r#"target=AIRCON params {"celsius":22}"#),
    ("ac.fan", r#"target=AIRCON params {"fan":"auto"}"#),
    ("speaker.volume", r#"target=SPEAKER params {"level":20}"#),
    ("speaker.mute", r#"target=SPEAKER params {"muted":true}"#),
    ("speaker.night", r#"target=SPEAKER params {"on":true}"#),
    ("speaker.play", "target=SPEAKER params {}"),
    ("speaker.pause", "target=SPEAKER params {}"),
    ("tv.power", r#"target=TV params {"on":true}"#),
    ("tv.input", r#"target=TV params {"input":"HDMI 1"}"#),
    ("tv.key", r#"target=TV params {"key":"play"}"#),
    ("pets.feed", r#"target=FEEDER params {"grams":10}"#),
    ("pets.clean", "target=LITTER params {}"),
];

const QUESTION_WORDS: &[&str] = &[
    "how", "what", "whats", "what's", "why", "when", "who", "where", "which", "is", "are", "do", "does", "did", "can",
    "could", "should", "would", "will", "tell",
];
const WEATHER_WORDS: &[&str] =
    &["weather", "forecast", "rain", "raining", "rainy", "snow", "snowing", "umbrella", "sunny"];
/// Words that make a turn about the home, besides the names of its rooms and devices and
/// the nouns of the kinds of device Swifty can act on.
const HOME_WORDS: &[&str] = &[
    "light",
    "lights",
    "lamp",
    "lamps",
    "bulb",
    "bulbs",
    "strip",
    "bright",
    "brighter",
    "dim",
    "dimmer",
    "dark",
    "darker",
    "room",
    "bedroom",
    "socket",
    "sockets",
    "plug",
    "plugs",
    "usb",
    "tv",
    "television",
    "watch",
    "movie",
    "speaker",
    "music",
    "volume",
    "loud",
    "louder",
    "quiet",
    "quieter",
    "mute",
    "unmute",
    "night",
    "scene",
    "evening",
    "focus",
    "feed",
    "feeder",
    "litter",
    "pet",
    "pets",
    "toilet",
    "turn",
    "switch",
    "pause",
    "play",
    "ac",
    "aircon",
    "conditioner",
    "conditioning",
    "cooler",
    "warmer",
    "cooling",
    "heating",
];
const SEARCH_OPENINGS: &[&str] = &[
    "search the web for ",
    "search online for ",
    "search the internet for ",
    "search for ",
    "look up ",
    "google ",
    "find online ",
];

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatMessage {
    pub role: String,
    pub text: String,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AssistantRequest {
    pub messages: Vec<ChatMessage>,
    /// What the person's page can see right now, and the time there.
    #[serde(default)]
    pub context: String,
    /// Which conversation this is, so the agent can follow it.
    #[serde(default)]
    pub chat: String,
    /// Said rather than typed, so a misheard word must not be able to change anything.
    #[serde(default)]
    pub spoken: bool,
    /// Typed into the home's search as a command that the page's own phrases did not
    /// match. Unless it is a question, it is about the home's actions.
    #[serde(default)]
    pub command: bool,
}

struct Weather {
    http: Http,
    place: String,
    latitude: f64,
    longitude: f64,
}

pub struct Assistant {
    models: Vec<Http>,
    name: String,
    key: Secret,
    budget: Duration,
    weather: Option<Weather>,
    agent: Option<Agent>,
    jobs: Jobs,
    /// The model answers one thing at a time.
    one_at_a_time: Mutex<()>,
    /// The learned phrase that last ran for each person, so that a "no" can take it back.
    recent_learned: StdMutex<HashMap<String, (String, String, Instant)>>,
}

fn client(grant: LiveGrant, base: &str, timeouts: Timeouts) -> Result<Http, ConfigError> {
    let with_slash = format!("{}/", base.trim_end_matches('/'));
    Ok(Http::new(grant, &with_slash, timeouts, None)
        .map_err(|why| ConfigError::Invalid(format!("assistant: {why}")))?
        .with_max_body(MAX_REPLY_BODY))
}

impl Assistant {
    pub fn new(grant: LiveGrant, config: &AssistantConfig) -> Result<Self, ConfigError> {
        let budget = Duration::from_secs(config.timeout_secs);
        let to_model = Timeouts { connect: CONNECT_WAIT, read: budget };
        let outside = Timeouts { connect: CONNECT_WAIT, read: OUTSIDE_WAIT };
        let models = config.urls.iter().map(|url| client(grant, url, to_model)).collect::<Result<Vec<_>, _>>()?;
        let weather = match &config.weather {
            Some(place) => Some(Weather {
                http: client(grant, &place.url, outside)?,
                place: place.place.clone(),
                latitude: place.latitude,
                longitude: place.longitude,
            }),
            None => None,
        };
        Ok(Self {
            models,
            name: config.model.clone(),
            key: read_secret_file(&config.key_file)?,
            budget,
            weather,
            agent: config.agent.as_ref().map(|agent| Agent::new(grant, agent)).transpose()?,
            jobs: Jobs::default(),
            one_at_a_time: Mutex::new(()),
            recent_learned: StdMutex::new(HashMap::new()),
        })
    }

    pub fn agent(&self) -> Option<&Agent> {
        self.agent.as_ref()
    }

    pub fn jobs(&self) -> &Jobs {
        &self.jobs
    }

    /// Asks for prose and passes on what has been written so far, each time there is
    /// more. Returns the finished answer. Moves to the next address only when nothing
    /// has been written yet.
    pub(crate) async fn stream_text(
        &self,
        messages: &[Value],
        mut shown: impl FnMut(&str),
    ) -> Result<String, ApiError> {
        let _turn = self.one_at_a_time.lock().await;
        let deadline = Instant::now() + self.budget;
        let body =
            json!({ "model": self.name, "messages": messages, "temperature": 0.2, "max_tokens": 700, "stream": true });
        for http in &self.models {
            let left = deadline.saturating_duration_since(Instant::now());
            if left < LEAST_LEFT {
                return Err(too_slow());
            }
            let url = http.url("chat/completions").map_err(|_| unavailable("Swifty's address is not valid."))?;
            let request =
                http.post(url).header(AUTHORIZATION, format!("Bearer {}", self.key.expose())).json(&body).timeout(left);
            let asked = Instant::now();
            let mut reading = Streamed::default();
            let result = http
                .stream(request, |chunk| {
                    if let Some(text) = reading.feed(chunk) {
                        shown(&text);
                    }
                })
                .await;
            match result {
                Ok(status) if status.is_success() => return reading.finish(),
                Ok(status) => {
                    tracing::warn!(%status, "the model refused a request");
                    return Err(unavailable("Swifty refused that request."));
                }
                Err(DeviceError::Unreachable(_)) if reading.shown.is_empty() => continue,
                Err(DeviceError::Timeout)
                    if reading.shown.is_empty() && asked.elapsed() < CONNECT_WAIT + Duration::from_secs(1) =>
                {
                    continue
                }
                Err(DeviceError::Timeout) => return Err(too_slow()),
                Err(_) => return Err(unavailable("Swifty stopped partway through.")),
            }
        }
        Err(unavailable("Swifty could not be reached."))
    }

    /// Asks the model, moving to the next address only when one cannot be reached.
    async fn complete(&self, messages: &[Value], deadline: Instant) -> Result<String, ApiError> {
        let body =
            json!({ "model": self.name, "messages": messages, "temperature": 0.2, "max_tokens": 700, "stream": false });
        for http in &self.models {
            let left = deadline.saturating_duration_since(Instant::now());
            if left < LEAST_LEFT {
                return Err(too_slow());
            }
            let url = http.url("chat/completions").map_err(|_| unavailable("Swifty's address is not valid."))?;
            let request =
                http.post(url).header(AUTHORIZATION, format!("Bearer {}", self.key.expose())).json(&body).timeout(left);
            let asked = Instant::now();
            match http.send(request).await {
                Ok(reply) if reply.status.is_success() => return reply_text(&reply),
                Ok(reply) => {
                    tracing::warn!(status = %reply.status, "the model refused a request");
                    return Err(unavailable("Swifty refused that request."));
                }
                Err(DeviceError::Unreachable(_)) => continue,
                // A timeout this early was the connection, so the next address is worth a try.
                Err(DeviceError::Timeout) if asked.elapsed() < CONNECT_WAIT + Duration::from_secs(1) => continue,
                Err(DeviceError::Timeout) => return Err(too_slow()),
                Err(_) => return Err(unavailable("Swifty sent something unreadable.")),
            }
        }
        Err(unavailable("Swifty could not be reached."))
    }

    async fn forecast(&self) -> Option<String> {
        let weather = self.weather.as_ref()?;
        let path = format!(
            "v1/forecast?latitude={}&longitude={}&timezone=auto&forecast_days=7&current=temperature_2m,weather_code&daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max,wind_speed_10m_max",
            weather.latitude, weather.longitude
        );
        let url = weather.http.url(&path).ok()?;
        let reply = weather.http.send(weather.http.get(url)).await.ok()?;
        if !reply.status.is_success() {
            return None;
        }
        forecast_text(&weather.place, &reply.json().ok()?)
    }
}

fn unavailable(message: &str) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, "assistant_unavailable", message.to_string())
}

fn too_slow() -> ApiError {
    ApiError::new(StatusCode::GATEWAY_TIMEOUT, "assistant_timeout", "Swifty took too long to answer.")
}

fn content_of(value: &Value) -> Option<String> {
    let content = value.pointer("/choices/0/message/content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    Some(content.as_array()?.iter().filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join(""))
}

fn reply_text(reply: &Reply) -> Result<String, ApiError> {
    let unreadable = || unavailable("Swifty sent something unreadable.");
    content_of(&reply.json().map_err(|_| unreadable())?).ok_or_else(unreadable)
}

/// What can be shown of a reply still being written: no thinking, even half of it, and
/// not the start of a tag that may turn out to be thinking.
fn visible_so_far(raw: &str) -> String {
    const OPEN: &str = "<think>";
    let mut out = strip_thinking(raw);
    if let Some(open) = out.find(OPEN) {
        out.truncate(open);
        return out;
    }
    if let Some(keep) = (1..OPEN.len()).rev().find(|keep| out.ends_with(&OPEN[..*keep])) {
        out.truncate(out.len() - keep);
    }
    out
}

/// A reply as it arrives: server-sent events from a model that streams, or one JSON
/// body from one that does not.
#[derive(Default)]
struct Streamed {
    pending: Vec<u8>,
    raw: String,
    whole: Vec<u8>,
    tool_call: bool,
    shown: String,
}

impl Streamed {
    /// Takes the next piece and returns the text so far, when there is more of it.
    fn feed(&mut self, chunk: &[u8]) -> Option<String> {
        self.pending.extend_from_slice(chunk);
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            self.line(&line);
        }
        if self.pending.len() > MAX_STREAM_LINE {
            let long = std::mem::take(&mut self.pending);
            self.line(&long);
        }
        let visible = visible_so_far(&self.raw);
        let visible = clip(visible.trim_start(), MAX_SAY);
        // An answer shaped like a plan is only readable once it is whole.
        if visible.starts_with('{') || visible == self.shown {
            return None;
        }
        self.shown.clone_from(&visible);
        Some(visible)
    }

    fn line(&mut self, line: &[u8]) {
        let text = String::from_utf8_lossy(line);
        let Some(data) = text.trim().strip_prefix("data:").map(str::trim) else {
            if self.whole.len() < MAX_STREAM_LINE {
                self.whole.extend_from_slice(line);
            }
            return;
        };
        let Ok(event) = serde_json::from_str::<Value>(data) else {
            return;
        };
        let delta = &event["choices"][0]["delta"];
        if let Some(words) = delta["content"].as_str() {
            self.raw.push_str(words);
        }
        if delta["tool_calls"].is_array() {
            self.tool_call = true;
        }
    }

    fn finish(mut self) -> Result<String, ApiError> {
        let last = std::mem::take(&mut self.pending);
        self.line(&last);
        if self.raw.is_empty() && !self.whole.is_empty() {
            let unreadable = || unavailable("Swifty sent something unreadable.");
            let value: Value = serde_json::from_slice(&self.whole).map_err(|_| unreadable())?;
            self.raw = content_of(&value).ok_or_else(unreadable)?;
        }
        let plan = read_plan(&self.raw);
        let wanted_a_tool = plan.name.is_some() || self.tool_call;
        let say = plan.say.filter(|say| !say.trim().is_empty()).unwrap_or_else(|| {
            if wanted_a_tool {
                NO_SEARCH.to_string()
            } else {
                "I do not have an answer for that.".to_string()
            }
        });
        Ok(clip(&say, MAX_SAY))
    }
}

fn clip(text: &str, most: usize) -> String {
    text.chars().take(most).collect()
}

fn weather_words(code: i64) -> &'static str {
    match code {
        0 => "clear",
        1 => "mostly clear",
        2 => "partly cloudy",
        3 => "overcast",
        45 | 48 => "fog",
        51 | 53 | 55 | 56 | 57 => "drizzle",
        61 | 63 | 65 | 66 | 67 | 80 | 81 | 82 => "rain",
        71 | 73 | 75 | 77 | 85 | 86 => "snow",
        95 | 96 | 99 => "thunderstorm",
        _ => "unsettled",
    }
}

fn forecast_text(place: &str, data: &Value) -> Option<String> {
    let mut lines = vec![format!("Weather for {place}.")];
    if let (Some(now), Some(code)) = (
        data.pointer("/current/temperature_2m").and_then(Value::as_f64),
        data.pointer("/current/weather_code").and_then(Value::as_i64),
    ) {
        lines.push(format!("Now: {now:.0} °C, {}.", weather_words(code)));
    }
    let daily = data.get("daily")?;
    let number = |name: &str, day: usize| daily[name][day].as_f64();
    for (day, date) in daily["time"].as_array()?.iter().enumerate() {
        let (Some(date), Some(low), Some(high)) =
            (date.as_str(), number("temperature_2m_min", day), number("temperature_2m_max", day))
        else {
            continue;
        };
        let code = daily["weather_code"][day].as_i64().unwrap_or(-1);
        let rain = number("precipitation_probability_max", day).unwrap_or(0.0);
        let wind = number("wind_speed_10m_max", day).unwrap_or(0.0);
        lines.push(format!(
            "{date}: {}, {low:.0} to {high:.0} °C, {rain:.0}% chance of rain, wind up to {wind:.0} km/h.",
            weather_words(code)
        ));
    }
    Some(lines.join("\n"))
}

fn strip_thinking(text: &str) -> String {
    let mut out = text.to_string();
    while let (Some(start), Some(end)) = (out.find("<think>"), out.find("</think>")) {
        if start > end {
            break;
        }
        out.replace_range(start..end + "</think>".len(), "");
    }
    out
}

#[derive(Debug, Default, Deserialize)]
struct Plan {
    #[serde(default)]
    say: Option<String>,
    #[serde(default, rename = "do")]
    actions: Option<Value>,
    /// Lines the model chose to keep from the person's words.
    #[serde(default)]
    remember: Option<Value>,
    /// Set when the reply is a tool call rather than an answer.
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Proposal {
    action: String,
    target: String,
    #[serde(default)]
    params: Value,
}

/// What the model said. Anything that is not the JSON it was asked for is simply its answer.
fn read_plan(text: &str) -> Plan {
    let cleaned = strip_thinking(text);
    if let (Some(start), Some(end)) = (cleaned.find('{'), cleaned.rfind('}')) {
        if start < end {
            if let Ok(plan) = serde_json::from_str::<Plan>(&cleaned[start..=end]) {
                return plan;
            }
        }
    }
    Plan { say: Some(cleaned.trim().to_string()), ..Plan::default() }
}

const POLITE_OPENINGS: &[&str] = &[
    "can you please ",
    "could you please ",
    "would you please ",
    "can you ",
    "can u ",
    "could you ",
    "could u ",
    "would you ",
    "would u ",
    "will you ",
    "will u ",
    "please ",
    "pls ",
];

/// "Can you turn off the lights" is a command said politely, not a question.
fn without_politeness(text: &str) -> &str {
    let lower = text.to_lowercase();
    POLITE_OPENINGS
        .iter()
        .find(|opening| lower.starts_with(*opening))
        .and_then(|opening| text.get(opening.len()..))
        .unwrap_or(text)
}

fn first_word(text: &str) -> String {
    text.split_whitespace().next().unwrap_or_default().to_lowercase()
}

/// A question goes to the model even when it contains words the parser would act on.
fn is_question(text: &str) -> bool {
    QUESTION_WORDS.contains(&first_word(text).as_str()) || text.trim_end().ends_with('?')
}

fn wants_weather(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.split(|c: char| !c.is_alphanumeric()).any(|word| WEATHER_WORDS.contains(&word))
        || (lower.contains("temperature") && lower.contains("outside"))
}

fn asks_to_search(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    SEARCH_OPENINGS.iter().any(|opening| lower.starts_with(opening))
}

/// The kinds of device Swifty can act on. Their nouns, the grammar's and the plugins',
/// make a turn about the home. Machines and hosts are never among them.
const SWIFTY_CLASSES: &[DeviceClass] = &[
    DeviceClass::Light,
    DeviceClass::Plug,
    DeviceClass::Aircon,
    DeviceClass::Speaker,
    DeviceClass::Tv,
    DeviceClass::Feeder,
    DeviceClass::Litter,
];

/// Whether the person's own words are about the home. Only then may anything be done.
/// The words are also read as voice reads them, so "Um set A C to twenty-seven" is about
/// the air conditioner although "A C" is two letters.
fn about_the_home(text: &str, lists: &str, lexicon: &Lexicon) -> bool {
    let known = lists.to_lowercase();
    let names: HashSet<&str> = known.split(|c: char| !c.is_alphanumeric()).filter(|word| word.len() >= 4).collect();
    let home = |word: &str| {
        HOME_WORDS.contains(&word)
            || names.contains(word)
            || lexicon.nouns.get(word).is_some_and(|class| SWIFTY_CLASSES.contains(class))
    };
    text.to_lowercase().split(|c: char| !c.is_alphanumeric()).any(home)
        || lexicon.normalize(text).split_whitespace().any(home)
}

/// When the parser is sure. Its "which one?" menu means it was not, and a loose
/// sentence like that is what the model is for.
fn acts_at_once(resolution: &Resolution) -> bool {
    matches!(
        resolution,
        Resolution::Action(_) | Resolution::VolumeStep { .. } | Resolution::Lock | Resolution::Nav { .. }
    )
}

/// The action lines this person may be offered.
fn action_lines(index: &DeviceIndex, role: Role) -> String {
    ACTIONS
        .iter()
        .filter(|(action, _)| policy::may_run(index, role, action, "-"))
        .map(|(action, shape)| format!("{action} {shape}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The rooms, and the devices of each kind Swifty can act on that this person may see,
/// as the index has them. A device that can only be read is left out.
fn home_lists(conn: &Connection, index: &DeviceIndex, role: Role) -> Result<String, ApiError> {
    let rooms = rooms::voice_index(conn)?
        .into_iter()
        .map(|room| {
            let scenes = room.scenes.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>().join(", ");
            if scenes.is_empty() {
                format!("{} = {}", room.id, room.name)
            } else {
                format!("{} = {} (scenes: {scenes})", room.id, room.name)
            }
        })
        .collect::<Vec<_>>();
    let named = |class: DeviceClass| {
        index
            .of_class(class)
            .filter(|device| !device.capabilities.is_empty() && policy::may_see(index, role, &device.target))
            .map(|device| format!("{} = {}", device.target, device.name))
            .collect::<Vec<_>>()
            .join("; ")
    };
    Ok(format!(
        "Rooms: {}\nLights: {}\nSockets: {}\nAir conditioners: {}\nSpeakers: {}\nTVs: {}\nFeeders: {}\nLitter boxes: {}",
        rooms.join("; "),
        named(DeviceClass::Light),
        named(DeviceClass::Plug),
        named(DeviceClass::Aircon),
        named(DeviceClass::Speaker),
        named(DeviceClass::Tv),
        named(DeviceClass::Feeder),
        named(DeviceClass::Litter)
    ))
}

fn instructions(index: &DeviceIndex, role: Role, lists: &str, context: &str, command: bool, notes: &str) -> String {
    let typed = if command {
        "The person typed this into the home's search as a command, and it matched none of its exact phrases. \
         Work out which of the actions below they most likely mean and do it. If none fits, say in one short \
         sentence that you could not match it to anything here.\n"
    } else {
        ""
    };
    format!(
        "You are Swifty, the friendly assistant of a private smart home. Keep answers short and natural.\n{typed}\
         Answer with one JSON object and nothing else, in this shape:\n\
         {{\"say\":\"what to tell the person\",\"do\":[{{\"action\":\"...\",\"target\":\"...\",\"params\":{{}}}}]}}\n\
         \"say\" is always there. Leave out \"do\" unless the person asks to change something in the home.\n\
         Answer only the person's last message. Earlier ones are there only to make sense of it, so never answer or mention them.\n\
         You may keep a note: add \"remember\":[\"one short line\"] only when the person tells you something lasting about \
         themselves or the home that will help later, like a preference or a habit. Never anything secret, like a code or \
         a key, never what you just did, and at most two.\n\
         Never say something was done unless it is in \"do\". Use only these actions, exactly as written:\n{}\n{lists}\n{notes}\nRight now: {context}",
        action_lines(index, role)
    )
}

/// For anything else. No actions are offered, the home's state stays out of it, and
/// whatever comes back cannot act.
fn general_instructions(context: &str, weather: Option<&str>, notes: &str) -> String {
    let when = context.split_inclusive(". ").next().unwrap_or_default().trim();
    let mut text = format!(
        "You are Swifty, the friendly assistant of a private smart home. Keep answers short and natural, in plain text.\n\
         You cannot change anything in the home in this answer, and you cannot search the web or use any tool here. If something needs current facts you do not have, say so plainly rather than guessing, and never name a source.\n\
         Right now: {when}"
    );
    if let Some(weather) = weather {
        text.push('\n');
        text.push_str(weather);
    }
    if !notes.is_empty() {
        text.push('\n');
        text.push_str(notes);
    }
    text
}

fn with_history(system: String, request: &AssistantRequest) -> Vec<Value> {
    let skip = request.messages.len().saturating_sub(HISTORY_SENT);
    std::iter::once(json!({ "role": "system", "content": system }))
        .chain(request.messages[skip..].iter().map(|message| json!({ "role": message.role, "content": message.text })))
        .collect()
}

/// What a turn that can act gets to see. Never the model's own replies, which may carry
/// something it read on the web, and of the person's earlier words only those about the
/// home, or it starts answering them again.
fn home_history(system: String, request: &AssistantRequest, lists: &str, lexicon: &Lexicon) -> Vec<Value> {
    let skip = request.messages.len().saturating_sub(HISTORY_SENT);
    let recent = &request.messages[skip..];
    let last = recent.len().saturating_sub(1);
    std::iter::once(json!({ "role": "system", "content": system }))
        .chain(
            recent
                .iter()
                .enumerate()
                .filter(|(at, message)| {
                    message.role == "user"
                        && (*at == last
                            || (about_the_home(&message.text, lists, lexicon)
                                && !wants_weather(&message.text)
                                && !asks_to_search(&message.text)))
                })
                .map(|(_, message)| json!({ "role": "user", "content": message.text })),
        )
        .collect()
}

fn check(request: &AssistantRequest) -> Result<&str, ApiError> {
    let invalid = |message: &str| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_chat", message.to_string());
    if request.messages.is_empty() || request.messages.len() > MAX_MESSAGES {
        return Err(invalid("A chat needs 1 to 12 messages."));
    }
    if request.context.chars().count() > MAX_CONTEXT {
        return Err(invalid("That is too much context."));
    }
    if request.chat.len() > 64 || !request.chat.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(invalid("That conversation cannot be followed."));
    }
    for message in &request.messages {
        if !matches!(message.role.as_str(), "user" | "assistant") || message.text.chars().count() > MAX_TEXT {
            return Err(invalid("One of those messages cannot be sent."));
        }
    }
    let last = request.messages.last().filter(|message| message.role == "user" && !message.text.trim().is_empty());
    last.map(|message| message.text.trim()).ok_or_else(|| invalid("The last message has to be a question."))
}

fn summary(reply: &Value) -> String {
    match reply["kind"].as_str().unwrap_or_default() {
        "executed" => format!("{}.", reply["label"].as_str().unwrap_or("Done")),
        // The page confirms these itself and shows how they went, so there is nothing to add.
        // Only a passkey still needs the person.
        "prepared" if reply["needs"] == "passkey" => "That one needs your passkey.".to_string(),
        "prepared" | "acknowledgement_required" => String::new(),
        "nav" => "Opening that.".to_string(),
        "locked" => "Locked.".to_string(),
        _ => reply["why"].as_str().unwrap_or("Nothing ran.").to_string(),
    }
}

fn needs_the_person(reply: &Value) -> bool {
    matches!(reply["kind"].as_str(), Some("prepared" | "acknowledgement_required"))
}

/// Turns what the model proposed into runs, each with what was asked for when it could
/// be read. Only the listed actions get this far.
async fn carry_out(
    app: &App,
    actor: &Actor,
    proposed: Option<Value>,
) -> Result<Vec<(Option<Candidate>, Value)>, ApiError> {
    let list = proposed.and_then(|value| value.as_array().cloned()).unwrap_or_default();
    let mut results = Vec::new();
    for item in list.into_iter().take(MAX_ACTIONS) {
        let Ok(proposal) = serde_json::from_value::<Proposal>(item) else {
            results.push((None, json!({ "kind": "refused", "why": "I could not work out what to do there." })));
            continue;
        };
        // An id an action was known by before is the same action.
        let asked = catalog::find(&proposal.action).map_or(proposal.action.as_str(), |def| def.id);
        let Some((action, _)) = ACTIONS.iter().find(|(action, _)| *action == asked) else {
            results.push((None, json!({ "kind": "refused", "why": "That is not something I can do from here." })));
            continue;
        };
        let Some(def) = catalog::find(action) else {
            continue;
        };
        let params = if proposal.params.is_object() { proposal.params } else { json!({}) };
        let chosen = Candidate { action, target: proposal.target, params, label: def.label.to_string() };
        let reply = voice::dispatch_candidate(app, actor, chosen.clone(), Channel::Assistant).await?;
        let stop = needs_the_person(&reply);
        results.push((Some(chosen), reply));
        if stop {
            break;
        }
    }
    Ok(results)
}

fn reply_of(say: &str) -> Value {
    json!({ "say": say, "results": [] })
}

/// "Remember that ...", "forget ..." and "what do you remember", without a model.
fn about_memory(app: &App, actor: &Actor, said: Said) -> Result<Value, ApiError> {
    let now = app.now();
    match said {
        Said::Remember { text, house } => {
            match app.store.tx(|tx| memory::add(tx, now, &actor.user_id, actor.role, &text, house)) {
                Ok(kept) => {
                    let whose = if house { "house" } else { "-" };
                    voice::audit_on(app, actor, Channel::Assistant, "memory.add", whose, Value::Null, "done")?;
                    let say = if house { "I'll remember that for the house." } else { "I'll remember that." };
                    Ok(json!({ "say": say, "results": [], "memory": [kept] }))
                }
                // Too long, secret-looking, full, or the house's when not the owner: said, not failed.
                Err(error) if error.status.is_client_error() => Ok(reply_of(&error.message)),
                Err(error) => Err(error),
            }
        }
        Said::Forget(about) => {
            let found = app.store.with(|conn| memory::matching(conn, &actor.user_id, actor.role, &about))?;
            match found.as_slice() {
                [] => Ok(reply_of("I don't remember anything like that.")),
                [one] => {
                    app.store.tx(|tx| memory::forget(tx, &actor.user_id, actor.role, &one.id))?;
                    voice::audit_on(
                        app,
                        actor,
                        Channel::Assistant,
                        "memory.forget",
                        if one.house { "house" } else { "-" },
                        Value::Null,
                        "done",
                    )?;
                    Ok(reply_of(&format!("Forgotten: {}", one.text)))
                }
                several => {
                    let named = several
                        .iter()
                        .take(3)
                        .map(|memory| format!("“{}”", memory.text))
                        .collect::<Vec<_>>()
                        .join(", ");
                    Ok(reply_of(&format!("Which one? {named}. Say more of it, or remove it on the Memory page.")))
                }
            }
        }
        Said::Recall => {
            let (mine, house) = app.store.with(|conn| memory::list(conn, &actor.user_id))?;
            if mine.is_empty() && house.is_empty() {
                return Ok(reply_of("I don't remember anything yet. Tell me “remember that …” and I will."));
            }
            let lines =
                |list: &[Memory]| list.iter().map(|memory| format!("- {}", memory.text)).collect::<Vec<_>>().join("\n");
            let mut say = Vec::new();
            if !mine.is_empty() {
                say.push(format!("About you:\n{}", lines(&mine)));
            }
            if !house.is_empty() {
                say.push(format!("About the home:\n{}", lines(&house)));
            }
            Ok(reply_of(&say.join("\n")))
        }
    }
}

/// A "no" soon after a learned phrase ran drops that phrase. Gives back the phrase dropped.
fn take_back(app: &App, assistant: &Assistant, actor: &Actor, phrase: &str) -> Result<Option<String>, ApiError> {
    if !TAKE_BACKS.iter().any(|no| phrase == *no || phrase.starts_with(&format!("{no} "))) {
        return Ok(None);
    }
    let recent = assistant.recent_learned.lock().unwrap_or_else(PoisonError::into_inner).remove(&actor.user_id);
    let Some((id, learned, _)) = recent.filter(|(_, _, at)| at.elapsed() < TAKE_BACK_WITHIN) else {
        return Ok(None);
    };
    let now = app.now();
    app.store.tx(|tx| -> rusqlite::Result<()> {
        memory::unlearn(tx, &id)?;
        memory::note_miss(tx, now, &actor.user_id, &learned, Outcome::Corrected)
    })?;
    voice::audit_on(app, actor, Channel::Assistant, "learned.forget", "-", Value::Null, "done")?;
    Ok(Some(learned))
}

/// A phrase learned before runs at once, through the same path as anything the model
/// proposes. One whose action can no longer run is dropped and worked out again.
async fn run_learned(app: &App, assistant: &Assistant, actor: &Actor, phrase: &str) -> Result<Option<Value>, ApiError> {
    if phrase.is_empty() {
        return Ok(None);
    }
    let Some(learned) = app.store.with(|conn| memory::find_learned(conn, phrase))? else {
        return Ok(None);
    };
    // A phrase learned under an id an action was known by before still runs it.
    let current = catalog::find(&learned.action).map_or(learned.action.as_str(), |def| def.id);
    let chosen = ACTIONS.iter().find(|(action, _)| *action == current).map(|(action, _)| Candidate {
        action,
        target: learned.target.clone(),
        params: learned.params.clone(),
        label: learned.label.clone(),
    });
    let ran = match chosen {
        Some(chosen) => voice::dispatch_candidate(app, actor, chosen, Channel::Assistant).await.ok(),
        None => None,
    };
    let now = app.now();
    let Some(reply) = ran else {
        app.store.tx(|tx| memory::unlearn(tx, &learned.id))?;
        return Ok(None);
    };
    app.store.tx(|tx| memory::used(tx, &learned.id, now))?;
    assistant
        .recent_learned
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(actor.user_id.clone(), (learned.id.clone(), learned.phrase.clone(), Instant::now()));
    Ok(Some(json!({ "say": summary(&reply), "results": [reply], "learned": true })))
}

/// After the model acted on a command the parsers missed: one everyday action that ran
/// is learned when the phrase can stand for it, and the turn is noted for the review.
fn learn_or_note(app: &App, actor: &Actor, phrase: &str, ran: &[(Option<Candidate>, Value)]) -> Result<(), ApiError> {
    if phrase.is_empty() {
        return Ok(());
    }
    let executed: Vec<&Candidate> =
        ran.iter().filter(|(_, reply)| reply["kind"] == "executed").filter_map(|(chosen, _)| chosen.as_ref()).collect();
    let acted = ran
        .iter()
        .any(|(_, reply)| matches!(reply["kind"].as_str(), Some("executed" | "prepared" | "acknowledgement_required")));
    let now = app.now();
    app.store.tx(|tx| -> rusqlite::Result<()> {
        let learned = match (ran.len(), executed.as_slice()) {
            (1, [only]) if memory::learnable(phrase, only.action, &only.params) => {
                memory::learn(tx, now, phrase, &actor.user_id, only)?
            }
            _ => false,
        };
        let outcome = if learned {
            Outcome::Learned
        } else if acted {
            Outcome::WorkedOut
        } else {
            Outcome::NotUnderstood
        };
        memory::note_miss(tx, now, &actor.user_id, phrase, outcome)
    })?;
    Ok(())
}

/// What the model chose to note from the person's words: kept as theirs, at most two.
/// A line that looks secret, or one already kept, is left out without a word.
fn keep_notes(app: &App, actor: &Actor, noted: Option<Value>) -> Result<Vec<Memory>, ApiError> {
    let lines: Vec<String> = noted
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|item| item.as_str().or_else(|| item["text"].as_str()).map(str::to_string))
        .take(MAX_NOTED)
        .collect();
    if lines.is_empty() {
        return Ok(Vec::new());
    }
    let now = app.now();
    let kept = app.store.tx(|tx| -> Result<Vec<Memory>, ApiError> {
        let mut kept = Vec::new();
        for line in &lines {
            match memory::add(tx, now, &actor.user_id, actor.role, line, false) {
                Ok(memory) if memory.created_at == now => kept.push(memory),
                Ok(_) => {}
                Err(error) if error.status.is_client_error() => {}
                Err(error) => return Err(error),
            }
        }
        Ok(kept)
    })?;
    for _ in &kept {
        voice::audit_on(app, actor, Channel::Assistant, "memory.add", "-", Value::Null, "done")?;
    }
    Ok(kept)
}

pub async fn answer(app: &App, actor: &Actor, request: AssistantRequest) -> Result<Value, ApiError> {
    let Some(assistant) = app.assistant.as_ref() else {
        return Err(if app.config.assistant.is_some() {
            ApiError::new(StatusCode::CONFLICT, "assistant_dry_run", "Dry-run: nothing is sent to Swifty.")
        } else {
            ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "assistant_not_configured", "Swifty is not set up here yet.")
        });
    };
    let text = check(&request)?.to_string();
    let text = voice::after_wake_word(&text).filter(|rest| !rest.is_empty()).unwrap_or(text);
    // Remembering and forgetting are the person's own say, and need no model.
    if let Some(said) = memory::said(&text) {
        return about_memory(app, actor, said);
    }
    let searching = asks_to_search(&text);
    if searching && assistant.agent.is_none() {
        return Ok(json!({ "say": NO_SEARCH, "results": [] }));
    }
    let weather_asked = wants_weather(&text);

    // What the parser already understands, or has been learned, needs no model and happens at once.
    let command = without_politeness(&text);
    let asks = if command.len() == text.len() {
        is_question(&text)
    } else {
        QUESTION_WORDS.contains(&first_word(command).as_str())
    };
    let commanding = !searching && !weather_asked && !asks;
    // The home's words: its devices' names, its rooms and the plugins' nouns.
    let lexicon = voice::lexicon_for(app)?;
    let phrase = if commanding { memory::phrase_key(command) } else { String::new() };
    if commanding {
        if let Some(dropped) = take_back(app, assistant, actor, &phrase)? {
            if TAKE_BACKS.contains(&phrase.as_str()) {
                return Ok(reply_of(&format!("Got it. “{dropped}” won't do that again.")));
            }
        }
        let parsed = voice::parse(command, &lexicon);
        if acts_at_once(&parsed.resolution) {
            let reply = voice::dispatch(app, actor, parsed, Channel::Assistant).await?;
            return Ok(json!({ "say": summary(&reply), "results": [reply] }));
        }
        if let Some(reply) = run_learned(app, assistant, actor, &phrase).await? {
            return Ok(reply);
        }
    }

    let notes = app.store.with(|conn| memory::notes(conn, &actor.user_id))?;
    let index = app.registry.index();
    let lists = app.store.with(|conn| home_lists(conn, &index, actor.role))?;
    let about_home =
        !searching && !weather_asked && ((request.command && !asks) || about_the_home(&text, &lists, &lexicon));
    if !about_home && !weather_asked && assistant.agent.is_some() {
        // The agent keeps its own conversation, so the notes go with the first message of a chat.
        let first = request.messages.iter().filter(|message| message.role == "user").count() == 1;
        let message = if first && !notes.is_empty() { format!("{notes}\n\nWhat they said:\n{text}") } else { text };
        return crate::agent::start(app, actor, &request.chat, message, request.spoken);
    }
    if !about_home {
        let weather = if weather_asked {
            Some(assistant.forecast().await.unwrap_or_else(|| "The weather forecast could not be fetched.".to_string()))
        } else {
            None
        };
        let messages = with_history(general_instructions(&request.context, weather.as_deref(), &notes), &request);
        return crate::agent::start_model(app, actor, messages);
    }

    let Ok(_turn) = assistant.one_at_a_time.try_lock() else {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "assistant_busy",
            "Swifty is still answering. Try again in a moment.",
        ));
    };
    let deadline = Instant::now() + assistant.budget;

    let messages = home_history(
        instructions(&index, actor.role, &lists, &request.context, request.command, &notes),
        &request,
        &lists,
        &lexicon,
    );
    let plan = read_plan(&assistant.complete(&messages, deadline).await?);

    let ran = carry_out(app, actor, plan.actions).await?;
    let noted = keep_notes(app, actor, plan.remember)?;
    if commanding {
        learn_or_note(app, actor, &phrase, &ran)?;
    }
    let results: Vec<Value> = ran.into_iter().map(|(_, reply)| reply).collect();
    voice::audit_on(app, actor, Channel::Assistant, "assistant", "-", Value::Null, "answered")?;
    let wanted_a_tool = plan.name.is_some();
    // What happened beats what the model said it would do.
    let done: Vec<String> = results.iter().filter(|result| result["kind"] == "executed").map(summary).collect();
    let say = if done.is_empty() {
        plan.say.filter(|say| !say.trim().is_empty()).unwrap_or_else(|| {
            if wanted_a_tool {
                NO_SEARCH.to_string()
            } else if results.is_empty() {
                "I do not have an answer for that.".to_string()
            } else {
                "Done.".to_string()
            }
        })
    } else {
        done.join(" ")
    };
    let mut reply = json!({ "say": clip(&say, MAX_SAY), "results": results });
    if !noted.is_empty() {
        reply["memory"] = json!(noted);
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_is_never_shown_even_half_written() {
        assert_eq!(visible_so_far("Hello"), "Hello");
        assert_eq!(visible_so_far("<think>secret"), "");
        assert_eq!(visible_so_far("<think>secret</think>Hello"), "Hello");
        assert_eq!(visible_so_far("Hello <thi"), "Hello ");
    }

    #[test]
    fn events_build_up_the_answer_across_broken_pieces() {
        let piece = |words: &str| format!("data: {}\n\n", json!({ "choices": [{ "delta": { "content": words } }] }));
        let all = format!("{}{}data: [DONE]\n\n", piece("Caf\u{e9} "), piece("time."));
        let mut reading = Streamed::default();
        let mut seen = Vec::new();
        for byte in all.as_bytes() {
            if let Some(text) = reading.feed(std::slice::from_ref(byte)) {
                seen.push(text);
            }
        }
        assert_eq!(seen, ["Caf\u{e9} ", "Caf\u{e9} time."]);
        assert_eq!(reading.finish().unwrap(), "Caf\u{e9} time.");
    }

    #[test]
    fn a_polite_command_is_still_a_command() {
        assert_eq!(without_politeness("can u turn off the lights"), "turn off the lights");
        assert_eq!(without_politeness("Please dim the bedroom"), "dim the bedroom");
        assert_eq!(without_politeness("what is the volume"), "what is the volume");
    }

    #[test]
    fn every_action_offered_is_in_the_catalog_and_nothing_dangerous_is() {
        for (action, _) in ACTIONS {
            assert!(catalog::find(action).is_some(), "{action}");
        }
        for prefix in ["vm.", "host.", "print.", "router.", "passkey.", "machine.", "user."] {
            assert!(!ACTIONS.iter().any(|(action, _)| action.starts_with(prefix)), "{prefix}");
        }
    }

    #[test]
    fn family_is_only_offered_what_family_may_run() {
        let index = crate::runtime::index::legacy_seeded();
        let action_lines = |role| action_lines(&index, role);
        assert!(action_lines(Role::Owner).contains("plug.power"));
        assert!(!action_lines(Role::Family).contains("plug.power"));
        assert!(action_lines(Role::Owner).contains("ac.temperature"));
        assert!(!action_lines(Role::Family).contains("ac.temperature"));
        assert!(action_lines(Role::Family).contains("room.power"));
    }

    #[test]
    fn a_plan_is_read_from_json_or_taken_as_the_answer() {
        let plan =
            read_plan(r#"Sure! {"say":"Dimming.","do":[{"action":"room.power","target":"a","params":{"on":false}}]}"#);
        assert_eq!(plan.say.as_deref(), Some("Dimming."));
        assert!(plan.actions.unwrap().is_array());
        let plan = read_plan("<think>hmm {not json}</think>Just words.");
        assert_eq!(plan.say.as_deref(), Some("Just words."));
        assert!(plan.actions.is_none());
    }

    #[test]
    fn questions_searches_and_weather_are_told_apart() {
        assert!(is_question("how do I turn off the lights in a car"));
        assert!(is_question("the lights are off, right?"));
        assert!(!is_question("turn off the lights"));
        assert!(asks_to_search("Search for cheap flights?"));
        assert!(asks_to_search("look up  rust traits"));
        assert!(!asks_to_search("turn off the lights"));
        assert!(wants_weather("What is the weather tomorrow"));
        assert!(wants_weather("will it rain"));
        assert!(wants_weather("temperature outside"));
        assert!(!wants_weather("cpu temperature"));
        assert!(!wants_weather("training"));
    }

    #[test]
    fn a_forecast_reads_as_plain_lines() {
        let data = json!({
            "current": { "temperature_2m": 7.4, "weather_code": 61 },
            "daily": {
                "time": ["2026-11-10"],
                "weather_code": [3],
                "temperature_2m_min": [2.1],
                "temperature_2m_max": [9.6],
                "precipitation_probability_max": [40],
                "wind_speed_10m_max": [18.2]
            }
        });
        let text = forecast_text("Home", &data).unwrap();
        assert!(text.contains("Now: 7 °C, rain."), "{text}");
        assert!(text.contains("2026-11-10: overcast, 2 to 10 °C, 40% chance of rain, wind up to 18 km/h."), "{text}");
    }

    #[test]
    fn a_turn_is_about_the_home_only_by_the_persons_own_words() {
        let lexicon = Lexicon::build(&crate::runtime::index::legacy_seeded(), Vec::new(), voice::LEGACY_WORDS);
        let about_the_home = |text: &str, lists: &str| about_the_home(text, lists, &lexicon);
        assert!(about_the_home("is the sonos on", ""), "a plugin's noun for a speaker");
        assert!(!about_the_home("what is the uptime of this machine", ""), "machines are not Swifty's to act on");
        assert!(!about_the_home("is the proxmox server up", ""));
        let lists = "Rooms: a1 = Office\nLights: hue-1 = Lightstrip\nSockets: plug-x = Desk";
        assert!(about_the_home("it is too bright in here", lists));
        assert!(about_the_home("do something about the office", lists));
        assert!(!about_the_home("what happened in the news today", lists));
        assert!(!about_the_home("tell me a joke", lists));
        assert!(about_the_home("Um set A C to twenty-seven degrees.", lists), "A C is the air conditioner");
        assert!(about_the_home("set the a.c. to 22", lists));
        assert!(about_the_home("set the air con to cool", lists));
        assert!(!about_the_home("what is a cat", lists));
    }
}
