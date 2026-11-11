//! `swifty config`: check, show, export and import.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args, Subcommand};
use swifty_hub::admin::init::load_config;
use swifty_hub::admin::site::{self, ImportOptions, ImportReport, SiteFile};
use swifty_hub::audit::AuditKey;
use swifty_hub::clock::{Clock, SystemClock};
use swifty_hub::runtime::Registry;
use swifty_hub::secrets::FileSecretStore;
use swifty_hub::store::{Store, SCHEMA_VERSION};

use super::{config_error, load_hub_config, CliError};
use crate::privileges;

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Check a configuration file. A glimdock-home file is named as one.
    Check(CheckArgs),
    /// Show the configuration in effect. Paths are shown, never a secret.
    Show,
    /// Write the site file: instances, device names, notes and scenes, never a secret.
    Export(ExportArgs),
    /// Apply a site file.
    Import(ImportArgs),
}

#[derive(Debug, Args)]
pub struct CheckArgs {
    /// The file to check. Default: the one --config names.
    #[arg(long, value_name = "FILE")]
    pub file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ExportArgs {
    /// Where to write it. Default: standard output.
    #[arg(long, value_name = "FILE")]
    pub out: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    /// The site file.
    pub file: PathBuf,
    /// Also remove the integrations, device names and notes the file does not list.
    #[arg(long)]
    pub prune: bool,
}

pub fn run(config_path: &Path, command: ConfigCommand) -> Result<ExitCode, CliError> {
    match command {
        ConfigCommand::Check(args) => check(args.file.as_deref().unwrap_or(config_path)),
        ConfigCommand::Show => show(config_path),
        ConfigCommand::Export(args) => export(config_path, args),
        ConfigCommand::Import(args) => import(config_path, args),
    }
}

/// Parses and checks the file, and says in a few lines what it sets up.
fn check(path: &Path) -> Result<ExitCode, CliError> {
    let file = load_config(path).map_err(|error| config_error(path, error))?;
    println!("{} is valid.", path.display());
    let mode = if file.live { "live: commands reach real devices" } else { "dry-run: no device is reached" };
    for (label, value) in [
        ("mode", mode.to_string()),
        ("listen", file.listen.clone()),
        ("origin", file.webauthn.origin.clone()),
        ("database", file.database_path().display().to_string()),
        ("secrets", file.secrets_path().display().to_string()),
    ] {
        println!("  {label:<9} {value}");
    }
    Ok(ExitCode::SUCCESS)
}

/// Whether a file is there, and its mode where there are modes. Reads no content.
fn file_state(path: &Path) -> String {
    match std::fs::metadata(path) {
        Err(_) => "missing".to_string(),
        Ok(metadata) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                format!("present, mode {:04o}", metadata.permissions().mode() & 0o7777)
            }
            #[cfg(not(unix))]
            {
                let _ = metadata;
                "present".to_string()
            }
        }
    }
}

/// The configuration in effect, every default written out. `swifty.toml` holds
/// paths and settings, never a secret, and no key file is opened.
fn show(path: &Path) -> Result<ExitCode, CliError> {
    let config = load_config(path).map_err(|error| config_error(path, error))?;
    let text = config.to_toml().map_err(|error| CliError::new(error.to_string()))?;
    println!("# The configuration in effect, read from {}, with every default written out.", path.display());
    print!("{text}");
    println!();
    println!("# Where things are. Files are only looked at, never read.");
    let mut places = vec![("database", config.database_path(), true), ("secrets", config.secrets_path(), false)];
    if let Some(key) = &config.audit.key_file {
        places.push(("audit key", key.clone(), true));
    }
    if let Some(speech) = &config.speech {
        places.push(("speech token", speech.token_file.clone(), true));
    }
    if let Some(assistant) = &config.assistant {
        places.push(("assistant key", assistant.key_file.clone(), true));
    }
    for (label, place, is_file) in places {
        let state = if is_file {
            file_state(&place)
        } else if place.is_dir() {
            "present".to_string()
        } else {
            "missing".to_string()
        };
        println!("#   {label:<14} {} ({state})", place.display());
    }
    Ok(ExitCode::SUCCESS)
}

/// The output file of an export, opened before the command gives up root: it may be
/// somewhere only the caller can write.
fn open_output(path: &Path) -> Result<std::fs::File, CliError> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|error| CliError::new(format!("cannot write {}: {error}", path.display())))
}

/// Opens the hub's database to read it, as the service would see it.
fn open_database(path: &Path) -> Result<Store, CliError> {
    if !path.exists() {
        return Err(CliError::new(format!("there is no database at {}.", path.display()))
            .hint("Check data_dir in the configuration, or run `sudo swifty init` for a new hub."));
    }
    let store = Store::open_for_inspection(path)?;
    let version: i64 = store
        .with(|conn| conn.query_row("PRAGMA user_version", [], |row| row.get(0)))
        .map_err(|error| CliError::new(format!("cannot read {}: {error}", path.display())))?;
    if version != SCHEMA_VERSION {
        return Err(CliError::new(format!("{} is at schema {version}, not {SCHEMA_VERSION}.", path.display()))
            .hint("Start the hub once (swifty serve) so it brings the database up to date."));
    }
    Ok(store)
}

