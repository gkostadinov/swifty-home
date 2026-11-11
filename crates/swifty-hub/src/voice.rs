//! The voice route: a WAV goes to the speech service, and the returned text becomes
//! a catalog action through a small deterministic parser.
//!
//! Tiers 0 and 1 execute. Tier 2 comes back prepared and needs a tap. Tier 3 can
//! only be prepared, because a voice can never supply the step-up. Low confidence,
//! an unclear target, a negation, an "or", or more than one target or value
//! returns choices and runs nothing. Audio and text are never stored: the audit
//! log gets only the resolved action.
//!
//! The grammar here (verbs, negation, disjunction, class nouns, numbers and the tier
//! routing) names no device, product or house. Device names, rooms and the plugins'
//! brand words arrive for each request in a [`Lexicon`] built from the device index.

pub mod lexicon;

use std::collections::HashSet;
use std::time::Duration;

use axum::http::StatusCode;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use serde_json::{json, Value};
use swifty_plugin_api::{CapKind, DeviceClass, Secret, State};

#[cfg(test)]
pub use lexicon::LEGACY_WORDS;
pub use lexicon::{register_plugin_words, registered_words, DroppedWord, Lexicon, PluginWords, SpokenDevice};

use crate::actions::{self, Actor};
use crate::app::App;
use crate::app::StartupError;
use crate::audit::Event;
use crate::catalog::{self, ACK_PARAM};
use crate::config::{is_loopback, read_secret_file, SpeechConfig};
use crate::error::ApiError;
use crate::policy::{self, Channel, VoiceRoute};
use crate::rooms;
use crate::sessions;

/// Below this the words are not trusted enough to act on.
pub const MIN_CONFIDENCE: f64 = 0.6;
const VOLUME_STEP: i64 = 5;

pub struct SpeechClient {
    endpoint: reqwest::Url,
    token: Secret,
    http: reqwest::Client,
    on_loopback: bool,
}

#[derive(Deserialize)]
struct SpeechReply {
    text: String,
    /// The speech service does not send this today. It is honoured if it ever does.
    #[serde(default)]
    confidence: Option<f64>,
}

impl SpeechClient {
    pub fn new(config: &SpeechConfig) -> Result<Self, StartupError> {
        let token = read_secret_file(&config.token_file)?;
        let base = reqwest::Url::parse(&config.url)
            .map_err(|e| crate::config::ConfigError::Invalid(format!("speech.url: {e}")))?;
        let endpoint =
            base.join("/v1/transcribe").map_err(|e| crate::config::ConfigError::Invalid(format!("speech.url: {e}")))?;
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(config.connect_timeout_secs))
            .timeout(Duration::from_secs(config.timeout_secs))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self { on_loopback: is_loopback(&base), endpoint, token, http })
    }

    async fn transcribe(&self, wav: Vec<u8>) -> Result<SpeechReply, ApiError> {
        let unavailable =
            |message: &str| ApiError::new(StatusCode::BAD_GATEWAY, "speech_unavailable", message.to_string());
        let response = self
            .http
            .post(self.endpoint.clone())
            .header(AUTHORIZATION, format!("Bearer {}", self.token.expose()))
            .header(CONTENT_TYPE, "audio/wav")
            .body(wav)
            .send()
            .await
            .map_err(|_| unavailable("The speech service did not answer. Nothing ran."))?;
        if !response.status().is_success() {
            tracing::warn!(status = %response.status(), "speech service refused a request");
            return Err(unavailable("The speech service could not read that. Nothing ran."));
        }
        response
            .json::<SpeechReply>()
            .await
            .map_err(|_| unavailable("The speech service sent an unreadable reply. Nothing ran."))
    }
}

/// A fully-formed catalog request the parser arrived at.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub action: &'static str,
    pub target: String,
    pub params: Value,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    Action(Candidate),
    Choices {
        why: String,
        options: Vec<Candidate>,
    },
    VolumeStep {
        delta: i64,
    },
    /// A spoken yes, which satisfies no guard.
    Ignored {
        why: String,
    },
    Nav {
        page: String,
    },
    Help,
    Lock,
    Nothing {
        why: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub normalized: String,
    pub resolution: Resolution,
    pub confidence: f64,
}

/// What the recogniser writes for words of the grammar itself. Mishearings of names
/// come from the plugins, through the [`Lexicon`].
const SPOKEN_FORMS: &[(&[&str], &[&str])] = &[
    // "A C", "a.c." and "a/c" all come out as two letters once the punctuation is gone.
    (&["a", "c"], &["ac"]),
    (&["air", "con"], &["aircon"]),
    (&["blue", "tooth"], &["bluetooth"]),
];

const UNITS: &[(&str, i64)] = &[
    ("zero", 0),
    ("one", 1),
    ("two", 2),
    ("three", 3),
    ("four", 4),
    ("five", 5),
    ("six", 6),
    ("seven", 7),
    ("eight", 8),
    ("nine", 9),
    ("ten", 10),
    ("eleven", 11),
    ("twelve", 12),
    ("thirteen", 13),
    ("fourteen", 14),
    ("fifteen", 15),
    ("sixteen", 16),
    ("seventeen", 17),
    ("eighteen", 18),
    ("nineteen", 19),
];
const TENS: &[(&str, i64)] = &[
    ("twenty", 20),
    ("thirty", 30),
    ("forty", 40),
    ("fifty", 50),
    ("sixty", 60),
    ("seventy", 70),
    ("eighty", 80),
    ("ninety", 90),
];

fn is_number(word: &str) -> bool {
    !word.is_empty() && word.chars().all(|c| c.is_ascii_digit())
}

/// Lower-cases, strips punctuation, applies the grammar's spoken forms and the plugins'
/// mishearings and turns number words into digits. Words that speech split apart are
/// joined back when together they are one of `joinable`.
pub(crate) fn normalize_with(
    raw: &str,
    aliases: &[(Vec<String>, Vec<String>)],
    joinable: Option<&HashSet<String>>,
) -> String {
    let lowered = raw.to_lowercase();
    let cleaned: String = lowered.chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
    let tokens: Vec<&str> = cleaned.split_whitespace().collect();

    let mut aliased: Vec<&str> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    'outer: while i < tokens.len() {
        for (from, to) in SPOKEN_FORMS {
            if tokens[i..].starts_with(from) {
                aliased.extend_from_slice(to);
                i += from.len();
                continue 'outer;
            }
        }
        for (from, to) in aliases {
            if tokens.len() - i >= from.len() && tokens[i..i + from.len()].iter().zip(from).all(|(a, b)| *a == b) {
                aliased.extend(to.iter().map(String::as_str));
                i += from.len();
                continue 'outer;
            }
        }
        aliased.push(tokens[i]);
        i += 1;
    }

    let value_of = |list: &[(&str, i64)], word: &str| list.iter().find(|(w, _)| *w == word).map(|(_, v)| *v);
    let mut out: Vec<String> = Vec::with_capacity(aliased.len());
    let mut i = 0;
    while i < aliased.len() {
        let word = aliased[i];
        let next = aliased.get(i + 1).copied();
        if (word == "a" || word == "one") && next == Some("hundred") {
            out.push("100".to_string());
            i += 2;
        } else if let Some(tens) = value_of(TENS, word) {
            match next.and_then(|n| value_of(UNITS, n)).filter(|u| (1..=9).contains(u)) {
                Some(unit) => {
                    out.push((tens + unit).to_string());
                    i += 2;
                }
                None => {
                    out.push(tens.to_string());
                    i += 1;
                }
            }
        } else if let Some(unit) = value_of(UNITS, word) {
            out.push(unit.to_string());
            i += 1;
        } else {
            out.push(word.to_string());
            i += 1;
        }
    }
    match joinable {
        Some(words) => join_split_words(out, words).join(" "),
        None => out.join(" "),
    }
}

/// "file store" is "filestore" when that is a name here. Only words that mean nothing
/// to the grammar are joined, so a name can never swallow a "no" or a number.
fn join_split_words(tokens: Vec<String>, words: &HashSet<String>) -> Vec<String> {
    let joinable = |token: &String| !is_number(token) && !is_grammar_word(token);
    let mut out = Vec::with_capacity(tokens.len());
    let mut i = 0;
    'outer: while i < tokens.len() {
        for n in [3, 2] {
            let Some(run) = tokens.get(i..i + n) else { continue };
            if run.iter().all(joinable) {
                let joined = run.concat();
                if words.contains(&joined) {
                    out.push(joined);
                    i += n;
                    continue 'outer;
                }
            }
        }
        out.push(tokens[i].clone());
        i += 1;
    }
    out
}

/// Lower-cases, strips punctuation, applies the spoken forms and today's plugin
/// mishearings, and turns number words into digits. This knows no device or room;
/// the parser normalises with [`Lexicon::normalize`], which does.
pub fn normalize(raw: &str) -> String {
    Lexicon::plain().normalize(raw)
}

const FILLER: &[&str] = &[
    "please", "the", "a", "an", "can", "could", "would", "will", "you", "my", "to", "of", "it", "now", "hey", "um",
    "uh", "uhm", "er", "for", "me", "i", "want", "need", "like", "just", "at", "in", "is", "are", "and", "do", "some",
    "this", "that", "set", "make", "turn", "switch", "with", "pets", "up", "down",
];

/// The grammar's own words. Names of devices, rooms and products are not here: they
/// come from the [`Lexicon`].
const KNOWN: &[&str] = &[
    "start",
    "boot",
    "power",
    "bring",
    "stop",
    "shutdown",
    "shut",
    "halt",
    "restart",
    "reboot",
    "reset",
    "bounce",
    "vm",
    "vms",
    "machine",
    "host",
    "server",
    "wireguard",
    "vpn",
    "router",
    "tunnel",
    "print",
    "printer",
    "printing",
    "pause",
    "resume",
    "status",
    "state",
    "feed",
    "feeder",
    "grams",
    "gram",
    "g",
    "clean",
    "litter",
    "scoop",
    "dispense",
    "scene",
    "evening",
    "focus",
    "movie",
    "all",
    "off",
    "everything",
    "on",
    "tv",
    "television",
    "hdmi",
    "live",
    "input",
    "mute",
    "unmute",
    "silence",
    "quiet",
    "quieter",
    "louder",
    "softer",
    "more",
    "less",
    "lower",
    "volume",
    "speaker",
    "sound",
    "music",
    "loud",
    "living",
    "room",
    "bulb",
    "bulbs",
    "light",
    "lights",
    "lamp",
    "lamps",
    "dim",
    "brighten",
    "bright",
    "brighter",
    "percent",
    // A colour, as in "light.hue". Never a light: a plugin noun "hue" is dropped.
    "hue",
    "red",
    "orange",
    "yellow",
    "green",
    "cyan",
    "blue",
    "purple",
    "pink",
    "warm",
    "cool",
    "white",
    "what",
    "how",
    "much",
    "which",
    "running",
    "temp",
    "temperature",
    "cpu",
    "package",
    "core",
    "processor",
    "hot",
    "watts",
    "wattage",
    "memory",
    "ram",
    "storage",
    "disk",
    "space",
    "bluetooth",
    "bt",
    "delay",
    "go",
    "open",
    "show",
    "page",
    "deck",
    "monitor",
    "control",
    "preview",
    "iot",
    "log",
    "keys",
    "help",
    "lock",
    "emergency",
    "estop",
    "e",
    "kill",
    "every",
    "brightness",
];

