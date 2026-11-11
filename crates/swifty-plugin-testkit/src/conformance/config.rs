//! Check 2: the config description and the typed config agree.

use serde_json::{json, Map, Value};
use swifty_plugin_api::{ConfigField, ConfigProblem, FieldKind, Plugin};

/// A key no plugin declares.
const UNKNOWN_KEY: &str = "conformance_unknown_key";

/// A value of the right kind for a field: its default when it has one,
/// otherwise a documentation address, the lowest number, the first option.
fn sample(field: &ConfigField) -> Value {
    if let Some(default) = field.default {
        return serde_json::from_str(default).unwrap_or_else(|_| json!(default));
    }
    match field.kind {
        FieldKind::Text { max_len } => json!("x".repeat(max_len.min(8))),
        FieldKind::Host => json!("192.0.2.1"),
        FieldKind::Url { schemes } => json!(format!("{}://192.0.2.1/", schemes.first().copied().unwrap_or("http"))),
        FieldKind::HostPort => json!("192.0.2.1:80"),
        FieldKind::Port => json!(80),
        FieldKind::Int { min, .. } => json!(min),
        FieldKind::Bool => json!(false),
        FieldKind::Choice { options } => json!(options.first().copied().unwrap_or("")),
        // From the range set aside for documentation (RFC 7042).
        FieldKind::Mac => json!("00:00:5e:00:53:01"),
        FieldKind::Rows { fields } => json!([sample_row(fields)]),
    }
}

/// A row with every required field of `fields`.
fn sample_row(fields: &[ConfigField]) -> Value {
    let mut row = Map::new();
    for field in fields.iter().filter(|field| field.required) {
        row.insert(field.key.to_string(), sample(field));
    }
    Value::Object(row)
}

/// Whether a refusal names `key`: in its field, or in its message.
fn names(problem: &ConfigProblem, key: &str) -> bool {
    problem.field == key
        || problem.field.ends_with(&format!(".{key}"))
        || problem.field.split(['.', '[', ']']).any(|part| part == key)
        || problem.message.contains(key)
}

/// Everything wrong with how a plugin checks its config, one sentence each.
/// `config` is the fixture's config, which must be valid.
pub fn config_problems(plugin: &dyn Plugin, config: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    let manifest = plugin.manifest();
    let Some(object) = config.as_object() else {
        return vec!["the fixture's config is not a JSON object".to_string()];
    };
    if let Err(problem) = plugin.validate(config) {
        return vec![format!("the fixture's config does not validate: {problem}")];
    }

    // Every key in the fixture is declared.
    for key in object.keys() {
        if !manifest.config.iter().any(|field| field.key == key) {
            problems.push(format!("the fixture's config has `{key}`, which the manifest does not declare"));
        }
    }

    // An unknown key is refused, and named.
    let mut unknown = object.clone();
    unknown.insert(UNKNOWN_KEY.to_string(), json!(true));
    match plugin.validate(&Value::Object(unknown)) {
        Ok(()) => {
            problems.push(format!("a config with the unknown key `{UNKNOWN_KEY}` validates; refuse unknown keys"))
        }
        Err(problem) if !names(&problem, UNKNOWN_KEY) => {
            problems.push(format!("the refusal of an unknown key does not name it: {problem}"))
        }
        Err(_) => {}
    }

    for field in manifest.config {
        let key = field.key;
        if field.required {
            // A required key is required.
            let mut without = object.clone();
            without.remove(key);
            match plugin.validate(&Value::Object(without)) {
                Ok(()) => problems.push(format!("a config without the required key `{key}` validates")),
                Err(problem) if !names(&problem, key) => {
                    problems.push(format!("the refusal of a config without `{key}` does not name it: {problem}"))
                }
                Err(_) => {}
            }
        } else if !object.contains_key(key) {
            // A declared key parses.
            let mut with = object.clone();
            with.insert(key.to_string(), sample(field));
            if let Err(problem) = plugin.validate(&Value::Object(with)) {
                problems.push(format!("the declared key `{key}` does not parse with a value of its kind: {problem}"));
            }
        }
        if let FieldKind::Rows { fields } = field.kind {
            row_problems(plugin, object, key, fields, &mut problems);
        }
    }

    // A current config is left alone.
    match plugin.migrate(manifest.config_version, config.clone()) {
        Ok(migrated) if &migrated == config => {}
        Ok(_) => problems.push("migrate changes a config that is already at config_version".to_string()),
        Err(problem) => problems.push(format!("migrate refuses a config that is already at config_version: {problem}")),
    }
    problems
}

/// The same checks for the first row of a `Rows` field, when the fixture has one.
fn row_problems(
    plugin: &dyn Plugin,
    object: &Map<String, Value>,
    key: &str,
    fields: &[ConfigField],
    problems: &mut Vec<String>,
) {
    let Some(first) = object.get(key).and_then(Value::as_array).and_then(|rows| rows.first()) else { return };
    let Some(row) = first.as_object() else {
        problems.push(format!("the first row of `{key}` in the fixture is not an object"));
        return;
    };
    let with_row = |row: Map<String, Value>| {
        let mut config = object.clone();
        let mut rows = config[key].as_array().cloned().unwrap_or_default();
        rows[0] = Value::Object(row);
        config.insert(key.to_string(), Value::Array(rows));
        plugin.validate(&Value::Object(config))
    };
    for name in row.keys() {
        if !fields.iter().any(|field| field.key == name) {
            problems.push(format!("a row of `{key}` in the fixture has `{name}`, which the manifest does not declare"));
        }
    }
    let mut unknown = row.clone();
    unknown.insert(UNKNOWN_KEY.to_string(), json!(true));
    match with_row(unknown) {
        Ok(()) => problems.push(format!("a row of `{key}` with an unknown key validates; refuse unknown keys")),
        Err(problem) if !names(&problem, UNKNOWN_KEY) => {
            problems.push(format!("the refusal of an unknown key in a row of `{key}` does not name it: {problem}"))
        }
        Err(_) => {}
    }
    for field in fields.iter().filter(|field| field.required) {
        let mut without = row.clone();
        without.remove(field.key);
        match with_row(without) {
            Ok(()) => problems.push(format!("a row of `{key}` without the required `{}` validates", field.key)),
            Err(problem) if !names(&problem, field.key) => problems
                .push(format!("the refusal of a row of `{key}` without `{}` does not name it: {problem}", field.key)),
            Err(_) => {}
        }
    }
}
