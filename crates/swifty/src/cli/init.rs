//! `swifty init`: the first run.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args;
use swifty_hub::admin::init::{
    self, InitOptions, StartingInstance, DEFAULT_CONFIG_PATH, DEFAULT_DATA_DIR, DEFAULT_ORIGIN,
};
use swifty_hub::clock::{Clock, SystemClock};
use swifty_hub::passkeys::ENROLL_TOKEN_TTL_MS;

use super::CliError;
use crate::privileges;

/// The demo home: the mock plugin's eight pretend devices, as one integration.
/// `None` in a build without the mock plugin.
#[cfg(feature = "mock")]
pub fn demo_home() -> Option<Vec<StartingInstance>> {
    use std::collections::BTreeSet;

    use swifty_plugin_api::{InstanceView, Plugin};
    use swifty_plugin_mock::PLUGIN;

    let plugin: &dyn Plugin = &PLUGIN;
    let manifest = plugin.manifest();
    let home = swifty_plugin_mock::demo_home();
    let mut instances = Vec::new();
    for instance in home.instances {
        plugin.validate(&instance.config).ok()?;
        let none = BTreeSet::new();
        let view = InstanceView { id: &instance.id, config: &instance.config, secrets_present: &none, adopted: &[] };
        let devices = plugin.devices(&view);
        instances.push(StartingInstance {
            id: instance.id,
            plugin: instance.plugin,
            version: manifest.version.to_string(),
            config: instance.config,
            config_version: instance.config_version,
            devices,
        });
    }
    Some(instances)
}

#[cfg(not(feature = "mock"))]
pub fn demo_home() -> Option<Vec<StartingInstance>> {
    None
}

/// The link that opens the app on its enrollment form with the token filled in.
/// The token is in the fragment, which a browser never sends to a server or a proxy.
pub fn enroll_link(origin: &str, token: &str) -> String {
    format!("{}/#enroll={token}", origin.trim_end_matches('/'))
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// The address people will use to open the app, such as https://home.example.org.
    /// Default: http://localhost:8790, which works on this machine only.
    #[arg(long, value_name = "URL")]
    pub origin: Option<String>,
    /// Where the hub listens. Default: 127.0.0.1 and the port of the address above.
    #[arg(long, value_name = "ADDR")]
    pub listen: Option<String>,
    /// The directory the hub writes into.
    #[arg(long, value_name = "DIR", default_value = DEFAULT_DATA_DIR)]
    pub data_dir: PathBuf,
    /// Start in live mode, where commands reach real devices. Default: dry-run.
    #[arg(long)]
    pub live: bool,
    /// Start with the demo home: pretend devices to try the app with.
    #[arg(long)]
    pub demo: bool,
    /// Ask nothing, and take the default for anything not given.
    #[arg(long)]
    pub yes: bool,
}

/// Asks one question on the terminal. An empty answer, or no answer at all, is `None`.
fn ask(question: &str) -> Option<String> {
    print!("{question} ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim().to_string()).filter(|answer| !answer.is_empty()),
    }
}

fn ask_origin() -> Result<String, CliError> {
    println!("Which address will people use to open Swifty Home?");
    println!("  - On this machine only: {DEFAULT_ORIGIN}");
    println!("  - From phones and other machines: a name you own, with https, such as https://home.example.org");
    println!("Passkeys are bound to this address. Changing it later means adding every passkey again.");
    for _ in 0..3 {
        let typed = ask(&format!("Address [{DEFAULT_ORIGIN}]:")).unwrap_or_default();
        match init::normalize_origin(&typed) {
            Ok(_) => return Ok(typed),
            Err(problem) => println!("{problem}."),
        }
    }
    Err(CliError::new("no usable address was given.").hint("Run it again with --origin URL."))
}

fn confirm(question: &str) -> bool {
    match ask(&format!("{question} [Y/n]")) {
        None => true,
        Some(answer) => matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes"),
    }
}

fn row(label: &str, value: impl std::fmt::Display) {
    println!("  {label:<15} {value}");
}

