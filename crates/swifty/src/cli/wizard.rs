//! A plugin's setup in the terminal: every kind of step a flow shows drawn as text, and what the person answers sent back to the
//! hub's setup engine. The web wizard draws the same steps and sends the same
//! inputs; the engine checks every input against the step it answers.
//!
//! - **Form** and **External**: each field in turn, the prefill as the default. A
//!   secret field is read without echo and never shown again. `--set key=value`
//!   answers a field ahead (never a secret: the command line is visible to others on
//!   the machine), and `--yes` takes every prefill without asking.
//! - **Choose**: the options numbered, the ticked ones marked; the person types the
//!   numbers. Enter keeps the ticked ones.
//! - **Wait**: the title and text, then a countdown while the hub polls the plugin.
//! - **Failed**: the reason, and the step again when it can be retried.
//! - **Done**: the summary, and nothing else: the config and the secrets never come
//!   back from the hub.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use swifty_hub::actions::Actor;
use swifty_hub::app::App;
use swifty_hub::error::ApiError;
use swifty_hub::setup::SetupView;
use swifty_plugin_api::{Choice, Field, FieldType, Scalar, SetupInput, SetupStep};

use super::terminal::Terminal;
use super::CliError;

/// The width text is wrapped to.
const WIDTH: usize = 76;
/// How often a wait looks at whether the step moved on.
const WAIT_LOOK: Duration = Duration::from_millis(200);
/// How long past a wait's own limit the terminal keeps watching before it gives up
/// on the setup. The hub's own limit ends the wait first.
const WAIT_GRACE: Duration = Duration::from_secs(30);

/// What answers a setup besides the person at the keyboard.
#[derive(Debug, Clone, Default)]
pub struct Answers {
    /// `--set key=value`: values given ahead, by field key.
    pub preset: BTreeMap<String, String>,
    /// `--yes`: take every prefill and the ticked choices without asking.
    pub take_defaults: bool,
}

impl Answers {
    /// Reads `--set` arguments: `key=value`, each key once.
    pub fn new(settings: &[String], take_defaults: bool) -> Result<Self, CliError> {
        let mut preset = BTreeMap::new();
        for setting in settings {
            let Some((key, value)) = setting.split_once('=') else {
                return Err(CliError::new(format!("--set {setting} has no value."))
                    .hint("Give each setting as --set key=value, such as --set url=http://192.0.2.10"));
            };
            let key = key.trim();
            if key.is_empty() {
                return Err(CliError::new(format!("--set {setting} has no key.")));
            }
            if preset.insert(key.to_string(), value.to_string()).is_some() {
                return Err(CliError::new(format!("--set gives {key} twice.")));
            }
        }
        Ok(Self { preset, take_defaults })
    }
}

/// Draws the steps of one setup and answers them, until it is done or fails.
pub struct Wizard<'a> {
    app: &'a App,
    actor: &'a Actor,
    term: &'a mut dyn Terminal,
    answers: Answers,
}

impl<'a> Wizard<'a> {
    pub fn new(app: &'a App, actor: &'a Actor, term: &'a mut dyn Terminal, answers: Answers) -> Self {
        Self { app, actor, term, answers }
    }

    /// Runs the setup from the step it shows now. Returns the summary of what was
    /// stored. Whatever stops it before `done` abandons it, so it leaves nothing.
    pub async fn run(mut self, first: SetupView) -> Result<String, CliError> {
        let id = first.setup_id.clone();
        let result = self.drive(first).await;
        if result.is_err() {
            let _ = self.app.setup.abandon(self.app, self.actor, &id).await;
        }
        result
    }

    async fn drive(&mut self, mut view: SetupView) -> Result<String, CliError> {
        loop {
            let step = view.step.clone();
            view = match step {
                SetupStep::Done(result) => {
                    self.term.say("");
                    self.term.say(&format!("Done. {}", result.summary));
                    return Ok(result.summary);
                }
                SetupStep::Failed { reason, retry: false } => {
                    return Err(CliError::new(reason).hint("Nothing was stored."));
                }
                SetupStep::Failed { reason, retry: true } => {
                    self.term.say("");
                    self.say_wrapped("", &reason);
                    let Some(again) = view.retry.clone() else {
                        return Err(CliError::new(reason).hint("Nothing was stored."));
                    };
                    // `--yes` with nobody at the keyboard would give the same answers
                    // again, and fail again.
                    if self.answers.take_defaults && !self.term.is_interactive() {
                        return Err(CliError::new(reason).hint("Nothing was stored."));
                    }
                    if !self.confirm("Try again?")? {
                        return Err(CliError::new("the setup was stopped.").hint("Nothing was stored."));
                    }
                    self.answer(&view, &again).await?
                }
                asking => self.answer(&view, &asking).await?,
            };
        }
    }

