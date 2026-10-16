//! The service's side of the GUI pipe: host status, Accept prompts and the "who is connected"
//! banner for the logged-in user's `TLP-Desk.exe`.
//!
//! While the service owns UDP 47400 the GUI cannot run its own host, so it subscribes here
//! ([`GuiHub`]) and sends the user's answers back ([`GuiCommands`]). Who may talk to the hub:
//! interactive users only (pipe ACL), and only a client in the **active console session**
//! (checked by the service with `GetNamedPipeClientSessionId`): the person at the machine
//! decides, not someone on another session of a terminal server.
//!
//! **No user logged on** (login screen, nobody at the machine): there is nobody to click
//! Accept, so [`route_prompt`] rejects Accept-prompt requests at once instead of letting the
//! viewer wait for the timeout; one-time code, permanent password and reconnect tickets keep
//! working.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use tlp_desk_proto::Permissions;

use super::ipc::{
    self, GuiChannel, GuiCommand, GuiEvent, GuiHello, GuiPeer, GuiReply, GuiRequest, GuiSession,
    GuiWelcome, ServiceStatus, WireDesktop, WireError, IPC_VERSION,
};
use crate::{HostError, HostEvent, PeerInfo};

/// Events queued per GUI before it counts as stuck and is dropped (it reconnects and gets a
/// fresh snapshot).
const GUI_QUEUE: usize = 256;

/// Longest silence on a GUI events connection; then the current desktop state is re-sent.
pub const GUI_HEARTBEAT: Duration = Duration::from_secs(2);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The host operations a GUI may trigger (implemented by the service around `Host`; faked in
/// tests).
pub trait GuiCommands: Send + Sync {
    /// Answer an Accept prompt. `false` if it no longer exists.
    fn respond(&self, request: u64, accept: bool) -> bool;
    fn rotate_code(&self);
    /// `false` if the session no longer exists.
    fn disconnect(&self, session: u64) -> bool;
    /// `false` if the session no longer exists.
    fn set_permissions(&self, session: u64, permissions: Permissions) -> bool;

    /// Runs an app-defined command ([`GuiCommand::App`]) and returns the app's answer;
    /// `None` when there is no app layer (the GUI gets [`GuiReply::NotFound`]).
    fn app_command(&self, request: &[u8]) -> Option<Vec<u8>> {
        let _ = request;
        None
    }

    /// App-defined events a newly connected GUI needs to catch up (running transfers, open
    /// questions), sent right after the status snapshot as [`GuiEvent::App`].
    fn app_snapshot(&self) -> Vec<Vec<u8>> {
        Vec::new()
    }
}

impl GuiCommands for crate::Host {
    fn respond(&self, request: u64, accept: bool) -> bool {
        let id = crate::RequestId(request);
        if accept {
            self.accept(id, None)
        } else {
            self.reject(id)
        }
    }
    fn rotate_code(&self) {
        crate::Host::rotate_code(self);
    }
    fn disconnect(&self, session: u64) -> bool {
        crate::Host::disconnect(self, crate::SessionId(session))
    }
    fn set_permissions(&self, session: u64, permissions: Permissions) -> bool {
        crate::Host::set_permissions(self, crate::SessionId(session), permissions)
    }
}

/// Where an Accept prompt goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptRoute {
    /// A GUI is connected: it shows the prompt.
    ShowInGui,
    /// A user is logged on but no GUI runs: start the GUI in their session, which then
    /// shows the pending prompt.
    LaunchGui,
    /// Nobody can answer: reject now (code/password/ticket still work).
    Reject,
}

/// Decides where an Accept prompt goes.
pub fn route_prompt(gui_connected: bool, user_logged_on: bool) -> PromptRoute {
    match (gui_connected, user_logged_on) {
        (true, _) => PromptRoute::ShowInGui,
        (false, true) => PromptRoute::LaunchGui,
        (false, false) => PromptRoute::Reject,
    }
}

fn gui_peer(p: &PeerInfo) -> GuiPeer {
    GuiPeer {
        device_id: p.device_id.as_u32(),
        name: p.name.clone(),
        address: p.addr.ip().to_string(),
    }
}

