//! The terminal a command talks to: lines out, answers in, and values that are
//! read without being shown.
//!
//! [`Terminal`] is what the setup renderer and the other commands use, so a test can
//! hand them a script instead of a keyboard. [`Console`] is the real one: standard
//! input and output. With a person at a terminal it turns the echo off while a
//! secret is typed; with standard input from a file or a pipe it reads one line per
//! answer, which is how a setup is scripted.

use std::io::{BufRead, IsTerminal, Read, Write};
use std::sync::Mutex;

use super::CliError;

/// The longest line an answer may be. A secret store slot holds more, but nobody
/// types that much at a prompt.
const MAX_LINE: usize = 64 * 1024;

/// Where a command's questions go and its answers come from.
pub trait Terminal {
    /// Prints one line.
    fn say(&mut self, line: &str);
    /// Asks and reads one line, without its line ending. `None` once the input has ended.
    fn ask(&mut self, prompt: &str) -> Result<Option<String>, CliError>;
    /// As [`Terminal::ask`], but what is typed is not shown.
    fn ask_hidden(&mut self, prompt: &str) -> Result<Option<String>, CliError>;
    /// Whether a person is at the keyboard.
    fn is_interactive(&self) -> bool;
    /// Shows a line the next call replaces, such as a countdown. Where nothing can be
    /// replaced (output into a file), only the first one is shown.
    fn status(&mut self, line: &str);
    /// Ends the line [`Terminal::status`] was showing.
    fn end_status(&mut self);
}

/// Standard input and output.
pub struct Console {
    /// Standard input is a terminal: a person types, and the terminal shows it.
    keyboard: bool,
    /// Standard output is a terminal: a line can be drawn over.
    screen: bool,
    /// The width of the status line shown last, to blank it out when it shrinks.
    status_width: Option<usize>,
}

impl Default for Console {
    fn default() -> Self {
        Self::new()
    }
}

impl Console {
    pub fn new() -> Self {
        Self { keyboard: std::io::stdin().is_terminal(), screen: std::io::stdout().is_terminal(), status_width: None }
    }

    fn prompt(prompt: &str) {
        let mut out = std::io::stdout().lock();
        let _ = write!(out, "{prompt}");
        let _ = out.flush();
    }

    fn read_line() -> Result<Option<String>, CliError> {
        let mut line = String::new();
        let read = std::io::stdin()
            .lock()
            .take(MAX_LINE as u64 + 1)
            .read_line(&mut line)
            .map_err(|error| CliError::new(format!("cannot read the answer: {error}")))?;
        if read == 0 {
            return Ok(None);
        }
        if line.len() > MAX_LINE {
            return Err(CliError::new("that answer is too long."));
        }
        Ok(Some(without_line_ending(&line).to_string()))
    }
}

impl Terminal for Console {
    fn say(&mut self, line: &str) {
        self.end_status();
        println!("{line}");
    }

    fn ask(&mut self, prompt: &str) -> Result<Option<String>, CliError> {
        self.end_status();
        Self::prompt(prompt);
        let answer = Self::read_line()?;
        if !self.keyboard {
            // Read from a file or a pipe, so nothing showed it: the answer is shown
            // after its question, as it would have been typed.
            println!("{}", answer.as_deref().unwrap_or_default());
        }
        Ok(answer)
    }

    fn ask_hidden(&mut self, prompt: &str) -> Result<Option<String>, CliError> {
        self.end_status();
        Self::prompt(prompt);
        let echo = self.keyboard.then(EchoOff::start);
        let answer = Self::read_line();
        drop(echo);
        // Neither the value nor the Enter key after it was shown.
        println!();
        answer
    }

    fn is_interactive(&self) -> bool {
        self.keyboard
    }

    fn status(&mut self, line: &str) {
        if !self.screen {
            if self.status_width.is_none() {
                println!("{line}");
                self.status_width = Some(0);
            }
            return;
        }
        let width = line.chars().count();
        let blank = self.status_width.unwrap_or(0).saturating_sub(width);
        let mut out = std::io::stdout().lock();
        let _ = write!(out, "\r{line}{}", " ".repeat(blank));
        let _ = out.flush();
        self.status_width = Some(width);
    }

    fn end_status(&mut self) {
        if self.status_width.take().is_some() && self.screen {
            println!();
        }
    }
}

/// A line without its `\n` or `\r\n`.
pub fn without_line_ending(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

// ----- the echo

/// How the terminal was before the echo went off, for whoever has to put it back:
/// the guard, or the Ctrl+C handler when the process stops in the middle.
static SAVED: Mutex<Option<SavedMode>> = Mutex::new(None);

#[cfg(unix)]
type SavedMode = libc::termios;
#[cfg(windows)]
type SavedMode = u32;
#[cfg(not(any(unix, windows)))]
type SavedMode = ();

/// Turns the echo of standard input off until it is dropped.
struct EchoOff;

impl EchoOff {
    fn start() -> Self {
        if let Some(saved) = echo_off() {
            *SAVED.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(saved);
        }
        Self
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        restore();
    }
}

/// Puts the terminal back as it was before a secret was asked for, if the echo is
/// off now. Safe to call at any time.
pub fn restore() {
    if let Some(saved) = SAVED.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take() {
        set_mode(&saved);
    }
}

#[cfg(unix)]
fn echo_off() -> Option<SavedMode> {
    // SAFETY: an all-zero termios is a valid value for tcgetattr to fill in.
    let mut mode: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: standard input is a valid descriptor and `mode` is valid for writing.
    if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut mode) } != 0 {
        return None;
    }
    let saved = mode;
    // Lines and Ctrl+C keep working; only what is typed is not shown.
    mode.c_lflag &= !(libc::ECHO | libc::ECHOE | libc::ECHOK);
    // SAFETY: standard input is a valid descriptor and `mode` came from tcgetattr.
    if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &mode) } != 0 {
        return None;
    }
    Some(saved)
}

