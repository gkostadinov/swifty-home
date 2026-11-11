//! Registering the Windows service, and taking over from the service of the old
//! name.
//!
//! The service used to be called `GlimdockSpeech` and kept its token and log in
//! `%ProgramData%\Glimdock\speech`. `install` finds that service and that folder,
//! moves the token and the log to the new folder, registers `SwiftySpeech` with
//! the old service's options, and removes the old service. Clients keep working
//! with the same token at the same address.
//!
//! What touches the machine goes through two seams, [`Services`] for the service
//! manager and [`Host`] for permissions and the token, so that the steps and their
//! order can be tested against temporary folders and a fake service manager.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use clap::Parser;

use crate::command_line;
use crate::options::{absolute, Cli, Command, Options};

pub const SERVICE_NAME: &str = "SwiftySpeech";
pub const DISPLAY_NAME: &str = "Swifty Speech";
pub const DESCRIPTION: &str = "Local speech-to-text for Swifty Home.";

/// The service of the old name, which `install` replaces.
pub const OLD_SERVICE_NAME: &str = "GlimdockSpeech";
/// Where the old service listened when it was given no `--bind`. It is kept on
/// the move, because clients were set up for it.
pub const OLD_DEFAULT_BIND: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8790);

/// The file names inside the data folder.
pub const TOKEN_FILE: &str = "token";
pub const LOG_FILE: &str = "speech.log";
/// The log rotated out of the way. See `logging`.
const LOG_BACKUP_FILE: &str = "speech.log.1";

/// The data folder, and the old service's data folder.
#[derive(Debug, Clone)]
pub struct Folders {
    pub data: PathBuf,
    pub old_data: PathBuf,
}

impl Folders {
    pub fn log(&self) -> PathBuf {
        self.data.join(LOG_FILE)
    }
}

/// A registered service, as far as `install` and `uninstall` need to know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registered {
    /// The program and its arguments, as one line.
    pub command_line: OsString,
    pub stopped: bool,
}

/// The service manager.
pub trait Services {
    fn query(&mut self, name: &str) -> Result<Option<Registered>, String>;
    /// Ask the service to stop and wait for it. `Ok(false)` if it did not stop in time.
    fn stop(&mut self, name: &str) -> Result<bool, String>;
    fn delete(&mut self, name: &str) -> Result<(), String>;
    /// Register [`SERVICE_NAME`] to start automatically and to be restarted when it fails.
    fn create(&mut self, executable: &Path, arguments: &[OsString]) -> Result<(), String>;
    fn start(&mut self, name: &str) -> Result<(), String>;
}

/// The rest of the machine: the installed files, permissions and the token.
pub trait Host {
    /// The executable to register and the options to give it, after checking that
    /// both are in a location only trusted accounts can change.
    fn prepare_launch(&self, options: &Options) -> Result<(PathBuf, Options), String>;
    /// Create the data folder, or make an existing one private to Administrators and SYSTEM.
    fn prepare_data_folder(&self, folder: &Path) -> Result<(), String>;
    /// Refuse to take files from a folder that someone else could have put them in.
    fn check_old_folder(&self, folder: &Path) -> Result<(), String>;
    /// Read the token, generating it if it does not exist yet: the given file, or
    /// the one in the data folder. Returns the file's path.
    fn resolve_token(&self, custom: Option<&Path>) -> Result<PathBuf, String>;
}

/// A line for the person running the command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    Info(String),
    Warning(String),
}