/// The grammar's class nouns. Plugins add brand words to these through the [`Lexicon`].
pub(crate) const BUILT_IN_NOUNS: &[(&str, DeviceClass)] = &[
    ("light", DeviceClass::Light),
    ("lights", DeviceClass::Light),
    ("lamp", DeviceClass::Light),
    ("lamps", DeviceClass::Light),
    ("bulb", DeviceClass::Light),
    ("bulbs", DeviceClass::Light),
    ("tv", DeviceClass::Tv),
    ("television", DeviceClass::Tv),
    ("speaker", DeviceClass::Speaker),
    ("printer", DeviceClass::Printer),
    ("feeder", DeviceClass::Feeder),
    ("litter", DeviceClass::Litter),
    ("vm", DeviceClass::Vm),
    ("vms", DeviceClass::Vm),
    ("machine", DeviceClass::Vm),
    ("host", DeviceClass::Host),
    ("server", DeviceClass::Host),
    ("router", DeviceClass::Router),
    ("tunnel", DeviceClass::Router),
    ("vpn", DeviceClass::Router),
    ("wireguard", DeviceClass::Router),
    ("ac", DeviceClass::Aircon),
    ("aircon", DeviceClass::Aircon),
    ("plug", DeviceClass::Plug),
    ("plugs", DeviceClass::Plug),
    ("socket", DeviceClass::Plug),
    ("sockets", DeviceClass::Plug),
];

/// A spoken yes, which the parser recognises in order to ignore it.
const ANSWERS: &[&str] = &["yes", "yeah", "confirm", "ok", "okay"];

/// Whether the grammar uses this word itself, so it can never be part of a name.
pub(crate) fn is_grammar_word(word: &str) -> bool {
    KNOWN.contains(&word)
        || FILLER.contains(&word)
        || NEGATIONS.contains(&word)
        || DISJUNCTIONS.contains(&word)
        || ANSWERS.contains(&word)
        || BUILT_IN_NOUNS.iter().any(|(noun, _)| *noun == word)
}

/// How many of the content words are recognised, also counting `extra`.
fn confidence(tokens: &[&str], lexicon: &Lexicon, extra: &[&str]) -> f64 {
    let content: Vec<&&str> = tokens.iter().filter(|t| !FILLER.contains(t)).collect();
    if content.is_empty() {
        return 0.0;
    }
    let known = content
        .iter()
        .filter(|t| KNOWN.contains(t) || extra.contains(t) || is_number(t) || is_compact_number(t) || lexicon.knows(t))
        .count();
    known as f64 / content.len() as f64
}

/// A room as voice knows it. `key` and the scene keys are in normalised form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpokenRoom {
    pub id: String,
    pub name: String,
    pub key: String,
    /// Each scene's name and key.
    pub scenes: Vec<(String, String)>,
}

/// Whether a phrase is a word the parser already acts on, so it cannot name a room,
/// a scene or a plugin's noun without changing what that word does.
pub fn is_reserved_phrase(key: &str) -> bool {
    let tokens: Vec<&str> = key.split_whitespace().collect();
    match tokens.as_slice() {
        [] => true,
        [word] => is_grammar_word(word) || is_number(word),
        _ => tokens.iter().all(|word| FILLER.contains(word) || NEGATIONS.contains(word) || DISJUNCTIONS.contains(word)),
    }
}

/// Where each named room is in the words: `(start, length, room)`, longest match
/// first, and never overlapping.
fn find_rooms<'r>(tokens: &[&str], rooms: &'r [SpokenRoom]) -> Vec<(usize, usize, &'r SpokenRoom)> {
    let mut candidates: Vec<(usize, usize, &SpokenRoom)> = Vec::new();
    for room in rooms {
        let words: Vec<&str> = room.key.split_whitespace().collect();
        if words.is_empty() || words.len() > tokens.len() {
            continue;
        }
        for start in 0..=tokens.len() - words.len() {
            if tokens[start..start + words.len()] == words[..] {
                candidates.push((start, words.len(), room));
            }
        }
    }
    candidates.sort_by_key(|(start, length, _)| (std::cmp::Reverse(*length), *start));
    let mut chosen: Vec<(usize, usize, &SpokenRoom)> = Vec::new();
    for candidate in candidates {
        let (start, length, _) = candidate;
        if !chosen.iter().any(|(s, l, _)| start < s + l && *s < start + length) {
            chosen.push(candidate);
        }
    }
    chosen.sort_by_key(|(start, _, _)| *start);
    chosen
}

fn is_compact_number(token: &str) -> bool {
    let prefixes = ["bulb", "light", "lamp", "hdmi", "vm"];
    if prefixes
        .iter()
        .any(|p| token.strip_prefix(p).is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())))
    {
        return true;
    }
    token.strip_suffix('g').is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

fn candidate(action: &'static str, target: &str, params: Value, label: impl Into<String>) -> Candidate {
    Candidate { action, target: target.to_string(), params, label: label.into() }
}

const NEGATIONS: &[&str] = &["not", "never", "no", "dont", "cant", "cannot", "wont"];
const DISJUNCTIONS: &[&str] = &["or", "either"];
/// The nouns a light's number follows: "bulb 2", "light 2", "lamp2".
const BULB_WORDS: &[&str] = &["bulb", "light", "lamp"];
/// The noun a machine's number follows: "vm 100", "vm100".
const VM_WORDS: &[&str] = &["vm"];

/// A kind of thing the parser acts on: its own words, and the classes whose nouns and
/// device names also name it.
struct Group {
    words: &'static [&'static str],
    classes: &'static [DeviceClass],
}

static LIGHTS: Group = Group {
    words: &["bulb", "bulbs", "light", "lights", "lamp", "lamps", "evening", "focus", "movie"],
    classes: &[DeviceClass::Light],
};
static TVS: Group = Group { words: &["tv", "television"], classes: &[DeviceClass::Tv] };
static AUDIO: Group = Group { words: &["volume", "speaker", "music", "sound"], classes: &[DeviceClass::Speaker] };
static PRINTING: Group = Group { words: &["print", "printer", "printing"], classes: &[DeviceClass::Printer] };
static PETS: Group = Group {
    words: &["feed", "dispense", "feeder", "litter", "scoop", "clean"],
    classes: &[DeviceClass::Feeder, DeviceClass::Litter],
};
static TUNNEL: Group = Group { words: &["wireguard", "vpn", "router", "tunnel"], classes: &[DeviceClass::Router] };
static MACHINES: Group = Group { words: &["vm", "vms", "machine"], classes: &[DeviceClass::Vm] };
/// Not one of [`GROUPS`]: "the vm on the host" is one machine, and says where it is.
static HOSTS: Group = Group { words: &["host", "server"], classes: &[DeviceClass::Host] };
/// Kinds of thing. Naming two kinds is two targets, and the parser below only ever acts
/// on one. Scene names count as lights.
static GROUPS: &[&Group] = &[&LIGHTS, &TVS, &AUDIO, &PRINTING, &PETS, &TUNNEL, &MACHINES];

const HUE_NAMES: &[(&str, i64)] = &[
    ("red", 0),
    ("orange", 28),
    ("yellow", 52),
    ("green", 120),
    ("cyan", 185),
    ("blue", 225),
    ("purple", 270),
    ("pink", 320),
    ("warm", 34),
    ("cool", 210),
    ("white", 40),
];
const PAGES: &[&str] = &["deck", "monitor", "control", "preview", "iot", "log", "keys"];

const NO_PRINTER: &str = "There is no printer here.";
const NO_FEEDER: &str = "There is no feeder here.";
const NO_LITTER: &str = "There is no litter box here.";
const NO_TV: &str = "There is no TV here.";
const NO_SPEAKER: &str = "There is no speaker here.";
const NO_HOST: &str = "There is no host here.";
const NO_ROUTER: &str = "There is no router here.";
const NO_READINGS: &str = "Nothing here reports that.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Verb {
    Start,
    Stop,
    Restart,
}

impl Verb {
    fn action(self) -> &'static str {
        match self {
            Verb::Start => "vm.start",
            Verb::Stop => "vm.stop",
            Verb::Restart => "vm.restart",
        }
    }

    fn word(self) -> &'static str {
        match self {
            Verb::Start => "Start",
            Verb::Stop => "Stop",
            Verb::Restart => "Restart",
        }
    }
}

fn vm_candidate(verb: Verb, vm: &SpokenDevice) -> Candidate {
    candidate(verb.action(), &vm.target, json!({}), format!("{} {}", verb.word(), vm.name))
}

/// Every machine there is, for this verb. With no machines, nothing.
fn vm_choices(lexicon: &Lexicon, verb: Verb, why: &str) -> Resolution {
    Resolution::Choices {
        why: why.to_string(),
        options: lexicon.of_class(DeviceClass::Vm).map(|vm| vm_candidate(verb, vm)).collect(),
    }
}

struct Words<'a> {
    text: String,
    tokens: Vec<&'a str>,
    lexicon: &'a Lexicon,
}

impl<'a> Words<'a> {
    fn of(text: &'a str, lexicon: &'a Lexicon) -> Self {
        Words { text: text.to_string(), tokens: text.split_whitespace().collect(), lexicon }
    }

    fn has(&self, word: &str) -> bool {
        self.tokens.contains(&word)
    }

    fn any(&self, words: &[&str]) -> bool {
        words.iter().any(|w| self.has(w))
    }

    fn phrase(&self, phrase: &str) -> bool {
        format!(" {} ", self.text).contains(&format!(" {phrase} "))
    }

    fn numbers(&self) -> Vec<i64> {
        self.tokens.iter().filter_map(|t| t.parse().ok()).collect()
    }

    /// Every number after `word`, as in "bulb 2", or joined to it, as in "bulb2".
    fn numbers_after(&self, words: &[&str]) -> Vec<i64> {
        let mut found = Vec::new();
        for (i, token) in self.tokens.iter().enumerate() {
            for word in words {
                let number: Option<i64> = if token == word {
                    self.tokens.get(i + 1).and_then(|t| t.parse().ok())
                } else {
                    token.strip_prefix(word).and_then(|rest| rest.parse().ok())
                };
                found.extend(number);
            }
        }
        found
    }

    fn number_after(&self, words: &[&str]) -> Option<i64> {
        self.numbers_after(words).first().copied()
    }

    fn negated(&self) -> bool {
        self.any(NEGATIONS) || self.tokens.windows(2).any(|pair| pair[1] == "t" && pair[0].ends_with('n'))
    }

    /// Whether a word here names only these classes of device: as a class noun, a
    /// plugin's noun, or the name of a device of that class.
    fn names(&self, classes: &[DeviceClass]) -> bool {
        self.tokens.iter().any(|token| {
            let named = self.lexicon.classes_of(token);
            !named.is_empty() && named.iter().all(|class| classes.contains(class))
        })
    }

