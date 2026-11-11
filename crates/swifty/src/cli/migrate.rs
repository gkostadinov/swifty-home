//! `swifty migrate`: a glimdock-home install to Swifty Home, in one command.
//!
//! The work is the hub's (`swifty_hub::migrate::run`). This decides who the new
//! files belong to and prints the report. The binary itself hands in the check of
//! the old address, because opening a listener is its business alone.

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args;
use swifty_hub::admin::files::Ownership;
use swifty_hub::admin::init::DEFAULT_DATA_DIR;
use swifty_hub::admin::site::SiteFile;
use swifty_hub::clock::{Clock, SystemClock};
use swifty_hub::migrate::{self, Request};

use super::CliError;
use crate::privileges;

/// The user the packaged service runs as.
#[cfg(unix)]
const SERVICE_USER: &str = "swifty-home";

#[derive(Debug, Args)]
pub struct MigrateArgs {
    /// The glimdock-home configuration, such as /etc/glimdock-home/config.toml.
    #[arg(long, value_name = "FILE")]
    pub from: PathBuf,
    /// A site file: device names, notes and scenes to apply, and the keys of each
    /// integration to set over what is converted from the old configuration.
    #[arg(long, value_name = "FILE")]
    pub site: Option<PathBuf>,
    /// The new install's data directory. The new swifty.toml goes where --config says.
    #[arg(long, value_name = "DIR", default_value = DEFAULT_DATA_DIR)]
    pub data_dir: PathBuf,
    /// Check, verify the old audit log, rehearse every step in memory and print the
    /// plan. Nothing is written.
    #[arg(long)]
    pub dry_run: bool,
}

/// Runs the migration. `listen_probe` says whether this process could listen on an
/// address: the old hub must not be answering there any more.
pub fn run(
    config_path: &Path,
    args: MigrateArgs,
    listen_probe: &dyn Fn(&str) -> io::Result<()>,
) -> Result<ExitCode, CliError> {
    let site = match &args.site {
        Some(path) => Some((path.clone(), SiteFile::load(path).map_err(|error| CliError::new(error.0))?)),
        None => None,
    };
    let (ownership, ownership_problem) = owner_of_new_files(&args.data_dir);
    let request = Request {
        from: args.from,
        site,
        config_path: config_path.to_path_buf(),
        data_dir: args.data_dir,
        plugins: crate::plugins::compiled_in(),
        ownership,
        ownership_problem,
        actor: privileges::user_name(),
        now: SystemClock.now_ms(),
        listen_probe,
        dry_run: args.dry_run,
    };
    match migrate::run(request) {
        Ok(report) => {
            print!("{}", report.render());
            Ok(ExitCode::SUCCESS)
        }
        Err(error) => Err(CliError { message: error.message, hint: error.hint }),
    }
}

/// Who the new files belong to: run as root, the owner of the new data directory
/// when install.sh made it, or else the service user. Anyone else keeps them.
#[cfg(unix)]
fn owner_of_new_files(data_dir: &Path) -> (Ownership, Option<String>) {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no preconditions and cannot fail.
    if unsafe { libc::geteuid() } != 0 {
        return (Ownership::Current, None);
    }
    if let Ok(metadata) = std::fs::metadata(data_dir) {
        if metadata.uid() != 0 {
            return (Ownership::Service { uid: metadata.uid(), gid: metadata.gid() }, None);
        }
    }
    match user_ids(SERVICE_USER) {
        Some((uid, gid)) if uid != 0 => (Ownership::Service { uid, gid }, None),
        _ => (Ownership::Current, Some(format!("there is no {SERVICE_USER} user yet for the new files to belong to"))),
    }
}

#[cfg(not(unix))]
fn owner_of_new_files(_data_dir: &Path) -> (Ownership, Option<String>) {
    (Ownership::Current, None)
}

/// A user's id and primary group, by name.
#[cfg(unix)]
fn user_ids(name: &str) -> Option<(u32, u32)> {
    // The C name of a user database entry, under a plainer one.
    use libc::passwd as UserEntry;
    let name = std::ffi::CString::new(name).ok()?;
    let mut buffer = vec![0 as libc::c_char; 4096];
    // SAFETY: an all-zero entry is a valid value for getpwnam_r to fill in.
    let mut entry: UserEntry = unsafe { std::mem::zeroed() };
    let mut found: *mut UserEntry = std::ptr::null_mut();
    // SAFETY: every pointer is valid for the call and the buffer's length is passed with it.
    let status = unsafe { libc::getpwnam_r(name.as_ptr(), &mut entry, buffer.as_mut_ptr(), buffer.len(), &mut found) };
    if status != 0 || found.is_null() {
        return None;
    }
    Some((entry.pw_uid, entry.pw_gid))
}
