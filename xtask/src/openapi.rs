//! `cargo xtask openapi`: writes `docs/api/openapi.v1.json` from the router, and
//! with `--check` fails when the committed file differs from it.
//!
//! The document is what `swifty openapi` prints: the hub makes it from its own
//! router, so this runs that command rather than linking the hub.

use std::path::Path;
use std::process::{Command, ExitCode};

use crate::gen_docs::workspace_root;

/// One line for `cargo xtask help`.
pub const ABOUT: &str = "Writes docs/api/openapi.v1.json from the router; --check fails when it differs";

/// Where the document is committed, from the workspace root.
pub const DOCUMENT: &str = "docs/api/openapi.v1.json";

/// Runs the command.
pub fn run(args: &[String]) -> ExitCode {
    let check = match args {
        [] => false,
        [flag] if flag == "--check" => true,
        _ => {
            eprintln!("Usage: cargo xtask openapi [--check]");
            return ExitCode::from(2);
        }
    };
    let root = workspace_root();
    let made = match document(&root) {
        Ok(made) => made,
        Err(error) => {
            eprintln!("cargo xtask openapi: {error}");
            return ExitCode::FAILURE;
        }
    };
    let path = root.join(DOCUMENT);
    if check {
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        return match first_difference(&made, &committed) {
            None => {
                println!("{DOCUMENT} is the document the router makes.");
                ExitCode::SUCCESS
            }
            Some((line, made, committed)) => {
                eprintln!("{DOCUMENT} differs from the document the router makes, from line {line}:");
                eprintln!("  committed: {committed}");
                eprintln!("  router:    {made}");
                eprintln!("Run `cargo xtask openapi` and commit {DOCUMENT}.");
                ExitCode::FAILURE
            }
        };
    }
    if let Some(dir) = path.parent() {
        if let Err(error) = std::fs::create_dir_all(dir) {
            eprintln!("cargo xtask openapi: cannot create {}: {error}", dir.display());
            return ExitCode::FAILURE;
        }
    }
    if std::fs::read_to_string(&path).is_ok_and(|committed| committed == made) {
        println!("{DOCUMENT} is up to date.");
        return ExitCode::SUCCESS;
    }
    match std::fs::write(&path, made) {
        Ok(()) => {
            println!("Wrote {DOCUMENT}.");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("cargo xtask openapi: cannot write {}: {error}", path.display());
            ExitCode::FAILURE
        }
    }
}

/// What `swifty openapi` prints, built from this checkout.
fn document(root: &Path) -> Result<String, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(cargo)
        .args(["run", "--quiet", "--locked", "-p", "swifty", "--", "openapi"])
        .current_dir(root)
        .output()
        .map_err(|error| format!("cannot run cargo: {error}"))?;
    if !output.status.success() {
        return Err(format!("`swifty openapi` failed:\n{}", String::from_utf8_lossy(&output.stderr)));
    }
    String::from_utf8(output.stdout).map_err(|_| "`swifty openapi` printed something that is not UTF-8".to_string())
}

/// The first line where two texts differ, by number from 1, with each side's line.
/// A missing line reads as `<end of file>`.
pub fn first_difference(made: &str, committed: &str) -> Option<(usize, String, String)> {
    if made == committed {
        return None;
    }
    let (mut a, mut b) = (made.lines(), committed.lines());
    let mut number = 1;
    loop {
        match (a.next(), b.next()) {
            (Some(x), Some(y)) if x == y => number += 1,
            (None, None) => return Some((number, "<end of file>".into(), "<a line ending differs>".into())),
            (x, y) => {
                let shown = |line: Option<&str>| line.map_or("<end of file>".to_string(), str::to_string);
                return Some((number, shown(x), shown(y)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_difference_is_found_by_line() {
        assert_eq!(first_difference("a\nb\n", "a\nb\n"), None);
        assert_eq!(first_difference("a\nb\n", "a\nc\n"), Some((2, "b".into(), "c".into())));
        assert_eq!(first_difference("a\nb\n", "a\n"), Some((2, "b".into(), "<end of file>".into())));
        assert_eq!(first_difference("a\n", ""), Some((1, "a".into(), "<end of file>".into())));
        assert!(first_difference("a\n", "a\r\n").is_some(), "line endings count");
    }

    #[test]
    fn it_takes_nothing_or_check() {
        assert_eq!(run(&["--fix".to_string()]), ExitCode::from(2));
    }
}
