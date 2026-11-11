//! Check 1: the manifest.

use std::collections::BTreeSet;

use swifty_plugin_api::{ConfigField, FieldKind, Manifest, ParamDef, ParamKind, Tier, API_VERSION};

/// Words the voice grammar already acts on: verbs, filler, negations,
/// quantities and questions. A plugin may not offer one of them as a mishearing
/// or a noun, because that would change what a sentence means.
///
/// The hub's own check, `voice::is_reserved_phrase`, is the final word and drops
/// such a word at startup. This list catches the plain cases early, in the
/// plugin's own tests, without the hub.
#[rustfmt::skip]
pub const GRAMMAR_WORDS: &[&str] = &[
    // filler
    "please", "the", "a", "an", "can", "could", "would", "will", "you", "my", "to", "of", "it", "now", "hey",
    "um", "uh", "uhm", "er", "for", "me", "i", "want", "need", "like", "just", "at", "in", "is", "are", "and",
    "do", "some", "this", "that", "set", "make", "turn", "switch", "with", "up", "down",
    // negations and disjunctions
    "not", "never", "no", "dont", "cant", "cannot", "wont", "or", "either",
    // verbs and states
    "start", "boot", "power", "bring", "stop", "shutdown", "shut", "halt", "restart", "reboot", "reset",
    "bounce", "pause", "resume", "status", "state", "on", "off", "open", "show", "go", "kill", "lock", "help",
    "mute", "unmute", "dim", "brighten", "louder", "quieter", "softer", "lower",
    // quantities and questions
    "all", "every", "everything", "more", "less", "much", "percent", "what", "how", "which",
    // the colour word, and the class nouns the grammar keeps for itself
    "hue", "ac", "aircon", "plug", "plugs", "socket", "sockets",
    // safety
    "emergency", "estop", "e",
];

/// Whether `word` is one of [`GRAMMAR_WORDS`], or a number.
pub fn is_grammar_word(word: &str) -> bool {
    GRAMMAR_WORDS.contains(&word) || (!word.is_empty() && word.chars().all(|c| c.is_ascii_digit()))
}

/// `[a-z][a-z0-9-]{1,31}`.
fn is_plugin_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && (2..=32).contains(&id.len())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `[a-z][a-z0-9_]{0,31}`: a config key, a slot name, a parameter or a verb.
fn is_snake_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && name.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// A secret slot: `name`, or `name/{key}` for one slot per row.
fn is_slot(slot: &str) -> bool {
    match slot.split_once('/') {
        None => is_snake_name(slot),
        Some((name, row)) => {
            is_snake_name(name)
                && row.strip_prefix('{').and_then(|rest| rest.strip_suffix('}')).is_some_and(is_snake_name)
        }
    }
}

/// One or more lower-case words of letters and digits, separated by single spaces.
fn is_words(text: &str) -> bool {
    !text.is_empty()
        && text
            .split(' ')
            .all(|word| !word.is_empty() && word.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()))
}

