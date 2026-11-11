//! `cargo xtask count-tests`: runs the workspace's tests and adds up every
//! `test result:` line, so that a package can show its count is not lower than
//! the wave's base.
//!
//! ```text
//! cargo xtask count-tests                         run `cargo test --workspace` and count
//! cargo xtask count-tests --min 910               also fail when fewer than 910 pass
//! cargo xtask count-tests --min 910 -- --locked   pass more arguments to `cargo test`
//! cargo xtask count-tests --log test.log          count a saved test log instead
//! ```
//!
//! It fails when `cargo test` fails, when any test fails, when no `test result:`
//! line is found, or when fewer tests pass than `--min`.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, ExitCode, Stdio};

/// One line for `cargo xtask help`.
pub const ABOUT: &str =
    "Runs the workspace's tests and adds up passed, failed and ignored: [--min N] [--log FILE] [-- ARGS]";

/// The sums of every `test result:` line.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Totals {
    /// Test binaries and doc-test runs that reported.
    pub binaries: u64,
    /// Tests that passed.
    pub passed: u64,
    /// Tests that failed.
    pub failed: u64,
    /// Tests that were ignored.
    pub ignored: u64,
    /// Tests left out by a filter.
    pub filtered_out: u64,
}

impl Totals {
    /// Adds one line, if it is a `test result:` line.
    pub fn add_line(&mut self, line: &str) {
        let Some(rest) = line.trim().strip_prefix("test result: ") else { return };
        self.binaries += 1;
        for part in rest.split(';') {
            // "ok. 6 passed", " 0 failed", " 3 filtered out": a number, then a label.
            let words: Vec<&str> = part.split_whitespace().collect();
            let (label, number) = match words.as_slice() {
                [.., number, "filtered", "out"] => ("filtered out", *number),
                [.., number, label] => (*label, *number),
                _ => continue,
            };
            let Ok(number) = number.parse::<u64>() else { continue };
            match label {
                "passed" => self.passed += number,
                "failed" => self.failed += number,
                "ignored" => self.ignored += number,
                "filtered out" => self.filtered_out += number,
                _ => {}
            }
        }
    }

    /// The sums of every line of a log.
    pub fn of(text: &str) -> Self {
        let mut totals = Self::default();
        for line in text.lines() {
            totals.add_line(line);
        }
        totals
    }
}

/// What the arguments ask for.
#[derive(Debug, Default, PartialEq, Eq)]
struct Options {
    min: Option<u64>,
    log: Option<String>,
    cargo_args: Vec<String>,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--min" => {
                let value = rest.next().ok_or("--min needs a number")?;
                options.min = Some(value.parse().map_err(|_| format!("--min needs a number, not `{value}`"))?);
            }
            "--log" => options.log = Some(rest.next().ok_or("--log needs a file")?.clone()),
            "--" => {
                options.cargo_args = rest.cloned().collect();
                break;
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(options)
}

/// Runs `cargo test --workspace`, echoing its output, and adds up what it reports.
/// Gives back the totals and whether cargo itself succeeded.
fn run_tests(cargo_args: &[String]) -> Result<(Totals, bool), String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut child = Command::new(cargo)
        .args(["test", "--workspace"])
        .args(cargo_args)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not start cargo: {error}"))?;
    let mut totals = Totals::default();
    let stdout = child.stdout.take().ok_or("cargo's output was not captured")?;
    let mut echo = std::io::stdout().lock();
    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|error| format!("could not read cargo's output: {error}"))?;
        let _ = writeln!(echo, "{line}");
        totals.add_line(&line);
    }
    let status = child.wait().map_err(|error| format!("cargo did not finish: {error}"))?;
    Ok((totals, status.success()))
}

/// Runs the command.
pub fn run(args: &[String]) -> ExitCode {
    let options = match parse(args) {
        Ok(options) => options,
        Err(problem) => {
            eprintln!("cargo xtask count-tests: {problem}\n{ABOUT}");
            return ExitCode::from(2);
        }
    };
    let (totals, cargo_ok) = match &options.log {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(text) => (Totals::of(&text), true),
            Err(error) => {
                eprintln!("cargo xtask count-tests: could not read {path}: {error}");
                return ExitCode::from(2);
            }
        },
        None => match run_tests(&options.cargo_args) {
            Ok(result) => result,
            Err(problem) => {
                eprintln!("cargo xtask count-tests: {problem}");
                return ExitCode::from(2);
            }
        },
    };
    let verdict = judge(&totals, cargo_ok, options.min);
    println!(
        "\ntests: {} passed, {} failed, {} ignored, {} filtered out, from {} test result lines",
        totals.passed, totals.failed, totals.ignored, totals.filtered_out, totals.binaries
    );
    match verdict {
        Ok(()) => ExitCode::SUCCESS,
        Err(problem) => {
            eprintln!("cargo xtask count-tests: {problem}");
            ExitCode::FAILURE
        }
    }
}

/// Whether the run is good enough.
fn judge(totals: &Totals, cargo_ok: bool, min: Option<u64>) -> Result<(), String> {
    if totals.binaries == 0 {
        return Err("no `test result:` line was found".to_string());
    }
    if totals.failed > 0 {
        return Err(format!("{} test(s) failed", totals.failed));
    }
    if !cargo_ok {
        return Err("cargo test failed".to_string());
    }
    match min {
        Some(min) if totals.passed < min => Err(format!("{} passed, fewer than the base of {min}", totals.passed)),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "\
running 6 tests
test a ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.14s

     Running tests/web.rs
test result: FAILED. 10 passed; 2 failed; 1 ignored; 0 measured; 3 filtered out; finished in 1.00s
   Doc-tests swifty_plugin_api
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result is not a line it counts
";

    #[test]
    fn every_result_line_is_added_up() {
        let totals = Totals::of(LOG);
        assert_eq!(totals, Totals { binaries: 3, passed: 16, failed: 2, ignored: 1, filtered_out: 3 });
    }

    #[test]
    fn a_run_is_judged_on_failures_cargo_and_the_base() {
        let good = Totals { binaries: 2, passed: 910, failed: 0, ignored: 1, filtered_out: 0 };
        assert_eq!(judge(&good, true, Some(910)), Ok(()));
        assert_eq!(judge(&good, true, None), Ok(()));
        assert!(judge(&good, true, Some(911)).unwrap_err().contains("fewer than the base of 911"));
        assert!(judge(&good, false, None).unwrap_err().contains("cargo test failed"));
        assert!(judge(&Totals { failed: 1, ..good }, true, None).unwrap_err().contains("1 test(s) failed"));
        assert!(judge(&Totals::default(), true, None).unwrap_err().contains("no `test result:` line"));
    }

    #[test]
    fn the_arguments_are_read() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(parse(&args(&[])), Ok(Options::default()));
        assert_eq!(
            parse(&args(&["--min", "910", "--", "--locked", "-p", "swifty"])),
            Ok(Options { min: Some(910), log: None, cargo_args: args(&["--locked", "-p", "swifty"]) })
        );
        assert_eq!(parse(&args(&["--log", "t.log"])).unwrap().log.as_deref(), Some("t.log"));
        assert!(parse(&args(&["--min", "many"])).is_err());
        assert!(parse(&args(&["--min"])).is_err());
        assert!(parse(&args(&["--fast"])).is_err());
    }
}
