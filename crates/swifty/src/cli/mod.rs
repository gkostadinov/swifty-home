//! The command tree, and what runs each command.
//!
//! Commands that need the plugin runtime parse their arguments, so `--help`
//! already describes them, and then say they are not in this build yet.

pub mod audit;
pub mod backup;
pub mod config;
pub mod device;
pub mod doctor;
pub mod init;
pub mod keys;
pub mod migrate;
pub mod mode;
pub mod plugin;
pub mod terminal;
pub mod version;
pub mod wizard;

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use swifty_hub::admin::init::{load_config, ConfigFileError, DEFAULT_CONFIG_PATH};
use swifty_hub::config::Config;

const FIRST_RUN: &str = "\
First time here? Run `sudo swifty init`. It writes the configuration, creates the
database and prints how to add the first passkey. `swifty doctor` checks everything.

Every command takes --config FILE. Without it, the file is /etc/swifty-home/swifty.toml.";

/// Swifty Home: a self-hosted smart home hub.
#[derive(Debug, Parser)]
#[command(
    name = "swifty",
    version,
    about = "Swifty Home: a self-hosted smart home hub.",
    after_help = FIRST_RUN,
    arg_required_else_help = true,
    disable_help_subcommand = true
)]
pub struct Cli {
    /// The configuration file.
    #[arg(long, value_name = "FILE", global = true, default_value = DEFAULT_CONFIG_PATH)]
    pub config: PathBuf,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Set up a new hub: the configuration, the directories and the database.
    Init(init::InitArgs),
    /// Run the hub.
    Serve,
    /// Make a one-time token for the first passkey.
    EnrollToken,
    /// Check the configuration, the files, the database and the audit log.
    Doctor(doctor::DoctorArgs),
    /// Check, show, export or import the configuration.
    #[command(subcommand)]
    Config(config::ConfigCommand),
    /// Add, set up, list and remove integrations, or start a new plugin.
    #[command(subcommand)]
    Plugin(plugin::PluginCommand),
    /// List the devices.
    #[command(subcommand)]
    Device(device::DeviceCommand),
    /// Let commands reach real devices (live mode).
    GoLive,
    /// Stop commands from reaching real devices (dry-run).
    GoDry,
    /// Check the audit log.
    #[command(subcommand)]
    Audit(audit::AuditCommand),
    /// The old name of `swifty audit verify`.
    #[command(hide = true)]
    AuditVerify(audit::VerifyArgs),
    /// Copy the database, the configuration and the secrets somewhere safe.
    Backup(backup::BackupArgs),
    /// Move a glimdock-home install to Swifty Home.
    Migrate(migrate::MigrateArgs),
    /// Set the token for the speech service.
    #[command(subcommand)]
    Speech(keys::SpeechCommand),
    /// Set the key for the assistant's model.
    #[command(subcommand)]
    Assistant(keys::AssistantCommand),
    /// Print the OpenAPI document of the hub's API.
    Openapi,
    /// Print the version and what is in this build.
    Version(version::VersionArgs),
}

/// An error for a person: what went wrong, and what to do about it.
#[derive(Debug)]
pub struct CliError {
    pub message: String,
    pub hint: Option<String>,
}

impl CliError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into(), hint: None }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error: {}", self.message)?;
        if let Some(hint) = &self.hint {
            for line in hint.lines() {
                write!(f, "\n  {line}")?;
            }
        }
        Ok(())
    }
}

impl From<String> for CliError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

/// What the hub runs on.
pub struct HubConfig {
    /// `swifty.toml`, version 2.
    pub hub: Config,
    /// The directory the hub writes into, whose owner the CLI works as.
    pub data_dir: PathBuf,
}

/// Reads the configuration the hub runs on: version 2 only. A glimdock-home file
/// is refused with the command that converts it.
pub fn load_hub_config(path: &Path) -> Result<HubConfig, CliError> {
    let hub = load_config(path).map_err(|error| config_error(path, error))?;
    let data_dir = hub.data_path();
    Ok(HubConfig { hub, data_dir })
}

/// The error for a configuration that cannot be read at all.
pub fn missing_config(path: &Path, error: &std::io::Error) -> CliError {
    let shown = path.display();
    match error.kind() {
        std::io::ErrorKind::NotFound => CliError::new(format!("there is no configuration at {shown}."))
            .hint("To set up a new hub, run: sudo swifty init\nTo use another file, add: --config FILE"),
        std::io::ErrorKind::PermissionDenied => {
            CliError::new(format!("cannot read {shown}: permission denied.")).hint("Run it with sudo.")
        }
        _ => CliError::new(format!("cannot read {shown}: {error}")),
    }
}

/// The error for a version 2 file that does not check.
pub fn config_error(path: &Path, error: ConfigFileError) -> CliError {
    match error {
        ConfigFileError::Read { source, .. } => missing_config(path, &source),
        ConfigFileError::Legacy => CliError::new(format!("This is a glimdock-home configuration: {}", path.display()))
            .hint(format!("Run: swifty migrate --from {}", path.display())),
        ConfigFileError::Invalid(message) => CliError::new(format!("invalid configuration: {message}"))
            .hint(format!("Fix it in {}, then run: swifty config check", path.display())),
    }
}

/// Runs every command but `serve`, which the binary runs itself.
pub fn run(cli: Cli) -> Result<ExitCode, CliError> {
    let config = cli.config;
    match cli.command {
        Command::Init(args) => init::run(&config, args),
        Command::Serve => Err(CliError::new("serve is run by the binary")),
        Command::EnrollToken => audit::enroll_token(&config),
        Command::Doctor(args) => doctor::run(&config, args),
        Command::Config(command) => config::run(&config, command),
        Command::Plugin(command) => plugin::run(&config, command),
        Command::Device(command) => device::run(&config, command),
        Command::GoLive => mode::go_live(&config),
        Command::GoDry => mode::go_dry(&config),
        Command::Audit(audit::AuditCommand::Verify(args)) | Command::AuditVerify(args) => audit::verify(&config, args),
        Command::Backup(args) => backup::run(&config, args),
        Command::Migrate(_) => Err(CliError::new("migrate is run by the binary")),
        Command::Speech(command) => keys::speech(&config, command),
        Command::Assistant(command) => keys::assistant(&config, command),
        Command::Openapi => {
            // The document needs no configuration, database or plugin: it is made from the router.
            print!("{}", swifty_hub::http::openapi::document_json());
            Ok(ExitCode::SUCCESS)
        }
        Command::Version(args) => version::run(args),
    }
}