#[cfg(unix)]
fn set_mode(saved: &SavedMode) {
    // SAFETY: standard input is a valid descriptor and `saved` came from tcgetattr.
    unsafe {
        libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, saved);
    }
}

#[cfg(windows)]
fn echo_off() -> Option<SavedMode> {
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_ECHO_INPUT, STD_INPUT_HANDLE,
    };
    let mut mode = 0;
    // SAFETY: GetStdHandle takes a plain constant; the handle is checked by
    // GetConsoleMode, which writes one u32 to a valid pointer.
    unsafe {
        let input = GetStdHandle(STD_INPUT_HANDLE);
        if GetConsoleMode(input, &mut mode) == 0 {
            return None;
        }
        if SetConsoleMode(input, mode & !ENABLE_ECHO_INPUT) == 0 {
            return None;
        }
    }
    Some(mode)
}

#[cfg(windows)]
fn set_mode(saved: &SavedMode) {
    use windows_sys::Win32::System::Console::{GetStdHandle, SetConsoleMode, STD_INPUT_HANDLE};
    // SAFETY: as above; the mode is the one GetConsoleMode returned.
    unsafe {
        SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), *saved);
    }
}

#[cfg(not(any(unix, windows)))]
fn echo_off() -> Option<SavedMode> {
    None
}

#[cfg(not(any(unix, windows)))]
fn set_mode(_saved: &SavedMode) {}

// ----- a script, for tests

/// A terminal that answers from a list and writes down everything it was shown.
/// What is typed at a hidden prompt never reaches the transcript.
#[derive(Debug, Default)]
pub struct Script {
    answers: std::collections::VecDeque<String>,
    /// Everything shown, one line per entry, prompts with their answers.
    pub transcript: Vec<String>,
    /// How many answers were read without being shown.
    pub hidden_reads: usize,
    /// Whether it says a person is at the keyboard.
    pub interactive: bool,
}

impl Script {
    pub fn new(answers: &[&str]) -> Self {
        Self { answers: answers.iter().map(|answer| answer.to_string()).collect(), ..Self::default() }
    }

    /// As a person at the keyboard would answer.
    pub fn interactive(answers: &[&str]) -> Self {
        Self { interactive: true, ..Self::new(answers) }
    }

    /// The transcript as one text.
    pub fn text(&self) -> String {
        self.transcript.join("\n")
    }

    /// Answers not used yet.
    pub fn left(&self) -> usize {
        self.answers.len()
    }
}

impl Terminal for Script {
    fn say(&mut self, line: &str) {
        self.transcript.push(line.to_string());
    }

    fn ask(&mut self, prompt: &str) -> Result<Option<String>, CliError> {
        let answer = self.answers.pop_front();
        self.transcript.push(format!("{prompt}{}", answer.as_deref().unwrap_or("")));
        Ok(answer)
    }

    fn ask_hidden(&mut self, prompt: &str) -> Result<Option<String>, CliError> {
        let answer = self.answers.pop_front();
        self.hidden_reads += 1;
        self.transcript.push(prompt.to_string());
        Ok(answer)
    }

    fn is_interactive(&self) -> bool {
        self.interactive
    }

    fn status(&mut self, line: &str) {
        self.transcript.push(line.to_string());
    }

    fn end_status(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_loses_its_ending_and_nothing_else() {
        assert_eq!(without_line_ending("abc\n"), "abc");
        assert_eq!(without_line_ending("abc\r\n"), "abc");
        assert_eq!(without_line_ending(" abc "), " abc ");
        assert_eq!(without_line_ending("abc\n\n"), "abc\n");
    }

    #[test]
    fn a_script_keeps_hidden_answers_out_of_its_transcript() {
        let mut script = Script::new(&["shown", "kept-out-of-sight"]);
        assert_eq!(script.ask("Name: ").unwrap().as_deref(), Some("shown"));
        assert_eq!(script.ask_hidden("Code: ").unwrap().as_deref(), Some("kept-out-of-sight"));
        assert_eq!(script.ask("More: ").unwrap(), None);
        assert!(!script.text().contains("kept-out-of-sight"));
        assert!(script.text().contains("Name: shown"));
        assert_eq!(script.hidden_reads, 1);
    }

    #[test]
    fn restoring_with_nothing_saved_does_nothing() {
        restore();
        restore();
    }
}