/// Register the service and start it, moving over from the old service if it is
/// there. Every check that can fail runs before anything is changed. The old
/// service is stopped first and removed only after the new one is registered, so
/// a run that fails part way can simply be repeated.
pub fn install(
    services: &mut dyn Services,
    host: &dyn Host,
    folders: &Folders,
    given: &Options,
    note: &mut dyn FnMut(Note),
) -> Result<(), String> {
    if services.query(SERVICE_NAME)?.is_some() {
        return Err(format!("the {SERVICE_NAME} service is already installed; run uninstall first"));
    }
    let old = services.query(OLD_SERVICE_NAME)?;

    let options = match &old {
        Some(old) if *given == Options::default() => {
            let options = old_options(&old.command_line)?;
            note(Note::Info(format!(
                "Found the old {OLD_SERVICE_NAME} service; {SERVICE_NAME} takes over its options and listens on {}.",
                options.bind.unwrap_or(OLD_DEFAULT_BIND)
            )));
            options
        }
        _ => given.clone(),
    };

    let (executable, persisted) = host.prepare_launch(&options)?;
    let arguments = persisted.service_arguments()?;
    host.prepare_data_folder(&folders.data)?;
    let moves = plan_moves(host, folders, note)?;

    if let Some(old) = &old {
        if !old.stopped && !services.stop(OLD_SERVICE_NAME)? {
            return Err(format!(
                "the old {OLD_SERVICE_NAME} service did not stop in time. Nothing was moved; run install again \
                 once it has stopped"
            ));
        }
    }

    for (from, to) in &moves {
        fs::rename(from, to).map_err(|error| {
            format!("cannot move {} to {}: {error}. Run install again to finish", from.display(), to.display())
        })?;
    }
    if !moves.is_empty() {
        note(Note::Info(format!(
            "Moved the token and log from {} to {}.",
            folders.old_data.display(),
            folders.data.display()
        )));
        remove_if_empty(&folders.old_data);
    }

    let token_file = persisted.token_file.as_deref().map(absolute).transpose()?;
    let token_path = host.resolve_token(token_file.as_deref())?;

    services.create(&executable, &arguments)?;
    note(Note::Info(format!("Registered the {SERVICE_NAME} service (automatic start).")));
    note(Note::Info(format!("Token file: {}", token_path.display())));
    note(Note::Info(format!("Log file: {}", folders.log().display())));

    if old.is_some() {
        match services.delete(OLD_SERVICE_NAME) {
            Ok(()) => note(Note::Info(format!("Removed the old {OLD_SERVICE_NAME} service."))),
            Err(error) => note(Note::Warning(format!(
                "cannot remove the old {OLD_SERVICE_NAME} service ({error}). It is stopped; remove it with \
                 `sc delete {OLD_SERVICE_NAME}` from an administrator prompt"
            ))),
        }
    }

    services.start(SERVICE_NAME).map_err(|error| {
        format!("the service is registered but could not be started: {error}. See {}", folders.log().display())
    })?;
    note(Note::Info(
        "Started the service. The model takes a few seconds to load; /healthz reports when it is ready.".into(),
    ));
    Ok(())
}

/// Stop the service if it is running, and remove it.
pub fn uninstall(services: &mut dyn Services, folders: &Folders, note: &mut dyn FnMut(Note)) -> Result<(), String> {
    let Some(registered) = services.query(SERVICE_NAME)? else {
        note(Note::Info(format!("The {SERVICE_NAME} service is not installed.")));
        if services.query(OLD_SERVICE_NAME)?.is_some() {
            note(Note::Info(format!(
                "The old {OLD_SERVICE_NAME} service is still installed; `install` replaces it, or remove it with \
                 `sc delete {OLD_SERVICE_NAME}`."
            )));
        }
        return Ok(());
    };

    if !registered.stopped && !services.stop(SERVICE_NAME)? {
        note(Note::Warning("the service did not stop in time; it will be removed when it exits".into()));
    }
    services.delete(SERVICE_NAME)?;
    note(Note::Info(format!("Removed the {SERVICE_NAME} service.")));
    note(Note::Info(format!("The token and log were kept in {}.", folders.data.display())));
    Ok(())
}

