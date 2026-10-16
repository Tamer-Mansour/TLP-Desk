//! [`AppServiceExtension`]: the session features, settings, connection log and GUI messages
//! of the service.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use tlp_desk_files::clipfiles::ClipFilesConfig;
use tlp_desk_files::fs::FsConfig;
use tlp_desk_files::shellclip::ShellClipboard;
use tlp_desk_files::{
    ClipboardConfig, ClipboardFeature, ConflictPolicy, FileTransfer, TransferConfig,
    TransferEvent,
};
use tlp_desk_host::service::ipc::GuiEvent;
use tlp_desk_host::service::{
    AgentFeatureHub, ConsoleUser, ServiceContext, ServiceExtension,
};
use tlp_desk_host::{HostConfig, HostEvent, PrivacyFeature, WhiteboardHost};
use tlp_desk_net::{EndpointConfig, FeatureSet};
use tlp_desk_proto::Permissions;

use super::proxies::{agent_file_source, AgentAudioCapture, AgentClipboard, AgentShellPort};
use super::settings;
use super::wire::{self, UiCommand, UiEvent, UiReply, WireConflict, WireTransfer};
use crate::backend::chat::{self, ChatBridge};
use crate::backend::files::conflict_prompt;
use crate::backend::files_bridge::{EngineStarter, FilesBridge};
use crate::backend::{privacy, settings_tray, BackendEvent, SessionId};
use crate::connection_log::{log_auth, Direction, LogPeer, LogStore, Recorder};
use crate::settings::HostSettings;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Called on a file the service creates for users to read (sets its ACL on Windows).
pub type ProtectFn = Arc<dyn Fn(&Path) + Send + Sync>;

/// The app's part of the service.
pub struct AppServiceExtension {
    data_dir: PathBuf,
    protect: ProtectFn,
    files: Arc<FileTransfer>,
    chat: ChatBridge,
    log: Recorder,
    settings: Mutex<HostSettings>,
    fsb: OnceLock<FilesBridge>,
    whiteboard: OnceLock<Arc<WhiteboardHost>>,
    ctx: OnceLock<ServiceContext>,
    transfers: Mutex<HashMap<u64, (u64, WireTransfer)>>,
    conflicts: Mutex<HashMap<u64, (u64, WireConflict)>>,
}

impl std::fmt::Debug for AppServiceExtension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppServiceExtension")
            .field("data_dir", &self.data_dir)
            .finish_non_exhaustive()
    }
}

/// Folder of the machine-wide connection log.
pub fn log_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("logs")
}

impl AppServiceExtension {
    /// The extension for the machine data folder `data_dir` (`%ProgramData%\TLP-Desk`).
    pub fn new(data_dir: PathBuf, protect: ProtectFn) -> Arc<Self> {
        let settings = settings::load(&data_dir);
        let files = FileTransfer::new(TransferConfig {
            conflict: incoming_policy(&settings),
            ..TransferConfig::default()
        });
        // Nobody logged on yet: no file access until the console user is known.
        files.set_allowed_roots(Some(Vec::new()));
        let store = LogStore::new(log_dir(&data_dir));
        Arc::new(Self {
            protect,
            files,
            chat: ChatBridge::default(),
            log: Recorder::new(Some(store)),
            settings: Mutex::new(settings),
            fsb: OnceLock::new(),
            whiteboard: OnceLock::new(),
            ctx: OnceLock::new(),
            transfers: Mutex::default(),
            conflicts: Mutex::default(),
            data_dir,
        })
    }

    /// The settings in effect.
    pub fn settings(&self) -> HostSettings {
        lock(&self.settings).clone()
    }

    fn publish(&self, ev: &UiEvent) {
        if let Some(ctx) = self.ctx.get() {
            ctx.hub.publish(GuiEvent::App(wire::encode(ev)));
        }
    }

    /// Applies validated settings live and saves them.
    fn apply_settings(&self, s: HostSettings) -> Result<(), String> {
        if let Some(ctx) = self.ctx.get() {
            settings_tray::apply(Some(&ctx.endpoint), None, &s);
            ctx.host.set_live_options(self.live_options(&s));
        }
        self.files.set_incoming_conflict(incoming_policy(&s));
        let saved = settings::save(&self.data_dir, &s).map_err(|e| e.to_string())?;
        (self.protect)(&saved);
        *lock(&self.settings) = s;
        Ok(())
    }

    /// The live options of `s`, with the service's restart / system permissions.
    fn live_options(&self, s: &HostSettings) -> tlp_desk_host::LiveOptions {
        let mut live = s.live_options();
        live.default_permissions = with_system(live.default_permissions);
        live
    }