/// Fans host status out to every connected GUI and keeps the snapshot new GUIs start from.
#[derive(Debug)]
pub struct GuiHub {
    status: Mutex<ServiceStatus>,
    subscribers: Mutex<Vec<SyncSender<GuiEvent>>>,
    connected: AtomicUsize,
}

/// Counts a connected GUI for as long as it lives.
struct Connected<'a>(&'a AtomicUsize);

impl<'a> Connected<'a> {
    fn new(n: &'a AtomicUsize) -> Self {
        n.fetch_add(1, Ordering::AcqRel);
        Self(n)
    }
}

impl Drop for Connected<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl GuiHub {
    /// A hub for this host (`device_id`, name and the current code).
    pub fn new(device_id: u32, device_name: String, one_time_code: String) -> Self {
        Self {
            status: Mutex::new(ServiceStatus {
                device_id,
                device_name,
                one_time_code,
                requests: Vec::new(),
                sessions: Vec::new(),
                desktop: WireDesktop::default(),
            }),
            subscribers: Mutex::default(),
            connected: AtomicUsize::new(0),
        }
    }

    /// The current snapshot.
    pub fn status(&self) -> ServiceStatus {
        lock(&self.status).clone()
    }

    /// Whether at least one GUI is connected. A GUI that went away is noticed within
    /// [`GUI_HEARTBEAT`] (the next write to its pipe fails).
    pub fn has_gui(&self) -> bool {
        self.connected.load(Ordering::Acquire) > 0
    }

    /// Registers a GUI: returns the snapshot to send first and the event stream after it.
    pub fn subscribe(&self) -> (ServiceStatus, Receiver<GuiEvent>) {
        let (tx, rx) = sync_channel(GUI_QUEUE);
        // Snapshot and registration under the status lock, so no event falls in between.
        let status = lock(&self.status);
        lock(&self.subscribers).push(tx);
        (status.clone(), rx)
    }

    /// Updates the snapshot and forwards `ev` to every GUI (dropping stuck or closed ones).
    pub fn publish(&self, ev: GuiEvent) {
        let mut status = lock(&self.status);
        apply(&mut status, &ev);
        let mut subs = lock(&self.subscribers);
        subs.retain(|s| match s.try_send(ev.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                tracing::warn!("GUI is not reading its events; dropping it");
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        });
    }

    /// Maps a host event to the GUI (`code` is the current one-time code, read by the
    /// caller from the host).
    pub fn on_host_event(&self, ev: &HostEvent, code: impl FnOnce() -> String) {
        let gui = match ev {
            HostEvent::IncomingRequest(r) => GuiEvent::IncomingRequest(GuiRequest {
                id: r.id.0,
                peer: gui_peer(&r.peer),
            }),
            HostEvent::RequestCancelled { id, .. } => GuiEvent::RequestCancelled { id: id.0 },
            HostEvent::CodeChanged => GuiEvent::CodeChanged { code: code() },
            HostEvent::SessionStarted(v) => GuiEvent::SessionStarted(GuiSession {
                id: v.id.0,
                peer: gui_peer(&v.peer),
                permissions: v.permissions,
            }),
            HostEvent::PermissionsChanged { id, permissions } => {
                let mut status = self.status();
                if let Some(s) = status.sessions.iter_mut().find(|s| s.id == id.0) {
                    s.permissions = *permissions;
                }
                GuiEvent::Status(status)
            }
            HostEvent::SessionEnded { id, .. } => GuiEvent::SessionEnded { id: id.0 },
            HostEvent::AuthFailed { .. }
            | HostEvent::InputBlocked { .. }
            | HostEvent::Feature { .. } => return,
        };
        self.publish(gui);
    }

    /// Publishes a secure-desktop / lock change.
    pub fn set_desktop(&self, desktop: WireDesktop) {
        if lock(&self.status).desktop != desktop {
            self.publish(GuiEvent::Desktop(desktop));
        }
    }
}