/// The options the old service was registered with. One that was given no
/// `--bind` listened on the old default port, so that address is kept.
pub fn old_options(command_line: &OsStr) -> Result<Options, String> {
    let unreadable = |why: String| {
        format!(
            "cannot read the options of the old {OLD_SERVICE_NAME} service ({why}). Give the options to install \
             yourself, or remove the old service with `sc delete {OLD_SERVICE_NAME}` first"
        )
    };
    let line = command_line.to_str().ok_or_else(|| unreadable("its command line is not valid Unicode".into()))?;
    let cli = Cli::try_parse_from(command_line::split(line))
        .map_err(|error| unreadable(error.kind().as_str().unwrap_or("not a service command line").to_string()))?;
    if cli.command != Command::Service {
        return Err(unreadable("it does not run the service command".into()));
    }
    let mut options = cli.options;
    options.bind.get_or_insert(OLD_DEFAULT_BIND);
    Ok(options)
}

/// The files to move from the old folder, as (from, to). Refuses an old folder
/// that is not safe to take files from, and a token in both places.
fn plan_moves(
    host: &dyn Host,
    folders: &Folders,
    note: &mut dyn FnMut(Note),
) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    match fs::symlink_metadata(&folders.old_data) {
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot read {}: {error}", folders.old_data.display())),
    }
    host.check_old_folder(&folders.old_data)?;

    let mut moves = Vec::new();
    for name in [TOKEN_FILE, LOG_FILE, LOG_BACKUP_FILE] {
        let from = folders.old_data.join(name);
        let metadata = match fs::symlink_metadata(&from) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("cannot read {}: {error}", from.display())),
        };
        if !metadata.is_file() {
            return Err(format!("{} is not a plain file; refusing to move it", from.display()));
        }
        let to = folders.data.join(name);
        if fs::symlink_metadata(&to).is_ok() {
            if name == TOKEN_FILE {
                return Err(format!(
                    "there is a token in both {} and {}. Delete the one the clients do not use, then run install again",
                    from.display(),
                    to.display()
                ));
            }
            note(Note::Info(format!("Kept {}, because {} already exists.", from.display(), to.display())));
            continue;
        }
        moves.push((from, to));
    }
    Ok(moves)
}

