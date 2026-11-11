//! swifty-speech: turns a short push-to-talk audio clip into text.

// The parts used only by the Windows service are unused elsewhere.
#![cfg_attr(not(windows), allow(dead_code))]

mod command_line;
mod install;
mod logging;
mod options;
mod recognizer;
mod server;
mod token;
mod wav;
mod worker;

#[cfg(windows)]
mod win_acl;
#[cfg(windows)]
mod win_service;

use std::future::Future;
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;

use logging::Logger;
use options::{Cli, Command, Options};
use recognizer::{ModelFiles, Recognize, SherpaRecognizer};
use token::Readers;

/// One above the hub's default port, so both can run on one machine.
const DEFAULT_BIND: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 8791);
const DEFAULT_THREADS: u16 = 4;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let options = cli.options;

    let outcome = match cli.command {
        Command::Run => run_foreground(&options),
        Command::Transcribe { file } => transcribe_file(&file, &options),
        Command::Install => install(&options),
        Command::Uninstall => uninstall(),
        Command::Service => run_service(options),
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            let _ = writeln!(io::stderr(), "swifty-speech: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Replace the default panic report, which would print the panic's message.
/// This one records only that a panic happened and where.
fn install_panic_hook(logger: Logger) {
    std::panic::set_hook(Box::new(move |info| {
        let location = info.location().map_or_else(String::new, ToString::to_string);
        logger.info(&format!("internal panic at {location}"));
    }));
}

fn run_foreground(options: &Options) -> Result<(), String> {
    let logger = Logger::stdout();
    install_panic_hook(logger.clone());
    serve_until(options, Readers::CurrentUserAdministratorsAndSystem, logger, Duration::ZERO, shutdown_signal())
}

/// Prepare everything and serve until `shutdown` completes. `token_readers` is
/// who may have access to a token file given with `--token-file`.
fn serve_until<S>(
    options: &Options,
    token_readers: Readers,
    logger: Logger,
    bind_wait: Duration,
    shutdown: S,
) -> Result<(), String>
where
    S: Future<Output = ()> + Send,
{
    let model_directory = recognizer::resolve_model_directory(options.model.as_deref())?;
    let files = ModelFiles::locate(&model_directory)?;
    let (token_path, token) = token::resolve(options.token_file.as_deref(), token_readers)?;
    let threads = options.threads.unwrap_or(DEFAULT_THREADS);

    logger.info(&format!("starting swifty-speech {}", env!("CARGO_PKG_VERSION")));
    logger.info(&format!("model directory: {}", model_directory.display()));
    logger.info(&format!("token file: {}", token_path.display()));

    let listening_logger = logger.clone();
    let settings = server::Settings {
        bind: options.bind.unwrap_or(DEFAULT_BIND),
        allow: options.allow.clone(),
        token,
        logger: logger.clone(),
        bind_wait,
    };
    let result = server::run(
        settings,
        move || SherpaRecognizer::load(&files, threads),
        shutdown,
        move |address| listening_logger.info(&format!("listening on {address}, loading the model")),
    );
    logger.info("stopped");
    result
}

/// Completes on Ctrl+C, and on Ctrl+Break or SIGTERM where those exist.
async fn shutdown_signal() {
    #[cfg(windows)]
    {
        use tokio::signal::windows::ctrl_break;
        if let Ok(mut interrupt) = ctrl_break() {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = interrupt.recv() => {}
            }
            return;
        }
    }
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut terminate) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

fn transcribe_file(file: &Path, options: &Options) -> Result<(), String> {
    let metadata = std::fs::metadata(file).map_err(|error| format!("cannot read {}: {error}", file.display()))?;
    if metadata.len() > server::MAX_BODY_BYTES as u64 {
        return Err(format!("{} is larger than the 8 MiB limit", file.display()));
    }
    let bytes = std::fs::read(file).map_err(|error| format!("cannot read {}: {error}", file.display()))?;
    let audio = wav::parse(&bytes).map_err(|error| format!("{}: {error}", file.display()))?;
    drop(bytes);

    let model_directory = recognizer::resolve_model_directory(options.model.as_deref())?;
    let files = ModelFiles::locate(&model_directory)?;
    let recognizer = SherpaRecognizer::load(&files, options.threads.unwrap_or(DEFAULT_THREADS))?;
    let text = recognizer.recognize(audio.sample_rate, &audio.samples).map_err(str::to_string)?;

    let mut out = io::stdout().lock();
    writeln!(out, "{text}").map_err(|error| format!("cannot write the result: {error}"))
}

#[cfg(windows)]
fn install(options: &Options) -> Result<(), String> {
    win_service::install(options)
}

#[cfg(windows)]
fn uninstall() -> Result<(), String> {
    win_service::uninstall()
}

#[cfg(windows)]
fn run_service(options: Options) -> Result<(), String> {
    win_service::run_as_service(options)
}

#[cfg(not(windows))]
fn install(_options: &Options) -> Result<(), String> {
    Err("install is only available on Windows".to_string())
}

#[cfg(not(windows))]
fn uninstall() -> Result<(), String> {
    Err("uninstall is only available on Windows".to_string())
}

#[cfg(not(windows))]
fn run_service(_options: Options) -> Result<(), String> {
    Err("service is only available on Windows".to_string())
}