/// Applies an event to the snapshot.
fn apply(status: &mut ServiceStatus, ev: &GuiEvent) {
    match ev {
        GuiEvent::Status(s) => *status = s.clone(),
        GuiEvent::IncomingRequest(r) => {
            if !status.requests.iter().any(|x| x.id == r.id) {
                status.requests.push(r.clone());
            }
        }
        GuiEvent::RequestCancelled { id } => status.requests.retain(|r| r.id != *id),
        GuiEvent::SessionStarted(s) => {
            if !status.sessions.iter().any(|x| x.id == s.id) {
                status.sessions.push(s.clone());
            }
        }
        GuiEvent::SessionEnded { id } => status.sessions.retain(|s| s.id != *id),
        GuiEvent::CodeChanged { code } => status.one_time_code.clone_from(code),
        GuiEvent::Desktop(d) => status.desktop = *d,
        // The app keeps its own state and replays it with `GuiCommands::app_snapshot`.
        GuiEvent::App(_) => {}
    }
}

/// Serves one GUI connection. `verify_client` is the service's admission check of the client
/// process ([`check_gui_client`](super::check_gui_client): console session, integrity at
/// least Medium, the service's own `TLP-Desk.exe`). It runs after the hello was read (the
/// client's token can be impersonated only then) and before anything is sent or done; a
/// refused client gets nothing but `Refused`.
pub fn serve_gui_connection<C: Read + Write + ?Sized>(
    conn: &mut C,
    hub: &GuiHub,
    commands: &dyn GuiCommands,
    verify_client: impl FnOnce() -> Result<(), super::GuiClientRefusal>,
) -> Result<(), HostError> {
    let hello: GuiHello = ipc::read_msg(conn)?;
    if hello.version != IPC_VERSION {
        ipc::write_msg(
            conn,
            &GuiWelcome::Refused {
                version: IPC_VERSION,
                error: WireError::VersionMismatch {
                    expected: IPC_VERSION,
                    got: hello.version,
                },
            },
        )?;
        return Err(HostError::IpcVersion {
            expected: IPC_VERSION,
            got: hello.version,
        });
    }
    if let Err(why) = verify_client() {
        tracing::warn!(?why, "GUI pipe client refused");
        return ipc::write_msg(
            conn,
            &GuiWelcome::Refused {
                version: IPC_VERSION,
                error: WireError::Other("client not allowed".into()),
            },
        );
    }
    ipc::write_msg(
        conn,
        &GuiWelcome::Ok {
            version: IPC_VERSION,
        },
    )?;
    match hello.channel {
        GuiChannel::Events => {
            let _connected = Connected::new(&hub.connected);
            let (snapshot, events) = hub.subscribe();
            ipc::write_msg(conn, &GuiEvent::Status(snapshot))?;
            for app in commands.app_snapshot() {
                ipc::write_msg(conn, &GuiEvent::App(app))?;
            }
            // Ends when the GUI disconnects (a write fails) or the hub drops us. Without
            // events, a heartbeat (the current desktop state, harmless to repeat) finds a
            // GUI that went away.
            loop {
                let ev = match events.recv_timeout(GUI_HEARTBEAT) {
                    Ok(ev) => ev,
                    Err(RecvTimeoutError::Timeout) => GuiEvent::Desktop(hub.status().desktop),
                    Err(RecvTimeoutError::Disconnected) => return Ok(()),
                };
                ipc::write_msg(conn, &ev)?;
            }
        }
        GuiChannel::Command => {
            let cmd: GuiCommand = ipc::read_msg(conn)?;
            let reply = run_command(commands, cmd);
            ipc::write_msg(conn, &reply)
        }
    }
}