/// `major.minor.patch`, with an optional pre-release or build suffix.
fn is_semver(version: &str) -> bool {
    let core = version.split(['-', '+']).next().unwrap_or("");
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3 && parts.iter().all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

/// Everything wrong with a manifest, one sentence each. `crate_version` is the
/// version of the plugin's crate, when it is known.
pub fn manifest_problems(manifest: &Manifest, crate_version: Option<&str>) -> Vec<String> {
    let mut problems = Vec::new();
    let id = manifest.id;
    if !is_plugin_id(id) {
        problems.push(format!("the id `{id}` must match [a-z][a-z0-9-]{{1,31}}"));
    }
    if manifest.name.trim().is_empty() {
        problems.push("the name is empty".to_string());
    }
    if manifest.summary.trim().is_empty() {
        problems.push("the summary is empty".to_string());
    }
    match crate_version {
        Some(expected) if manifest.version != expected => problems.push(format!(
            "version is `{}` but the crate is `{expected}`; use env!(\"CARGO_PKG_VERSION\")",
            manifest.version
        )),
        _ if !is_semver(manifest.version) => {
            problems.push(format!("version `{}` is not a crate version", manifest.version))
        }
        _ => {}
    }
    if manifest.api != API_VERSION {
        problems.push(format!("api is {} but this hub speaks plugin API {API_VERSION}", manifest.api));
    }
    if manifest.classes.is_empty() {
        problems.push("classes is empty: a plugin offers at least one class of device".to_string());
    }

    config_field_problems(manifest.config, "config", &mut problems);

    let mut slots = BTreeSet::new();
    for slot in manifest.secrets {
        if !is_slot(slot.slot) {
            problems.push(format!("the secret slot `{}` must be `name` or `name/{{key}}`, in [a-z0-9_]", slot.slot));
        }
        if !slots.insert(slot.slot) {
            problems.push(format!("the secret slot `{}` is declared twice", slot.slot));
        }
        if manifest.config.iter().any(|field| field.key == slot.slot) {
            problems
                .push(format!("`{}` is both a config key and a secret slot; secrets never live in config", slot.slot));
        }
    }

    let mut actions = BTreeSet::new();
    for action in manifest.actions {
        let verb = action.id.strip_prefix(id).and_then(|rest| rest.strip_prefix('.'));
        match verb {
            Some(verb) if is_snake_name(verb) => {}
            _ => problems
                .push(format!("the action `{}` must be named `{id}.<verb>`, with the verb in [a-z0-9_]", action.id)),
        }
        if !actions.insert(action.id) {
            problems.push(format!("the action `{}` is declared twice", action.id));
        }
        if matches!(action.tier, Tier::Read | Tier::SafeDirection) {
            problems.push(format!(
                "the action `{}` asks for tier {}; a custom action is comfort, consequential or critical",
                action.id,
                action.tier.name()
            ));
        }
        if !manifest.classes.contains(&action.class) {
            problems.push(format!("the action `{}` acts on a class the manifest does not offer", action.id));
        }
        if action.label.trim().is_empty() || action.description.trim().is_empty() {
            problems.push(format!("the action `{}` needs a label and a description", action.id));
        }
        param_problems(action.id, action.params, &mut problems);
    }

    for (heard, meant) in manifest.vocabulary {
        if !is_words(heard) || !is_words(meant) {
            problems.push(format!("vocabulary (\"{heard}\", \"{meant}\") must be lower-case words"));
        }
        if heard.split(' ').any(is_grammar_word) {
            problems.push(format!("vocabulary \"{heard}\" is a word the voice grammar already acts on"));
        }
        if heard == meant {
            problems.push(format!("vocabulary (\"{heard}\", \"{meant}\") maps a word to itself"));
        }
    }
    for (noun, class) in manifest.nouns {
        if !is_words(noun) {
            problems.push(format!("the noun \"{noun}\" must be lower-case words"));
        }
        if noun.split(' ').any(is_grammar_word) {
            problems.push(format!("the noun \"{noun}\" is a word the voice grammar already acts on"));
        }
        if !manifest.classes.contains(class) {
            problems.push(format!("the noun \"{noun}\" names a class the manifest does not offer"));
        }
    }
    problems
}

fn param_problems(action: &str, params: &[ParamDef], problems: &mut Vec<String>) {
    let mut names = BTreeSet::new();
    for param in params {
        let name = param.name;
        if !is_snake_name(name) {
            problems.push(format!("the parameter `{name}` of `{action}` must match [a-z][a-z0-9_]*"));
        }
        if !names.insert(name) {
            problems.push(format!("the parameter `{name}` of `{action}` is declared twice"));
        }
        match param.kind {
            ParamKind::Int { min, max } if min > max => {
                problems.push(format!("the parameter `{name}` of `{action}` has min {min} above max {max}"))
            }
            ParamKind::Enum(options) => {
                if options.is_empty() {
                    problems.push(format!("the parameter `{name}` of `{action}` has no options"));
                }
                if options.iter().collect::<BTreeSet<_>>().len() != options.len() {
                    problems.push(format!("the parameter `{name}` of `{action}` repeats an option"));
                }
                if options.iter().any(|option| !is_snake_name(option)) {
                    problems.push(format!("an option of the parameter `{name}` of `{action}` is not [a-z][a-z0-9_]*"));
                }
            }
            ParamKind::Label { max_len: 0 } => {
                problems.push(format!("the parameter `{name}` of `{action}` allows no characters"))
            }
            _ => {}
        }
    }
}

/// Keys, kinds and defaults of config fields, rows included.
fn config_field_problems(fields: &[ConfigField], at: &str, problems: &mut Vec<String>) {
    let mut keys = BTreeSet::new();
    for field in fields {
        let key = field.key;
        if !is_snake_name(key) {
            problems.push(format!("the {at} key `{key}` must match [a-z][a-z0-9_]*"));
        }
        if !keys.insert(key) {
            problems.push(format!("the {at} key `{key}` is declared twice"));
        }
        if field.required && field.default.is_some() {
            problems.push(format!("the {at} key `{key}` is required and has a default; it is one or the other"));
        }
        match field.kind {
            FieldKind::Text { max_len: 0 } => problems.push(format!("the {at} key `{key}` allows no characters")),
            FieldKind::Int { min, max } if min > max => {
                problems.push(format!("the {at} key `{key}` has min {min} above max {max}"))
            }
            FieldKind::Url { schemes: [] } => problems.push(format!("the {at} key `{key}` allows no URL scheme")),
            FieldKind::Choice { options } => {
                if options.is_empty() {
                    problems.push(format!("the {at} key `{key}` has no options"));
                }
                if let Some(default) = field.default {
                    if !options.contains(&default) {
                        problems.push(format!("the default of the {at} key `{key}` is not one of its options"));
                    }
                }
            }
            FieldKind::Rows { fields } => {
                if fields.is_empty() {
                    problems.push(format!("the rows of the {at} key `{key}` have no fields"));
                }
                config_field_problems(fields, &format!("{at} row of `{key}`"), problems);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grammars_hold() {
        for good in ["ab", "wiz", "acme-blinds", "a1"] {
            assert!(is_plugin_id(good), "{good}");
        }
        for bad in ["a", "", "Wiz", "1wiz", "wiz_x", "wiz.x", &"a".repeat(33)] {
            assert!(!is_plugin_id(bad), "{bad}");
        }
        for good in ["app_key", "local_key/{id}", "token"] {
            assert!(is_slot(good), "{good}");
        }
        for bad in ["", "App", "local_key/id", "local_key/{}", "key/{id}/x", "a-b"] {
            assert!(!is_slot(bad), "{bad}");
        }
        assert!(is_words("whiz") && is_words("living room") && !is_words("Whiz") && !is_words("a  b"));
        assert!(is_semver("0.1.0") && is_semver("1.2.3-rc.1") && !is_semver("1.2") && !is_semver("x.y.z"));
        assert!(is_grammar_word("off") && is_grammar_word("42") && !is_grammar_word("wiz"));
    }
}
