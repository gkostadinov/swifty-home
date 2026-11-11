//! `swifty audit verify` (and its old name `audit-verify`) and `swifty enroll-token`.

use std::path::Path;
use std::process::ExitCode;

use clap::{Args, Subcommand};
use swifty_hub::audit::{self, AuditKey, Checkpoint, VerifyOptions};
use swifty_hub::clock::{Clock, SystemClock};
use swifty_hub::passkeys::{Passkeys, ENROLL_TOKEN_TTL_MS};
use swifty_hub::store::Store;

use super::init::enroll_link;
use super::{load_hub_config, CliError};
use crate::privileges;

#[derive(Debug, Subcommand)]
pub enum AuditCommand {
    /// Check the audit log's hash chain, and its signature when there is a key.
    Verify(VerifyArgs),
}

#[derive(Debug, Args)]
pub struct VerifyArgs {
    /// A checkpoint exported earlier, written seq:hash. The log must still contain it.
    #[arg(long, value_name = "SEQ:HASH")]
    pub expect_head: Option<String>,
    /// Accept a log with no rows. Only for a hub that has never been used.
    #[arg(long)]
    pub allow_empty: bool,
}

/// Prints the report as JSON. Exit code 1 when the log does not verify.
pub fn verify(config_path: &Path, args: VerifyArgs) -> Result<ExitCode, CliError> {
    let config = load_hub_config(config_path)?;
    let database = &config.hub.database_path();
    if !database.exists() {
        return Err(CliError::new(format!("there is no database at {}.", database.display()))
            .hint("Check data_dir in the configuration, or run `sudo swifty init` for a new hub."));
    }
    let expected: Option<Checkpoint> = args.expect_head.as_deref().map(str::parse).transpose()?;
    privileges::become_owner_of(&config.data_dir)?;
    let key = config.hub.audit.key_file.as_deref().map(AuditKey::load).transpose().map_err(|e| e.to_string())?;
    // Read-only, and refused if it is not a database this service has run on:
    // the check must never turn an emptied file into a clean log.
    let store = Store::open_for_inspection(database)?;
    let options = VerifyOptions { key: key.as_ref(), expected: expected.as_ref(), allow_empty: args.allow_empty };
    let report = store.with(|conn| audit::verify_with(conn, options)).map_err(|e| e.to_string())?;
    println!("{}", serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?);
    Ok(if report.ok { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

/// Prints a new one-time token for the first passkey on standard output, and
/// on standard error the link that opens the app with the token filled in
/// (the same link `swifty init` prints) and what else to do with it.
pub fn enroll_token(config_path: &Path) -> Result<ExitCode, CliError> {
    let config = load_hub_config(config_path)?;
    privileges::become_owner_of(&config.data_dir)?;
    let store = Store::open(&config.hub.database_path()).map_err(|e| e.to_string())?;
    let (token, _expires) = Passkeys::issue_enroll_token(&store, SystemClock.now_ms()).map_err(|e| e.message)?;
    println!("{token}");
    let origin = &config.hub.webauthn.origin;
    eprintln!(
        "One-time token for the first passkey. Open {} or open {origin} and paste the token. It expires in {}          minutes and is shown only this once.",
        enroll_link(origin, &token),
        ENROLL_TOKEN_TTL_MS / 60_000
    );
    Ok(ExitCode::SUCCESS)
}
