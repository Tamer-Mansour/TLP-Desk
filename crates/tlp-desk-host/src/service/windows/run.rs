//! The service's main loop: owns the endpoint and the [`Host`], keeps the session agent in the
//! console session, forwards desktop state to viewers and host status to the GUI.

use std::fs::File;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tlp_desk_crypto::Identity;
use tlp_desk_net::{Endpoint, EndpointConfig, DEFAULT_PORT};
use tokio::sync::watch;

use super::folders::console_user;
use super::launch::{active_console_session, launch, user_logged_on, LaunchAs, WinAgentLauncher};
use super::{pipe, sas};
use crate::service::ipc::{self, AgentChannel, AgentEvent, WireDesktop};
use crate::service::{
    agent_pipe_name, check_gui_client, route_prompt, serve_gui_connection, AgentConn,
    AgentConnector, AgentFeatureHub, AgentLink, AgentSupervisor, CredentialFiles, DesktopStatus,
    GuiClientFacts, GuiClientRefusal, GuiHub, PipeSpec, PromptRoute, RestartPolicy,
    ServiceCommands, ServiceContext, ServiceExtension, SessionAction, SessionChange,
    SessionTracker, SupervisorEvent, SystemActions, SystemControl, NO_CONSOLE_SESSION,
};
use crate::{Host, HostConfig, HostError, HostEvent, HostEvents, MAX_CAPTURE_SIZE};

/// What the SCM handler tells the main loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlEvent {
    Stop,
    /// `SERVICE_CONTROL_SESSIONCHANGE`: reason and session id.
    Session(SessionChange, u32),
}

/// How the service runs; built by the app when the SCM starts the service.
#[derive(Clone)]
pub struct ServiceOptions {
    /// Host settings (device name, permissions, password verifier, features...). The capture,
    /// input and monitor factories are replaced by the agent proxies; the `system` feature is
    /// added.
    pub host: HostConfig,
    /// Identity directory (`%ProgramData%\TLP-Desk`).
    pub data_dir: PathBuf,
    /// UDP bind address (default `[::]:47400`).
    pub bind: SocketAddr,
    /// `TLP-Desk.exe` (started as the agent and, for Accept prompts, as the user's GUI).
    pub exe: PathBuf,
    /// The app's session features, settings and GUI messages (see [`ServiceExtension`]).
    pub extension: Option<Arc<dyn ServiceExtension>>,
}

impl std::fmt::Debug for ServiceOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceOptions")
            .field("host", &self.host)
            .field("data_dir", &self.data_dir)
            .field("bind", &self.bind)
            .field("exe", &self.exe)
            .field("extension", &self.extension.is_some())
            .finish()
    }
}

impl Default for ServiceOptions {
    fn default() -> Self {
        Self {
            host: HostConfig::default(),
            data_dir: tlp_desk_crypto::default_dir(),
            bind: SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), DEFAULT_PORT),
            exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("TLP-Desk.exe")),
            extension: None,
        }
    }
}

/// How often the supervisor checks the agent.
const TICK: Duration = Duration::from_millis(500);
/// How long the service waits for a new agent's pipe.
const AGENT_STARTUP: Duration = Duration::from_secs(15);
/// Least time between two GUI launches for Accept prompts.
const GUI_LAUNCH_INTERVAL: Duration = Duration::from_secs(30);

/// Connects to one agent's pipe and checks that its server is that agent's process.
#[derive(Debug)]
struct PipeConnector {
    name: String,
    pid: u32,
}

impl AgentConnector for PipeConnector {
    fn connect(&self) -> Result<Box<dyn AgentConn>, HostError> {
        let f: File = pipe::connect(&self.name, None)?;
        let server = pipe::server_process_id(&f)?;
        if server != self.pid {
            tracing::error!(
                expected = self.pid,
                actual = server,
                "agent pipe served by another process; refusing"
            );
            return Err(HostError::PipeImpostor {
                expected: self.pid,
                actual: server,
            });
        }
        Ok(Box::new(f))
    }
}

/// Ctrl+Alt+Del from the service, lock through the agent.
#[derive(Debug)]
struct WinSystemActions {
    link: Arc<AgentLink>,
}

impl SystemActions for WinSystemActions {
    fn send_ctrl_alt_del(&self) -> Result<(), HostError> {
        sas::send_sas()
    }
    fn lock_workstation(&self) -> Result<(), HostError> {
        self.link.lock_workstation()
    }
}

