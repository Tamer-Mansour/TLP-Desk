//! Service Control Manager: install, uninstall and the service entry point (`windows-service`
//! crate; Microsoft Learn: "Service Control Manager", `HandlerEx`, `SERVICE_CONTROL_SESSIONCHANGE`).

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::mpsc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows_service::service::{
    ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept,
    ServiceErrorControl, ServiceExitCode, ServiceFailureActions, ServiceFailureResetPeriod,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType, SessionChangeReason,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

use super::run::{run_service, ControlEvent, ServiceOptions};
use super::sas;
use crate::service::{SessionChange, SERVICE_ARG, SERVICE_NAME};
use crate::HostError;

/// Win32 error when the service does not exist.
const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
/// Win32 error when the service is already running.
const ERROR_SERVICE_ALREADY_RUNNING: i32 = 1056;
/// Win32 error when the service is not running.
const ERROR_SERVICE_NOT_ACTIVE: i32 = 1062;

fn svc_err(call: &'static str, e: windows_service::Error) -> HostError {
    let code = match &e {
        windows_service::Error::Winapi(io) => io.raw_os_error().unwrap_or(-1),
        _ => -1,
    };
    HostError::Os { call, code }
}

fn raw_code(e: &windows_service::Error) -> Option<i32> {
    match e {
        windows_service::Error::Winapi(io) => io.raw_os_error(),
        _ => None,
    }
}

/// User-visible service texts (localized by the caller from the i18n table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceTexts {
    /// Shown in services.msc ("TLP-Desk Service").
    pub display_name: String,
    pub description: String,
}

/// Installs (or updates) and starts the service: LocalSystem, automatic start, restart on
/// failure, `exe --service`. Also allows services to send Ctrl+Alt+Del (SoftwareSASGeneration
/// policy). Needs an elevated caller.
pub fn install_service(exe: &Path, texts: &ServiceTexts) -> Result<(), HostError> {
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .map_err(|e| svc_err("OpenSCManagerW", e))?;
    let info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(&texts.display_name),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe.to_path_buf(),
        launch_arguments: vec![OsString::from(SERVICE_ARG)],
        dependencies: vec![],
        // None = LocalSystem.
        account_name: None,
        account_password: None,
    };
    let access = ServiceAccess::QUERY_STATUS
        | ServiceAccess::START
        | ServiceAccess::CHANGE_CONFIG
        | ServiceAccess::QUERY_CONFIG;
    let service = match manager.open_service(SERVICE_NAME, access) {
        Ok(s) => {
            s.change_config(&info)
                .map_err(|e| svc_err("ChangeServiceConfigW", e))?;
            tracing::info!("service configuration updated");
            s
        }
        Err(e) if raw_code(&e) == Some(ERROR_SERVICE_DOES_NOT_EXIST) => {
            let s = manager
                .create_service(&info, access)
                .map_err(|e| svc_err("CreateServiceW", e))?;
            tracing::info!("service created");
            s
        }
        Err(e) => return Err(svc_err("OpenServiceW", e)),
    };
    service
        .set_description(&texts.description)
        .map_err(|e| svc_err("ChangeServiceConfig2W(description)", e))?;
    // Restart after 5 s, 10 s, then every 60 s; the failure count resets after a day.
    let restart = |s| ServiceAction {
        action_type: ServiceActionType::Restart,
        delay: Duration::from_secs(s),
    };
    service
        .update_failure_actions(ServiceFailureActions {
            reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 3600)),
            reboot_msg: None,
            command: None,
            actions: Some(vec![restart(5), restart(10), restart(60)]),
        })
        .map_err(|e| svc_err("ChangeServiceConfig2W(failure actions)", e))?;
    if let Err(e) = service.set_failure_actions_on_non_crash_failures(true) {
        tracing::debug!(%e, "cannot enable failure actions on non-crash failures");
    }
    match sas::enable_software_sas_for_services() {
        Ok(v) => tracing::info!(value = v, "Ctrl+Alt+Del from the service is allowed"),
        Err(e) => {
            tracing::warn!(%e, "cannot set the SoftwareSASGeneration policy; Ctrl+Alt+Del may not work")
        }
    }
    match service.start(&[] as &[&OsStr]) {
        Ok(()) => tracing::info!("service started"),
        Err(e) if raw_code(&e) == Some(ERROR_SERVICE_ALREADY_RUNNING) => {}
        Err(e) => return Err(svc_err("StartServiceW", e)),
    }
    Ok(())
}

/// Stops and deletes the service. Not installed is success. Needs an elevated caller.
pub fn uninstall_service() -> Result<(), HostError> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|e| svc_err("OpenSCManagerW", e))?;
    let access = ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE;
    let service = match manager.open_service(SERVICE_NAME, access) {
        Ok(s) => s,
        Err(e) if raw_code(&e) == Some(ERROR_SERVICE_DOES_NOT_EXIST) => return Ok(()),
        Err(e) => return Err(svc_err("OpenServiceW", e)),
    };
    match service.stop() {
        Ok(_) => {}
        Err(e) if raw_code(&e) == Some(ERROR_SERVICE_NOT_ACTIVE) => {}
        Err(e) => tracing::warn!(%e, "stopping the service failed; deleting anyway"),
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        match service.query_status() {
            Ok(s) if s.current_state == ServiceState::Stopped => break,
            Ok(_) => std::thread::sleep(Duration::from_millis(200)),
            Err(_) => break,
        }
    }
    service.delete().map_err(|e| svc_err("DeleteService", e))?;
    tracing::info!("service removed");
    Ok(())
}