    fn names_group(&self, group: &Group) -> bool {
        let names = |word: &str, token: &str| {
            token == word
                || token
                    .strip_prefix(word)
                    .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
        };
        group.words.iter().any(|word| self.tokens.iter().any(|token| names(word, token))) || self.names(group.classes)
    }

    fn names_host(&self) -> bool {
        self.names_group(&HOSTS)
    }

    /// How many different kinds of thing were named.
    fn groups_named(&self) -> usize {
        let groups = GROUPS.iter().filter(|group| self.names_group(group)).count();
        // "The vm on the host" is one target and says where it is.
        if self.names_host() && !self.names_group(&MACHINES) {
            groups + 1
        } else {
            groups
        }
    }

    /// Whether a word here calls this device by its own name.
    fn calls(&self, device: &SpokenDevice) -> bool {
        self.tokens.iter().any(|token| self.lexicon.calls(token, device))
    }

    /// The devices of a class called by a word that tells them apart: "desk" when one
    /// lamp is the desk lamp, but not a word every one of them shares.
    fn called(&self, class: DeviceClass) -> Vec<&'a SpokenDevice> {
        let lexicon = self.lexicon;
        let all: Vec<&'a SpokenDevice> = lexicon.of_class(class).collect();
        let telling: Vec<&str> = self
            .tokens
            .iter()
            .copied()
            .filter(|token| {
                let callees = all.iter().filter(|device| lexicon.calls(token, device)).count();
                callees > 0 && (callees < all.len() || all.len() == 1)
            })
            .collect();
        all.into_iter().filter(|device| telling.iter().any(|token| lexicon.calls(token, device))).collect()
    }

    fn named_bulbs(&self) -> Vec<i64> {
        let mut bulbs = self.numbers_after(BULB_WORDS);
        bulbs.sort_unstable();
        bulbs.dedup();
        bulbs
    }

    /// The lights named by number or by their own names, each once.
    fn named_lights(&self) -> Vec<&'a SpokenDevice> {
        let mut lights: Vec<&'a SpokenDevice> = Vec::new();
        for n in self.named_bulbs() {
            lights.extend(self.lexicon.of_class(DeviceClass::Light).filter(|light| light.numbered(BULB_WORDS, n)));
        }
        lights.extend(self.called(DeviceClass::Light));
        let mut seen = HashSet::new();
        lights.retain(|light| seen.insert(light.target.clone()));
        lights
    }

    /// Whether this number is what a machine here is called after "vm".
    fn is_vm_number(&self, n: i64) -> bool {
        self.lexicon.of_class(DeviceClass::Vm).any(|vm| vm.numbered(VM_WORDS, n))
    }

    /// VMs named outright, and by bare number when nothing else here takes a number.
    fn named_vms(&self) -> Vec<&'a SpokenDevice> {
        let by_id = self.verb().is_some() && !self.names_a_non_vm_device();
        let ids = self.numbers_after(VM_WORDS);
        let numbers = self.numbers();
        self.lexicon
            .of_class(DeviceClass::Vm)
            .filter(|vm| {
                self.calls(vm)
                    || ids.iter().any(|n| vm.numbered(VM_WORDS, *n))
                    || (by_id && numbers.iter().any(|n| vm.numbered(VM_WORDS, *n)))
            })
            .collect()
    }

    fn names_a_non_vm_device(&self) -> bool {
        GROUPS.iter().filter(|group| !std::ptr::eq(**group, &MACHINES)).any(|group| self.names_group(group))
            || !self.named_bulbs().is_empty()
    }

    /// Numbers that are values, not part of a name such as "bulb 2" or "hdmi 2".
    fn values(&self) -> Vec<i64> {
        let by_id = self.verb().is_some() && !self.names_a_non_vm_device();
        let mut values = Vec::new();
        for (i, token) in self.tokens.iter().enumerate() {
            let Ok(value) = token.parse::<i64>() else { continue };
            let names_something = i
                .checked_sub(1)
                .is_some_and(|before| matches!(self.tokens[before], "bulb" | "light" | "lamp" | "hdmi" | "vm"));
            let is_vm_id = by_id && self.is_vm_number(value);
            if !names_something && !is_vm_id {
                values.push(value);
            }
        }
        values
    }

    /// Two numbers joined by "and", as in "bulbs one and two".
    fn joined_numbers(&self) -> bool {
        let number = |token: &str| token.parse::<i64>().is_ok();
        self.tokens.windows(3).any(|w| number(w[0]) && w[1] == "and" && number(w[2]))
    }

    fn names_several(&self) -> bool {
        self.groups_named() > 1
            || self.named_bulbs().len() > 1
            || self.named_lights().len() > 1
            || self.named_vms().len() > 1
            || self.values().len() > 1
            || self.joined_numbers()
    }

    /// Why this cannot be taken as one clear instruction, if it cannot.
    fn ambiguity(&self) -> Option<&'static str> {
        if self.negated() {
            return Some("That sounded like a \"not\", so nothing ran. Tap if you did mean this.");
        }
        if self.any(DISJUNCTIONS) {
            return Some("That offered more than one option, so nothing ran. Pick one.");
        }
        if self.names_several() {
            return Some("That named more than one thing, so nothing ran. Pick one.");
        }
        None
    }

    fn verb(&self) -> Option<Verb> {
        if self.any(&["restart", "reboot", "reset", "bounce"]) {
            Some(Verb::Restart)
        } else if self.any(&["stop", "shutdown", "halt"]) || self.phrase("shut down") || self.phrase("power off") {
            Some(Verb::Stop)
        } else if self.any(&["start", "boot"]) || self.phrase("power on") || self.phrase("bring up") {
            Some(Verb::Start)
        } else {
            None
        }
    }

    /// The first machine named outright or by number.
    fn vm(&self) -> Option<&'a SpokenDevice> {
        let numbers: Vec<i64> = self.numbers().into_iter().chain(self.numbers_after(VM_WORDS)).collect();
        self.lexicon
            .of_class(DeviceClass::Vm)
            .find(|vm| self.calls(vm) || numbers.iter().any(|n| vm.numbered(VM_WORDS, *n)))
    }
}

fn nothing(why: &str) -> Resolution {
    Resolution::Nothing { why: Some(why.to_string()) }
}

fn action(c: Candidate) -> Resolution {
    Resolution::Action(c)
}

fn read(target: &str, metric: Option<&str>, label: &str) -> Candidate {
    let params = match metric {
        Some(metric) => json!({ "metric": metric }),
        None => json!({}),
    };
    candidate("state.read", target, params, label)
}

/// One action on a device of a class that can take it. With several, the ones named
/// outright, or else a choice. With none, a plain answer and nothing runs.
fn on_one(
    w: &Words<'_>,
    class: DeviceClass,
    cap: Option<CapKind>,
    none: &str,
    build: impl Fn(&SpokenDevice) -> Candidate,
) -> Resolution {
    let able: Vec<&SpokenDevice> =
        w.lexicon.of_class(class).filter(|device| cap.is_none_or(|c| device.has(c))).collect();
    let named: Vec<&SpokenDevice> = able.iter().copied().filter(|device| w.calls(device)).collect();
    let chosen = if named.is_empty() { able } else { named };
    match chosen.as_slice() {
        [] => nothing(none),
        [one] => action(build(one)),
        several => Resolution::Choices {
            why: "Which one?".to_string(),
            options: several.iter().map(|device| build(device)).collect(),
        },
    }
}

fn classify(words: &Words<'_>) -> Resolution {
    let w = words;
    if w.tokens.is_empty() {
        return Resolution::Nothing { why: None };
    }
    let first = w.tokens[0];
    if ANSWERS.contains(&first)
        || w.text == "do it"
        || w.text.starts_with("do it ")
        || w.text == "go ahead"
        || w.text.starts_with("go ahead ")
    {
        return Resolution::Ignored { why: "A spoken yes satisfies no guard. Nothing ran.".to_string() };
    }
    if matches!(w.text.as_str(), "help" | "what can i say" | "what can you do") {
        return Resolution::Help;
    }
    const NAV_WORDS: &[&str] = &["go", "to", "open", "show", "page", "switch", "the"];
    let remaining: Vec<&&str> = w.tokens.iter().filter(|t| !NAV_WORDS.contains(t)).collect();
    if remaining.len() == 1 && PAGES.contains(remaining[0]) {
        return Resolution::Nav { page: remaining[0].to_string() };
    }
    if matches!(w.text.as_str(), "lock" | "lock deck" | "lock the deck") {
        return Resolution::Lock;
    }
    if w.phrase("emergency stop")
        || w.phrase("e stop")
        || w.has("estop")
        || w.phrase("halt the printer")
        || w.phrase("kill the printer")
    {
        return on_one(w, DeviceClass::Printer, Some(CapKind::EmergencyStop), NO_PRINTER, |printer| {
            candidate("print.estop", &printer.target, json!({}), "Emergency stop")
        });
    }

    if let Some(reading) = classify_reading(w) {
        return reading;
    }
    if let Some(verb) = w.verb() {
        if let Some(found) = classify_verb(w, verb) {
            return found;
        }
    }

    // Printer
    let printer_word = w.names_group(&PRINTING);
    if w.has("pause") && (printer_word || w.text == "pause") {
        return on_one(w, DeviceClass::Printer, Some(CapKind::PrintJob), NO_PRINTER, |printer| {
            candidate("print.pause", &printer.target, json!({}), "Pause print")
        });
    }
    if w.has("resume") {
        return on_one(w, DeviceClass::Printer, Some(CapKind::PrintJob), NO_PRINTER, |printer| {
            candidate("print.resume", &printer.target, json!({}), "Resume print")
        });
    }

    // Pets
    if w.any(&["feed", "dispense"]) || (w.has("feeder") && w.has("start")) {
        let grams = w
            .tokens
            .windows(2)
            .find_map(|pair| matches!(pair[1], "g" | "gram" | "grams").then(|| pair[0].parse::<i64>().ok()).flatten())
            .or_else(|| w.tokens.iter().find_map(|t| t.strip_suffix('g').and_then(|n| n.parse().ok())))
            .unwrap_or(10);
        return on_one(w, DeviceClass::Feeder, Some(CapKind::Dispense), NO_FEEDER, |feeder| {
            candidate("pets.feed", &feeder.target, json!({ "grams": grams }), format!("Feed {grams} g"))
        });
    }
    if w.any(&["clean", "litter", "scoop"]) {
        return on_one(w, DeviceClass::Litter, Some(CapKind::Clean), NO_LITTER, |litter| {
            candidate("pets.clean", &litter.target, json!({}), "Clean cycle")
        });
    }
    if w.has("feeder") {
        return on_one(w, DeviceClass::Feeder, None, NO_FEEDER, |feeder| read(&feeder.target, None, "Read the feeder"));
    }

    // Scenes
    for (word, target) in [("evening", "evening"), ("focus", "focus")] {
        if w.has(word) {
            return action(candidate("scene.apply", target, json!({}), format!("Scene {word}")));
        }
    }
    if w.has("movie") {
        return action(candidate("scene.movie", "movie", json!({}), "Scene Movie"));
    }
    if w.phrase("all off") || w.phrase("everything off") {
        return action(candidate("scene.apply", "all_off", json!({}), "Scene All off"));
    }

    if w.names_group(&TVS) {
        return classify_tv(w);
    }
    if let Some(found) = classify_audio(w) {
        return found;
    }
    if w.any(&["dim", "bright", "brighten", "brighter"])
        || w.names(&[DeviceClass::Light])
        || w.number_after(BULB_WORDS).is_some()
    {
        return classify_lights(w);
    }
    Resolution::Nothing { why: None }
}

