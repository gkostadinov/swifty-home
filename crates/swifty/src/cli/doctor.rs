//! `swifty doctor`: every check, each with its fix. Exit code 1 on any error.

use std::path::Path;
use std::process::ExitCode;

use clap::Args;
use swifty_hub::admin::doctor::{self, Check, Report, Status};
use swifty_hub::admin::init::load_config;
use swifty_hub::clock::{Clock, SystemClock};

use super::CliError;
use crate::privileges;

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Print JSON instead of text.
    #[arg(long)]
    pub json: bool,
}

pub fn run(config_path: &Path, args: DoctorArgs) -> Result<ExitCode, CliError> {
    // As root, look at everything as the service does: as the owner of its data.
    let data_dir = load_config(config_path).ok().map(|config| config.data_path());
    if let Some(dir) = data_dir.as_deref().filter(|dir| dir.is_dir()) {
        privileges::become_owner_of(dir)?;
    }
    let mut report = doctor::run(config_path, SystemClock.now_ms(), &crate::plugins::compiled_in());
    if let Some(dir) = data_dir.as_deref().filter(|dir| dir.is_dir()) {
        report.checks.push(free_space(dir));
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?);
    } else {
        print_report(&report);
    }
    Ok(if report.has_errors() { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

fn print_report(report: &Report) {
    println!("Swifty Home doctor: {}", report.config.display());
    println!();
    for check in &report.checks {
        let status = match check.status {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Error => "ERROR",
            Status::Skip => "skip",
        };
        println!("  {status:<6} {:<10} {}", check.name, check.detail);
        if let Some(fix) = &check.fix {
            println!("  {:<6} {:<10} fix: {fix}", "", "");
        }
    }
    println!();
    let (errors, warnings) = (report.count(Status::Error), report.count(Status::Warn));
    match (errors, warnings) {
        (0, 0) => println!("All good."),
        (0, w) => println!("No problems, {w} warning(s)."),
        (e, w) => println!("{e} problem(s), {w} warning(s). Fix the problems first; each one says how."),
    }
}

/// Free space where the database grows.
fn free_space(dir: &Path) -> Check {
    match available_bytes(dir) {
        None => Check::new("space", Status::Skip, "Free space is checked on Linux only."),
        Some(bytes) => {
            let mib = bytes / (1024 * 1024);
            let shown = format!("{mib} MiB free in {}", dir.display());
            if mib < 100 {
                Check::new("space", Status::Error, shown).with_fix("Free some space: the database cannot grow.")
            } else if mib < 1024 {
                Check::new("space", Status::Warn, shown).with_fix("Free some space before backups fill it.")
            } else {
                Check::new("space", Status::Ok, shown)
            }
        }
    }
}

#[cfg(unix)]
fn available_bytes(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: an all-zero statvfs is a valid value for statvfs to fill in.
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: the path is NUL-terminated and `stats` is valid for writing.
    if unsafe { libc::statvfs(path.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    // The field types differ between systems; on some they are u64 already.
    #[allow(clippy::unnecessary_cast)]
    let bytes = stats.f_bavail as u64 * stats.f_frsize as u64;
    Some(bytes)
}

#[cfg(not(unix))]
fn available_bytes(_dir: &Path) -> Option<u64> {
    None
}
