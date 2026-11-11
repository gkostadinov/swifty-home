//! Windows service registration and the service-manager entry point.
//!
//! The steps of `install` and `uninstall` are in `install`. This module provides
//! the two things they work through: the real service manager and the real host.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::thread::sleep;
use std::time::{Duration, Instant};

use windows_service::service::{
    Service, ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept,
    ServiceErrorControl, ServiceExitCode, ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo,
    ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult, ServiceStatusHandle};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

use crate::install::{self, Folders, Host, Note, Registered, Services, DESCRIPTION, DISPLAY_NAME, SERVICE_NAME};
use crate::logging::Logger;
use crate::options::Options;
use crate::token::Readers;
use crate::{recognizer, serve_until, token, win_acl};

const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;
const ERROR_SERVICE_EXISTS: i32 = 1073;
const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
const STOP_WAIT: Duration = Duration::from_secs(30);
/// How long a service started at boot waits for its address to appear.
const BIND_WAIT: Duration = Duration::from_secs(120);

/// How long the manager should expect a stop to take.
const STOP_WAIT_HINT: Duration = Duration::from_secs(10);

static SERVICE_OPTIONS: OnceLock<Options> = OnceLock::new();
static STATUS_HANDLE: OnceLock<ServiceStatusHandle> = OnceLock::new();

fn folders() -> Folders {
    Folders { data: token::data_directory(), old_data: token::old_data_directory() }
}

fn describe(error: windows_service::Error) -> String {
    match std::error::Error::source(&error) {
        Some(source) => format!("{error}: {source}"),
        None => error.to_string(),
    }
}

fn os_error(error: &windows_service::Error) -> Option<i32> {
    match error {
        windows_service::Error::Winapi(source) => source.raw_os_error(),
        _ => None,
    }
}

fn require_administrator(command: &str) -> Result<(), String> {
    if win_acl::is_elevated() {
        Ok(())
    } else {
        Err(format!("{command} must be run from an administrator prompt"))
    }
}

fn print(note: install::Note) {
    match note {
        Note::Info(text) => println!("{text}"),
        Note::Warning(text) => eprintln!("warning: {text}"),
    }
}

