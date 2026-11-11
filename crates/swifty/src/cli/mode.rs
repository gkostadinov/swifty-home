//! `swifty go-live` and `swifty go-dry`: the only way to change the mode.
//!
//! Each rewrites the one line `live = ...` of `swifty.toml` and leaves every other
//! line, comment and setting as it was. The new file is checked before it replaces
//! the old one, and it keeps the old one's owner, group and mode. The running hub
//! reads the mode when it starts, so the command says to restart it.
//!
//! The configuration directory belongs to root (`/etc/swifty-home`, root and the
//! service's group, 0750), so the file is written as the user who runs the command,
//! before the command becomes the owner of the data to write the audit row.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::json;
use swifty_hub::admin::init::parse_config;
use swifty_hub::audit::{self, AuditKey};
use swifty_hub::clock::{Clock, SystemClock};
use swifty_hub::config::Config;
use swifty_hub::store::Store;

use super::{config_error, missing_config, CliError};
use crate::privileges;

/// The audit action of a change of mode.
pub const MODE_ACTION: &str = "system.mode";

pub fn go_live(config_path: &Path) -> Result<ExitCode, CliError> {
    set_mode(config_path, true)
}

pub fn go_dry(config_path: &Path) -> Result<ExitCode, CliError> {
    set_mode(config_path, false)
}

fn set_mode(config_path: &Path, live: bool) -> Result<ExitCode, CliError> {
    let user = privileges::user_name();
    let text = std::fs::read_to_string(config_path).map_err(|error| missing_config(config_path, &error))?;
    let config = parse_config(&text).map_err(|error| config_error(config_path, error))?;
    let (now_word, restart) = if live {
        ("live mode: commands reach real devices", "Restart the hub so commands reach real devices")
    } else {
        ("dry-run: no command reaches a device", "Restart the hub so no command reaches a device")
    };
    if config.live == live {
        println!("Swifty Home is set to {now_word} already. Nothing changed.");
        return Ok(ExitCode::SUCCESS);
    }
    let changed = with_live(&text, live);
    // What is written must read back as this mode, and as valid as before.
    match parse_config(&changed) {
        Ok(checked) if checked.live == live => {}
        _ => {
            return Err(CliError::new(format!(
                "the live line of {} could not be changed safely.",
                config_path.display()
            ))
            .hint(format!("Set `live = {live}` in it by hand, then check it: swifty config check")))
        }
    }
    replace(config_path, &changed)?;
    println!("Swifty Home is set to {now_word}.");
    if let Err(problem) = audit_change(&config, &user, live) {
        println!("note: the change is not in the audit log: {problem}");
    }
    println!();
    println!("{restart}:");
    println!("  sudo systemctl restart swifty-home");
    println!("(or stop `swifty serve` and start it again).");
    Ok(ExitCode::SUCCESS)
}

/// The text with its top-level `live = ...` set to `live`: that line changed in
/// place, or one added after `config_version` when there is none.
pub fn with_live(text: &str, live: bool) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let new_value = if live { "true" } else { "false" };
    let mut out = String::with_capacity(text.len() + 16);
    let mut done = false;
    let mut in_table = false;
    for line in &lines {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            if !done {
                // No top-level line: it goes before the first table.
                out.push_str(&format!("live = {new_value}\n\n"));
                done = true;
            }
            in_table = true;
        }
        if !done && !in_table && is_live_line(trimmed) {
            let indent = &line[..line.len() - trimmed.len()];
            let comment = trimmed.find('#').map(|at| format!(" {}", trimmed[at..].trim_end())).unwrap_or_default();
            let ending = if line.ends_with("\r\n") {
                "\r\n"
            } else if line.ends_with('\n') {
                "\n"
            } else {
                ""
            };
            out.push_str(&format!("{indent}live = {new_value}{comment}{ending}"));
            done = true;
            continue;
        }
        out.push_str(line);
    }
    if !done {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!("live = {new_value}\n"));
    }
    out
}

