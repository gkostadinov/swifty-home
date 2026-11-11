//! Check 10's yardstick: text a device supplied, made safe.

use serde_json::Value;

/// The longest device-supplied text, in characters, that may come back.
/// `swifty_plugin_api::net::device_text` keeps 160.
pub const MAX_DEVICE_TEXT: usize = 256;

/// What the suite asks a device to report as its name: control characters,
/// an escape sequence, a line break, then 10 kB of text.
pub const HOSTILE_TEXT: &str = "Hostile\u{0}\u{7}\u{1b}[2J\r\nname\u{9f}";

/// [`HOSTILE_TEXT`] followed by 10 240 more characters.
pub(super) fn hostile() -> String {
    format!("{HOSTILE_TEXT}{}", "x".repeat(10_240))
}

/// No control characters, and not too long.
pub(super) fn is_clean(text: &str) -> bool {
    !text.chars().any(char::is_control) && text.chars().count() <= MAX_DEVICE_TEXT
}

/// Every string in a JSON value, keys included, with where it is.
pub(super) fn strings(value: &Value, at: &str, out: &mut Vec<(String, String)>) {
    match value {
        Value::String(text) => out.push((at.to_string(), text.clone())),
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                strings(item, &format!("{at}[{index}]"), out);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                out.push((format!("{at} key"), key.clone()));
                strings(item, &format!("{at}.{key}"), out);
            }
        }
        _ => {}
    }
}

/// A short, printable picture of `text` for a problem's sentence.
pub(super) fn preview(text: &str) -> String {
    let shown: String = text.chars().take(40).collect::<String>().escape_debug().to_string();
    let count = text.chars().count();
    if count > 40 {
        format!("\"{shown}...\" ({count} characters)")
    } else {
        format!("\"{shown}\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hostile_text_is_neither_clean_nor_short() {
        assert!(!is_clean(HOSTILE_TEXT));
        let long = hostile();
        assert!(long.len() > 10_000);
        let cleaned = swifty_plugin_api::net::device_text(&long);
        assert!(is_clean(&cleaned), "device_text passes the check");
        assert!(preview(&long).ends_with(&format!("({} characters)", long.chars().count())));
    }
}
