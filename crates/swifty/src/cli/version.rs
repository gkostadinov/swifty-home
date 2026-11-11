//! `swifty version`.

use std::process::ExitCode;

use clap::Args;
use swifty_hub::admin::init::CONFIG_VERSION;

use super::CliError;

#[derive(Debug, Args)]
pub struct VersionArgs {
    /// Also list the plugins compiled into this build.
    #[arg(long)]
    pub plugins: bool,
}

/// The first line is what scripts read: `Swifty Home <version>`.
pub fn run(args: VersionArgs) -> Result<ExitCode, CliError> {
    println!("Swifty Home {}", env!("CARGO_PKG_VERSION"));
    println!("API v1, plugin API {}, configuration version {CONFIG_VERSION}", swifty_plugin_api::API_VERSION);
    if args.plugins {
        let plugins = crate::plugins::compiled_in();
        println!();
        println!("{} plugins compiled into this build:", plugins.len());
        for plugin in plugins {
            let manifest = plugin.manifest();
            println!("  {:<10} {:<8} {}", manifest.id, manifest.version, manifest.name);
        }
    }
    Ok(ExitCode::SUCCESS)
}