/// Whether the service is installed and running (the GUI then talks to it instead of
/// hosting itself).
pub fn service_running() -> bool {
    let Ok(manager) = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
    else {
        return false;
    };
    let Ok(service) = manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS) else {
        return false;
    };
    service
        .query_status()
        .is_ok_and(|s| s.current_state == ServiceState::Running)
}

/// Builds the service options when the SCM starts the service (set by [`run_dispatcher`]).
static OPTIONS: OnceLock<fn() -> ServiceOptions> = OnceLock::new();

define_windows_service!(ffi_service_main, service_main);

/// Hands the process to the Service Control Manager (call from `main` for `--service`);
/// returns when the service stopped. `options` is called once the service starts (load
/// settings there, not before: the SCM wants the dispatcher within 30 s).
pub fn run_dispatcher(options: fn() -> ServiceOptions) -> Result<(), HostError> {
    let _ = OPTIONS.set(options);
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
        .map_err(|e| svc_err("StartServiceCtrlDispatcherW", e))
}

fn session_change(reason: SessionChangeReason) -> SessionChange {
    match reason {
        SessionChangeReason::ConsoleConnect => SessionChange::ConsoleConnect,
        SessionChangeReason::ConsoleDisconnect => SessionChange::ConsoleDisconnect,
        SessionChangeReason::RemoteConnect => SessionChange::RemoteConnect,
        SessionChangeReason::RemoteDisconnect => SessionChange::RemoteDisconnect,
        SessionChangeReason::SessionLogon => SessionChange::Logon,
        SessionChangeReason::SessionLogoff => SessionChange::Logoff,
        SessionChangeReason::SessionLock => SessionChange::Lock,
        SessionChangeReason::SessionUnlock => SessionChange::Unlock,
        SessionChangeReason::SessionRemoteControl => SessionChange::RemoteControl,
        SessionChangeReason::SessionCreate => SessionChange::Create,
        SessionChangeReason::SessionTerminate => SessionChange::Terminate,
    }
}

fn status(state: ServiceState, accept: ServiceControlAccept, code: u32) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: accept,
        exit_code: ServiceExitCode::Win32(code),
        checkpoint: 0,
        wait_hint: Duration::from_secs(10),
        process_id: None,
    }
}

fn service_main(_arguments: Vec<OsString>) {
    let (tx, rx) = mpsc::channel::<ControlEvent>();
    let handler = move |control: ServiceControl| -> ServiceControlHandlerResult {
        match control {
            ServiceControl::Stop | ServiceControl::Shutdown | ServiceControl::Preshutdown => {
                let _ = tx.send(ControlEvent::Stop);
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::SessionChange(p) => {
                let _ = tx.send(ControlEvent::Session(
                    session_change(p.reason),
                    p.notification.session_id,
                ));
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };
    let handle = match service_control_handler::register(SERVICE_NAME, handler) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(%e, "cannot register the service control handler");
            return;
        }
    };
    let running = ServiceControlAccept::STOP
        | ServiceControlAccept::SHUTDOWN
        | ServiceControlAccept::SESSION_CHANGE;
    if let Err(e) = handle.set_service_status(status(ServiceState::Running, running, 0)) {
        tracing::error!(%e, "cannot report the running state");
    }
    let code = match OPTIONS.get() {
        Some(options) => match run_service(options(), &rx) {
            Ok(()) => 0,
            Err(e) => {
                tracing::error!(%e, "service failed");
                1
            }
        },
        None => {
            tracing::error!("service started without options");
            1
        }
    };
    if let Err(e) = handle.set_service_status(status(
        ServiceState::Stopped,
        ServiceControlAccept::empty(),
        code,
    )) {
        tracing::error!(%e, "cannot report the stopped state");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_session_change_reason_maps() {
        use SessionChangeReason as R;
        let all = [
            (R::ConsoleConnect, SessionChange::ConsoleConnect),
            (R::ConsoleDisconnect, SessionChange::ConsoleDisconnect),
            (R::RemoteConnect, SessionChange::RemoteConnect),
            (R::RemoteDisconnect, SessionChange::RemoteDisconnect),
            (R::SessionLogon, SessionChange::Logon),
            (R::SessionLogoff, SessionChange::Logoff),
            (R::SessionLock, SessionChange::Lock),
            (R::SessionUnlock, SessionChange::Unlock),
            (R::SessionRemoteControl, SessionChange::RemoteControl),
            (R::SessionCreate, SessionChange::Create),
            (R::SessionTerminate, SessionChange::Terminate),
        ];
        for (raw, want) in all {
            assert_eq!(session_change(raw), want);
        }
    }

    /// Installs, starts, stops and removes the real service. Never run on a developer PC
    /// without meaning to: needs an elevated prompt and changes the system.
    #[test]
    #[ignore = "installs a Windows service and sets a policy; elevated manual test only"]
    fn install_and_uninstall_roundtrip() {
        let exe = std::env::current_exe().unwrap();
        install_service(
            &exe,
            &ServiceTexts {
                display_name: "TLP-Desk Service (test)".into(),
                description: "test".into(),
            },
        )
        .unwrap();
        uninstall_service().unwrap();
    }
}