fn room_candidate(action: &'static str, room: &SpokenRoom, params: Value, label: String) -> Candidate {
    candidate(action, &room.id, params, label)
}

fn room_brightness_choices(room: &SpokenRoom, values: [i64; 3]) -> Resolution {
    let options = values
        .iter()
        .map(|percent| {
            room_candidate(
                "room.brightness",
                room,
                json!({ "percent": percent }),
                format!("Set {} to {percent}%", room.name),
            )
        })
        .collect();
    Resolution::Choices { why: "How bright?".to_string(), options }
}

const LIGHT_WORDS: &[&str] = &[
    "light",
    "lights",
    "lamp",
    "lamps",
    "bulb",
    "bulbs",
    "dim",
    "bright",
    "brighten",
    "brighter",
    "brightness",
    "percent",
];
pub const GLOBAL_SCENES: &[&str] = &["evening", "focus", "movie"];

/// Words left after the room's name was taken out. `None` means this is not about
/// the room's lights, and the ordinary parser should have it.
fn classify_room(w: &Words<'_>, found: &[(usize, usize, &SpokenRoom)]) -> Option<Resolution> {
    let about_lights = w.any(LIGHT_WORDS) || w.has("on") || w.has("off");
    if found.len() > 1 {
        if !about_lights {
            return None;
        }
        let params = json!({ "on": !w.has("off") });
        let options = found
            .iter()
            .map(|(_, _, room)| room_candidate("room.power", room, params.clone(), format!("Switch {}", room.name)))
            .collect();
        return Some(Resolution::Choices {
            why: "That named more than one room, so nothing ran. Pick one.".to_string(),
            options,
        });
    }
    let room = found.first()?.2;
    if names_beyond_lights(w) {
        // "The kitchen and the TV" is two things. "The TV in the kitchen" is the TV.
        return w.has("and").then(|| Resolution::Choices {
            why: "That named more than one thing, so nothing ran. Pick one.".to_string(),
            options: Vec::new(),
        });
    }
    for (name, key) in &room.scenes {
        if w.phrase(key) {
            return Some(action(room_candidate(
                "room.scene",
                room,
                json!({ "scene": name }),
                format!("Scene {name} in {}", room.name),
            )));
        }
    }
    if w.any(GLOBAL_SCENES) {
        // The whole-house scene would reach past this room, so it is not offered.
        return Some(nothing("That room has no scene by that name. Nothing ran."));
    }
    if w.has("off") {
        return Some(action(room_candidate(
            "room.power",
            room,
            json!({ "on": false }),
            format!("Turn off {}", room.name),
        )));
    }
    let numbers: Vec<i64> = w.numbers();
    if w.any(LIGHT_WORDS) {
        if let Some(percent) = numbers.first() {
            return Some(action(room_candidate(
                "room.brightness",
                room,
                json!({ "percent": percent }),
                format!("Set {} to {percent}%", room.name),
            )));
        }
    }
    if w.has("dim") {
        return Some(room_brightness_choices(room, [25, 40, 60]));
    }
    if w.any(&["bright", "brighten", "brighter"]) {
        return Some(room_brightness_choices(room, [60, 80, 100]));
    }
    if w.has("on") || w.any(&["light", "lights", "lamp", "lamps", "bulb", "bulbs"]) {
        return Some(action(room_candidate(
            "room.power",
            room,
            json!({ "on": true }),
            format!("Turn on {}", room.name),
        )));
    }
    None
}

/// Whether anything other than the room's lights was named: another kind of thing,
/// the host, or one light by its number or its own name.
fn names_beyond_lights(w: &Words<'_>) -> bool {
    GROUPS.iter().filter(|group| !std::ptr::eq(**group, &LIGHTS)).any(|group| w.names_group(group))
        || w.names_host()
        || !w.named_bulbs().is_empty()
        || !w.called(DeviceClass::Light).is_empty()
}

fn classify_reading(w: &Words<'_>) -> Option<Resolution> {
    const QUESTION: &[&str] = &["what", "how", "show", "is", "are", "tell", "which", "any", "status"];
    const NOUNS: &[&str] = &[
        "temp",
        "temperature",
        "power",
        "memory",
        "ram",
        "storage",
        "disk",
        "bluetooth",
        "bt",
        "printer status",
        "printer",
        "vms",
        "vm status",
        "status",
    ];
    let metric = |metric: &'static str, label: &'static str| {
        on_one(w, DeviceClass::Telemetry, Some(CapKind::Metrics), NO_READINGS, |source| {
            read(&source.target, Some(metric), label)
        })
    };
    let question = QUESTION.contains(&w.tokens[0]) || NOUNS.contains(&w.text.as_str());
    // The host's own name stands for its processor, as in "how hot is <host>".
    let chip = w.any(&["cpu", "package", "core", "processor"])
        || w.lexicon.of_class(DeviceClass::Host).any(|host| w.calls(host));
    if (question && chip && w.any(&["temp", "temperature", "hot"]))
        || w.tokens[0] == "temp"
        || (question && w.has("temperature"))
        || w.phrase("how hot")
    {
        return Some(metric("cpu_temp", "Read the package temperature"));
    }
    if question && w.any(&["power", "watts", "wattage"]) {
        return Some(metric("power", "Read the package power"));
    }
    if question && w.any(&["memory", "ram"]) {
        return Some(metric("memory", "Read memory use"));
    }
    if question && w.any(&["storage", "disk", "space"]) {
        return Some(metric("storage", "Read storage use"));
    }
    if w.has("bluetooth") || w.phrase("bt delay") || w.text == "bt" {
        return Some(metric("bluetooth", "Read the Bluetooth delay"));
    }
    let printer_word = w.names_group(&PRINTING);
    if (printer_word && w.any(&["status", "state"])) || w.text == "printer" || w.phrase("how is the print") {
        return Some(on_one(w, DeviceClass::Printer, None, NO_PRINTER, |printer| {
            read(&printer.target, None, "Read the printer")
        }));
    }
    if matches!(w.text.as_str(), "vm" | "vms" | "vm status" | "what is running")
        || (w.tokens[0] == "which" && w.any(&["vm", "vms"]) && w.has("running"))
    {
        if w.lexicon.of_class(DeviceClass::Vm).next().is_none() {
            return Some(nothing("There are no VMs here."));
        }
        return Some(action(read(catalog::GROUPS[1], None, "Read VM states")));
    }
    None
}

fn classify_verb(w: &Words<'_>, verb: Verb) -> Option<Resolution> {
    let vm = w.vm();
    if w.names_host() && vm.is_none() {
        return Some(match verb {
            Verb::Start => nothing("The host is already up. Nothing here can power it on from off."),
            Verb::Restart => on_one(w, DeviceClass::Host, Some(CapKind::NodePower), NO_HOST, |host| {
                candidate("host.reboot", &host.target, json!({}), format!("Reboot {}", host.name))
            }),
            Verb::Stop => on_one(w, DeviceClass::Host, Some(CapKind::NodePower), NO_HOST, |host| {
                candidate("host.shutdown", &host.target, json!({}), format!("Shut down {}", host.name))
            }),
        });
    }
    if w.names_group(&TUNNEL) {
        return Some(if verb == Verb::Restart {
            on_one(w, DeviceClass::Router, Some(CapKind::Tunnel), NO_ROUTER, |router| {
                candidate("router.wireguard_restart", &router.target, json!({}), "Restart WireGuard")
            })
        } else {
            nothing("WireGuard can only be restarted from here. Say restart wireguard.")
        });
    }
    if w.names_group(&PRINTING) {
        return Some(if verb == Verb::Start {
            on_one(w, DeviceClass::Printer, Some(CapKind::PrintJob), NO_PRINTER, |printer| {
                candidate("print.start", &printer.target, json!({}), "Start print")
            })
        } else {
            nothing("To stop the printer for good, say emergency stop. Say pause print to pause.")
        });
    }
    if w.names(&[DeviceClass::Light]) {
        return Some(nothing("Lights turn on, off, dim or brighten. Say turn off the lights or dim bulb 2."));
    }
    if let Some(vm) = vm {
        return Some(action(vm_candidate(verb, vm)));
    }
    if w.has("feeder") || w.names_group(&TVS) || w.names(&[DeviceClass::Speaker]) {
        return None;
    }
    let last = *w.tokens.last()?;
    let prefixed: Vec<&SpokenDevice> = if last.len() >= 2 && !is_grammar_word(last) {
        w.lexicon
            .of_class(DeviceClass::Vm)
            .filter(|vm| vm.own_words().iter().any(|own| own.starts_with(last)))
            .collect()
    } else {
        Vec::new()
    };
    if prefixed.len() == 1 && !w.any(&["vm", "machine"]) {
        return Some(action(vm_candidate(verb, prefixed[0])));
    }
    if w.any(&["vm", "machine"])
        || matches!(w.text.as_str(), "start" | "stop" | "restart" | "reboot" | "shutdown")
        || prefixed.len() > 1
    {
        return Some(vm_choices(w.lexicon, verb, "Which VM?"));
    }
    None
}

fn classify_tv(w: &Words<'_>) -> Resolution {
    if let Some(n) = w.number_after(&["hdmi"]) {
        let input = format!("HDMI {n}");
        return on_one(w, DeviceClass::Tv, Some(CapKind::Inputs), NO_TV, |tv| {
            candidate("tv.input", &tv.target, json!({ "input": input }), format!("Switch {} to {input}", tv.name))
        });
    }
    if w.phrase("live tv") {
        return on_one(w, DeviceClass::Tv, Some(CapKind::Inputs), NO_TV, |tv| {
            candidate("tv.input", &tv.target, json!({ "input": "Live TV" }), format!("Switch {} to Live TV", tv.name))
        });
    }
    if w.has("off") {
        return on_one(w, DeviceClass::Tv, Some(CapKind::Power), NO_TV, |tv| {
            candidate("tv.power", &tv.target, json!({ "on": false }), format!("Turn off {}", tv.name))
        });
    }
    if w.has("on") {
        return on_one(w, DeviceClass::Tv, Some(CapKind::Power), NO_TV, |tv| {
            candidate("tv.power", &tv.target, json!({ "on": true }), format!("Turn on {}", tv.name))
        });
    }
    on_one(w, DeviceClass::Tv, None, NO_TV, |tv| read(&tv.target, None, "Read the TV"))
}