    /// Draws one step that asks for something, and sends what was answered.
    async fn answer(&mut self, view: &SetupView, step: &SetupStep) -> Result<SetupView, CliError> {
        match step {
            SetupStep::Form { title, text, fields } => {
                self.header(title, text);
                self.fill(view, fields).await
            }
            SetupStep::External { title, text, link, fields } => {
                self.header(title, text);
                if let Some(link) = link {
                    self.term.say(&format!("  Open: {link}"));
                }
                if fields.is_empty() && !self.answers.take_defaults {
                    self.pause("Press Enter when it is done.")?;
                }
                self.fill(view, fields).await
            }
            SetupStep::Choose { title, text, options, multiple, min } => {
                self.header(title, text);
                let ids = self.choose(options, *multiple, *min)?;
                self.send(view, SetupInput::Chosen { ids }).await
            }
            SetupStep::Wait { title, text, timeout_s, .. } => {
                self.header(title, text);
                if !matches!(view.step, SetupStep::Wait { .. }) {
                    // A wait offered again: asking once starts it over.
                    let next = self.send(view, SetupInput::Poll).await?;
                    if !matches!(&next.step, SetupStep::Wait { title: shown, .. } if shown == title) {
                        return Ok(next);
                    }
                    return self.wait(&next, title, *timeout_s).await;
                }
                self.wait(view, title, *timeout_s).await
            }
            SetupStep::Done(_) | SetupStep::Failed { .. } => Ok(view.clone()),
        }
    }

    async fn send(&self, view: &SetupView, input: SetupInput) -> Result<SetupView, CliError> {
        self.app.setup.advance(self.app, self.actor, &view.setup_id, input).await.map_err(refused)
    }

    // ----- drawing

    fn header(&mut self, title: &str, text: &str) {
        self.term.say("");
        self.term.say(title);
        self.say_wrapped("  ", text);
    }

    fn say_wrapped(&mut self, indent: &str, text: &str) {
        for line in wrap(text, WIDTH - indent.len()) {
            self.term.say(&format!("{indent}{line}"));
        }
    }

    fn ask(&mut self, prompt: &str) -> Result<String, CliError> {
        self.term.ask(prompt)?.ok_or_else(input_ended)
    }

    fn pause(&mut self, prompt: &str) -> Result<(), CliError> {
        self.ask(&format!("  {prompt} ")).map(|_| ())
    }

    /// A yes-or-no question whose answer is yes unless the person says no.
    fn confirm(&mut self, question: &str) -> Result<bool, CliError> {
        if self.answers.take_defaults {
            return Ok(true);
        }
        loop {
            let answer = self.ask(&format!("  {question} [Y/n]: "))?;
            match yes_or_no(&answer, true) {
                Some(yes) => return Ok(yes),
                None if self.term.is_interactive() => self.term.say("  Answer y or n."),
                None => return Err(CliError::new(format!("\"{answer}\" is not y or n."))),
            }
        }
    }

    // ----- forms

    /// Fills in a form and sends it. A value the hub refuses is asked for again.
    async fn fill(&mut self, view: &SetupView, fields: &[Field]) -> Result<SetupView, CliError> {
        let mut values = BTreeMap::new();
        for field in fields {
            if let Some(value) = self.value_for(field)? {
                values.insert(field.key.clone(), value);
            }
        }
        loop {
            let input = SetupInput::Values { values: values.clone() };
            match self.app.setup.advance(self.app, self.actor, &view.setup_id, input).await {
                Ok(next) => return Ok(next),
                Err(error) if error.code == "invalid_input" => {
                    let field = error
                        .extra
                        .as_ref()
                        .and_then(|extra| extra.get("field"))
                        .and_then(|field| field.as_str())
                        .and_then(|key| fields.iter().find(|field| field.key == key));
                    let Some(field) = field else { return Err(refused(error)) };
                    self.term.say(&format!("  {}", error.message));
                    if self.answers.preset.remove(&field.key).is_some() && !self.term.is_interactive() {
                        return Err(refused(error).hint(format!("It was given with --set {}=...", field.key)));
                    }
                    match self.ask_field(field)? {
                        Some(value) => values.insert(field.key.clone(), value),
                        None => values.remove(&field.key),
                    };
                }
                Err(error) => return Err(refused(error)),
            }
        }
    }