    fn command(&self, cmd: UiCommand) -> UiReply {
        match cmd {
            UiCommand::SendChat { session, text } => match self.chat.send_host(session, &text) {
                Ok(_) => UiReply::Ok,
                Err(e) => UiReply::Error(format!("{e:?}")),
            },
            UiCommand::ResolveConflict { id, answer } => {
                let answer = answer.as_ref().map(tlp_desk_files::ConflictResolution::from);
                if self
                    .files
                    .resolve_conflicts(tlp_desk_files::TransferId(id), answer)
                {
                    UiReply::Ok
                } else {
                    UiReply::Error("no such question".into())
                }
            }
            UiCommand::CancelTransfer { id } => {
                self.files.cancel(tlp_desk_files::TransferId(id));
                UiReply::Ok
            }
            UiCommand::ApplySettings { json } => match settings::parse(&json) {
                Ok(s) => match self.apply_settings(s) {
                    Ok(()) => {
                        tracing::info!("machine host settings changed from the GUI");
                        UiReply::Ok
                    }
                    Err(e) => UiReply::Error(e),
                },
                Err(e) => {
                    tracing::warn!(%e, "refused host settings from the GUI");
                    UiReply::Error(e)
                }
            },
            UiCommand::GetSettings => match serde_json::to_string(&self.settings()) {
                Ok(j) => UiReply::Settings(j),
                Err(e) => UiReply::Error(e.to_string()),
            },
        }
    }

    /// Forwards the engine's events to the GUI (transfers and upload questions of host
    /// sessions). Runs on its own thread for the life of the service.
    fn pump_files(self: Arc<Self>) {
        let mut rx = self.files.subscribe();
        loop {
            let (key, event) = match rx.blocking_recv() {
                Ok(x) => x,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return,
            };
            match &event {
                TransferEvent::ConflictsFound(req) => {
                    let prompt = WireConflict::from(&conflict_prompt(req));
                    lock(&self.conflicts).insert(prompt.id, (key, prompt.clone()));
                    self.publish(&UiEvent::Conflict {
                        session: key,
                        prompt,
                    });
                }
                TransferEvent::ConflictsResolved(id) => {
                    lock(&self.conflicts).remove(&id.0);
                    self.publish(&UiEvent::ConflictClosed { id: id.0 });
                }
                _ => {
                    let Some(snap) = self.files.snapshot(event.id()) else {
                        continue;
                    };
                    let view = WireTransfer::from(&crate::backend::real::transfer_view(&snap));
                    {
                        let mut t = lock(&self.transfers);
                        t.insert(view.id, (key, view.clone()));
                        // Keep the catch-up list short: drop finished transfers beyond 50.
                        if t.len() > 50 {
                            let done: Vec<u64> = t
                                .iter()
                                .filter(|(_, (_, v))| {
                                    !matches!(
                                        v.status,
                                        wire::WireStatus::Running | wire::WireStatus::Preparing
                                    )
                                })
                                .map(|(id, _)| *id)
                                .collect();
                            for id in done.into_iter().take(t.len().saturating_sub(50)) {
                                t.remove(&id);
                            }
                        }
                    }
                    self.publish(&UiEvent::Transfer { session: key, view });
                }
            }
        }
    }

    fn log_host_event(&self, ev: &HostEvent) {
        match ev {
            HostEvent::SessionStarted(v) => self.log.begin(
                Direction::Incoming,
                v.id.0,
                LogPeer::from(&v.peer),
                log_auth(v.auth),
                v.permissions,
            ),
            HostEvent::PermissionsChanged { id, permissions } => {
                self.log
                    .permissions(Direction::Incoming, id.0, *permissions);
            }
            HostEvent::SessionEnded { id, reason, .. } => {
                let (end, detail) = settings_tray::host_end(reason);
                self.log.end(Direction::Incoming, id.0, end, detail);
            }
            HostEvent::AuthFailed { peer, auth, reason } => self.log.refused(
                Direction::Incoming,
                LogPeer::from(peer),
                log_auth(*auth),
                settings_tray::refusal(*reason),
            ),
            _ => {}
        }
    }
}

/// The incoming-conflict policy of the settings.
fn incoming_policy(s: &HostSettings) -> ConflictPolicy {
    if s.ask_on_upload_conflict {
        ConflictPolicy::Ask
    } else {
        ConflictPolicy::Rename
    }
}

/// The service always offers restart, lock and Ctrl+Alt+Del to sessions (unattended access).
fn with_system(p: Permissions) -> Permissions {
    p.union(Permissions::RESTART).union(Permissions::SYSTEM)
}

/// A UI event for a feature event of host session `session`, if it is one the GUI shows.
pub fn ui_event(session: u64, event: &tlp_desk_net::FeatureEvent) -> Option<UiEvent> {
    let s = SessionId(session);
    match chat::backend_event(s, event).or_else(|| privacy::host_event(s, event))? {
        BackendEvent::ChatReceived { line, .. } => Some(UiEvent::Chat {
            session,
            line: (&line).into(),
        }),
        BackendEvent::ChatAvailability { allowed, .. } => {
            Some(UiEvent::ChatAvailability { session, allowed })
        }
        BackendEvent::HostPrivacyChanged { active, .. } => {
            Some(UiEvent::Privacy { session, active })
        }
        _ => None,
    }
}