fn classify_audio(w: &Words<'_>) -> Option<Resolution> {
    let speaker = |cap: CapKind, build: &dyn Fn(&SpokenDevice) -> Candidate| {
        on_one(w, DeviceClass::Speaker, Some(cap), NO_SPEAKER, build)
    };
    if w.has("unmute") {
        return Some(speaker(CapKind::Mute, &|s| {
            candidate("speaker.mute", &s.target, json!({ "muted": false }), format!("Unmute {}", s.name))
        }));
    }
    if w.any(&["mute", "silence", "quiet"]) {
        return Some(speaker(CapKind::Mute, &|s| {
            candidate("speaker.mute", &s.target, json!({ "muted": true }), format!("Mute {}", s.name))
        }));
    }
    if w.has("louder") || w.phrase("turn it up") {
        return Some(Resolution::VolumeStep { delta: VOLUME_STEP });
    }
    if w.any(&["quieter", "softer"]) || w.phrase("turn it down") {
        return Some(Resolution::VolumeStep { delta: -VOLUME_STEP });
    }
    if w.any(&["volume", "speaker", "sound", "music", "loud"])
        || w.names(&[DeviceClass::Speaker])
        || (w.phrase("living room") && !w.numbers().is_empty())
    {
        if let Some(level) = w.numbers().into_iter().next() {
            return Some(speaker(CapKind::Volume, &|s| {
                candidate(
                    "speaker.volume",
                    &s.target,
                    json!({ "level": level }),
                    format!("Set {} volume to {level}", s.name),
                )
            }));
        }
        if w.any(&["up", "more"]) {
            return Some(Resolution::VolumeStep { delta: VOLUME_STEP });
        }
        if w.any(&["down", "lower", "less"]) {
            return Some(Resolution::VolumeStep { delta: -VOLUME_STEP });
        }
        // A speaker named outright is read. Otherwise the one whose volume can change.
        if let [one] = w.called(DeviceClass::Speaker).as_slice() {
            return Some(action(read(&one.target, None, &format!("Read {}", one.name))));
        }
        return Some(speaker(CapKind::Volume, &|s| read(&s.target, None, &format!("Read {}", s.name))));
    }
    None
}

fn brightness_choices(light: Option<&SpokenDevice>, values: [i64; 3]) -> Resolution {
    let options = values
        .iter()
        .map(|percent| match light {
            Some(light) => candidate(
                "light.brightness",
                &light.target,
                json!({ "percent": percent }),
                format!("Set {} to {percent}%", light.name),
            ),
            None => candidate(
                "lights.brightness",
                catalog::GROUPS[0],
                json!({ "percent": percent }),
                format!("Set all the lights to {percent}%"),
            ),
        })
        .collect();
    Resolution::Choices { why: "How bright?".to_string(), options }
}

fn classify_lights(w: &Words<'_>) -> Resolution {
    let every_light = catalog::GROUPS[0];
    if w.lexicon.of_class(DeviceClass::Light).next().is_none() {
        return nothing("There are no lights here.");
    }
    let number = w.number_after(BULB_WORDS);
    let named: Vec<&SpokenDevice> = match number {
        Some(n) => {
            let found: Vec<&SpokenDevice> =
                w.lexicon.of_class(DeviceClass::Light).filter(|light| light.numbered(BULB_WORDS, n)).collect();
            if found.is_empty() {
                return nothing(&format!("No light here has the number {n}."));
            }
            found
        }
        None => w.called(DeviceClass::Light),
    };
    let all = w.any(&["all", "every", "lights", "bulbs", "lamps"]) && named.is_empty();
    let off = w.has("off");
    let on = w.has("on") && !off;
    let dim = w.has("dim");
    let bright = w.any(&["bright", "brighten", "brighter"]);
    let mut numbers = w.numbers();
    if let Some(n) = number {
        if let Some(position) = numbers.iter().position(|v| *v == n) {
            numbers.remove(position);
        }
    }
    let value = numbers.last().copied();
    let colour = HUE_NAMES.iter().find(|(name, _)| w.has(name));

    if !all && named.len() != 1 {
        let build = |light: &SpokenDevice| {
            let (target, name) = (&light.target, &light.name);
            if off {
                candidate("light.power", target, json!({ "on": false }), format!("Turn off {name}"))
            } else if let Some(percent) = value {
                candidate(
                    "light.brightness",
                    target,
                    json!({ "percent": percent }),
                    format!("Set {name} to {percent}%"),
                )
            } else {
                candidate("light.power", target, json!({ "on": true }), format!("Turn on {name}"))
            }
        };
        if !named.is_empty() {
            return Resolution::Choices {
                why: "Which light?".to_string(),
                options: named.iter().map(|light| build(light)).collect(),
            };
        }
        let mut options: Vec<Candidate> = w.lexicon.of_class(DeviceClass::Light).map(build).collect();
        options.push(if off {
            candidate("lights.power", every_light, json!({ "on": false }), "Turn off all the lights")
        } else if let Some(percent) = value {
            candidate(
                "lights.brightness",
                every_light,
                json!({ "percent": percent }),
                format!("Set all the lights to {percent}%"),
            )
        } else {
            candidate("lights.power", every_light, json!({ "on": true }), "Turn on all the lights")
        });
        return Resolution::Choices { why: "Which light?".to_string(), options };
    }

    if all {
        if off {
            return action(candidate("lights.power", every_light, json!({ "on": false }), "Turn off all the lights"));
        }
        if let Some(percent) = value {
            return action(candidate(
                "lights.brightness",
                every_light,
                json!({ "percent": percent }),
                format!("Set all the lights to {percent}%"),
            ));
        }
        if dim {
            return brightness_choices(None, [25, 40, 60]);
        }
        if bright {
            return brightness_choices(None, [60, 80, 100]);
        }
        return action(candidate("lights.power", every_light, json!({ "on": true }), "Turn on all the lights"));
    }

    let light = named[0];
    let (target, name) = (&light.target, &light.name);
    if let Some((colour, hue)) = colour {
        return action(candidate("light.hue", target, json!({ "hue": hue }), format!("Set {name} colour to {colour}")));
    }
    if off {
        return action(candidate("light.power", target, json!({ "on": false }), format!("Turn off {name}")));
    }
    if let Some(percent) = value {
        return action(candidate(
            "light.brightness",
            target,
            json!({ "percent": percent }),
            format!("Set {name} to {percent}%"),
        ));
    }
    if dim {
        return brightness_choices(Some(light), [25, 40, 60]);
    }
    if bright {
        return brightness_choices(Some(light), [60, 80, 100]);
    }
    if on {
        return action(candidate("light.power", target, json!({ "on": true }), format!("Turn on {name}")));
    }
    action(read(target, None, "Read the bulb"))
}

/// Text to resolution, knowing this home's devices, rooms and words. A room can only
/// be reached through its own name, and with no rooms this is exactly [`parse_plain`].
pub fn parse(raw: &str, lexicon: &Lexicon) -> Parsed {
    let normalized = lexicon.normalize(raw);
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    let found = find_rooms(&tokens, &lexicon.rooms);
    if !found.is_empty() {
        let rest: Vec<&str> = tokens
            .iter()
            .enumerate()
            .filter(|(index, _)| !found.iter().any(|(start, length, _)| index >= start && *index < start + length))
            .map(|(_, token)| *token)
            .collect();
        let words = Words { text: rest.join(" "), tokens: rest, lexicon };
        if let Some(resolution) = classify_room(&words, &found) {
            // The room's own words and its scene names are recognised words.
            let scene_words: Vec<&str> = found
                .iter()
                .flat_map(|(_, _, room)| room.scenes.iter())
                .flat_map(|(_, key)| key.split_whitespace())
                .collect();
            let score = if words.tokens.iter().all(|t| FILLER.contains(t)) {
                1.0
            } else {
                confidence(&words.tokens, lexicon, &scene_words)
            };
            let mut resolution = resolution;
            if let Some(why) = words.ambiguity() {
                resolution = lower_for_ambiguity(resolution, &words, why);
            }
            if score < MIN_CONFIDENCE {
                resolution = lower_for_confidence(resolution, &words);
            }
            return Parsed { normalized, resolution, confidence: score };
        }
    }
    parse_plain(raw, lexicon)
}

/// Text to resolution, with no room in it. Pure: the same text and the same words
/// always give the same answer.
fn parse_plain(raw: &str, lexicon: &Lexicon) -> Parsed {
    let normalized = lexicon.normalize(raw);
    let words = Words::of(&normalized, lexicon);
    let score = confidence(&words.tokens, lexicon, &[]);
    let mut resolution = classify(&words);

    if let Some(why) = words.ambiguity() {
        resolution = lower_for_ambiguity(resolution, &words, why);
    }
    if score < MIN_CONFIDENCE {
        resolution = lower_for_confidence(resolution, &words);
    }
    let normalized = words.text.clone();
    Parsed { normalized, resolution, confidence: score }
}

/// What the person may have meant by an instruction that could mean several
/// things: each target or value they named, or both directions of a switch.
fn alternatives(chosen: Candidate, w: &Words<'_>) -> Vec<Candidate> {
    let with_params = |params: Value, label: String| Candidate {
        action: chosen.action,
        target: chosen.target.clone(),
        params,
        label,
    };
    let options: Vec<Candidate> = match chosen.action {
        "vm.start" | "vm.stop" | "vm.restart" => match w.verb() {
            Some(verb) => w.named_vms().into_iter().map(|vm| vm_candidate(verb, vm)).collect(),
            None => Vec::new(),
        },
        "light.power" | "light.brightness" | "light.hue" if w.named_lights().len() > 1 => {
            let chosen_name = w.lexicon.device(&chosen.target).map(|light| light.name.as_str()).unwrap_or_default();
            w.named_lights()
                .into_iter()
                .map(|light| Candidate {
                    action: chosen.action,
                    target: light.target.clone(),
                    params: chosen.params.clone(),
                    label: if chosen_name.is_empty() {
                        chosen.label.clone()
                    } else {
                        chosen.label.replace(chosen_name, &light.name)
                    },
                })
                .collect()
        }
        "speaker.volume" => w
            .values()
            .into_iter()
            .map(|level| with_params(json!({ "level": level }), format!("Set volume to {level}")))
            .collect(),
        "light.brightness" | "lights.brightness" => w
            .values()
            .into_iter()
            .map(|percent| with_params(json!({ "percent": percent }), format!("Set to {percent}%")))
            .collect(),
        "light.power" | "lights.power" | "tv.power" if w.has("on") && w.has("off") => [true, false]
            .into_iter()
            .map(|on| with_params(json!({ "on": on }), format!("Turn {}", if on { "on" } else { "off" })))
            .collect(),
        "speaker.mute" if w.has("unmute") && w.any(&["mute", "silence", "quiet"]) => [true, false]
            .into_iter()
            .map(|muted| with_params(json!({ "muted": muted }), (if muted { "Mute" } else { "Unmute" }).to_string()))
            .collect(),
        _ => Vec::new(),
    };
    if options.is_empty() {
        vec![chosen]
    } else {
        options
    }
}

