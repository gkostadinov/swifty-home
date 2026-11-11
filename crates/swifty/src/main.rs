//! The `swifty` command line. One binary: `swifty serve` is the hub, and the
//! other commands work on its configuration, database and files.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use swifty::cli::{self, Cli, CliError, Command};
use swifty::privileges;
use swifty_hub::app::App;
use swifty_hub::audit;
use swifty_hub::clock::{Clock, SystemClock};
use swifty_hub::config::Config;
use swifty_hub::runtime::Registry;
use swifty_hub::secrets::FileSecretStore;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Serve => serve(&cli.config),
        Command::Migrate(args) => cli::migrate::run(&cli.config, args, &listen_probe),
        _ => cli::run(cli),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// Whether this process could listen on `address`. `swifty migrate` refuses while
/// the old hub still answers there, and the new hub will listen on it. Opening a
/// listener is this file's business alone, so the check is here.
fn listen_probe(address: &str) -> std::io::Result<()> {
    std::net::TcpListener::bind(address).map(drop)
}

/// Reads the configuration, becomes the owner of the data directory when run as
/// root, and only then starts the runtime and the hub.
fn serve(config_path: &std::path::Path) -> Result<ExitCode, CliError> {
    let config = cli::load_hub_config(config_path)?;
    privileges::become_owner_of(&config.data_dir)?;
    // Colours only on a terminal: the journal and a redirected log get plain text.
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stderr());
    tracing_subscriber::fmt().with_env_filter("info").with_writer(std::io::stderr).with_ansi(ansi).init();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| CliError::new(format!("cannot start: {e}")))?;
    runtime.block_on(run_hub(config.hub)).map_err(CliError::new)?;
    Ok(ExitCode::SUCCESS)
}

/// How often the service logs the audit checkpoint.
const CHECKPOINT_LOG_INTERVAL: Duration = Duration::from_secs(300);

/// Puts the audit checkpoint in the service log whenever it has moved, so that a
/// log shipper can carry it off this machine. A checkpoint is a hash, not a secret.
fn log_audit_checkpoints(app: App) {
    tokio::spawn(async move {
        let mut last = None;
        let mut interval = tokio::time::interval(CHECKPOINT_LOG_INTERVAL);
        loop {
            interval.tick().await;
            match app.store.with(audit::checkpoint) {
                Ok(Some(checkpoint)) if last.as_ref() != Some(&checkpoint) => {
                    tracing::info!(%checkpoint, "audit checkpoint");
                    last = Some(checkpoint);
                }
                Ok(_) => {}
                Err(error) => tracing::error!(%error, "could not read the audit checkpoint"),
            }
        }
    });
}

async fn run_hub(config: Config) -> Result<(), String> {
    let listen = config.listen.clone();
    let live = config.live;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let secrets = Arc::new(FileSecretStore::new(config.secrets_path()));
    let registry = Registry::new(live, swifty::plugins::compiled_in(), secrets, (&config.network).into());
    let app = App::new(config, clock, registry).map_err(|e| e.to_string())?;
    let listener =
        tokio::net::TcpListener::bind(&listen).await.map_err(|e| format!("cannot listen on {listen}: {e}"))?;
    tracing::info!(%listen, live, "Swifty Home is running");
    log_audit_checkpoints(app.clone());
    swifty_hub::background::start(app.clone());
    swifty_hub::background::watch_instances(app.clone());
    axum::serve(listener, swifty_hub::http::router(app))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| e.to_string())
}

#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let Ok(mut terminate) = signal(SignalKind::terminate()) else {
        let _ = tokio::signal::ctrl_c().await;
        return;
    };
    tokio::select! {
        _ = terminate.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
