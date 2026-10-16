//! Unattended access (M3): TLP-Desk as a Windows service.
//!
//! # Process model
//!
//! ```text
//!  session 0 (SYSTEM)                         active console session (1, 2, ...)
//! +-------------------------------+          +-----------------------------------+
//! | TLP-Desk.exe --service        |  launch  | TLP-Desk.exe --agent (SYSTEM      |
//! |  owns UDP 47400, runs `Host`  | -------> |   token, winsta0\default)         |
//! |  session tracker, supervisor  |  pipes   |  capture + input on the current   |
//! |  SendSAS (Ctrl+Alt+Del)       | <======> |  input desktop (Default/Winlogon) |
//! +-------------------------------+          +-----------------------------------+
//!          ^  GUI pipe (interactive users, console session only)
//!          |
//!  TLP-Desk.exe (GUI, the logged-in user): shows ID, code, Accept prompt, banner
//! ```
//!
//! * The service is installed with `TLP-Desk.exe install-service` (elevated) as
//!   [`SERVICE_NAME`], auto-start, LocalSystem.
//! * Services cannot see the user's desktop (session 0 isolation), so the service starts a
//!   **session agent** in the active console session with a duplicate of its own SYSTEM token
//!   (SYSTEM is needed to open the Winlogon desktop: lock screen, login screen, UAC prompts).
//!   The agent follows session changes (logon, logoff, lock, fast user switching).
//! * The agent serves capture and input over named pipes restricted to SYSTEM; the host's
//!   capturer/injector factories in the service are proxies to it.
//! * Ctrl+Alt+Del is sent by the service with `SendSAS`; the `SoftwareSASGeneration` policy
//!   must allow services (set at install).
//!
//! * **User-session features** (IPC v2): the clipboard, Explorer paste, the whiteboard
//!   overlay, the pointer marker, the local-input hooks, the privacy shield and loopback audio
//!   must run on the user's desktop, so the agent owns them ([`FeatureHost`] around the app's
//!   [`AgentFeatures`]) and the service's session features use proxies
//!   ([`AgentFeatureHub`]). Host-side windows (transfers, chat, conflict questions) are shown
//!   by the user's GUI through app messages on the GUI pipe.
//!
//! The pure logic lives in small, unit-tested modules (IPC framing in [`ipc`]); Windows calls
//! are confined to the platform module.

mod agent;
mod agent_features;
mod client;
mod desktop;
mod extension;
mod feature_proxy;
mod gui;
pub mod ipc;
mod pipe_security;
mod remote;
mod session;
mod supervisor;
mod system;
#[cfg(test)]
mod testutil;
#[cfg(windows)]
#[allow(unsafe_code)]
pub mod windows;

pub use agent::{
    serve_connection, AgentPlatform, DESKTOP_CHECK_EVERY, DESKTOP_POLL_EVERY, MAX_FRAME_WAIT,
};
pub use agent_features::{
    AgentFeatures, AudioSource, FeatureHost, NoFeatures, StageLink, ACTIVITY_POLL, AUDIO_KEEPALIVE,
    MAX_FEATURE_OBJECTS,
};
pub use client::{gui_command_on, gui_subscribe_on, GuiSubscription};
pub use extension::{ConsoleUser, ServiceCommands, ServiceContext, ServiceExtension};
pub use feature_proxy::{
    AgentActivity, AgentAudio, AgentFeatureHub, AgentMarker, AgentOverlay, AgentShield,
    StageHandler, OVERLAY_POINTS_PER_REQUEST,
};
pub use gui::{
    route_prompt, serve_gui_connection, GuiCommands, GuiHub, PromptRoute, GUI_HEARTBEAT,
};
pub use remote::{AgentConn, AgentConnector, AgentLink, RemoteCapturer, RemoteInjector};

pub use desktop::{
    is_secure_desktop, DesktopChange, DesktopFollower, DesktopSource, DesktopStatus,
    DesktopWatcher, DEFAULT_DESKTOP, INACCESSIBLE_DESKTOP,
};
pub use pipe_security::{
    agent_pipe_name, check_gui_client, is_agent_pipe_name, same_image, GuiClientFacts,
    GuiClientRefusal, PipeSpec, AGENT_PIPE_SDDL, GUI_CLIENT_ACCESS, GUI_PIPE_NAME, GUI_PIPE_SDDL,
    MEDIUM_INTEGRITY_RID,
};
pub use session::{
    console_session, SessionAction, SessionChange, SessionTracker, NO_CONSOLE_SESSION,
};
pub use supervisor::{
    AgentLauncher, AgentProcess, AgentSupervisor, RestartPolicy, SupervisorEvent,
};
pub use system::{
    SystemAction, SystemActions, SystemControl, SystemEvent, MIN_ACTION_INTERVAL, SYSTEM_FEATURE,
};

/// Service name in the Service Control Manager (`sc query TLP-Desk`).
pub const SERVICE_NAME: &str = "TLP-Desk";

/// Command-line switch that makes `TLP-Desk.exe` run as the service (set by the installer).
pub const SERVICE_ARG: &str = "--service";

/// Command-line switch of the session agent process (started by the service only).
pub const AGENT_ARG: &str = "--agent";

/// CLI subcommand that installs and starts the service (elevated).
pub const INSTALL_SUBCOMMAND: &str = "install-service";

/// CLI subcommand that stops and removes the service (elevated).
pub const UNINSTALL_SUBCOMMAND: &str = "uninstall-service";
