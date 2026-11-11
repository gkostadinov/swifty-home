//! `swifty speech set-token` and `swifty assistant set-key`: a hub key file,
//! written from standard input.
//!
//! The file is the one `swifty.toml` names (`[speech] token_file`, `[assistant]
//! key_file`), or `speech.token` / `assistant.key` beside `swifty.toml` when there is
//! no such section yet. It is replaced whole, mode 0600, and run as root it is given
//! to the owner of the data directory, which is who the service runs as. The value is
//! never shown: on a terminal it is typed without echo. One audit row says which key
//! was set (`system.key_set`, channel `cli`), never its value.

use std::io::{self, BufRead as _, IsTerminal, Read as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Subcommand;
use serde_json::json;
use swifty_hub::admin::{files, site};
use swifty_hub::audit::{self, AuditKey};
use swifty_hub::clock::{Clock, SystemClock};
use swifty_hub::store::Store;

use super::{load_hub_config, CliError, HubConfig};
use crate::privileges;

/// The audit action of setting a hub key file.
const ACTION: &str = "system.key_set";

/// The longest value read.
const MAX_VALUE: usize = 4096;

#[derive(Debug, Subcommand)]
pub enum SpeechCommand {
    /// Read the speech service's token from standard input and store it.
    SetToken,
}

#[derive(Debug, Subcommand)]
pub enum AssistantCommand {
    /// Read the model's API key from standard input and store it.
    SetKey,
}

/// Which key file.
#[derive(Clone, Copy)]
enum Key {
    Speech,
    Assistant,
}

impl Key {
    fn what(self) -> &'static str {
        match self {
            Key::Speech => "the speech service's token",
            Key::Assistant => "the model's API key",
        }
    }

    /// How the audit row names it.
    fn name(self) -> &'static str {
        match self {
            Key::Speech => "speech_token",
            Key::Assistant => "assistant_key",
        }
    }

    fn default_name(self) -> &'static str {
        match self {
            Key::Speech => "speech.token",
            Key::Assistant => "assistant.key",
        }
    }

    /// What to add to `swifty.toml` when it has no section for this key yet.
    fn section(self, path: &Path) -> String {
        let path = toml::Value::String(path.display().to_string()).to_string();
        match self {
            Key::Speech => format!("[speech]\nurl = \"http://127.0.0.1:8791\"\ntoken_file = {path}"),
            Key::Assistant => {
                format!("[assistant]\nurls = [\"http://127.0.0.1:8080/v1\"]\nmodel = \"my-model\"\nkey_file = {path}")
            }
        }
    }
}

pub fn speech(config_path: &Path, command: SpeechCommand) -> Result<ExitCode, CliError> {
    match command {
        SpeechCommand::SetToken => set(config_path, Key::Speech),
    }
}

pub fn assistant(config_path: &Path, command: AssistantCommand) -> Result<ExitCode, CliError> {
    match command {
        AssistantCommand::SetKey => set(config_path, Key::Assistant),
    }
}

/// One line from a terminal, typed without echo.
#[cfg(unix)]
fn read_line_hidden() -> io::Result<String> {
    use std::os::fd::AsRawFd;
    let fd = io::stdin().as_raw_fd();
    // SAFETY: an all-zero termios is a valid value for tcgetattr to fill in.
    let mut saved: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: fd is this process's standard input and `saved` is valid for writing.
    let hidden = unsafe { libc::tcgetattr(fd, &mut saved) } == 0;
    if hidden {
        let mut quiet = saved;
        quiet.c_lflag &= !libc::ECHO;
        quiet.c_lflag |= libc::ECHONL;
        // SAFETY: as above; `quiet` is a copy of what tcgetattr gave.
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &quiet) };
    }
    let mut line = String::new();
    let read = io::stdin().lock().take(MAX_VALUE as u64 + 1).read_line(&mut line);
    if hidden {
        // SAFETY: puts back exactly what tcgetattr read.
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &saved) };
    }
    read.map(|_| line)
}

#[cfg(not(unix))]
fn read_line_hidden() -> io::Result<String> {
    let mut line = String::new();
    io::stdin().lock().take(MAX_VALUE as u64 + 1).read_line(&mut line)?;
    Ok(line)
}

