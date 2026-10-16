//! Windows platform code of the service: pipes, sessions and process launch, desktops,
//! `SendSAS`, the Service Control Manager, the service main loop and the agent process. The
//! only place in this crate with `unsafe`; every block has a `// SAFETY:` comment. Sources:
//! Microsoft Learn only.
//!
//! Entry points for `TLP-Desk.exe`:
//! * `--service` -> [`run_dispatcher`] (only the SCM starts it);
//! * `--agent --pipe <name> --parent <pid>` -> [`run_agent`] (only the service starts it);
//! * `install-service` / `uninstall-service` -> [`install_service`] / [`uninstall_service`];
//! * the GUI -> [`service_running`], [`gui_subscribe`], [`gui_command`].

mod agent_main;
mod desktop;
mod folders;
mod launch;
mod pipe;
mod run;
mod sas;
mod scm;

use std::fs::File;

pub use agent_main::run_agent;
pub use desktop::WinDesktopSource;
pub use folders::{
    allow_interactive_com_calls, console_user, print_to_parent_console, set_file_dacl,
    SETTINGS_FILE_SDDL,
};
pub use launch::{
    active_console_session, current_session_id, launch, user_logged_on, wait_for_exit, LaunchAs,
    WinAgentLauncher, WinProcess,
};
pub use run::{run_service, ControlEvent, ServiceOptions};
pub use sas::{
    enable_software_sas_for_services, lock_workstation, sas_policy_value, send_sas, SAS_POLICY_KEY,
    SAS_POLICY_VALUE,
};
pub use scm::{install_service, run_dispatcher, service_running, uninstall_service, ServiceTexts};

use super::ipc::{GuiCommand, GuiReply, ServiceStatus};
use super::{gui_command_on, gui_subscribe_on, GuiSubscription, GUI_CLIENT_ACCESS, GUI_PIPE_NAME};
use crate::HostError;

/// Maps a `windows` crate error to [`HostError::Os`].
pub(crate) fn os_err(call: &'static str, e: &windows::core::Error) -> HostError {
    HostError::Os {
        call,
        code: e.code().0,
    }
}

/// Opens the GUI pipe and checks that its server runs in session 0 (only services do), so a
/// user process that grabbed the name cannot pose as the service.
fn gui_connect() -> Result<File, HostError> {
    let f = pipe::connect(GUI_PIPE_NAME, Some(GUI_CLIENT_ACCESS))?;
    let session = pipe::server_session_id(&f)?;
    if session != 0 {
        tracing::error!(session, "GUI pipe is not served by a service; ignoring it");
        return Err(HostError::PipeImpostor {
            expected: 0,
            actual: session,
        });
    }
    Ok(f)
}

/// Subscribes to the running service's host status (GUI side).
pub fn gui_subscribe() -> Result<(ServiceStatus, GuiSubscription<File>), HostError> {
    gui_subscribe_on(gui_connect()?)
}

/// Sends one command to the running service (GUI side).
pub fn gui_command(cmd: &GuiCommand) -> Result<GuiReply, HostError> {
    gui_command_on(gui_connect()?, cmd)
}