/// This could not be taken as one clear instruction. What was heard is offered
/// to tap, and nothing runs.
fn lower_for_ambiguity(resolution: Resolution, words: &Words<'_>, why: &str) -> Resolution {
    let why = why.to_string();
    match resolution {
        Resolution::Action(chosen) => Resolution::Choices { why, options: alternatives(chosen, words) },
        Resolution::Choices { options, .. } => Resolution::Choices { why, options },
        Resolution::VolumeStep { .. } => Resolution::Choices { why, options: Vec::new() },
        other => other,
    }
}

/// The words were not clear enough to act on. Whatever was heard is offered as
/// a choice, and a recognised verb with no clear target offers the VMs.
fn lower_for_confidence(resolution: Resolution, words: &Words<'_>) -> Resolution {
    let why = "Not confident enough. Nothing ran.".to_string();
    match resolution {
        Resolution::Action(candidate) => Resolution::Choices { why, options: vec![candidate] },
        Resolution::Choices { options, .. } => Resolution::Choices { why, options },
        Resolution::Ignored { .. } => resolution,
        _ => match words.verb() {
            Some(verb) => vm_choices(words.lexicon, verb, &why),
            None => Resolution::Choices { why, options: Vec::new() },
        },
    }
}

/// The words for this home right now: its devices, its rooms and its plugins' words.
pub fn lexicon_for(app: &App) -> Result<Lexicon, ApiError> {
    let rooms = app.store.with(rooms::voice_index)?;
    Ok(Lexicon::build(&app.registry.index(), rooms, &app.plugin_words))
}

fn audit_outcome(
    app: &App,
    actor: &Actor,
    action: &str,
    target: &str,
    params: Value,
    result: &str,
) -> Result<(), ApiError> {
    audit_on(app, actor, Channel::Voice, action, target, params, result)
}

pub(crate) fn audit_on(
    app: &App,
    actor: &Actor,
    channel: Channel,
    action: &str,
    target: &str,
    params: Value,
    result: &str,
) -> Result<(), ApiError> {
    app.audit(Event {
        ts: app.now(),
        action: action.to_string(),
        target: target.to_string(),
        params,
        credential_ref: actor.credential_id.clone(),
        session_ref: actor.session_ref.clone(),
        user_ref: actor.user_id.clone(),
        device: actor.device.clone(),
        channel: channel.name().to_string(),
        step_up: "none".to_string(),
        result: result.to_string(),
        rtt_ms: None,
        run_id: None,
    })
}

fn choice_json(c: &Candidate) -> Value {
    let tier = catalog::find(c.action).map(|def| def.tier.wire());
    json!({
        "label": c.label,
        "action": c.action,
        "target": c.target,
        "params": c.params,
        "tier": tier,
    })
}

/// Handles one utterance end to end.
const WAKE_OPENERS: &[&str] = &["hey", "hi", "hello", "ok", "okay", "yo"];
/// The name, and what the speech model tends to write when it hears the name.
const WAKE_NAMES: &[&[&str]] = &[
    &["swifty"],
    &["swiftie"],
    &["swifti"],
    &["swiftee"],
    &["shifty"],
    &["swift", "e"],
    &["swift", "he"],
    &["swift", "t"],
    &["swift", "tea"],
];

/// What follows "Swifty," at the start of something said or typed.
pub fn after_wake_word(raw: &str) -> Option<String> {
    let words: Vec<&str> = raw.split_whitespace().collect();
    let plain: Vec<String> =
        words.iter().map(|word| word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()).collect();
    let start = usize::from(plain.first().is_some_and(|word| WAKE_OPENERS.contains(&word.as_str())));
    WAKE_NAMES.iter().find_map(|name| {
        let end = start + name.len();
        (plain.len() >= end && plain[start..end].iter().map(String::as_str).eq(name.iter().copied()))
            .then(|| words[end..].join(" "))
    })
}

fn usable_speech(app: &App) -> Result<&SpeechClient, ApiError> {
    let Some(speech) = app.speech.as_ref() else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "speech_not_configured",
            "The speech service is not configured.",
        ));
    };
    // In dry-run nothing leaves this machine, and that includes the audio and the
    // bearer token. A speech service on loopback does not leave it.
    if !app.registry.is_live() && !speech.on_loopback {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "speech_not_local",
            "Dry-run: the speech service is not on this machine, so no audio was sent.",
        ));
    }
    Ok(speech)
}

/// The words in some audio, and nothing else: this runs nothing and writes nothing down.
/// It is what shows them on the screen while the person is still talking.
pub async fn words(app: &App, wav: Vec<u8>) -> Result<Value, ApiError> {
    let reply = usable_speech(app)?.transcribe(wav).await?;
    Ok(match after_wake_word(&reply.text).filter(|_| app.assistant.is_some()) {
        Some(rest) => json!({ "text": rest, "to_swifty": true }),
        None => json!({ "text": reply.text, "to_swifty": false }),
    })
}

pub async fn handle(app: &App, actor: &Actor, wav: Vec<u8>) -> Result<Value, ApiError> {
    let reply = usable_speech(app)?.transcribe(wav).await?;
    let mut said = reply.text.clone();
    if let Some(rest) = after_wake_word(&reply.text) {
        // Said to Swifty, so the page takes it there. Nothing runs from this route.
        if app.assistant.is_some() {
            return Ok(json!({ "kind": "swifty", "ask": rest, "heard": normalize(&reply.text) }));
        }
        said = rest;
    }
    // About Swifty's memory, so it goes to Swifty too: "remember that the bedroom is best
    // at 22" is something to keep, not a command to the bedroom.
    if app.assistant.is_some() && crate::memory::said(&said).is_some() {
        return Ok(json!({ "kind": "swifty", "ask": said, "heard": normalize(&said) }));
    }
    let lexicon = lexicon_for(app)?;
    let mut parsed = parse(&said, &lexicon);
    if let Some(confidence) = reply.confidence {
        if confidence < parsed.confidence {
            parsed.confidence = confidence;
        }
        if confidence < MIN_CONFIDENCE && parsed.confidence == confidence {
            let words = Words::of(&parsed.normalized, &lexicon);
            parsed.resolution = lower_for_confidence(parsed.resolution, &words);
        }
    }
    let heard = parsed.normalized.clone();
    let score = parsed.confidence;
    let mut response = dispatch(app, actor, parsed, Channel::Voice).await?;
    if let Some(object) = response.as_object_mut() {
        // Returned to the person who spoke. Never written anywhere.
        object.insert("heard".to_string(), json!(heard));
        object.insert("confidence".to_string(), json!(score));
    }
    Ok(response)
}

/// Acts on a parsed utterance according to the policy for its tier.
pub async fn dispatch(app: &App, actor: &Actor, parsed: Parsed, channel: Channel) -> Result<Value, ApiError> {
    match parsed.resolution {
        Resolution::Action(candidate) => dispatch_candidate(app, actor, candidate, channel).await,
        Resolution::VolumeStep { delta } => {
            // "Louder" names no speaker: it is the first one whose volume can change.
            let speaker = app
                .registry
                .index()
                .of_class(DeviceClass::Speaker)
                .find(|device| device.has(CapKind::Volume))
                .map(|device| (device.target.clone(), device.name.clone()));
            let Some((target, name)) = speaker else {
                audit_outcome(app, actor, "speaker.volume", "-", Value::Null, "refused: no_speaker")?;
                return Ok(json!({ "kind": "nothing", "why": "There is no speaker here. Nothing ran." }));
            };
            let current = app.registry.read_state(&target).await;
            let Ok(Some(State::Speaker { vol, .. })) = current else {
                audit_outcome(app, actor, "speaker.volume", &target, Value::Null, "refused: current_volume_unknown")?;
                return Ok(json!({
                    "kind": "nothing",
                    "why": "The current volume is not known, so a relative change cannot be worked out. Nothing ran."
                }));
            };
            let level = (vol as i64 + delta).clamp(0, 100);
            let chosen = candidate(
                "speaker.volume",
                &target,
                json!({ "level": level }),
                format!("Set {name} volume to {level}"),
            );
            dispatch_candidate(app, actor, chosen, channel).await
        }
        Resolution::Choices { why, options } => {
            audit_outcome(app, actor, "voice", "-", Value::Null, "choices: nothing ran")?;
            Ok(json!({
                "kind": "choices",
                "why": why,
                "choices": options
                    .iter()
                    .filter(|option| policy::may_run(&app.registry.index(), actor.role, option.action, &option.target))
                    .map(choice_json)
                    .collect::<Vec<_>>(),
            }))
        }
        Resolution::Ignored { why } => {
            audit_outcome(app, actor, "voice", "-", Value::Null, "ignored: speech never authorises")?;
            Ok(json!({ "kind": "ignored", "why": why }))
        }
        Resolution::Nav { page } => Ok(json!({ "kind": "nav", "page": page })),
        Resolution::Help => Ok(json!({ "kind": "help" })),
        Resolution::Lock => {
            let now = app.now();
            app.store.with(|conn| sessions::lock(conn, now, &actor.session_ref))?;
            app.events.recheck_sessions();
            audit_outcome(app, actor, "session.lock", "-", Value::Null, "locked")?;
            Ok(json!({ "kind": "locked" }))
        }
        Resolution::Nothing { why } => {
            audit_outcome(app, actor, "voice", "-", Value::Null, "not a command: nothing ran")?;
            Ok(json!({ "kind": "nothing", "why": why }))
        }
    }
}

pub(crate) async fn dispatch_candidate(
    app: &App,
    actor: &Actor,
    chosen: Candidate,
    channel: Channel,
) -> Result<Value, ApiError> {
    let Some(def) = catalog::find(chosen.action) else {
        return Err(ApiError::internal("parser produced an action outside the catalog"));
    };
    // Only someone who may run this hears about it. Anyone else takes the normal
    // route below, and is refused the same way as for any other computer.
    if policy::may_run(&app.registry.index(), actor.role, def.id, &chosen.target)
        && catalog::takes_console_offline(def, &chosen.target, actions::runs_on(app).as_deref())
    {
        audit_on(app, actor, channel, def.id, &chosen.target, json!({}), "refused: acknowledgement_required")?;
        return Ok(json!({
            "kind": "acknowledgement_required",
            "action": def.id,
            "target": chosen.target,
            "params": { ACK_PARAM: true },
            "tier": def.tier.wire(),
            "warning": catalog::console_offline_warning(def),
        }));
    }
    match policy::voice_route(def.tier) {
        VoiceRoute::Execute => {
            let outcome =
                actions::execute_on_channel(app, actor, def.id, &chosen.target, &chosen.params, channel, None).await;
            match outcome {
                Ok(run) => {
                    // A client reading only `kind` must not see success for a
                    // device that never answered.
                    let kind = match run.phase.as_str() {
                        "confirmed" | "simulated" => "executed",
                        other => other,
                    };
                    Ok(
                        json!({ "kind": kind, "phase": run.phase, "tier": def.tier.wire(), "label": chosen.label, "run": run }),
                    )
                }
                Err(error) => refused_reply(error),
            }
        }
        VoiceRoute::PrepareForTap | VoiceRoute::PrepareForStepUp => {
            let needs = if policy::voice_route(def.tier) == VoiceRoute::PrepareForTap { "tap" } else { "passkey" };
            let prepared = actions::prepare_on_channel(app, actor, def.id, &chosen.target, &chosen.params, channel);
            match prepared {
                Ok(prepared) => {
                    Ok(json!({ "kind": "prepared", "needs": needs, "tier": def.tier.wire(), "prepared": prepared }))
                }
                Err(error) => refused_reply(error),
            }
        }
    }
}