/// Everything the loop needs to act on session and supervisor events.
struct Runner {
    link: Arc<AgentLink>,
    supervisor: AgentSupervisor<WinAgentLauncher>,
    tracker: SessionTracker,
    desktop: watch::Sender<DesktopStatus>,
    hub: Arc<GuiHub>,
    features: Arc<AgentFeatureHub>,
    extension: Option<Arc<dyn ServiceExtension>>,
    console: Arc<AtomicU32>,
    /// The console user last reported to the extension.
    console_user: Option<u32>,
    generation: Arc<AtomicU64>,
    service_pid: u32,
}

fn wire(d: DesktopStatus) -> WireDesktop {
    WireDesktop {
        secure_desktop: d.secure_desktop,
        locked: d.locked,
    }
}

impl Runner {
    fn apply(&mut self, actions: Vec<SessionAction>) {
        let now = Instant::now();
        for a in actions {
            tracing::info!(?a, "session action");
            let events = match a {
                SessionAction::StartAgent(s) => {
                    self.console.store(s, Ordering::Release);
                    self.link.request_exit();
                    self.supervisor.set_target(Some(s), now)
                }
                SessionAction::RestartAgent(_) => {
                    self.link.request_exit();
                    self.supervisor.restart(now)
                }
                SessionAction::StopAgent => {
                    self.console.store(NO_CONSOLE_SESSION, Ordering::Release);
                    self.link.request_exit();
                    self.supervisor.set_target(None, now)
                }
                SessionAction::LockChanged(locked) => {
                    self.desktop.send_modify(|d| d.locked = locked);
                    self.hub.set_desktop(wire(*self.desktop.borrow()));
                    Vec::new()
                }
            };
            self.handle(events);
        }
        self.refresh_console_user();
    }

    /// Tells the extension who is logged on at the console (their folders), when it changed.
    fn refresh_console_user(&mut self) {
        let session = self.console.load(Ordering::Acquire);
        let user = (session != NO_CONSOLE_SESSION)
            .then(|| console_user(session))
            .flatten();
        let key = user.as_ref().map(|u| u.session);
        if key == self.console_user {
            return;
        }
        self.console_user = key;
        tracing::info!(session = ?key, "console user changed");
        if let Some(ext) = &self.extension {
            ext.console_user(user);
        }
    }

    fn handle(&mut self, events: Vec<SupervisorEvent>) {
        for ev in events {
            match ev {
                SupervisorEvent::Started { session, pid, seq } => {
                    tracing::info!(session, pid, seq, "agent running");
                    self.link.set(Some(Arc::new(PipeConnector {
                        name: agent_pipe_name(session, self.service_pid, seq),
                        pid,
                    })));
                    let gen = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
                    self.spawn_event_reader(gen);
                }
                SupervisorEvent::Exited {
                    session,
                    pid,
                    code,
                    retry_in,
                } => {
                    tracing::warn!(session, pid, code, ?retry_in, "agent exited; restarting");
                    self.link.set(None);
                    self.features.agent_changed();
                }
                SupervisorEvent::Stopped { session, pid } => {
                    tracing::info!(session, pid, "agent stopped");
                    self.link.set(None);
                    self.features.agent_changed();
                }
                SupervisorEvent::LaunchFailed {
                    session,
                    error,
                    retry_in,
                } => {
                    tracing::warn!(session, %error, ?retry_in, "cannot start the agent");
                }
            }
        }
    }