    /// The value of one field: given ahead, the prefill under `--yes`, or asked for.
    fn value_for(&mut self, field: &Field) -> Result<Option<Scalar>, CliError> {
        let hidden = matches!(field.kind, FieldType::Secret);
        if let Some(given) = self.answers.preset.get(&field.key).cloned() {
            if hidden {
                return Err(CliError::new(format!("--set cannot give {}: it is a secret.", field.key)).hint(
                    "Anyone on this machine can read a command line. Type it when the setup asks, \
                     or pipe it in with standard input.",
                ));
            }
            self.term.say(&format!("  {}: {given}", field.label));
            return interpret(field, &given).map_err(CliError::new);
        }
        if self.answers.take_defaults {
            if let Some(value) = &field.value {
                self.term.say(&format!("  {}: {}", field.label, shown(value)));
                return Ok(Some(value.clone()));
            }
            if !field.required {
                return Ok(match field.kind {
                    FieldType::Bool => Some(Scalar::Bool(false)),
                    _ => None,
                });
            }
        }
        self.ask_field(field)
    }

    /// Asks for one field until the answer fits it.
    fn ask_field(&mut self, field: &Field) -> Result<Option<Scalar>, CliError> {
        self.term.say("");
        if !field.help.is_empty() {
            self.say_wrapped("  ", &field.help);
        }
        let prompt = format!("  {}: ", prompt_for(field));
        loop {
            let answer = match field.kind {
                FieldType::Secret => self.term.ask_hidden(&prompt)?.ok_or_else(|| ended_at(field))?,
                FieldType::Multiline { .. } => self.ask_lines(&prompt, field)?,
                _ => self.term.ask(&prompt)?.ok_or_else(|| ended_at(field))?,
            };
            match interpret(field, &answer) {
                Ok(value) => return Ok(value),
                Err(problem) if self.term.is_interactive() => self.term.say(&format!("  {problem}")),
                Err(problem) => return Err(CliError::new(problem)),
            }
        }
    }

    /// A longer paste: lines until an empty one.
    fn ask_lines(&mut self, prompt: &str, field: &Field) -> Result<String, CliError> {
        self.term.say(&format!("{prompt}paste it, then an empty line."));
        let mut lines: Vec<String> = Vec::new();
        loop {
            match self.term.ask("  | ")? {
                Some(line) if !line.is_empty() => lines.push(line),
                Some(_) => break,
                None if lines.is_empty() => return Err(ended_at(field)),
                None => break,
            }
        }
        Ok(lines.join("\n"))
    }

    // ----- choices

    /// The ids picked from a list.
    fn choose(&mut self, options: &[Choice], multiple: bool, min: u32) -> Result<Vec<String>, CliError> {
        let width = options.iter().map(|option| option.label.chars().count()).max().unwrap_or(0).min(32);
        self.term.say("");
        for (n, option) in options.iter().enumerate() {
            let mark = if option.selected { "[x]" } else { "[ ]" };
            let line = format!("    {:>2}. {mark} {:<width$}  {}", n + 1, option.label, option.detail);
            self.term.say(line.trim_end());
        }
        let ticked: Vec<usize> = (0..options.len()).filter(|&n| options[n].selected).collect();
        let enough = |picked: &[usize]| picked.len() >= min as usize && (multiple || picked.len() <= 1);
        if self.answers.take_defaults && enough(&ticked) {
            let names: Vec<&str> = ticked.iter().map(|&n| options[n].label.as_str()).collect();
            self.term
                .say(&format!("  Picked: {}", if names.is_empty() { "none".to_string() } else { names.join(", ") }));
            return Ok(ticked.into_iter().map(|n| options[n].id.clone()).collect());
        }
        let prompt = if multiple {
            if ticked.is_empty() {
                "  Numbers to pick, such as 1 3, or all: ".to_string()
            } else {
                "  Numbers to pick, such as 1 3, or all (Enter keeps the ticked ones): ".to_string()
            }
        } else {
            match ticked.first() {
                Some(n) => format!("  Number to pick [{}]: ", n + 1),
                None => "  Number to pick: ".to_string(),
            }
        };
        loop {
            let answer = self.ask(&prompt)?;
            let picked =
                if answer.trim().is_empty() { Ok(ticked.clone()) } else { parse_picks(&answer, options.len()) };
            let problem = match picked {
                Ok(picked) if !multiple && picked.len() > 1 => "Pick one.".to_string(),
                Ok(picked) if picked.len() < min as usize => {
                    if min == 1 {
                        "Pick at least one.".to_string()
                    } else {
                        format!("Pick at least {min}.")
                    }
                }
                Ok(picked) => return Ok(picked.into_iter().map(|n| options[n].id.clone()).collect()),
                Err(problem) => problem,
            };
            if !self.term.is_interactive() {
                return Err(CliError::new(problem));
            }
            self.term.say(&format!("  {problem}"));
        }
    }