/// Register the service and start it, taking over from the old service and its
/// folder if they are there.
pub fn install(options: &Options) -> Result<(), String> {
    require_administrator("install")?;
    let mut services = WindowsServices::connect(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
    install::install(&mut services, &WindowsHost, &folders(), options, &mut print)
}

/// Stop the service if it is running, and remove it.
pub fn uninstall() -> Result<(), String> {
    require_administrator("uninstall")?;
    let mut services = WindowsServices::connect(ServiceManagerAccess::CONNECT)?;
    install::uninstall(&mut services, &folders(), &mut print)
}

/// The Windows service manager.
struct WindowsServices {
    manager: ServiceManager,
}

impl WindowsServices {
    fn connect(access: ServiceManagerAccess) -> Result<Self, String> {
        let manager = ServiceManager::local_computer(None::<&str>, access)
            .map_err(|error| format!("cannot open the service manager: {}", describe(error)))?;
        Ok(Self { manager })
    }

    fn open(&self, name: &str, access: ServiceAccess) -> Result<Service, String> {
        self.manager
            .open_service(name, access)
            .map_err(|error| format!("cannot open the {name} service: {}", describe(error)))
    }
}

impl Services for WindowsServices {
    fn query(&mut self, name: &str) -> Result<Option<Registered>, String> {
        let access = ServiceAccess::QUERY_CONFIG | ServiceAccess::QUERY_STATUS;
        let service = match self.manager.open_service(name, access) {
            Ok(service) => service,
            Err(error) if os_error(&error) == Some(ERROR_SERVICE_DOES_NOT_EXIST) => return Ok(None),
            Err(error) => return Err(format!("cannot open the {name} service: {}", describe(error))),
        };
        let failed = |error| format!("cannot query the {name} service: {}", describe(error));
        let config = service.query_config().map_err(failed)?;
        let status = service.query_status().map_err(failed)?;
        Ok(Some(Registered {
            // The crate calls it a path, but it is the whole command line.
            command_line: config.executable_path.into_os_string(),
            stopped: status.current_state == ServiceState::Stopped,
        }))
    }

    fn stop(&mut self, name: &str) -> Result<bool, String> {
        let service = self.open(name, ServiceAccess::QUERY_STATUS | ServiceAccess::STOP)?;
        let state = |service: &Service| service.query_status().map(|status| status.current_state);
        // Errors are ignored here: it may already be stopping. The wait decides.
        let _ = service.stop();
        let deadline = Instant::now() + STOP_WAIT;
        while !matches!(state(&service), Ok(ServiceState::Stopped)) {
            if Instant::now() >= deadline {
                return Ok(false);
            }
            sleep(Duration::from_millis(250));
        }
        Ok(true)
    }

    fn delete(&mut self, name: &str) -> Result<(), String> {
        self.open(name, ServiceAccess::DELETE)?
            .delete()
            .map_err(|error| format!("cannot remove the {name} service: {}", describe(error)))
    }

    fn create(&mut self, executable: &Path, arguments: &[OsString]) -> Result<(), String> {
        let info = ServiceInfo {
            name: OsString::from(SERVICE_NAME),
            display_name: OsString::from(DISPLAY_NAME),
            service_type: SERVICE_TYPE,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            // The crate quotes the path and every argument for the command line.
            executable_path: executable.to_path_buf(),
            launch_arguments: arguments.to_vec(),
            dependencies: vec![],
            account_name: None, // LocalSystem, which can read the restricted token.
            account_password: None,
        };
        let service = self
            .manager
            .create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::QUERY_CONFIG)
            .map_err(|error| match os_error(&error) {
                Some(ERROR_SERVICE_EXISTS) => {
                    format!("the {SERVICE_NAME} service is already installed; run uninstall first")
                }
                _ => format!("cannot create the service: {}", describe(error)),
            })?;

        if let Err(error) = service.set_description(DESCRIPTION) {
            eprintln!("warning: cannot set the service description: {}", describe(error));
        }
        // A native crash cannot be caught in-process, so let the manager restart it.
        let restart = ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(5) };
        let recovery = ServiceFailureActions {
            reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(86_400)),
            reboot_msg: None,
            command: None,
            actions: Some(vec![restart.clone(), restart]),
        };
        if let Err(error) = service.update_failure_actions(recovery) {
            eprintln!("warning: cannot set the restart policy: {}", describe(error));
        }
        if let Ok(config) = service.query_config() {
            println!("Service command: {}", config.executable_path.display());
        }
        Ok(())
    }

    fn start(&mut self, name: &str) -> Result<(), String> {
        self.open(name, ServiceAccess::START)?.start::<&OsStr>(&[]).map_err(describe)
    }
}

/// This machine's files, permissions and token.
struct WindowsHost;

impl Host for WindowsHost {
    fn prepare_launch(&self, options: &Options) -> Result<(PathBuf, Options), String> {
        // Real paths, because the service must not depend on a link someone could
        // later point elsewhere. SYSTEM will run what is there, so it must be protected.
        let current =
            std::env::current_exe().map_err(|error| format!("cannot find this executable's path: {error}"))?;
        let executable = win_acl::real_path(&current)?;
        let model_directory = win_acl::real_path(&recognizer::resolve_model_directory(options.model.as_deref())?)?;
        let model_files = recognizer::ModelFiles::locate(&model_directory)?;
        win_acl::verify_install_location(&executable, &model_files.paths())?;

        let persisted = Options { model: options.model.as_ref().map(|_| model_directory.clone()), ..options.clone() };
        Ok((executable, persisted))
    }