    /// Reads desktop switches from the new agent until it goes away.
    fn spawn_event_reader(&self, gen: u64) {
        let link = self.link.clone();
        let generation = self.generation.clone();
        let desktop = self.desktop.clone();
        let hub = self.hub.clone();
        let features = self.features.clone();
        let spawned = std::thread::Builder::new()
            .name("tlp-svc-agent-events".into())
            .spawn(move || {
                let deadline = Instant::now() + AGENT_STARTUP;
                let mut conn = loop {
                    if generation.load(Ordering::Acquire) != gen {
                        return;
                    }
                    match link.open(AgentChannel::Events) {
                        Ok((c, session)) => {
                            tracing::info!(session, "connected to the agent's desktop events");
                            break c;
                        }
                        Err(e) if Instant::now() >= deadline => {
                            tracing::warn!(%e, "agent events unavailable");
                            return;
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(200)),
                    }
                };
                // A new agent: its user-session features start from scratch.
                features.agent_changed();
                while let Ok(ev) = ipc::read_msg::<_, AgentEvent>(&mut conn) {
                    if generation.load(Ordering::Acquire) != gen {
                        return;
                    }
                    match ev {
                        AgentEvent::Desktop { name, secure } => {
                            tracing::info!(desktop = %name, secure, "viewer desktop state");
                            desktop.send_modify(|d| d.secure_desktop = secure);
                            hub.set_desktop(wire(*desktop.borrow()));
                        }
                        other => features.on_event(other),
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(%e, "cannot start the agent events thread");
        }
    }
}

/// Runs the service until [`ControlEvent::Stop`].
pub fn run_service(
    opts: ServiceOptions,
    control: &Receiver<ControlEvent>,
) -> Result<(), HostError> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("tlp-desk-service")
        .enable_all()
        .build()?;
    let identity = Arc::new(Identity::load_or_generate(&opts.data_dir)?);
    let device_name = opts.host.device_name.clone();
    let extension = opts.extension.clone();
    let credentials = CredentialFiles {
        data_dir: opts.data_dir.clone(),
        tickets: opts.host.reconnect_tickets.clone(),
    };
    let mut ep_cfg = EndpointConfig {
        bind: opts.bind,
        device_name: device_name.clone(),
        listen: true,
        ..EndpointConfig::default()
    };
    if let Some(ext) = &extension {
        ep_cfg = ext.endpoint_config(ep_cfg);
    }
    let endpoint = Arc::new(rt.block_on(async move { Endpoint::bind(identity, ep_cfg) })?);

    let link = AgentLink::new();
    let features = AgentFeatureHub::new(link.clone());
    let (desktop_tx, desktop_rx) = watch::channel(DesktopStatus::default());
    let system = SystemControl::new(
        Arc::new(WinSystemActions { link: link.clone() }),
        desktop_rx,
    );
    let mut cfg = HostConfig {
        capturer_factory: link.capturer_factory(Some(MAX_CAPTURE_SIZE)),
        injector_factory: link.injector_factory(),
        monitor_lister: link.monitor_lister(),
        // Low-level hooks and the marker window run in the agent, on the user's desktop.
        local_activity_factory: features.local_activity_factory(),
        pointer_marker_factory: features.pointer_marker_factory(),
        features: opts.host.features.clone().with(Arc::new(system)),
        ..opts.host
    };
    if let Some(ext) = &extension {
        cfg = ext.configure(&features, cfg);
    }
    let (host, events) = rt.block_on(Host::start(endpoint.clone(), cfg))?;
    let host = Arc::new(host);
    let hub = Arc::new(GuiHub::new(
        host.device_id().as_u32(),
        device_name,
        host.one_time_code().current(),
    ));
    let console = Arc::new(AtomicU32::new(NO_CONSOLE_SESSION));
    rt.spawn(pump_host_events(
        events,
        host.clone(),
        hub.clone(),
        console.clone(),
        opts.exe.clone(),
        extension.clone(),
    ));
    if let Some(ext) = &extension {
        ext.started(ServiceContext {
            host: host.clone(),
            endpoint,
            hub: hub.clone(),
            features: features.clone(),
        });
    }
    spawn_gui_server(
        hub.clone(),
        Arc::new(
            ServiceCommands::new(host.clone(), extension.clone()).with_credentials(credentials),
        ),
        opts.exe.clone(),
    );

    let service_pid = std::process::id();
    let mut runner = Runner {
        link: link.clone(),
        supervisor: AgentSupervisor::new(
            WinAgentLauncher {
                exe: opts.exe.clone(),
                service_pid,
            },
            RestartPolicy::default(),
        ),
        tracker: SessionTracker::new(),
        desktop: desktop_tx,
        hub,
        features,
        extension,
        console,
        console_user: None,
        generation: Arc::default(),
        service_pid,
    };
    tracing::info!(device_id = %host.device_id(), "service running");
    let start = runner.tracker.start(active_console_session());
    runner.apply(start);
    loop {
        match control.recv_timeout(TICK) {
            Ok(ControlEvent::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(ControlEvent::Session(change, session)) => {
                tracing::info!(?change, session, "session change");
                let actions = runner
                    .tracker
                    .on_change(change, session, active_console_session());
                runner.apply(actions);
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        let events = runner.supervisor.tick(Instant::now());
        runner.handle(events);
    }
    tracing::info!("service stopping");
    link.request_exit();
    let events = runner.supervisor.shutdown();
    runner.handle(events);
    host.shutdown();
    rt.shutdown_timeout(Duration::from_secs(2));
    Ok(())
}

async fn pump_host_events(
    mut events: HostEvents,
    host: Arc<Host>,
    hub: Arc<GuiHub>,
    console: Arc<AtomicU32>,
    exe: PathBuf,
    extension: Option<Arc<dyn ServiceExtension>>,
) {
    let last_launch: Arc<Mutex<Option<Instant>>> = Arc::default();
    while let Some(ev) = events.recv().await {
        hub.on_host_event(&ev, || host.one_time_code().current());
        if let Some(ext) = &extension {
            ext.on_host_event(&ev);
        }
        let HostEvent::IncomingRequest(req) = &ev else {
            continue;
        };
        let session = console.load(Ordering::Acquire);
        let session = (session != NO_CONSOLE_SESSION).then_some(session);
        let logged_on = match session {
            Some(s) => tokio::task::spawn_blocking(move || user_logged_on(s))
                .await
                .unwrap_or(false),
            None => false,
        };
        match (route_prompt(hub.has_gui(), logged_on), session) {
            (PromptRoute::ShowInGui, _) => {}
            (PromptRoute::LaunchGui, Some(s)) => {
                let due = {
                    let mut last = last_launch.lock().unwrap_or_else(|p| p.into_inner());
                    let due = last.is_none_or(|t| t.elapsed() >= GUI_LAUNCH_INTERVAL);
                    if due {
                        *last = Some(Instant::now());
                    }
                    due
                };
                if due {
                    let exe = exe.clone();
                    let res = tokio::task::spawn_blocking(move || {
                        launch(s, LaunchAs::User, &exe, &[]).map(|p| {
                            use crate::service::AgentProcess as _;
                            p.pid()
                        })
                    })
                    .await;
                    match res {
                        Ok(Ok(pid)) => {
                            tracing::info!(session = s, pid, "started the GUI for an Accept prompt")
                        }
                        Ok(Err(e)) => {
                            tracing::warn!(%e, "cannot start the GUI for an Accept prompt")
                        }
                        Err(e) => tracing::warn!(%e, "GUI launch task failed"),
                    }
                }
            }
            (PromptRoute::LaunchGui, None) | (PromptRoute::Reject, _) => {
                tracing::info!(
                    request = req.id.0,
                    "nobody can answer an Accept prompt (no user logged on); rejecting"
                );
                host.reject(req.id);
            }
        }
    }
}

/// Reads what the GUI admission check needs about the client of `pipe` (audit F2). Each
/// failed query leaves its fact `None`, which refuses the client.
fn gui_client_facts(pipe: &File) -> GuiClientFacts {
    let image = pipe::client_process_id(pipe)
        .and_then(pipe::process_image)
        .map_err(|e| tracing::warn!(%e, "cannot read the GUI client's image"))
        .ok();
    let integrity_rid = pipe::client_integrity_rid(pipe)
        .map_err(|e| tracing::warn!(%e, "cannot read the GUI client's integrity level"))
        .ok();
    let facts = GuiClientFacts {
        session: pipe::client_session_id(pipe).ok(),
        console_session: active_console_session(),
        integrity_rid,
        image,
    };
    tracing::debug!(?facts, "GUI pipe client");
    facts
}

/// Accepts GUI connections forever (the thread ends with the process). Only clients that
/// pass [`check_gui_client`] against `exe` (the service's own `TLP-Desk.exe`) are served.
fn spawn_gui_server(hub: Arc<GuiHub>, commands: Arc<ServiceCommands>, exe: PathBuf) {
    let spawned = std::thread::Builder::new()
        .name("tlp-svc-gui".into())
        .spawn(move || {
            let spec = PipeSpec::gui();
            let mut first = true;
            loop {
                let mut conn = match pipe::create_instance(&spec, first) {
                    Ok(c) => c,
                    Err(e) if first => {
                        // Someone else owns the name: never talk to a GUI through it.
                        tracing::error!(%e, "cannot create the GUI pipe; the GUI will not see this service");
                        return;
                    }
                    Err(e) => {
                        tracing::warn!(%e, "cannot create a GUI pipe instance");
                        std::thread::sleep(Duration::from_secs(1));
                        continue;
                    }
                };
                first = false;
                if let Err(e) = pipe::accept(&conn) {
                    tracing::debug!(%e, "GUI pipe connect failed");
                    continue;
                }
                let hub = hub.clone();
                let commands = commands.clone();
                let exe = exe.clone();
                let spawned = std::thread::Builder::new()
                    .name("tlp-svc-gui-conn".into())
                    .spawn(move || {
                        // A second handle to the same pipe instance for the admission check,
                        // which runs once the hello has been read.
                        let probe = conn.try_clone();
                        let verify = || match &probe {
                            Ok(p) => check_gui_client(&gui_client_facts(p), &exe),
                            Err(_) => Err(GuiClientRefusal::NotConsoleSession),
                        };
                        if let Err(e) = serve_gui_connection(&mut conn, &hub, &*commands, verify)
                        {
                            tracing::debug!(%e, "GUI connection ended");
                        }
                    });
                if let Err(e) = spawned {
                    tracing::warn!(%e, "cannot serve a GUI connection");
                }
            }
        });
    if let Err(e) = spawned {
        tracing::error!(%e, "cannot start the GUI pipe server");
    }
}
