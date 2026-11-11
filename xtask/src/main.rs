//! `cargo xtask <command>`: the developer commands that need `cargo` or the whole
//! workspace, so the shipped `swifty` binary never runs a process.
//!
//! Each command lives in its own file, so the package that builds a command
//! edits only that file.

use std::process::ExitCode;

mod add_plugin;
mod check_plugin;
mod check_public_tree;
mod check_template;
mod count_tests;
mod dist;
mod gen_docs;
mod openapi;

/// A command: its name, one line about it, and what runs it.
struct Command {
    name: &'static str,
    about: &'static str,
    run: fn(&[String]) -> ExitCode,
}

const COMMANDS: &[Command] = &[
    Command { name: "count-tests", about: count_tests::ABOUT, run: count_tests::run },
    Command { name: "gen-docs", about: gen_docs::ABOUT, run: gen_docs::run },
    Command { name: "openapi", about: openapi::ABOUT, run: openapi::run },
    Command { name: "check-plugin", about: check_plugin::ABOUT, run: check_plugin::run },
    Command { name: "check-template", about: check_template::ABOUT, run: check_template::run },
    Command { name: "add-plugin", about: add_plugin::ABOUT, run: add_plugin::run },
    Command { name: "check-public-tree", about: check_public_tree::ABOUT, run: check_public_tree::run },
    Command { name: "dist", about: dist::ABOUT, run: dist::run },
];

fn usage() -> String {
    let mut text = String::from("Usage: cargo xtask <command> [arguments]\n\nCommands:\n");
    for command in COMMANDS {
        text.push_str(&format!("  {:<18} {}\n", command.name, command.about));
    }
    text
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(name) = args.first() else {
        eprint!("{}", usage());
        return ExitCode::from(2);
    };
    if matches!(name.as_str(), "help" | "-h" | "--help") {
        print!("{}", usage());
        return ExitCode::SUCCESS;
    }
    match COMMANDS.iter().find(|command| command.name == name) {
        Some(command) => (command.run)(&args[1..]),
        None => {
            eprint!("cargo xtask: no command `{name}`.\n\n{}", usage());
            ExitCode::from(2)
        }
    }
}