    // ----- waiting

    /// Shows a countdown while the hub polls the plugin, until the step moves on.
    async fn wait(&mut self, view: &SetupView, title: &str, timeout_s: u32) -> Result<SetupView, CliError> {
        let limit = Duration::from_secs(u64::from(timeout_s));
        let started = Instant::now();
        self.term.say("");
        loop {
            let waited = started.elapsed();
            let left = limit.saturating_sub(waited);
            self.term.status(&countdown(left, timeout_s > 0));
            tokio::time::sleep(WAIT_LOOK).await;
            let now = self.app.setup.view(self.app, self.actor, &view.setup_id).map_err(refused)?;
            let same_wait = matches!(&now.step, SetupStep::Wait { title: shown, .. } if shown == title);
            if !same_wait || now.finished {
                self.term.end_status();
                return Ok(now);
            }
            if waited > limit + WAIT_GRACE {
                self.term.end_status();
                return Err(CliError::new("the setup stopped answering.").hint("Nothing was stored. Run it again."));
            }
        }
    }
}

// ----- what is typed

/// What an answer means for a field. `Ok(None)` leaves the field out.
fn interpret(field: &Field, answer: &str) -> Result<Option<Scalar>, String> {
    let label = field.label.as_str();
    // A secret is taken as typed; anything else without the spaces around it.
    let answer = if matches!(field.kind, FieldType::Secret) { answer } else { answer.trim() };
    if answer.is_empty() {
        return match (&field.value, field.required, &field.kind) {
            (Some(prefill), _, _) => Ok(Some(prefill.clone())),
            (None, true, _) => Err(format!("{label} is required.")),
            (None, false, FieldType::Bool) => Ok(Some(Scalar::Bool(false))),
            // An empty text keeps what is stored, where the plugin offers that.
            (None, false, _) => Ok(Some(Scalar::Text(String::new()))),
        };
    }
    match &field.kind {
        FieldType::Bool => {
            yes_or_no(answer, false).map(|yes| Some(Scalar::Bool(yes))).ok_or_else(|| "Answer y or n.".to_string())
        }
        FieldType::Port => match answer.parse::<i64>() {
            Ok(port) if (1..=65535).contains(&port) => Ok(Some(Scalar::Int(port))),
            _ => Err(format!("{label} must be a port, from 1 to 65535.")),
        },
        FieldType::Int { min, max } => match answer.parse::<i64>() {
            Ok(n) if (*min..=*max).contains(&n) => Ok(Some(Scalar::Int(n))),
            _ => Err(format!("{label} must be a whole number from {min} to {max}.")),
        },
        FieldType::Choice { options } => options
            .iter()
            .find(|option| option.eq_ignore_ascii_case(answer))
            .map(|option| Some(Scalar::Text(option.clone())))
            .ok_or_else(|| format!("{label} must be one of: {}.", options.join(", "))),
        FieldType::Text { max_len } if answer.chars().count() > *max_len => {
            Err(format!("{label} must be at most {max_len} characters."))
        }
        FieldType::Multiline { max_len } if answer.chars().count() > *max_len => {
            Err(format!("{label} must be at most {max_len} characters."))
        }
        _ => Ok(Some(Scalar::Text(answer.to_string()))),
    }
}

/// `y`, `yes`, `n`, `no` and the like. An empty answer is `empty`.
pub fn yes_or_no(answer: &str, empty: bool) -> Option<bool> {
    match answer.trim().to_ascii_lowercase().as_str() {
        "" => Some(empty),
        "y" | "yes" | "true" | "on" => Some(true),
        "n" | "no" | "false" | "off" => Some(false),
        _ => None,
    }
}