fn run_command(commands: &dyn GuiCommands, cmd: GuiCommand) -> GuiReply {
    let found = match cmd {
        GuiCommand::App(request) => {
            return commands
                .app_command(&request)
                .map_or(GuiReply::NotFound, GuiReply::App)
        }
        GuiCommand::Respond { request, accept } => {
            tracing::info!(request, accept, "accept prompt answered in the GUI");
            commands.respond(request, accept)
        }
        GuiCommand::RotateCode => {
            commands.rotate_code();
            true
        }
        GuiCommand::Disconnect { session } => commands.disconnect(session),
        GuiCommand::SetPermissions {
            session,
            permissions,
        } => commands.set_permissions(session, permissions),
    };
    if found {
        GuiReply::Ok
    } else {
        GuiReply::NotFound
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Instant, SystemTime};

    use tlp_desk_crypto::Identity;

    use super::super::testutil::duplex;
    use super::super::GuiClientRefusal;
    use super::*;
    use crate::{AuthKind, ConnectedViewer, IncomingRequest, RequestId, SessionId};

    #[derive(Default)]
    struct Fake {
        log: Mutex<Vec<String>>,
    }

    impl GuiCommands for Fake {
        fn respond(&self, request: u64, accept: bool) -> bool {
            lock(&self.log).push(format!("respond {request} {accept}"));
            request == 7
        }
        fn rotate_code(&self) {
            lock(&self.log).push("rotate".into());
        }
        fn disconnect(&self, session: u64) -> bool {
            lock(&self.log).push(format!("disconnect {session}"));
            true
        }
        fn set_permissions(&self, session: u64, p: Permissions) -> bool {
            lock(&self.log).push(format!("perms {session} {}", p.0));
            true
        }
    }

    fn peer() -> PeerInfo {
        let id = Identity::generate().unwrap();
        PeerInfo {
            device_id: id.device_id(),
            fingerprint: id.fingerprint(),
            name: "حاسوب سارة".into(),
            addr: "192.168.1.30:47400".parse().unwrap(),
        }
    }

    #[test]
    fn prompt_routing() {
        assert_eq!(route_prompt(true, true), PromptRoute::ShowInGui);
        assert_eq!(route_prompt(true, false), PromptRoute::ShowInGui);
        assert_eq!(route_prompt(false, true), PromptRoute::LaunchGui);
        assert_eq!(route_prompt(false, false), PromptRoute::Reject);
    }

    #[test]
    fn hub_tracks_requests_sessions_and_code() {
        let hub = GuiHub::new(123, "مكتب".into(), "111111".into());
        let p = peer();
        hub.on_host_event(
            &HostEvent::IncomingRequest(IncomingRequest {
                id: RequestId(7),
                peer: p.clone(),
                auth: AuthKind::AcceptPrompt,
                deadline: Instant::now(),
            }),
            String::new,
        );
        let (snap, rx) = hub.subscribe();
        assert_eq!(snap.requests.len(), 1);
        assert_eq!(snap.requests[0].peer.name, "حاسوب سارة");
        assert_eq!(snap.requests[0].peer.address, "192.168.1.30");
        hub.on_host_event(
            &HostEvent::RequestCancelled {
                id: RequestId(7),
                reason: crate::CancelReason::ViewerLeft,
            },
            String::new,
        );
        hub.on_host_event(&HostEvent::CodeChanged, || "222222".into());
        hub.on_host_event(
            &HostEvent::SessionStarted(ConnectedViewer {
                id: SessionId(9),
                peer: p.clone(),
                auth: AuthKind::OneTimeCode,
                permissions: Permissions::VIEW,
                since: SystemTime::now(),
            }),
            String::new,
        );
        hub.on_host_event(
            &HostEvent::PermissionsChanged {
                id: SessionId(9),
                permissions: Permissions::VIEW | Permissions::SYSTEM,
            },
            String::new,
        );
        let s = hub.status();
        assert!(s.requests.is_empty());
        assert_eq!(s.one_time_code, "222222");
        assert_eq!(s.sessions.len(), 1);
        assert!(s.sessions[0].permissions.contains(Permissions::SYSTEM));
        // The subscriber saw every change in order.
        let got: Vec<_> = rx.try_iter().collect();
        assert!(matches!(got[0], GuiEvent::RequestCancelled { id: 7 }));
        assert!(matches!(&got[1], GuiEvent::CodeChanged { code } if code == "222222"));
        assert!(matches!(got[2], GuiEvent::SessionStarted(_)));
        assert!(matches!(got[3], GuiEvent::Status(_)));
        hub.on_host_event(
            &HostEvent::SessionEnded {
                id: SessionId(9),
                peer: p,
                reason: crate::EndReason::Local,
            },
            String::new,
        );
        assert!(hub.status().sessions.is_empty());
    }

    #[test]
    fn stuck_or_gone_guis_are_dropped() {
        let hub = GuiHub::new(1, String::new(), String::new());
        let (_s, rx) = hub.subscribe();
        drop(rx);
        hub.publish(GuiEvent::CodeChanged { code: "1".into() });
        assert!(lock(&hub.subscribers).is_empty());
        let (_s, _rx_kept) = hub.subscribe();
        for i in 0..=GUI_QUEUE {
            hub.publish(GuiEvent::CodeChanged {
                code: i.to_string(),
            });
        }
        assert!(
            lock(&hub.subscribers).is_empty(),
            "full queue drops the GUI"
        );
    }

    #[test]
    fn gui_connection_streams_snapshot_then_events() {
        let hub = Arc::new(GuiHub::new(42, "جهاز".into(), "123456".into()));
        let fake = Arc::new(Fake::default());
        let (mut a, mut b) = duplex();
        let (h, f) = (hub.clone(), fake.clone());
        let t = std::thread::spawn(move || serve_gui_connection(&mut b, &h, &*f, || Ok(())));
        ipc::write_msg(
            &mut a,
            &GuiHello {
                version: IPC_VERSION,
                channel: GuiChannel::Events,
            },
        )
        .unwrap();
        assert!(matches!(
            ipc::read_msg::<_, GuiWelcome>(&mut a).unwrap(),
            GuiWelcome::Ok { .. }
        ));
        let GuiEvent::Status(s) = ipc::read_msg::<_, GuiEvent>(&mut a).unwrap() else {
            panic!("snapshot first");
        };
        assert_eq!(s.device_name, "جهاز");
        assert!(hub.has_gui());
        hub.set_desktop(WireDesktop {
            secure_desktop: true,
            locked: true,
        });
        assert!(matches!(
            ipc::read_msg::<_, GuiEvent>(&mut a).unwrap(),
            GuiEvent::Desktop(WireDesktop {
                secure_desktop: true,
                locked: true
            })
        ));
        drop(a);
        hub.publish(GuiEvent::CodeChanged { code: "x".into() });
        assert!(t.join().unwrap().is_err(), "write to a closed GUI fails");
        assert!(!hub.has_gui());
    }

    #[test]
    fn gui_commands_reach_the_host() {
        let hub = GuiHub::new(1, String::new(), String::new());
        let fake = Fake::default();
        for (cmd, want) in [
            (
                GuiCommand::Respond {
                    request: 7,
                    accept: true,
                },
                GuiReply::Ok,
            ),
            (
                GuiCommand::Respond {
                    request: 8,
                    accept: false,
                },
                GuiReply::NotFound,
            ),
            (GuiCommand::RotateCode, GuiReply::Ok),
            (GuiCommand::Disconnect { session: 3 }, GuiReply::Ok),
            (
                GuiCommand::SetPermissions {
                    session: 3,
                    permissions: Permissions::VIEW,
                },
                GuiReply::Ok,
            ),
        ] {
            let (mut a, mut b) = duplex();
            ipc::write_msg(
                &mut a,
                &GuiHello {
                    version: IPC_VERSION,
                    channel: GuiChannel::Command,
                },
            )
            .unwrap();
            ipc::write_msg(&mut a, &cmd).unwrap();
            serve_gui_connection(&mut b, &hub, &fake, || Ok(())).unwrap();
            let _: GuiWelcome = ipc::read_msg(&mut a).unwrap();
            assert_eq!(ipc::read_msg::<_, GuiReply>(&mut a).unwrap(), want);
        }
        assert_eq!(
            *lock(&fake.log),
            vec![
                "respond 7 true",
                "respond 8 false",
                "rotate",
                "disconnect 3",
                "perms 3 1"
            ]
        );
    }

    #[test]
    fn clients_outside_the_console_session_are_refused() {
        let hub = GuiHub::new(1, String::new(), String::new());
        let fake = Fake::default();
        let (mut a, mut b) = duplex();
        ipc::write_msg(
            &mut a,
            &GuiHello {
                version: IPC_VERSION,
                channel: GuiChannel::Command,
            },
        )
        .unwrap();
        ipc::write_msg(&mut a, &GuiCommand::RotateCode).unwrap();
        serve_gui_connection(&mut b, &hub, &fake, || {
            Err(GuiClientRefusal::IntegrityTooLow)
        })
        .unwrap();
        assert!(matches!(
            ipc::read_msg::<_, GuiWelcome>(&mut a).unwrap(),
            GuiWelcome::Refused { .. }
        ));
        assert!(lock(&fake.log).is_empty());
    }
}