    fn prepare_data_folder(&self, folder: &Path) -> Result<(), String> {
        prepare_directory(folder)
    }

    fn check_old_folder(&self, folder: &Path) -> Result<(), String> {
        win_acl::check_trusted_directory(folder)
    }

    fn resolve_token(&self, custom: Option<&Path>) -> Result<PathBuf, String> {
        token::resolve(custom, Readers::AdministratorsAndSystem).map(|(path, _token)| path)
    }
}

define_windows_service!(ffi_service_main, service_main);

/// Hand this process to the service manager. Blocks until the service stops.
pub fn run_as_service(options: Options) -> Result<(), String> {
    let _ = SERVICE_OPTIONS.set(options);
    service_dispatcher::start(SERVICE_NAME, ffi_service_main).map_err(|error| {
        format!(
            "could not connect to the service manager ({}). This command is for the service manager only; \
             use `swifty-speech run` in a console.",
            describe(error)
        )
    })
}

// The Windows-supplied arguments are the ones given to StartService. The
// persisted options are on the process command line and were parsed in main.
fn service_main(_arguments: Vec<OsString>) {
    // The log lives in this directory, so nothing is written until it has passed
    // its checks. If it does not, install reported why when it ran the same
    // checks, and the service manager records the exit code.
    let folders = folders();
    let prepared = prepare_directory(&folders.data);
    let logger = if prepared.is_ok() { Logger::file(folders.log()) } else { Logger::discard() };
    crate::install_panic_hook(logger.clone());
    if let Err(message) = service_body(&logger, prepared) {
        logger.info(&format!("service stopped with an error: {message}"));
    }
}

fn service_body(logger: &Logger, prepared: Result<(), String>) -> Result<(), String> {
    let (stop_sender, mut stop_receiver) = tokio::sync::watch::channel(false);

    let status_handle = service_control_handler::register(SERVICE_NAME, move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            if let Some(handle) = STATUS_HANDLE.get() {
                let _ = set_state(handle, ServiceState::StopPending, ServiceControlAccept::empty(), 0);
            }
            let _ = stop_sender.send(true);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })
    .map_err(describe)?;
    let _ = STATUS_HANDLE.set(status_handle);

    // Reported before the model loads: loading must not count against the
    // manager's start timeout, and /healthz says when the model is ready.
    set_state(&status_handle, ServiceState::Running, ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN, 0)?;

    let result = prepared.and_then(|()| {
        let options = SERVICE_OPTIONS.get().cloned().unwrap_or_default();
        let shutdown = async move {
            let _ = stop_receiver.wait_for(|stopping| *stopping).await;
        };
        serve_until(&options, Readers::AdministratorsAndSystem, logger.clone(), BIND_WAIT, shutdown)
    });

    let exit_code = u32::from(result.is_err());
    set_state(&status_handle, ServiceState::Stopped, ServiceControlAccept::empty(), exit_code)?;
    result
}

/// Make the data directory private and clear out a log file left there by
/// anyone else, before anything running as SYSTEM writes to it.
fn prepare_directory(directory: &Path) -> Result<(), String> {
    win_acl::ensure_private_directory(directory)?;
    win_acl::remove_unless_protected(&directory.join(install::LOG_FILE))
}

fn set_state(
    handle: &ServiceStatusHandle,
    state: ServiceState,
    accepted: ServiceControlAccept,
    exit_code: u32,
) -> Result<(), String> {
    handle
        .set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: state,
            controls_accepted: accepted,
            exit_code: if exit_code == 0 {
                ServiceExitCode::Win32(0)
            } else {
                ServiceExitCode::ServiceSpecific(exit_code)
            },
            checkpoint: u32::from(state == ServiceState::StopPending),
            wait_hint: if state == ServiceState::StopPending { STOP_WAIT_HINT } else { Duration::ZERO },
            process_id: None,
        })
        .map_err(describe)
}