impl ServiceExtension for AppServiceExtension {
    fn endpoint_config(&self, cfg: EndpointConfig) -> EndpointConfig {
        EndpointConfig {
            allow_list: lock(&self.settings).effective_allow_list(),
            ..cfg
        }
    }

    fn configure(&self, features: &Arc<AgentFeatureHub>, cfg: HostConfig) -> HostConfig {
        let s = self.settings();
        let clipboard = ClipboardFeature::new(
            Arc::new(AgentClipboard::new(features.clone())),
            ClipboardConfig::default(),
        );
        let fsb = FilesBridge::new(
            FsConfig::default(),
            ClipFilesConfig {
                source: agent_file_source(features.clone()),
                ..ClipFilesConfig::default()
            },
        )
        .with_starter(Arc::new(EngineStarter::new(
            self.files.clone(),
            incoming_policy(&s),
        )));
        fsb.link_engine(&self.files);
        // Ctrl+C on files in Explorer on the viewer, Ctrl+V in Explorer here: the virtual files
        // go on the user's clipboard through the agent.
        fsb.enable_explorer_paste_with(
            &self.files,
            incoming_policy(&s),
            ShellClipboard::with_port(AgentShellPort::new(features.clone())),
            self.data_dir.join("paste"),
        );
        let whiteboard = Arc::new(WhiteboardHost::with_overlay_factory(
            features.overlay_factory(),
        ));
        let audio = Arc::new(tlp_desk_audio::AudioHost::new(Arc::new(
            AgentAudioCapture::new(features.clone()),
        )));
        let set: FeatureSet = crate::backend::device_info::with_host_feature(
            fsb.register(cfg.features.clone().with(self.files.clone()))
                .with(clipboard)
                .with(audio)
                .with(Arc::new(PrivacyFeature::with_shield_factory(
                    privacy::text_fn(),
                    features.shield_factory(),
                )))
                .with(whiteboard.clone()),
        );
        let mut cfg = settings_tray::host_config(
            HostConfig {
                features: set,
                ..cfg
            },
            &s,
        );
        cfg.default_permissions = with_system(cfg.default_permissions);
        self.chat.register_host(&mut cfg);
        let _ = self.fsb.set(fsb);
        let _ = self.whiteboard.set(whiteboard);
        cfg
    }

    fn started(&self, ctx: ServiceContext) {
        if let Some(w) = self.whiteboard.get() {
            w.set_monitor_source(ctx.host.streamed_monitor_fn());
        }
        let _ = self.ctx.set(ctx);
        // The machine log is readable by users, so the GUI can show it next to its own.
        let log = LogStore::new(log_dir(&self.data_dir)).path();
        if let Some(dir) = log.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .is_ok()
        {
            (self.protect)(&log);
        }
    }

    fn on_host_event(&self, ev: &HostEvent) {
        self.log_host_event(ev);
        if let HostEvent::Feature { id, event } = ev {
            if let Some(ui) = ui_event(id.0, event) {
                self.publish(&ui);
            }
        }
    }

    fn console_user(&self, user: Option<ConsoleUser>) {
        // The service writes as SYSTEM: viewers only reach the console user's own folders.
        match &user {
            Some(u) => {
                self.files
                    .set_allowed_roots(Some(vec![u.profile.clone()]));
                self.files.set_default_upload_dir(Some(u.desktop.clone()));
                if let Some(fsb) = self.fsb.get() {
                    fsb.set_fs_config(FsConfig {
                        allowed_roots: vec![u.profile.clone()],
                    });
                }
            }
            None => {
                self.files.set_allowed_roots(Some(Vec::new()));
                self.files.set_default_upload_dir(None);
                if let Some(fsb) = self.fsb.get() {
                    // A folder that does not exist: nothing to browse while nobody is logged on.
                    fsb.set_fs_config(FsConfig {
                        allowed_roots: vec![self.data_dir.join("no-user")],
                    });
                }
            }
        }
    }

    fn app_command(&self, request: &[u8]) -> Option<Vec<u8>> {
        let cmd: UiCommand = wire::decode(request)?;
        Some(wire::encode(&self.command(cmd)))
    }

    fn app_snapshot(&self) -> Vec<Vec<u8>> {
        let mut out: Vec<Vec<u8>> = lock(&self.transfers)
            .values()
            .map(|(session, view)| {
                wire::encode(&UiEvent::Transfer {
                    session: *session,
                    view: view.clone(),
                })
            })
            .collect();
        out.extend(lock(&self.conflicts).values().map(|(session, prompt)| {
            wire::encode(&UiEvent::Conflict {
                session: *session,
                prompt: prompt.clone(),
            })
        }));
        out
    }
}

/// Starts the files pump of `ext` (call once, after the service started).
pub fn spawn_files_pump(ext: &Arc<AppServiceExtension>) {
    let ext = ext.clone();
    let spawned = std::thread::Builder::new()
        .name("tlp-svc-files".into())
        .spawn(move || ext.pump_files());
    if let Err(e) = spawned {
        tracing::warn!(%e, "cannot forward transfers to the GUI");
    }
}

#[cfg(test)]
mod tests;