/// A refusal from the policy is an answer to the speaker, not a server fault.
fn refused_reply(error: ApiError) -> Result<Value, ApiError> {
    if error.status.is_server_error() {
        return Err(error);
    }
    Ok(json!({ "kind": "refused", "code": error.code, "why": error.message }))
}

#[cfg(test)]
mod wake_tests {
    use super::*;

    #[test]
    fn the_name_at_the_start_is_what_counts() {
        assert_eq!(after_wake_word("Swifty, what is the time?").as_deref(), Some("what is the time?"));
        assert_eq!(after_wake_word("hey swifty turn off the lights").as_deref(), Some("turn off the lights"));
        assert_eq!(after_wake_word("swift e what is up").as_deref(), Some("what is up"));
        assert_eq!(after_wake_word("Swifty").as_deref(), Some(""));
        assert_eq!(after_wake_word("turn off swifty"), None);
        assert_eq!(after_wake_word("hey there"), None);
        assert_eq!(after_wake_word(""), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::index::{legacy_seeded, DeviceIndex, IndexedDevice};

    /// Today's house, every device under a generic name: four machines, a host and a
    /// printer with names of their own, the lights numbered 1 to 4.
    fn house() -> DeviceIndex {
        const NAMES: &[(&str, &str)] = &[
            ("bulb1", "Bulb 1"),
            ("bulb2", "Bulb 2"),
            ("bulb3", "Bulb 3"),
            ("bulb4", "Bulb 4"),
            ("hue", "Bridge"),
            ("sonos", "Sound bar"),
            ("sub", "Subwoofer"),
            ("tv", "Television"),
            ("feeder", "Feeder"),
            ("litter", "Litter box"),
            ("printer", "Workshop printer"),
            ("vm100", "alpha · VM 100"),
            ("vm101", "filestore · VM 101"),
            ("vm102", "charlie · VM 102"),
            ("vm103", "delta · VM 103"),
            ("host", "Atlas · Proxmox host"),
            ("router", "Router"),
            ("camera", "Camera"),
            ("plex", "Media library"),
            ("collector", "Collector"),
        ];
        let devices: Vec<IndexedDevice> = legacy_seeded()
            .all()
            .map(|device| {
                let mut device = device.clone();
                let (_, name) =
                    NAMES.iter().find(|(target, _)| *target == device.target).expect("every device is named");
                device.name = name.to_string();
                device
            })
            .collect();
        assert_eq!(devices.len(), NAMES.len());
        DeviceIndex::new(devices)
    }

    fn lexicon() -> Lexicon {
        Lexicon::build(&house(), Vec::new(), LEGACY_WORDS)
    }

    fn heard(text: &str) -> Parsed {
        parse(text, &lexicon())
    }

    fn resolved(text: &str) -> Candidate {
        match heard(text).resolution {
            Resolution::Action(c) => c,
            other => panic!("{text:?} gave {other:?}"),
        }
    }

    #[test]
    fn normalization_applies_aliases_and_numbers() {
        // The running hub registers its plugins' words at startup; these are theirs.
        register_plugin_words(LEGACY_WORDS);
        assert_eq!(normalize("Whiz bulb two"), "wiz bulb 2");
        assert_eq!(normalize("Clipper status"), "klipper status");
        assert_eq!(normalize("pet feeder"), "petkit feeder");
        assert_eq!(lexicon().normalize("File Store"), "filestore");
        assert_eq!(normalize("Blue Tooth"), "bluetooth");
        assert_eq!(normalize("LGTV off"), "lg tv off");
        assert_eq!(normalize("volume twenty five."), "volume 25");
        assert_eq!(normalize("a hundred percent"), "100 percent");
        assert_eq!(normalize("sixty"), "60");
        assert_eq!(normalize("set a c to twenty-seven"), "set ac to 27");
        assert_eq!(normalize("A/C off"), "ac off");
        assert_eq!(normalize("the a.c. please"), "the ac please");
        assert_eq!(normalize("air con to cool"), "aircon to cool");
        assert_eq!(normalize("a cat"), "a cat");
    }

    #[test]
    fn known_mishearings_reach_the_right_action() {
        let c = resolved("whiz bulb one off");
        assert_eq!((c.action, c.target.as_str()), ("light.power", "bulb1"));
        assert_eq!(c.params, json!({ "on": false }));

        let c = resolved("pause clipper");
        assert_eq!(c.action, "print.pause");

        let c = resolved("clipper status");
        assert_eq!((c.action, c.target.as_str()), ("state.read", "printer"));

        let c = resolved("feed with the pet feeder 20 grams");
        assert_eq!(c.action, "pets.feed");
        assert_eq!(c.params, json!({ "grams": 20 }));

        let c = resolved("stop file store");
        assert_eq!((c.action, c.target.as_str()), ("vm.stop", "vm101"));

        let c = resolved("blue tooth delay");
        assert_eq!((c.action, c.target.as_str()), ("state.read", "collector"));
        assert_eq!(c.params, json!({ "metric": "bluetooth" }));

        let c = resolved("LGTV off");
        assert_eq!((c.action, c.target.as_str()), ("tv.power", "tv"));
        assert_eq!(c.params, json!({ "on": false }));
    }

    #[test]
    fn the_prototype_phrases_resolve() {
        let c = resolved("Dim the lights to 20 percent");
        assert_eq!((c.action, c.params), ("lights.brightness", json!({ "percent": 20 })));
        let c = resolved("Feed the pets 10 grams");
        assert_eq!((c.action, c.params), ("pets.feed", json!({ "grams": 10 })));
        let c = resolved("Restart the Alpha VM");
        assert_eq!((c.action, c.target.as_str()), ("vm.restart", "vm100"));
        let c = resolved("Set the volume to 80");
        assert_eq!((c.action, c.params), ("speaker.volume", json!({ "level": 80 })));
        let c = resolved("Stop filestore");
        assert_eq!((c.action, c.target.as_str()), ("vm.stop", "vm101"));
        let c = resolved("What is the CPU temperature?");
        assert_eq!(c.params, json!({ "metric": "cpu_temp" }));
        let c = resolved("set bulb 2 to 30");
        assert_eq!((c.action, c.target.as_str(), c.params), ("light.brightness", "bulb2", json!({ "percent": 30 })));
        let c = resolved("bulb 2 warm");
        assert_eq!((c.action, c.params), ("light.hue", json!({ "hue": 34 })));
        let c = resolved("tv hdmi 2");
        assert_eq!((c.action, c.params), ("tv.input", json!({ "input": "HDMI 2" })));
        let c = resolved("scene movie");
        assert_eq!((c.action, c.target.as_str()), ("scene.movie", "movie"));
        let c = resolved("scene evening");
        assert_eq!((c.action, c.target.as_str()), ("scene.apply", "evening"));
        let c = resolved("restart wireguard");
        assert_eq!(c.action, "router.wireguard_restart");
        let c = resolved("shutdown atlas");
        assert_eq!(c.action, "host.shutdown");
        let c = resolved("reboot atlas");
        assert_eq!(c.action, "host.reboot");
        let c = resolved("mute");
        assert_eq!((c.action, c.params), ("speaker.mute", json!({ "muted": true })));
        let c = resolved("unmute");
        assert_eq!((c.action, c.params), ("speaker.mute", json!({ "muted": false })));
        let c = resolved("clean litter");
        assert_eq!(c.action, "pets.clean");
        assert_eq!(heard("louder").resolution, Resolution::VolumeStep { delta: 5 });
        assert_eq!(heard("quieter").resolution, Resolution::VolumeStep { delta: -5 });
        assert_eq!(heard("go to monitor").resolution, Resolution::Nav { page: "monitor".into() });
    }

    #[test]
    fn unclear_targets_return_choices() {
        let Resolution::Choices { options, .. } = heard("turn off the light").resolution else {
            panic!("expected choices");
        };
        assert_eq!(options.len(), 5);
        assert!(options.iter().all(|o| o.params == json!({ "on": false })));

        let Resolution::Choices { options, .. } = heard("stop the vm").resolution else {
            panic!("expected choices");
        };
        assert_eq!(options.len(), 4);
        assert!(options.iter().all(|o| o.action == "vm.stop"));
        assert!(matches!(heard("stop").resolution, Resolution::Choices { .. }));
    }

    #[test]
    fn low_confidence_only_offers() {
        let parsed = heard("mumbled stop something");
        assert!(parsed.confidence < MIN_CONFIDENCE);
        let Resolution::Choices { why, options } = parsed.resolution else {
            panic!("expected choices");
        };
        assert!(why.contains("confident"));
        assert_eq!(options.len(), 4);

        let parsed = heard("blarg foo mute bazzle");
        assert!(parsed.confidence < MIN_CONFIDENCE);
        assert!(matches!(parsed.resolution, Resolution::Choices { .. }));

        let parsed = heard("zxcv qwer");
        assert_eq!(parsed.confidence, 0.0);
        assert_eq!(
            parsed.resolution,
            Resolution::Choices { why: "Not confident enough. Nothing ran.".into(), options: vec![] }
        );
    }

    /// What each of the phrases has to offer. Nothing here may be an Action.
    fn choices(text: &str) -> Vec<Candidate> {
        match heard(text).resolution {
            Resolution::Choices { options, .. } => options,
            other => panic!("{text:?} gave {other:?}"),
        }
    }

    #[test]
    fn a_negated_instruction_runs_nothing() {
        for text in [
            "do not turn off the lights",
            "don't turn off the lights",
            "dont turn off the lights",
            "never mute the volume",
            "not the lights off",
            "please don't stop alpha",
            "no volume 30",
            "I can't feed the pets",
        ] {
            choices(text);
        }
        assert!(matches!(heard("don't turn it up").resolution, Resolution::Choices { .. }));
    }

    #[test]
    fn an_either_or_offers_both_sides() {
        let options = choices("turn the lights on or off");
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].params, json!({ "on": true }));
        assert_eq!(options[1].params, json!({ "on": false }));

        let options = choices("mute or unmute");
        assert_eq!(
            options.iter().map(|o| &o.params).collect::<Vec<_>>(),
            [&json!({ "muted": true }), &json!({ "muted": false })]
        );

        let options = choices("set the volume to twenty or thirty");
        assert_eq!(
            options.iter().map(|o| &o.params).collect::<Vec<_>>(),
            [&json!({ "level": 20 }), &json!({ "level": 30 })]
        );

        choices("either restart alpha or stop it");
    }

