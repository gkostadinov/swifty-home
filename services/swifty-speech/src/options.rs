//! The command line.

use std::ffi::OsString;
use std::net::{IpAddr, SocketAddr};
use std::path::{self, PathBuf};

use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "swifty-speech", version, about = "Local speech-to-text service for Swifty Home")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    #[command(flatten)]
    pub options: Options,
}

#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum Command {
    /// Run in the foreground, logging to stdout
    Run,
    /// Print the text of one WAV file, without any network
    Transcribe { file: PathBuf },
    /// Register and start the SwiftySpeech Windows service, replacing the old GlimdockSpeech one (administrator prompt)
    Install,
    /// Stop and remove the SwiftySpeech Windows service (administrator prompt)
    Uninstall,
    /// Internal entry point used by the Windows service manager
    #[command(hide = true)]
    Service,
}

#[derive(Args, Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    /// Address and port to listen on [default: 127.0.0.1:8791]
    #[arg(long, global = true, value_name = "ADDR:PORT")]
    pub bind: Option<SocketAddr>,

    /// Accept requests only from this client address (repeatable)
    #[arg(long, global = true, value_name = "IP")]
    pub allow: Vec<IpAddr>,

    /// Number of decoding threads [default: 4]
    #[arg(long, global = true, value_name = "N", value_parser = clap::value_parser!(u16).range(1..=256))]
    pub threads: Option<u16>,

    /// Token file, generated if it does not exist
    #[arg(long, global = true, value_name = "PATH")]
    pub token_file: Option<PathBuf>,

    /// Model directory [default: "model" beside the executable]
    #[arg(long, global = true, value_name = "DIR")]
    pub model: Option<PathBuf>,
}

impl Options {
    /// The arguments for the service's command line: the `service` command and
    /// the options that were given. Paths become absolute, because a service
    /// does not start in the directory `install` was run from. Each value is
    /// one element here; quoting them is left to the command-line builder.
    pub fn service_arguments(&self) -> Result<Vec<OsString>, String> {
        let mut arguments = vec![OsString::from("service")];
        if let Some(bind) = self.bind {
            arguments.push("--bind".into());
            arguments.push(bind.to_string().into());
        }
        for address in &self.allow {
            arguments.push("--allow".into());
            arguments.push(address.to_string().into());
        }
        if let Some(threads) = self.threads {
            arguments.push("--threads".into());
            arguments.push(threads.to_string().into());
        }
        if let Some(file) = &self.token_file {
            arguments.push("--token-file".into());
            arguments.push(absolute(file)?.into_os_string());
        }
        if let Some(directory) = &self.model {
            arguments.push("--model".into());
            arguments.push(absolute(directory)?.into_os_string());
        }
        Ok(arguments)
    }
}

pub fn absolute(path: &std::path::Path) -> Result<PathBuf, String> {
    path::absolute(path).map_err(|error| format!("cannot resolve {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("swifty-speech").chain(arguments.iter().copied()))
    }

    #[test]
    fn accepts_every_command() {
        assert_eq!(parse(&["run"]).unwrap().command, Command::Run);
        assert_eq!(parse(&["install"]).unwrap().command, Command::Install);
        assert_eq!(parse(&["uninstall"]).unwrap().command, Command::Uninstall);
        assert_eq!(parse(&["service"]).unwrap().command, Command::Service);
        assert_eq!(
            parse(&["transcribe", "clip one.wav"]).unwrap().command,
            Command::Transcribe { file: PathBuf::from("clip one.wav") }
        );
    }

    #[test]
    fn version_is_available() {
        assert_eq!(parse(&["--version"]).unwrap_err().kind(), clap::error::ErrorKind::DisplayVersion);
    }

    #[test]
    fn parses_options_after_the_command() {
        let cli = parse(&[
            "run",
            "--bind",
            "0.0.0.0:9000",
            "--allow",
            "192.0.2.2",
            "--allow",
            "fd00::2",
            "--threads",
            "2",
            "--token-file",
            r"C:\a b\token",
            "--model",
            r"D:\m",
        ])
        .unwrap();
        assert_eq!(cli.options.bind.unwrap().to_string(), "0.0.0.0:9000");
        assert_eq!(cli.options.allow.len(), 2);
        assert_eq!(cli.options.threads, Some(2));
        assert_eq!(cli.options.token_file.unwrap(), PathBuf::from(r"C:\a b\token"));
        assert_eq!(cli.options.model.unwrap(), PathBuf::from(r"D:\m"));
    }

    #[test]
    fn refuses_bad_values() {
        assert!(parse(&["run", "--bind", "not-an-address"]).is_err());
        assert!(parse(&["run", "--bind", "127.0.0.1"]).is_err());
        assert!(parse(&["run", "--allow", "example.com"]).is_err());
        assert!(parse(&["run", "--threads", "0"]).is_err());
        assert!(parse(&["run", "--threads", "257"]).is_err());
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn nothing_is_added_when_no_options_are_given() {
        let arguments = Options::default().service_arguments().unwrap();
        assert_eq!(arguments, vec![OsString::from("service")]);
    }

    #[test]
    fn service_arguments_round_trip_through_the_parser() {
        let base = std::env::temp_dir().join("space in name");
        let given = Options {
            bind: Some("192.0.2.30:8791".parse().unwrap()),
            allow: vec!["192.0.2.10".parse().unwrap(), "192.0.2.11".parse().unwrap()],
            threads: Some(6),
            token_file: Some(base.join("token file")),
            model: Some(base.join("the model")),
        };

        let arguments = given.service_arguments().unwrap();
        let strings: Vec<&std::ffi::OsStr> = arguments.iter().map(OsString::as_os_str).collect();
        // A path with spaces must stay a single element, unquoted, at this level.
        assert!(strings.contains(&base.join("token file").as_os_str()));

        let cli = Cli::try_parse_from(std::iter::once(OsString::from("swifty-speech")).chain(arguments)).unwrap();
        assert_eq!(cli.command, Command::Service);
        assert_eq!(cli.options, given);
    }

    #[test]
    fn relative_paths_become_absolute() {
        let given = Options { token_file: Some(PathBuf::from(r"relative\token")), ..Options::default() };
        let arguments = given.service_arguments().unwrap();
        assert!(PathBuf::from(&arguments[2]).is_absolute());
        assert!(PathBuf::from(&arguments[2]).ends_with(r"relative\token"));
    }
}