/// The value: one line from a terminal, or all of standard input otherwise. Never shown.
fn read_value(key: Key) -> Result<String, CliError> {
    let stdin = io::stdin();
    let text = if stdin.is_terminal() {
        eprint!("Type or paste {} (it is not shown), then press Enter: ", key.what());
        read_line_hidden()
    } else {
        let mut text = String::new();
        stdin.lock().take(MAX_VALUE as u64 + 1).read_to_string(&mut text).map(|_| text)
    }
    .map_err(|error| CliError::new(format!("cannot read standard input: {error}")))?;
    if text.len() > MAX_VALUE {
        return Err(CliError::new(format!("{} is longer than {MAX_VALUE} bytes", key.what())));
    }
    let value = text.trim();
    if value.is_empty() {
        return Err(CliError::new(format!("nothing was read for {}", key.what()))
            .hint("Give it on standard input, for example: swifty speech set-token < token.txt"));
    }
    if value.chars().any(char::is_control) {
        return Err(CliError::new(format!("{} must be one line of plain characters", key.what())));
    }
    Ok(value.to_string())
}

/// Who the service runs as: the owner of the data directory, when this runs as root
/// and that is not root.
#[cfg(unix)]
fn service_owner(data_dir: &Path) -> Option<(u32, u32)> {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no preconditions and cannot fail.
    if unsafe { libc::geteuid() } != 0 {
        return None;
    }
    let metadata = std::fs::metadata(data_dir).ok()?;
    (metadata.uid() != 0).then(|| (metadata.uid(), metadata.gid()))
}

#[cfg(not(unix))]
fn service_owner(_data_dir: &Path) -> Option<(u32, u32)> {
    None
}

/// One audit row for the change, as the owner of the data directory. A hub with no
/// database yet has no log to write it in.
fn audit_row(config: &HubConfig, key: Key, actor: &str) -> Result<(), CliError> {
    let database = config.hub.database_path();
    if !database.exists() {
        eprintln!("There is no database at {} yet, so no audit row was written.", database.display());
        return Ok(());
    }
    privileges::become_owner_of(&config.data_dir)?;
    let failed = |error: String| CliError::new(format!("the key was stored, but the audit row was not: {error}"));
    let store = Store::open(&database).map_err(|error| failed(error.to_string()))?;
    let audit_key =
        config.hub.audit.key_file.as_deref().map(AuditKey::load).transpose().map_err(|e| failed(e.to_string()))?;
    let event = site::cli_event(ACTION, "-", json!({ "key": key.name() }), actor, SystemClock.now_ms());
    store.with(|conn| audit::append(conn, &event, audit_key.as_ref())).map_err(|error| failed(error.to_string()))?;
    Ok(())
}

fn set(config_path: &Path, key: Key) -> Result<ExitCode, CliError> {
    let config = load_hub_config(config_path)?;
    let actor = privileges::user_name();
    let named: Option<PathBuf> = match key {
        Key::Speech => config.hub.speech.as_ref().map(|speech| speech.token_file.clone()),
        Key::Assistant => config.hub.assistant.as_ref().map(|assistant| assistant.key_file.clone()),
    };
    let configured = named.is_some();
    let config_dir = config_path.parent().filter(|dir| !dir.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let path = named.unwrap_or_else(|| config_dir.join(key.default_name()));
    let value = read_value(key)?;
    files::write_atomic(&path, format!("{value}\n").as_bytes(), 0o600, true).map_err(|error| {
        let denied = error.kind() == io::ErrorKind::PermissionDenied;
        let error = CliError::new(format!("cannot write {}: {error}", path.display()));
        if denied {
            error.hint("Run it with sudo.")
        } else {
            error
        }
    })?;
    if let Some((uid, gid)) = service_owner(&config.data_dir) {
        files::set_owner(&path, uid, gid)
            .map_err(|error| CliError::new(format!("cannot give {} to the service: {error}", path.display())))?;
    }
    println!("Stored {} in {} (mode 0600).", key.what(), path.display());
    audit_row(&config, key, &actor)?;
    if configured {
        println!("Restart the hub so it reads it: sudo systemctl restart swifty-home");
    } else {
        println!("swifty.toml has no section for it yet. Add this to {}, then restart the hub:", config_path.display());
        println!();
        println!("{}", key.section(&path));
    }
    Ok(ExitCode::SUCCESS)
}