/// Writes the site file: every integration with its config, the names and notes
/// people gave devices, and the scenes. Never a credential.
fn export(config_path: &Path, args: ExportArgs) -> Result<ExitCode, CliError> {
    let config = load_hub_config(config_path)?;
    let mut out = args.out.as_deref().map(open_output).transpose()?;
    privileges::become_owner_of(&config.data_dir)?;
    let store = open_database(&config.hub.database_path())?;
    let site = store.with(site::export).map_err(|error| CliError::new(format!("cannot read the database: {error}")))?;
    let text = site.to_toml().map_err(|error| CliError::new(error.0))?;
    match (&mut out, &args.out) {
        (Some(file), Some(path)) => {
            file.write_all(text.as_bytes())
                .and_then(|()| file.sync_all())
                .map_err(|error| CliError::new(format!("cannot write {}: {error}", path.display())))?;
            eprintln!(
                "Wrote {}: {} integrations, {} devices with a name or notes, {} scenes. It holds no credential.",
                path.display(),
                site.instances.len(),
                site.devices.len(),
                site.scenes.len()
            );
        }
        _ => print!("{text}"),
    }
    Ok(ExitCode::SUCCESS)
}

fn list(ids: &[String]) -> String {
    if ids.is_empty() {
        "none".to_string()
    } else {
        ids.join(", ")
    }
}

fn print_import(path: &Path, database: &Path, report: &ImportReport) {
    println!("Imported {} into {}.", path.display(), database.display());
    println!("  integrations added:     {}", list(&report.added));
    println!("  integrations updated:   {}", list(&report.updated));
    println!("  integrations unchanged: {}", list(&report.unchanged));
    if !report.removed.is_empty() {
        println!(
            "  integrations removed:   {} (with their devices, room places, favourites and secrets)",
            list(&report.removed)
        );
    }
    println!("  new devices:            {}", list(&report.devices_added));
    println!("  names set:              {}", list(&report.arranged.names));
    println!("  notes set:              {}", list(&report.arranged.notes));
    println!("  scenes changed:         {}", list(&report.arranged.scenes));
    if !report.pruned_names.is_empty() || !report.pruned_notes.is_empty() {
        println!("  names removed:          {}", list(&report.pruned_names));
        println!("  notes removed:          {}", list(&report.pruned_notes));
    }
    if !report.arranged.unknown_targets.is_empty() {
        println!(
            "  no device has these targets yet; their names and notes are kept for when one appears: {}",
            report.arranged.unknown_targets.join(", ")
        );
    }
    for difference in &report.hub_differs {
        println!("  note: {difference}. Import does not change swifty.toml: edit it, then restart the service.");
    }
    println!();
    println!("A running hub picks this up within a few seconds. Integrations that need a credential wait for it:");
    println!("  swifty plugin list    shows what each one still needs");
    println!("  swifty plugin setup <instance>    or    swifty plugin secret set <instance> <slot>");
}

/// Applies a site file to this hub's database, in one transaction with one audit row.
fn import(config_path: &Path, args: ImportArgs) -> Result<ExitCode, CliError> {
    let config = load_hub_config(config_path)?;
    // Read before giving up root: the file may be somewhere only the caller can read.
    let site = SiteFile::load(&args.file).map_err(|error| CliError::new(error.0))?;
    let actor = privileges::user_name();
    privileges::become_owner_of(&config.data_dir)?;
    let database = config.hub.database_path();
    if !database.exists() {
        return Err(CliError::new(format!("there is no database at {}.", database.display()))
            .hint("Run `sudo swifty init` first, then import."));
    }
    let store = Store::open(&database).map_err(|error| CliError::new(format!("cannot open the database: {error}")))?;
    let key = config.hub.audit.key_file.as_deref().map(AuditKey::load).transpose().map_err(|e| e.to_string())?;
    let secrets = Arc::new(FileSecretStore::new(config.hub.secrets_path()));
    let registry = Registry::new(false, crate::plugins::compiled_in(), secrets, (&config.hub.network).into());
    let options = ImportOptions {
        prune: args.prune,
        actor: &actor,
        now: SystemClock.now_ms(),
        audit_key: key.as_ref(),
        config: Some(&config.hub),
    };
    let report = site::import(&store, &registry, &site, &options).map_err(|error| CliError::new(error.0))?;
    print_import(&args.file, &database, &report);
    Ok(ExitCode::SUCCESS)
}