pub fn run(config_path: &Path, args: InitArgs) -> Result<ExitCode, CliError> {
    if config_path.exists() {
        return Err(CliError::new(format!("{} already exists.", config_path.display())).hint(
            "`swifty init` never replaces a configuration. Check the one you have with `swifty config check`,\n\
             or move it away to start again.",
        ));
    }
    let interactive = !args.yes && std::io::stdin().is_terminal();
    let typed_origin = match args.origin {
        Some(origin) => origin,
        None if interactive => ask_origin()?,
        None => String::new(),
    };
    let actor = privileges::user_name();
    // The service does not start where this runs, so every path it is given is absolute.
    let absolute = |path: &Path| {
        std::path::absolute(path).map_err(|e| CliError::new(format!("cannot use the path {}: {e}", path.display())))
    };
    let options = InitOptions {
        config_path: absolute(config_path)?,
        origin: typed_origin,
        listen: args.listen,
        data_dir: absolute(&args.data_dir)?,
        live: args.live,
        actor,
    };
    let plan = init::plan(&options).map_err(|problem| CliError::new(format!("{problem}.")))?;

    println!();
    println!("Swifty Home: first run");
    println!();
    row("Address", &plan.origin);
    row("Listens on", &plan.listen);
    row("Configuration", plan.config_path.display());
    row("Data", plan.data_dir.display());
    if plan.live {
        row("Mode", "live: commands reach real devices");
    } else {
        row("Mode", "dry-run: no device is reached until you run `sudo swifty go-live`");
    }
    let home = if args.demo { demo_home() } else { None };
    match (&home, args.demo) {
        (Some(home), true) => {
            let devices: usize = home.iter().map(|instance| instance.devices.len()).sum();
            row("Home", format!("the demo home: {devices} pretend devices, which reach nothing"));
        }
        (None, true) => row("Home", "empty: this build has no demo home (it needs the mock plugin)"),
        _ => row("Home", "empty: add devices with `sudo swifty plugin add <plugin>`"),
    }
    for warning in &plan.warnings {
        println!();
        println!("note: {warning}");
    }
    println!();
    if interactive && !confirm("Write these files?") {
        println!("Nothing was written.");
        return Ok(ExitCode::FAILURE);
    }

    // Run as root, it works as the owner of the data directory from here on, or
    // stops and says how to create that directory for the service.
    privileges::become_owner_of(&plan.data_dir)?;
    let starting = home.unwrap_or_default();
    let outcome =
        init::run_with(&plan, SystemClock.now_ms(), &starting).map_err(|error| CliError::new(error.to_string()))?;

    println!("Done.");
    for path in &outcome.created {
        println!("  created {}", path.display());
    }
    for path in &outcome.kept {
        println!("  kept    {}", path.display());
    }
    for instance in &starting {
        println!("  added   the integration {} with {} devices", instance.id, instance.devices.len());
    }
    let installed = config_path == Path::new(DEFAULT_CONFIG_PATH);
    let config_flag = if installed { String::new() } else { format!(" --config {}", config_path.display()) };
    // The service runs the installed configuration; any other file is run by hand.
    let start = if installed {
        "sudo systemctl start swifty-home   (or: swifty serve)".to_string()
    } else {
        format!("swifty serve{config_flag}")
    };
    println!();
    println!("Next:");
    println!("  1. Start the hub:   {start}");
    println!(
        "  2. Open this link and add your first passkey. It works once, for {} minutes:",
        ENROLL_TOKEN_TTL_MS / 60_000
    );
    println!();
    println!("       {}", enroll_link(&plan.origin, &outcome.enroll_token));
    println!();
    println!("     Or open {} and paste this one-time token:", plan.origin);
    println!();
    println!("       {}", outcome.enroll_token);
    println!();
    println!("     `sudo swifty enroll-token{config_flag}` makes a new one.");
    println!("  3. Check everything: swifty doctor{config_flag}");
    if starting.is_empty() {
        println!(
            "  4. Add devices: sudo swifty plugin add <plugin>{config_flag}   (or in the app, under Integrations)"
        );
    }
    Ok(ExitCode::SUCCESS)
}