/// The question for a field: its label, what it takes, and its prefill.
fn prompt_for(field: &Field) -> String {
    let label = &field.label;
    let default = field.value.as_ref().map(|value| format!(" [{}]", shown(value))).unwrap_or_default();
    let optional = if field.required || field.value.is_some() { "" } else { ", optional" };
    match &field.kind {
        FieldType::Bool => {
            let yes = matches!(field.value, Some(Scalar::Bool(true)));
            format!("{label}? [{}]", if yes { "Y/n" } else { "y/N" })
        }
        FieldType::Secret => format!("{label} (hidden{optional})"),
        FieldType::Port => format!("{label} (1-65535{optional}){default}"),
        FieldType::Int { min, max } => format!("{label} ({min}-{max}{optional}){default}"),
        FieldType::Choice { options } => format!("{label} ({}{optional}){default}", options.join(", ")),
        _ if optional.is_empty() => format!("{label}{default}"),
        _ => format!("{label} (optional){default}"),
    }
}

/// A prefill as it is shown.
fn shown(value: &Scalar) -> String {
    match value {
        Scalar::Text(text) => text.clone(),
        Scalar::Int(n) => n.to_string(),
        Scalar::Bool(true) => "yes".to_string(),
        Scalar::Bool(false) => "no".to_string(),
    }
}

/// The options picked, by position from 0: numbers from 1, ranges such as `2-4`,
/// `all` or `none`, separated by spaces or commas.
pub fn parse_picks(answer: &str, count: usize) -> Result<Vec<usize>, String> {
    let answer = answer.trim().to_ascii_lowercase();
    match answer.as_str() {
        "all" => return Ok((0..count).collect()),
        "none" => return Ok(Vec::new()),
        _ => {}
    }
    let out_of_range = |n: usize| format!("There is no {n}: pick from 1 to {count}.");
    let mut picked = Vec::new();
    for word in answer.split(|c: char| c == ',' || c.is_whitespace()).filter(|word| !word.is_empty()) {
        let (from, to) = match word.split_once('-') {
            Some((from, to)) => (from, to),
            None => (word, word),
        };
        let (Ok(from), Ok(to)) = (from.parse::<usize>(), to.parse::<usize>()) else {
            return Err(format!("\"{word}\" is not a number from the list."));
        };
        for n in [from, to] {
            if n == 0 || n > count {
                return Err(out_of_range(n));
            }
        }
        for n in from.min(to)..=from.max(to) {
            if !picked.contains(&(n - 1)) {
                picked.push(n - 1);
            }
        }
    }
    Ok(picked)
}

/// The countdown line of a wait.
fn countdown(left: Duration, limited: bool) -> String {
    if !limited {
        return "  Waiting. Ctrl+C stops; nothing is stored.".to_string();
    }
    let secs = left.as_secs() + u64::from(left.subsec_nanos() > 0);
    format!("  Waiting: {}:{:02} left. Ctrl+C stops; nothing is stored.", secs / 60, secs % 60)
}

/// Words in lines of at most `width` characters.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        lines.push(line);
    }
    lines
}

// ----- errors

/// What the hub refused, for a person.
pub fn refused(error: ApiError) -> CliError {
    let message = error.message.trim_end_matches('.').to_string();
    let hint = match error.code {
        "instance_exists" => error
            .extra
            .as_ref()
            .and_then(|extra| extra.get("instance"))
            .and_then(|id| id.as_str())
            .map(|id| format!("Set that one up again: sudo swifty plugin setup {id}")),
        "setup_open" => Some("A setup of it is open in the app. Finish or abandon it there first.".to_string()),
        "setup_busy" => Some("Setups open in the app count too.".to_string()),
        _ => None,
    };
    let error = CliError::new(format!("{message}."));
    match hint {
        Some(hint) => error.hint(hint),
        None => error,
    }
}

fn input_ended() -> CliError {
    CliError::new("standard input ended before the setup finished.")
        .hint("Nothing was stored. Answer every question, one line each, or run it in a terminal.")
}