    #[test]
    fn more_than_one_target_or_value_offers_each() {
        let options = choices("turn off bulb one and bulb two");
        assert_eq!(options.iter().map(|o| o.target.as_str()).collect::<Vec<_>>(), ["bulb1", "bulb2"]);
        assert!(options.iter().all(|o| o.params == json!({ "on": false })));
        assert_eq!(options[1].label, "Turn off Bulb 2");

        let options = choices("stop alpha and charlie");
        assert_eq!(options.iter().map(|o| o.target.as_str()).collect::<Vec<_>>(), ["vm100", "vm102"]);
        assert!(options.iter().all(|o| o.action == "vm.stop"));

        let options = choices("restart 100 and 103");
        assert_eq!(options.iter().map(|o| o.target.as_str()).collect::<Vec<_>>(), ["vm100", "vm103"]);

        choices("turn off bulbs one and two");
        choices("turn off the lights and the tv");
        choices("turn the tv off and set the volume to 20");
        choices("feed the pets and turn off the lights");
        choices("set the volume to 20 30");
    }

    #[test]
    fn one_clear_target_with_a_number_is_not_ambiguous() {
        for text in [
            "set bulb 2 to 30",
            "set bulb 2 to 2",
            "bulb2 off",
            "tv hdmi 2",
            "set the volume to 100",
            "restart vm 100",
            "stop the alpha vm on atlas",
            "feed 20 grams",
            "evening lights",
            "dim the lights to 20 percent",
        ] {
            assert!(heard(text).resolution != Resolution::Nothing { why: None }, "{text}");
            assert!(!matches!(heard(text).resolution, Resolution::Choices { .. }), "{text}");
        }
    }

    fn rooms() -> Vec<SpokenRoom> {
        vec![
            SpokenRoom {
                id: "room1".into(),
                name: "Kitchen".into(),
                key: "kitchen".into(),
                scenes: vec![("Breakfast".into(), "breakfast".into())],
            },
            SpokenRoom { id: "room2".into(), name: "Living Room".into(), key: "living room".into(), scenes: vec![] },
            SpokenRoom { id: "room3".into(), name: "TV Room".into(), key: "tv room".into(), scenes: vec![] },
        ]
    }

    fn with_rooms() -> Lexicon {
        Lexicon::build(&house(), rooms(), LEGACY_WORDS)
    }

    fn in_room(text: &str) -> Candidate {
        match parse(text, &with_rooms()).resolution {
            Resolution::Action(c) => c,
            other => panic!("{text:?} gave {other:?}"),
        }
    }

    #[test]
    fn a_room_name_reaches_only_that_room() {
        let c = in_room("turn off the kitchen lights");
        assert_eq!((c.action, c.target.as_str(), c.params), ("room.power", "room1", json!({ "on": false })));
        let c = in_room("living room lights on");
        assert_eq!((c.action, c.target.as_str(), c.params), ("room.power", "room2", json!({ "on": true })));
        let c = in_room("kitchen off");
        assert_eq!((c.action, c.params), ("room.power", json!({ "on": false })));
        let c = in_room("set the kitchen lights to 30 percent");
        assert_eq!((c.action, c.params), ("room.brightness", json!({ "percent": 30 })));
        let c = in_room("breakfast in the kitchen");
        assert_eq!((c.action, c.params), ("room.scene", json!({ "scene": "Breakfast" })));
        let c = in_room("turn off the tv room");
        assert_eq!((c.action, c.target.as_str()), ("room.power", "room3"));
    }

    #[test]
    fn with_no_rooms_nothing_changes() {
        let lexicon = lexicon();
        for text in ["turn off the lights", "living room 30", "stop alpha", "bulb 2 off", "mute"] {
            assert_eq!(parse(text, &lexicon), parse_plain(text, &lexicon), "{text}");
        }
    }

    #[test]
    fn a_room_does_not_take_over_other_commands() {
        let c = in_room("living room 30");
        assert_eq!((c.action, c.params), ("speaker.volume", json!({ "level": 30 })));
        let c = in_room("set the volume in the living room to 20");
        assert_eq!((c.action, c.params), ("speaker.volume", json!({ "level": 20 })));
        let c = in_room("turn the tv off in the kitchen");
        assert_eq!((c.action, c.params), ("tv.power", json!({ "on": false })));
    }

    #[test]
    fn room_commands_keep_every_guard() {
        for text in [
            "do not turn off the kitchen lights",
            "turn the kitchen lights on or off",
            "turn off the kitchen and the living room",
            "turn off the kitchen and the tv",
            "turn off the kitchen and bulb 2",
        ] {
            assert!(matches!(parse(text, &with_rooms()).resolution, Resolution::Choices { .. }), "{text}");
        }
    }

    #[test]
    fn a_whole_house_scene_is_never_run_for_a_room() {
        assert!(matches!(parse("evening in the kitchen", &with_rooms()).resolution, Resolution::Nothing { .. }));
    }

    #[test]
    fn command_words_cannot_name_a_room() {
        for reserved in ["tv", "lights", "off", "all", "stop", "the", "or", "5", "hue", "ac", "plug"] {
            assert!(is_reserved_phrase(reserved), "{reserved}");
        }
        for fine in ["kitchen", "living room", "tv room", "office 2"] {
            assert!(!is_reserved_phrase(fine), "{fine}");
        }
    }

    #[test]
    fn a_spoken_yes_is_ignored() {
        for text in ["yes", "Yes, confirm", "ok", "go ahead", "do it"] {
            assert!(matches!(heard(text).resolution, Resolution::Ignored { .. }), "{text}");
        }
    }

    #[test]
    fn emergency_stop_is_said_by_its_name() {
        for text in ["emergency stop", "e-stop", "estop", "halt the printer", "kill the printer"] {
            assert_eq!(resolved(text).action, "print.estop", "{text}");
        }
        assert!(
            matches!(heard("stop the printer").resolution, Resolution::Nothing { .. }),
            "a plain stop is never the emergency stop"
        );
    }

    #[test]
    fn the_verb_has_to_fit_the_target() {
        assert!(matches!(heard("start atlas").resolution, Resolution::Nothing { .. }));
        assert!(matches!(heard("stop wireguard").resolution, Resolution::Nothing { .. }));
        assert!(matches!(heard("stop the printer").resolution, Resolution::Nothing { .. }));
    }

    #[test]
    fn every_candidate_is_in_the_catalog() {
        let index = house();
        for text in [
            "stop alpha",
            "start charlie",
            "restart delta",
            "feed",
            "clean",
            "pause print",
            "resume",
            "tv on",
            "tv off",
            "volume 30",
            "lights off",
            "lights on",
            "bulb 3 off",
            "start print",
        ] {
            let c = resolved(text);
            let def = catalog::find(c.action).unwrap_or_else(|| panic!("{}", c.action));
            def.admits(&index, &c.target).unwrap_or_else(|e| panic!("{text}: {e:?}"));
            def.validate_params(&c.params).unwrap_or_else(|e| panic!("{text}: {e:?}"));
        }
    }

    // ----- what the lexicon brings

    fn renamed(target: &str, name: &str) -> Lexicon {
        let index = DeviceIndex::new(house().all().map(|device| {
            let mut device = device.clone();
            if device.target == target {
                device.name = name.to_string();
            }
            device
        }));
        Lexicon::build(&index, Vec::new(), LEGACY_WORDS)
    }

    #[test]
    fn a_renamed_machine_answers_to_its_new_name_and_not_its_old_one() {
        let lexicon = renamed("vm100", "Gaming rig");
        let Resolution::Action(c) = parse("stop the gaming rig", &lexicon).resolution else {
            panic!("the new name was not understood");
        };
        assert_eq!((c.action, c.target.as_str(), c.label.as_str()), ("vm.stop", "vm100", "Stop Gaming rig"));
        match parse("stop alpha", &lexicon).resolution {
            Resolution::Action(c) => panic!("the old name still reached {c:?}"),
            Resolution::Choices { options, .. } => assert_ne!(options.len(), 1, "the old name narrowed it down"),
            _ => {}
        }
        // Its target still names it.
        assert_eq!(parse("restart vm 100", &lexicon).resolution, Resolution::Action(c_with(&c, "vm.restart")));
        // A shortened name reaches the only machine it fits.
        assert_eq!(resolved("stop fil").target, "vm101");
        assert_eq!(resolved("stop store").target, "vm101");
    }

    fn c_with(c: &Candidate, action: &'static str) -> Candidate {
        Candidate { action, label: c.label.replace("Stop", "Restart"), ..c.clone() }
    }

    #[test]
    fn a_renamed_host_and_printer_answer_to_their_new_names() {
        let lexicon = renamed("host", "Basement rack");
        let Resolution::Action(c) = parse("reboot the basement rack", &lexicon).resolution else {
            panic!("the host's new name was not understood");
        };
        assert_eq!((c.action, c.target.as_str()), ("host.reboot", "host"));
        assert!(!matches!(parse("reboot atlas", &lexicon).resolution, Resolution::Action(_)));

        let c = resolved("pause the workshop");
        assert_eq!((c.action, c.target.as_str()), ("print.pause", "printer"));
    }

    #[test]
    fn a_light_is_called_by_its_own_name() {
        let lexicon = renamed("bulb3", "Desk lamp");
        let Resolution::Action(c) = parse("turn off the desk lamp", &lexicon).resolution else {
            panic!("the lamp was not reached by name");
        };
        assert_eq!((c.action, c.target.as_str(), c.label.as_str()), ("light.power", "bulb3", "Turn off Desk lamp"));
        // "bulb 3" is still its target.
        assert_eq!(parse("bulb 3 off", &lexicon).resolution, Resolution::Action(c));
        assert!(matches!(parse("turn off the desk lamp and bulb 1", &lexicon).resolution, Resolution::Choices { .. }));
    }

    #[test]
    fn hue_is_a_colour_word_and_never_a_light() {
        assert!(!lexicon().nouns.contains_key("hue"));
        let c = resolved("set the hue of bulb 2 to blue");
        assert_eq!((c.action, c.target.as_str(), c.params), ("light.hue", "bulb2", json!({ "hue": 225 })));
        assert!(!matches!(heard("turn off the hue").resolution, Resolution::Action(_)));
    }

    #[test]
    fn with_an_empty_index_no_sentence_reaches_a_machine() {
        let empty = Lexicon::build(&DeviceIndex::default(), Vec::new(), LEGACY_WORDS);
        for text in [
            "stop alpha",
            "stop the vm",
            "stop",
            "restart",
            "restart vm 100",
            "restart 100 and 103",
            "start vm100",
            "reboot the host",
            "shutdown proxmox",
            "stop the alpha vm on atlas",
            "mumbled stop something",
            "vm status",
            "which vms are running",
            "restart wireguard",
            "emergency stop",
            "what is the cpu temperature",
        ] {
            let parsed = parse(text, &empty);
            let offered: Vec<Candidate> = match parsed.resolution {
                Resolution::Action(c) => vec![c],
                Resolution::Choices { options, .. } => options,
                _ => Vec::new(),
            };
            for c in offered {
                assert!(
                    !["vm.", "host.", "console.", "machine.", "router.", "print."]
                        .iter()
                        .any(|p| c.action.starts_with(p)),
                    "{text:?} offered {c:?}"
                );
                assert_ne!(c.target, catalog::GROUPS[1], "{text:?} offered {c:?}");
            }
        }
    }
}
