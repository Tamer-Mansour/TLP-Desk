//! `TLP-Desk.exe --agent --pipe <name> --parent <service pid>`: the session agent process.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tlp_desk_capture::{CaptureBackend, CaptureError, CaptureOptions, MonitorInfo, ScreenCapturer};
use tlp_desk_input::{InputError, InputInjector, MonitorRect};

use super::desktop::WinDesktopSource;
use super::launch::{current_session_id, wait_for_exit};
use super::{pipe, sas};
use crate::service::{
    is_agent_pipe_name, serve_connection, AgentPlatform, DesktopSource, PipeSpec,
};
use crate::HostError;

/// The real OS for the agent.
#[derive(Debug)]
struct WinAgentPlatform {
    session: u32,
}

impl AgentPlatform for WinAgentPlatform {
    fn session_id(&self) -> u32 {
        self.session
    }

    fn open_capturer(
        &self,
        monitor: u32,
        max_size: Option<(u32, u32)>,
        secure_desktop: bool,
    ) -> Result<Box<dyn ScreenCapturer>, CaptureError> {
        // Windows.Graphics.Capture does not work on the Winlogon desktop; DXGI desktop
        // duplication does when the process runs as SYSTEM in the session.
        let backend = secure_desktop.then_some(CaptureBackend::DxgiDuplication);
        tlp_desk_capture::open_capturer_opts(monitor, CaptureOptions { backend, max_size })
    }

    fn list_monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError> {
        tlp_desk_capture::list_monitors()
    }

    fn open_injector(&self, rect: MonitorRect) -> Result<Box<dyn InputInjector>, InputError> {
        tlp_desk_input::injector_for_monitor(rect)
    }

    fn lock_workstation(&self) -> Result<(), HostError> {
        sas::lock_workstation()
    }

    fn desktop_source(&self) -> Box<dyn DesktopSource> {
        Box::new(WinDesktopSource::new())
    }
}

/// Runs the agent until the service tells it to exit, the service process ends, or the pipe
/// cannot be served. `pipe_name` must be a well-formed agent pipe name.
pub fn run_agent(pipe_name: &str, parent_pid: u32) -> Result<(), HostError> {
    if !is_agent_pipe_name(pipe_name) {
        return Err(HostError::IpcRefused(format!(
            "not an agent pipe name: {pipe_name:?}"
        )));
    }
    if let Err(e) = tlp_desk_input::ensure_dpi_aware() {
        tracing::warn!(%e, "agent could not enable per-monitor DPI awareness");
    }
    let session = current_session_id()?;
    tracing::info!(session, %pipe_name, parent_pid, "session agent starting");

    // Never outlive the service (it may be killed without telling us).
    std::thread::Builder::new()
        .name("tlp-agent-parent".into())
        .spawn(move || {
            wait_for_exit(parent_pid);
            tracing::info!("service process ended; agent exiting");
            std::process::exit(0);
        })?;

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        std::thread::Builder::new()
            .name("tlp-agent-stop".into())
            .spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(100));
                }
                tracing::info!("agent exiting on request");
                // Give the Exit reply a moment to reach the service.
                std::thread::sleep(Duration::from_millis(100));
                std::process::exit(0);
            })?;
    }

    let platform: Arc<WinAgentPlatform> = Arc::new(WinAgentPlatform { session });
    let spec = PipeSpec {
        name: pipe_name.to_owned(),
        ..PipeSpec::agent(0, 0, 0)
    };
    let mut first = true;
    loop {
        let mut conn = pipe::create_instance(&spec, first)?;
        first = false;
        pipe::accept(&conn)?;
        let platform = platform.clone();
        let stop = stop.clone();
        std::thread::Builder::new()
            .name("tlp-agent-conn".into())
            .spawn(move || {
                if let Err(e) = serve_connection(&mut conn, &*platform, &stop) {
                    tracing::debug!(%e, "agent connection ended with an error");
                }
            })?;
    }
}