fn ended_at(field: &Field) -> CliError {
    let hint = if matches!(field.kind, FieldType::Secret) {
        "Nothing was stored. Answer every question on standard input, one line each.".to_string()
    } else {
        format!(
            "Nothing was stored. Answer every question on standard input, one line each, \
             or give it ahead: --set {}=VALUE",
            field.key
        )
    };
    CliError::new(format!("standard input ended before the setup asked for {}.", field.label)).hint(hint)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(kind: FieldType, required: bool, value: Option<Scalar>) -> Field {
        Field { key: "k".into(), label: "Thing".into(), help: String::new(), kind, required, value }
    }

    #[test]
    fn picks_are_numbers_ranges_all_or_none() {
        assert_eq!(parse_picks("1 3", 4), Ok(vec![0, 2]));
        assert_eq!(parse_picks("2-4, 1", 4), Ok(vec![1, 2, 3, 0]));
        assert_eq!(parse_picks("3,3,1", 4), Ok(vec![2, 0]));
        assert_eq!(parse_picks("ALL", 3), Ok(vec![0, 1, 2]));
        assert_eq!(parse_picks("none", 3), Ok(vec![]));
        assert!(parse_picks("0", 3).is_err());
        assert!(parse_picks("4", 3).unwrap_err().contains("from 1 to 3"));
        assert!(parse_picks("lamp", 3).is_err());
    }

    #[test]
    fn answers_are_read_as_the_field_takes_them() {
        let port = field(FieldType::Port, false, Some(Scalar::Int(80)));
        assert_eq!(interpret(&port, ""), Ok(Some(Scalar::Int(80))));
        assert_eq!(interpret(&port, " 8080 "), Ok(Some(Scalar::Int(8080))));
        assert!(interpret(&port, "70000").is_err());
        let level = field(FieldType::Int { min: 1, max: 5 }, true, None);
        assert!(interpret(&level, "").unwrap_err().contains("required"));
        assert!(interpret(&level, "9").unwrap_err().contains("from 1 to 5"));
        let on = field(FieldType::Bool, false, None);
        assert_eq!(interpret(&on, "Yes"), Ok(Some(Scalar::Bool(true))));
        assert_eq!(interpret(&on, ""), Ok(Some(Scalar::Bool(false))));
        assert!(interpret(&on, "maybe").is_err());
        let region = field(FieldType::Choice { options: vec!["eu".into(), "us".into()] }, true, None);
        assert_eq!(interpret(&region, "EU"), Ok(Some(Scalar::Text("eu".into()))));
        assert!(interpret(&region, "asia").unwrap_err().contains("eu, us"));
        let name = field(FieldType::Text { max_len: 3 }, false, None);
        assert_eq!(interpret(&name, ""), Ok(Some(Scalar::Text(String::new()))));
        assert!(interpret(&name, "four").is_err());
        // A secret keeps its spaces; anything else loses them.
        let code = field(FieldType::Secret, false, None);
        assert_eq!(interpret(&code, " a b "), Ok(Some(Scalar::Text(" a b ".into()))));
        let host = field(FieldType::Host, true, None);
        assert_eq!(interpret(&host, " 192.0.2.1 "), Ok(Some(Scalar::Text("192.0.2.1".into()))));
    }

    #[test]
    fn prompts_say_what_a_field_takes() {
        assert_eq!(prompt_for(&field(FieldType::Port, false, Some(Scalar::Int(80)))), "Thing (1-65535) [80]");
        assert_eq!(prompt_for(&field(FieldType::Secret, false, None)), "Thing (hidden, optional)");
        assert_eq!(prompt_for(&field(FieldType::Secret, true, None)), "Thing (hidden)");
        assert_eq!(prompt_for(&field(FieldType::Bool, false, Some(Scalar::Bool(true)))), "Thing? [Y/n]");
        assert_eq!(prompt_for(&field(FieldType::Url, true, None)), "Thing");
        assert_eq!(prompt_for(&field(FieldType::Url, false, None)), "Thing (optional)");
    }

    #[test]
    fn a_countdown_shows_minutes_and_seconds_left() {
        assert!(countdown(Duration::from_secs(90), true).contains("1:30 left"));
        assert!(countdown(Duration::from_millis(1500), true).contains("0:02 left"));
        assert!(countdown(Duration::ZERO, true).contains("0:00 left"));
        assert!(!countdown(Duration::ZERO, false).contains("left"));
    }

    #[test]
    fn settings_given_ahead_are_read_once_each() {
        let answers = Answers::new(&["url=http://192.0.2.10".into(), "name = a=b".into()], false).unwrap();
        assert_eq!(answers.preset["url"], "http://192.0.2.10");
        assert_eq!(answers.preset["name"], " a=b");
        assert!(Answers::new(&["url".into()], false).is_err());
        assert!(Answers::new(&["=x".into()], false).is_err());
        assert!(Answers::new(&["a=1".into(), "a=2".into()], false).is_err());
    }

    #[test]
    fn text_is_wrapped_at_word_boundaries() {
        let lines = wrap("one two three four", 9);
        assert_eq!(lines, ["one two", "three", "four"]);
        assert_eq!(wrap("", 10), Vec::<String>::new());
        assert_eq!(wrap("a\nb", 10), ["a", "b"]);
    }
}