/// `live = ...` (with any spacing), and not `lively = ...` or a comment.
fn is_live_line(trimmed: &str) -> bool {
    trimmed.strip_prefix("live").is_some_and(|rest| rest.trim_start().starts_with('='))
}

/// The file next to `path` that is written first and then renamed over it.
fn pending_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".new");
    PathBuf::from(name)
}

/// Writes `text` over `path`: a new file beside it with the same owner, group and
/// mode, synced, then renamed over the old one, so the old file is whole until the
/// new one is.
fn replace(path: &Path, text: &str) -> Result<(), CliError> {
    let cannot = |error: std::io::Error| {
        let shown = path.display();
        match error.kind() {
            std::io::ErrorKind::PermissionDenied => {
                CliError::new(format!("cannot write {shown}: permission denied.")).hint("Run it with sudo.")
            }
            _ => CliError::new(format!("cannot write {shown}: {error}")),
        }
    };
    let metadata = std::fs::metadata(path).map_err(cannot)?;
    let pending = pending_path(path);
    let _ = std::fs::remove_file(&pending);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::fs::PermissionsExt;
        options.mode(metadata.permissions().mode() & 0o777);
    }
    let written = (|| -> std::io::Result<()> {
        let mut file = options.open(&pending)?;
        file.write_all(text.as_bytes())?;
        file.set_permissions(metadata.permissions())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let mine = file.metadata()?;
            if (mine.uid(), mine.gid()) != (metadata.uid(), metadata.gid()) {
                std::os::unix::fs::fchown(&file, Some(metadata.uid()), Some(metadata.gid()))?;
            }
        }
        file.sync_all()?;
        std::fs::rename(&pending, path)
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&pending);
        return Err(cannot(error));
    }
    Ok(())
}

/// One audit row for the change, written as the owner of the data like any other.
fn audit_change(config: &Config, user: &str, live: bool) -> Result<(), String> {
    let data = config.data_path();
    privileges::become_owner_of(&data).map_err(|error| error.message)?;
    let database = config.database_path();
    if !database.is_file() {
        return Err(format!("there is no database at {}", database.display()));
    }
    let key = config.audit.key_file.as_deref().map(AuditKey::load).transpose().map_err(|error| error.to_string())?;
    let store = Store::open(&database).map_err(|error| error.to_string())?;
    let event = audit::Event {
        ts: SystemClock.now_ms(),
        action: MODE_ACTION.to_string(),
        target: "-".to_string(),
        params: json!({ "live": live }),
        credential_ref: "-".to_string(),
        session_ref: "local".to_string(),
        user_ref: "-".to_string(),
        device: user.to_string(),
        channel: "cli".to_string(),
        step_up: "local".to_string(),
        result: "done".to_string(),
        rtt_ms: None,
        run_id: None,
    };
    store.with(|conn| audit::append(conn, &event, key.as_ref())).map(|_| ()).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_live_line_changes() {
        let text = "# comment\nconfig_version = 2\n\n# Dry-run is the default.\nlive = false\n\nlisten = \"127.0.0.1:8790\"\n[webauthn]\nlive_note = 1\n";
        let changed = with_live(text, true);
        assert_eq!(changed, text.replace("live = false\n", "live = true\n"));
        assert_eq!(with_live(&changed, false), text);
    }

    #[test]
    fn a_comment_and_odd_spacing_are_kept() {
        assert_eq!(with_live("live=false # mine\r\nx = 1\r\n", true), "live = true # mine\r\nx = 1\r\n");
        assert_eq!(with_live("  live   =  true\n", false), "  live = false\n");
        // Not the key, and not a comment.
        assert_eq!(with_live("lively = 1\n# live = false\n", true), "lively = 1\n# live = false\nlive = true\n");
    }

    #[test]
    fn a_file_without_the_line_gets_it_before_its_first_table() {
        assert_eq!(
            with_live("config_version = 2\n[webauthn]\nlive = false\n", true),
            "config_version = 2\nlive = true\n\n[webauthn]\nlive = false\n"
        );
        assert_eq!(with_live("config_version = 2", true), "config_version = 2\nlive = true\n");
    }
}