/// Remove the old folder and its parent if nothing else is left in them.
fn remove_if_empty(folder: &Path) {
    if fs::remove_dir(folder).is_ok() {
        if let Some(parent) = folder.parent() {
            let _ = fs::remove_dir(parent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const OLD_EXE: &str = r#""C:\Program Files\Old Speech\old-speech.exe""#;

    /// A service manager that keeps its services in a map and records every call.
    #[derive(Default)]
    struct FakeServices {
        registered: BTreeMap<String, Registered>,
        calls: Vec<String>,
        created: Option<(PathBuf, Vec<OsString>)>,
        stops_in_time: bool,
        fail_create: bool,
    }

    impl FakeServices {
        fn with_old(command_line: &str, running: bool) -> Self {
            let mut services = Self { stops_in_time: true, ..Self::default() };
            services
                .registered
                .insert(OLD_SERVICE_NAME.into(), Registered { command_line: command_line.into(), stopped: !running });
            services
        }

        fn names(&self) -> Vec<&str> {
            self.registered.keys().map(String::as_str).collect()
        }
    }

    impl Services for FakeServices {
        fn query(&mut self, name: &str) -> Result<Option<Registered>, String> {
            self.calls.push(format!("query {name}"));
            Ok(self.registered.get(name).cloned())
        }

        fn stop(&mut self, name: &str) -> Result<bool, String> {
            self.calls.push(format!("stop {name}"));
            let service = self.registered.get_mut(name).ok_or("not installed")?;
            service.stopped = self.stops_in_time;
            Ok(self.stops_in_time)
        }

        fn delete(&mut self, name: &str) -> Result<(), String> {
            self.calls.push(format!("delete {name}"));
            self.registered.remove(name).map(drop).ok_or_else(|| "not installed".to_string())
        }

        fn create(&mut self, executable: &Path, arguments: &[OsString]) -> Result<(), String> {
            self.calls.push(format!("create {SERVICE_NAME}"));
            if self.fail_create {
                return Err("cannot create the service: access denied".into());
            }
            let line = format!("{} {}", executable.display(), arguments.join(OsStr::new(" ")).to_string_lossy());
            self.registered.insert(SERVICE_NAME.into(), Registered { command_line: line.into(), stopped: true });
            self.created = Some((executable.to_path_buf(), arguments.to_vec()));
            Ok(())
        }

        fn start(&mut self, name: &str) -> Result<(), String> {
            self.calls.push(format!("start {name}"));
            self.registered.get_mut(name).ok_or("not installed")?.stopped = false;
            Ok(())
        }
    }

    /// Plain folders and files, with no permission checks.
    struct FakeHost {
        data: PathBuf,
    }

    impl Host for FakeHost {
        fn prepare_launch(&self, options: &Options) -> Result<(PathBuf, Options), String> {
            Ok((PathBuf::from(r"C:\Program Files\Swifty Speech\swifty-speech.exe"), options.clone()))
        }

        fn prepare_data_folder(&self, folder: &Path) -> Result<(), String> {
            fs::create_dir_all(folder).map_err(|error| error.to_string())
        }

        fn check_old_folder(&self, folder: &Path) -> Result<(), String> {
            let metadata = fs::symlink_metadata(folder).map_err(|error| error.to_string())?;
            if metadata.is_dir() {
                Ok(())
            } else {
                Err(format!("{} is not a plain directory", folder.display()))
            }
        }

        fn resolve_token(&self, custom: Option<&Path>) -> Result<PathBuf, String> {
            let path = custom.map_or_else(|| self.data.join(TOKEN_FILE), Path::to_path_buf);
            if !path.exists() {
                fs::write(&path, "f".repeat(64)).map_err(|error| error.to_string())?;
            }
            Ok(path)
        }
    }

    /// A temporary `ProgramData` with the old and new data folders in it.
    struct Machine {
        root: PathBuf,
        folders: Folders,
        host: FakeHost,
    }

    impl Machine {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("ss-install-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            let folders = Folders {
                data: root.join("SwiftyHome").join("speech"),
                old_data: root.join("Glimdock").join("speech"),
            };
            let host = FakeHost { data: folders.data.clone() };
            Self { root, folders, host }
        }

        /// The old folder as the old service left it.
        fn with_old_files(self) -> Self {
            fs::create_dir_all(&self.folders.old_data).unwrap();
            fs::write(self.folders.old_data.join(TOKEN_FILE), format!("{TOKEN}\r\n")).unwrap();
            fs::write(self.folders.old_data.join(LOG_FILE), "current log\n").unwrap();
            fs::write(self.folders.old_data.join(LOG_BACKUP_FILE), "older log\n").unwrap();
            self
        }

        fn install(&self, services: &mut FakeServices, given: &Options) -> (Result<(), String>, Vec<Note>) {
            let mut notes = Vec::new();
            let result = install(services, &self.host, &self.folders, given, &mut |line| notes.push(line));
            (result, notes)
        }

        fn new_file(&self, name: &str) -> Option<String> {
            fs::read_to_string(self.folders.data.join(name)).ok()
        }

        fn old_file(&self, name: &str) -> Option<String> {
            fs::read_to_string(self.folders.old_data.join(name)).ok()
        }
    }

    impl Drop for Machine {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn arguments(services: &FakeServices) -> Vec<String> {
        let (_, arguments) = services.created.as_ref().expect("the service was created");
        arguments.iter().map(|argument| argument.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn an_old_folder_with_a_token_becomes_the_new_folder_with_the_same_token() {
        let machine = Machine::new("move").with_old_files();
        let line = format!("{OLD_EXE} service --bind 192.0.2.30:8790 --allow 192.0.2.20 --threads 6");
        let mut services = FakeServices::with_old(&line, true);

        let (result, notes) = machine.install(&mut services, &Options::default());
        result.unwrap();

        assert_eq!(machine.new_file(TOKEN_FILE).unwrap(), format!("{TOKEN}\r\n"));
        assert_eq!(machine.new_file(LOG_FILE).unwrap(), "current log\n");
        assert_eq!(machine.new_file(LOG_BACKUP_FILE).unwrap(), "older log\n");
        assert!(!machine.folders.old_data.exists(), "the emptied old folder is removed");
        assert!(!machine.folders.old_data.parent().unwrap().exists(), "and so is its emptied parent");

        assert_eq!(services.names(), [SERVICE_NAME]);
        assert!(!services.registered[SERVICE_NAME].stopped);
        assert_eq!(
            arguments(&services),
            ["service", "--bind", "192.0.2.30:8790", "--allow", "192.0.2.20", "--threads", "6"]
        );
        assert_eq!(
            services.calls,
            [
                "query SwiftySpeech",
                "query GlimdockSpeech",
                "stop GlimdockSpeech",
                "create SwiftySpeech",
                "delete GlimdockSpeech",
                "start SwiftySpeech",
            ]
        );
        assert!(notes.iter().all(|note| matches!(note, Note::Info(_))), "{notes:?}");
        assert!(notes.iter().all(|note| !format!("{note:?}").contains(TOKEN)), "the token is never shown");
    }

    #[test]
    fn an_old_service_without_an_address_keeps_listening_on_the_old_port() {
        let machine = Machine::new("old-port").with_old_files();
        let mut services = FakeServices::with_old(&format!("{OLD_EXE} service"), false);

        let (result, _) = machine.install(&mut services, &Options::default());
        result.unwrap();

        assert_eq!(arguments(&services), ["service", "--bind", "127.0.0.1:8790"]);
        assert!(!services.calls.contains(&"stop GlimdockSpeech".to_string()), "it was already stopped");
        assert_eq!(machine.new_file(TOKEN_FILE).unwrap(), format!("{TOKEN}\r\n"));
    }

    #[test]
    fn the_old_services_paths_are_carried_over() {
        let machine = Machine::new("old-paths").with_old_files();
        let token = machine.root.join("my token");
        let model = machine.root.join("the model");
        let line = format!(
            r#"{OLD_EXE} service --bind 127.0.0.1:8790 --token-file "{}" --model "{}""#,
            token.display(),
            model.display()
        );
        let mut services = FakeServices::with_old(&line, true);

        let (result, _) = machine.install(&mut services, &Options::default());
        result.unwrap();

        let expected = [
            "service".to_string(),
            "--bind".into(),
            "127.0.0.1:8790".into(),
            "--token-file".into(),
            token.display().to_string(),
            "--model".into(),
            model.display().to_string(),
        ];
        assert_eq!(arguments(&services), expected);
        assert!(token.exists(), "the token named by the old service is the one in use");
        assert_eq!(machine.new_file(TOKEN_FILE).unwrap(), format!("{TOKEN}\r\n"), "the default token still moves");
    }

    #[test]
    fn options_given_to_install_replace_the_old_ones() {
        let machine = Machine::new("given").with_old_files();
        let mut services = FakeServices::with_old(&format!("{OLD_EXE} service --bind 192.0.2.30:8790"), true);
        let given = Options { threads: Some(2), ..Options::default() };

        let (result, _) = machine.install(&mut services, &given);
        result.unwrap();

        assert_eq!(arguments(&services), ["service", "--threads", "2"]);
        assert_eq!(services.names(), [SERVICE_NAME]);
        assert_eq!(machine.new_file(TOKEN_FILE).unwrap(), format!("{TOKEN}\r\n"));
    }

    #[test]
    fn a_fresh_machine_gets_the_new_defaults() {
        let machine = Machine::new("fresh");
        let mut services = FakeServices::default();

        let (result, notes) = machine.install(&mut services, &Options::default());
        result.unwrap();

        assert_eq!(arguments(&services), ["service"], "no --bind: the new default port applies");
        assert_eq!(machine.new_file(TOKEN_FILE).unwrap().len(), 64, "a token was generated");
        assert!(!machine.folders.old_data.exists());
        assert_eq!(
            services.calls,
            ["query SwiftySpeech", "query GlimdockSpeech", "create SwiftySpeech", "start SwiftySpeech"]
        );
        let text = format!("{notes:?}");
        assert!(text.contains("Token file:") && text.contains("Log file:"), "{text}");
        assert!(!text.contains("Moved") && !text.contains(OLD_SERVICE_NAME), "{text}");
    }

    #[test]
    fn the_old_folder_moves_even_when_the_old_service_is_gone() {
        let machine = Machine::new("folder-only").with_old_files();
        let mut services = FakeServices::default();

        let (result, _) = machine.install(&mut services, &Options::default());
        result.unwrap();

        assert_eq!(machine.new_file(TOKEN_FILE).unwrap(), format!("{TOKEN}\r\n"));
        assert!(!machine.folders.old_data.exists());
        assert_eq!(arguments(&services), ["service"]);
    }

    #[test]
    fn something_else_left_in_the_old_folder_keeps_it() {
        let machine = Machine::new("leftover").with_old_files();
        fs::write(machine.folders.old_data.join("notes.txt"), "mine").unwrap();
        let mut services = FakeServices::default();

        let (result, _) = machine.install(&mut services, &Options::default());
        result.unwrap();

        assert_eq!(machine.new_file(TOKEN_FILE).unwrap(), format!("{TOKEN}\r\n"));
        assert_eq!(machine.old_file("notes.txt").unwrap(), "mine");
        assert!(machine.old_file(TOKEN_FILE).is_none());
    }

    #[test]
    fn a_token_in_both_folders_is_refused_before_anything_changes() {
        let machine = Machine::new("both").with_old_files();
        fs::create_dir_all(&machine.folders.data).unwrap();
        fs::write(machine.folders.data.join(TOKEN_FILE), "e".repeat(64)).unwrap();
        let mut services = FakeServices::with_old(&format!("{OLD_EXE} service"), true);

        let (result, _) = machine.install(&mut services, &Options::default());
        let message = result.unwrap_err();

        assert!(message.contains("token in both"), "{message}");
        assert!(!message.contains(TOKEN));
        assert_eq!(machine.old_file(TOKEN_FILE).unwrap(), format!("{TOKEN}\r\n"));
        assert_eq!(machine.new_file(TOKEN_FILE).unwrap(), "e".repeat(64));
        assert_eq!(services.calls, ["query SwiftySpeech", "query GlimdockSpeech"]);
        assert!(!services.registered[OLD_SERVICE_NAME].stopped, "the old service keeps running");
    }

    #[test]
    fn logs_already_in_the_new_folder_are_kept() {
        let machine = Machine::new("logs").with_old_files();
        fs::create_dir_all(&machine.folders.data).unwrap();
        fs::write(machine.folders.data.join(LOG_FILE), "new log\n").unwrap();
        let mut services = FakeServices::default();

        let (result, notes) = machine.install(&mut services, &Options::default());
        result.unwrap();

        assert_eq!(machine.new_file(LOG_FILE).unwrap(), "new log\n");
        assert_eq!(machine.old_file(LOG_FILE).unwrap(), "current log\n");
        assert_eq!(machine.new_file(TOKEN_FILE).unwrap(), format!("{TOKEN}\r\n"));
        assert!(format!("{notes:?}").contains("Kept"), "{notes:?}");
    }

    #[test]
    fn a_new_service_that_is_already_installed_stops_everything() {
        let machine = Machine::new("installed").with_old_files();
        let mut services = FakeServices::with_old(&format!("{OLD_EXE} service"), true);
        services.registered.insert(SERVICE_NAME.into(), Registered { command_line: "x".into(), stopped: false });

        let (result, _) = machine.install(&mut services, &Options::default());

        assert!(result.unwrap_err().contains("already installed"));
        assert_eq!(services.calls, ["query SwiftySpeech"]);
        assert!(machine.old_file(TOKEN_FILE).is_some());
        assert!(machine.new_file(TOKEN_FILE).is_none());
    }

    #[test]
    fn an_old_service_that_does_not_stop_keeps_its_files_and_a_second_run_finishes() {
        let machine = Machine::new("slow-stop").with_old_files();
        let mut services = FakeServices::with_old(&format!("{OLD_EXE} service --threads 3"), true);
        services.stops_in_time = false;

        let (result, _) = machine.install(&mut services, &Options::default());
        assert!(result.unwrap_err().contains("did not stop in time"));
        assert!(machine.old_file(TOKEN_FILE).is_some());
        assert!(machine.new_file(TOKEN_FILE).is_none());
        assert_eq!(services.names(), [OLD_SERVICE_NAME]);

        services.stops_in_time = true;
        let (result, _) = machine.install(&mut services, &Options::default());
        result.unwrap();
        assert_eq!(machine.new_file(TOKEN_FILE).unwrap(), format!("{TOKEN}\r\n"));
        assert_eq!(arguments(&services), ["service", "--bind", "127.0.0.1:8790", "--threads", "3"]);
        assert_eq!(services.names(), [SERVICE_NAME]);
    }

    #[test]
    fn a_failed_registration_leaves_the_old_service_so_a_second_run_keeps_its_options() {
        let machine = Machine::new("failed-create").with_old_files();
        let mut services = FakeServices::with_old(&format!("{OLD_EXE} service --bind 192.0.2.30:8790"), true);
        services.fail_create = true;

        let (result, _) = machine.install(&mut services, &Options::default());
        assert!(result.unwrap_err().contains("access denied"));
        assert_eq!(services.names(), [OLD_SERVICE_NAME], "the old service is still registered");
        assert_eq!(machine.new_file(TOKEN_FILE).unwrap(), format!("{TOKEN}\r\n"), "the files moved already");

        services.fail_create = false;
        let (result, _) = machine.install(&mut services, &Options::default());
        result.unwrap();
        assert_eq!(arguments(&services), ["service", "--bind", "192.0.2.30:8790"]);
        assert_eq!(machine.new_file(TOKEN_FILE).unwrap(), format!("{TOKEN}\r\n"), "the same token");
        assert_eq!(services.names(), [SERVICE_NAME]);
    }

    #[test]
    fn something_other_than_a_file_in_the_old_folder_is_not_moved() {
        let machine = Machine::new("not-a-file");
        fs::create_dir_all(machine.folders.old_data.join(TOKEN_FILE)).unwrap();
        let mut services = FakeServices::default();

        let (result, _) = machine.install(&mut services, &Options::default());

        assert!(result.unwrap_err().contains("not a plain file"));
        assert!(machine.folders.old_data.join(TOKEN_FILE).is_dir());
        assert!(services.created.is_none());
    }

    #[test]
    fn an_old_service_whose_options_cannot_be_read_is_explained() {
        let machine = Machine::new("unreadable").with_old_files();
        let mut services = FakeServices::with_old(&format!("{OLD_EXE} run --bind 192.0.2.30:8790"), true);

        let (result, _) = machine.install(&mut services, &Options::default());
        let message = result.unwrap_err();
        assert!(
            message.contains("cannot read the options") && message.contains("sc delete GlimdockSpeech"),
            "{message}"
        );
        assert_eq!(services.calls, ["query SwiftySpeech", "query GlimdockSpeech"]);
        assert!(machine.old_file(TOKEN_FILE).is_some());

        // With options given, the old ones are not needed.
        let (result, _) = machine.install(&mut services, &Options { threads: Some(4), ..Options::default() });
        result.unwrap();
        assert_eq!(services.names(), [SERVICE_NAME]);
    }

    #[test]
    fn reads_the_options_of_an_old_command_line() {
        let line = r#""C:\Program Files\Old Speech\old-speech.exe" service --bind 192.0.2.30:8790 --allow 192.0.2.20 --allow 192.0.2.21 --threads 6 --token-file "C:\Program Data\a b\token" --model "C:\Program Files\Old Speech\model""#;
        let options = old_options(OsStr::new(line)).unwrap();
        assert_eq!(options.bind, Some("192.0.2.30:8790".parse().unwrap()));
        assert_eq!(options.allow, ["192.0.2.20".parse::<IpAddr>().unwrap(), "192.0.2.21".parse().unwrap()]);
        assert_eq!(options.threads, Some(6));
        assert_eq!(options.token_file, Some(PathBuf::from(r"C:\Program Data\a b\token")));
        assert_eq!(options.model, Some(PathBuf::from(r"C:\Program Files\Old Speech\model")));

        assert_eq!(old_options(OsStr::new(r"C:\old.exe service")).unwrap().bind, Some(OLD_DEFAULT_BIND));
        assert!(old_options(OsStr::new(r"C:\old.exe service --bind nowhere")).is_err());
        assert!(old_options(OsStr::new(r"C:\old.exe")).is_err());
    }

    #[test]
    fn uninstall_stops_and_removes_the_service_and_keeps_the_token() {
        let machine = Machine::new("uninstall");
        let mut services = FakeServices::default();
        machine.install(&mut services, &Options::default()).0.unwrap();
        services.calls.clear();

        let mut notes = Vec::new();
        uninstall(&mut services, &machine.folders, &mut |line| notes.push(line)).unwrap();

        assert_eq!(services.calls, ["query SwiftySpeech", "stop SwiftySpeech", "delete SwiftySpeech"]);
        assert!(services.registered.is_empty());
        assert!(machine.new_file(TOKEN_FILE).is_some());
        assert!(format!("{notes:?}").contains("were kept"), "{notes:?}");
    }

    #[test]
    fn uninstall_warns_about_a_slow_stop_and_mentions_the_old_service() {
        let machine = Machine::new("uninstall-slow");
        let mut services = FakeServices::default();
        services.registered.insert(SERVICE_NAME.into(), Registered { command_line: "x".into(), stopped: false });
        let mut notes = Vec::new();
        uninstall(&mut services, &machine.folders, &mut |line| notes.push(line)).unwrap();
        assert!(notes.iter().any(|note| matches!(note, Note::Warning(text) if text.contains("did not stop"))));
        assert!(services.registered.is_empty());

        let mut services = FakeServices::with_old(&format!("{OLD_EXE} service"), true);
        let mut notes = Vec::new();
        uninstall(&mut services, &machine.folders, &mut |line| notes.push(line)).unwrap();
        assert!(format!("{notes:?}").contains("not installed") && format!("{notes:?}").contains(OLD_SERVICE_NAME));
        assert_eq!(services.names(), [OLD_SERVICE_NAME], "uninstall leaves the old service to install");
    }

    /// A rename keeps a file's own access list, so a token that was private in the
    /// old folder is still private, and still accepted, in the new one.
    #[cfg(windows)]
    #[test]
    fn a_moved_token_keeps_its_permissions() {
        use crate::token::{load_or_create, Readers};
        use crate::win_acl;

        let machine = Machine::new("acl");
        fs::create_dir_all(&machine.folders.old_data).unwrap();
        let readers = Readers::CurrentUserAdministratorsAndSystem;
        let old_token = machine.folders.old_data.join(TOKEN_FILE);
        win_acl::create_new_private_file(&old_token, TOKEN.as_bytes(), readers).unwrap();
        let mut services = FakeServices::default();

        let (result, _) = machine.install(&mut services, &Options::default());
        result.unwrap();

        let new_token = machine.folders.data.join(TOKEN_FILE);
        let file = fs::File::open(&new_token).unwrap();
        win_acl::verify_token_file(&file, &new_token, readers).unwrap();
        drop(file);
        assert!(load_or_create(&new_token, readers).unwrap().matches(TOKEN.as_bytes()));
    }
}
