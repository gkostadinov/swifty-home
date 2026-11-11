//! `swifty backup`: an online copy of the database, the configuration and the secrets.
//! The packaged nightly timer runs `swifty backup --out /var/backups/swifty-home --keep-days 14`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args;
use swifty_hub::admin::backup::{self, BackupOptions};
use swifty_hub::clock::{Clock, SystemClock};

use super::{load_hub_config, CliError};
use crate::privileges;

#[derive(Debug, Args)]
pub struct BackupArgs {
    /// The directory to write the backup into. Each backup is a directory of its own in it.
    #[arg(long, value_name = "DIR")]
    pub out: PathBuf,
    /// Remove backups in that directory older than this many days.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=36_500))]
    pub keep_days: Option<u32>,
}

pub fn run(config_path: &Path, args: BackupArgs) -> Result<ExitCode, CliError> {
    let config = load_hub_config(config_path)?;
    // As the service: the backup's files are its own, and opening the database as
    // root could leave a file beside it that the service cannot use.
    privileges::become_owner_of(&config.data_dir)?;
    let options = BackupOptions {
        config: &config.hub,
        config_path,
        out: &args.out,
        keep_days: args.keep_days,
        now_ms: SystemClock.now_ms(),
    };
    let report = backup::run(&options).map_err(|error| CliError::new(error.0))?;
    println!("Backed up to {}", report.dir.display());
    for (file, size) in &report.files {
        println!("  {file:<28} {size} bytes");
    }
    println!("  the secret store: {} slots in {}", report.secrets, backup::SECRETS_ARCHIVE);
    if let Some(checkpoint) = &report.checkpoint {
        println!("  audit checkpoint: {checkpoint}");
    }
    for removed in &report.removed {
        println!("Removed an older backup: {}", removed.display());
    }
    println!("The backup holds every credential of every integration. Keep a copy elsewhere, encrypted.");
    Ok(ExitCode::SUCCESS)
}
