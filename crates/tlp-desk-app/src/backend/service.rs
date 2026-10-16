//! TLP-service lane, GUI side of unattended access.
//!
//! * **Viewer**: [`DesktopStateFeature`] turns the host's `DesktopState` into
//!   [`BackendEvent::RemoteDesktopState`] (locked / secure desktop notice, Ctrl+Alt+Del).
//! * **Host under the service**: when the installed service owns UDP 47400, this GUI cannot
//!   host. [`ServiceMirror`] mirrors the service's status (ID, code, Accept prompts, who is
//!   connected) into the usual backend events, and the user's answers go back as commands.
//!   The service's app messages (`service_host::wire`) become the same events the GUI host
//!   produces: host-side transfers and upload questions, chat, privacy mode; chat replies,
//!   conflict answers, cancels and host settings go back as app commands.
//!   [`ServiceLink`] runs the pipe I/O on its own thread (Windows only).

use std::net::{IpAddr, Ipv4Addr};

use tlp_desk_host::service::ipc::{GuiEvent, GuiPeer, GuiRequest, GuiSession, ServiceStatus};
use tlp_desk_net::{Feature, FeatureCtx};
use tlp_desk_proto::Message;
use tokio::sync::mpsc;

use super::files::TransferId;
use super::{BackendEvent, DeviceId, EndReason, PeerInfo, RequestId, SessionId};
use crate::service_host::wire::{self, UiEvent};

/// [`Feature::name`] of [`DesktopStateFeature`].
pub const DESKTOP_FEATURE: &str = "desktop-state";

/// The host's desktop as last reported (`Message::DesktopState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RemoteDesktop {
    /// The lock screen, sign-in screen or a UAC prompt receives input.
    pub secure_desktop: bool,
    /// The host's console session is locked.
    pub locked: bool,
}

/// Viewer-side feature: forwards `DesktopState` to the UI.
#[derive(Debug, Default)]
pub struct DesktopStateFeature;

impl Feature for DesktopStateFeature {
    fn name(&self) -> &'static str {
        DESKTOP_FEATURE
    }

    fn handles(&self, msg: &Message) -> bool {
        matches!(msg, Message::DesktopState { .. })
    }

    fn start(&self, ctx: FeatureCtx, mut inbox: mpsc::UnboundedReceiver<Message>) {
        tokio::spawn(async move {
            while let Some(msg) = inbox.recv().await {
                if let Message::DesktopState {
                    secure_desktop,
                    locked,
                } = msg
                {
                    ctx.emit(
                        DESKTOP_FEATURE,
                        RemoteDesktop {
                            secure_desktop,
                            locked,
                        },
                    );
                }
            }
        });
    }
}

/// What the mirror wants the backend to do.
#[derive(Debug)]
pub enum MirrorOut {
    Event(BackendEvent),
    /// The service's device identity and name (home screen).
    Identity {
        device_id: DeviceId,
        name: String,
    },
    /// The service's one-time code changed.
    Code(String),
}

/// Ids handed to the UI for service requests/sessions start here, far away from the backend's
/// own counter, so the two never collide. Service transfer ids are shifted by it too.
pub const MIRROR_ID_BASE: u64 = 1 << 48;

/// The UI's id for a service transfer.
pub fn mirror_transfer_id(service: u64) -> Option<TransferId> {
    MIRROR_ID_BASE.checked_add(service).map(TransferId)
}

/// The service's id for a UI transfer id, if it is one of the service's.
pub fn service_transfer_id(id: TransferId) -> Option<u64> {
    id.0.checked_sub(MIRROR_ID_BASE)
        .filter(|_| id.0 > MIRROR_ID_BASE)
}

/// Maps the service's GUI events to backend events and remembers which UI ids belong to the
/// service.
#[derive(Debug)]
pub struct ServiceMirror {
    next: u64,
    requests: Vec<(RequestId, u64)>,
    sessions: Vec<(SessionId, u64)>,
    /// Ids of chat lines written here (shown before the service confirms).
    lines: u64,
}

impl Default for ServiceMirror {
    fn default() -> Self {
        Self {
            next: MIRROR_ID_BASE,
            requests: Vec::new(),
            sessions: Vec::new(),
            lines: 0,
        }
    }
}

fn peer(p: &GuiPeer) -> PeerInfo {
    PeerInfo {
        device_id: DeviceId(p.device_id),
        name: p.name.clone(),
        address: p
            .address
            .parse()
            .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
    }
}

impl ServiceMirror {
    /// A fresh id for a chat line this GUI sends through the service.
    pub fn next_line(&mut self) -> u64 {
        self.lines = self.lines.wrapping_add(1);
        MIRROR_ID_BASE.wrapping_add(self.lines)
    }

    fn id(&mut self) -> u64 {
        self.next = self.next.wrapping_add(1);
        self.next
    }

    /// The service's request id for a UI request, removing it (the user answered).
    pub fn take_request(&mut self, request: RequestId) -> Option<u64> {
        let i = self.requests.iter().position(|(r, _)| *r == request)?;
        Some(self.requests.swap_remove(i).1)
    }

    /// Whether `request` is a service prompt (still unanswered).
    pub fn has_request(&self, request: RequestId) -> bool {
        self.requests.iter().any(|(r, _)| *r == request)
    }

    /// The UI's session for a service session id.
    pub fn ui_session(&self, service: u64) -> Option<SessionId> {
        self.sessions
            .iter()
            .find(|(_, id)| *id == service)
            .map(|(s, _)| *s)
    }

    /// One app message from the service: the backend event the GUI host would emit.
    pub fn on_app(&self, bytes: &[u8]) -> Option<BackendEvent> {
        let ev: UiEvent = wire::decode(bytes)?;
        Some(match ev {
            UiEvent::Transfer { session, view } => BackendEvent::Transfer {
                session: self.ui_session(session)?,
                view: view.view(mirror_transfer_id(view.id)?),
            },
            UiEvent::Conflict { session, prompt } => BackendEvent::TransferConflict {
                session: self.ui_session(session)?,
                prompt: prompt.prompt(mirror_transfer_id(prompt.id)?),
            },
            UiEvent::ConflictClosed { id } => BackendEvent::TransferConflictClosed {
                id: mirror_transfer_id(id)?,
            },
            UiEvent::Chat { session, line } => BackendEvent::ChatReceived {
                session: self.ui_session(session)?,
                line: (&line).into(),
            },
            UiEvent::ChatAvailability { session, allowed } => BackendEvent::ChatAvailability {
                session: self.ui_session(session)?,
                allowed,
            },
            UiEvent::Privacy { session, active } => BackendEvent::HostPrivacyChanged {
                session: self.ui_session(session)?,
                active,
            },
        })
    }

    /// The service's session id for a UI host session.
    pub fn session(&self, session: SessionId) -> Option<u64> {
        self.sessions
            .iter()
            .find(|(s, _)| *s == session)
            .map(|(_, id)| *id)
    }

    fn add_request(&mut self, r: &GuiRequest, out: &mut Vec<MirrorOut>) {
        if self.requests.iter().any(|(_, id)| *id == r.id) {
            return;
        }
        let request = RequestId(self.id());
        self.requests.push((request, r.id));
        out.push(MirrorOut::Event(BackendEvent::IncomingRequest {
            request,
            peer: peer(&r.peer),
        }));
    }

    fn drop_request(&mut self, id: u64, out: &mut Vec<MirrorOut>) {
        if let Some(i) = self.requests.iter().position(|(_, s)| *s == id) {
            let (request, _) = self.requests.swap_remove(i);
            out.push(MirrorOut::Event(BackendEvent::IncomingRequestCancelled {
                request,
            }));
        }
    }

    fn add_session(&mut self, s: &GuiSession, out: &mut Vec<MirrorOut>) {
        if self.sessions.iter().any(|(_, id)| *id == s.id) {
            return;
        }
        let session = SessionId(self.id());
        self.sessions.push((session, s.id));
        out.push(MirrorOut::Event(BackendEvent::HostSessionStarted {
            session,
            peer: peer(&s.peer),
        }));
    }

    fn drop_session(&mut self, id: u64, reason: EndReason, out: &mut Vec<MirrorOut>) {
        if let Some(i) = self.sessions.iter().position(|(_, s)| *s == id) {
            let (session, _) = self.sessions.swap_remove(i);
            out.push(MirrorOut::Event(BackendEvent::SessionEnded {
                session,
                reason,
            }));
        }
    }

    /// A full snapshot: reconcile prompts and sessions with what the UI shows.
    pub fn on_status(&mut self, s: &ServiceStatus) -> Vec<MirrorOut> {
        let mut out = vec![
            MirrorOut::Identity {
                device_id: DeviceId(s.device_id),
                name: s.device_name.clone(),
            },
            MirrorOut::Code(s.one_time_code.clone()),
        ];
        let gone: Vec<u64> = self
            .requests
            .iter()
            .map(|(_, id)| *id)
            .filter(|id| !s.requests.iter().any(|r| r.id == *id))
            .collect();
        for id in gone {
            self.drop_request(id, &mut out);
        }
        for r in &s.requests {
            self.add_request(r, &mut out);
        }
        let ended: Vec<u64> = self
            .sessions
            .iter()
            .map(|(_, id)| *id)
            .filter(|id| !s.sessions.iter().any(|x| x.id == *id))
            .collect();
        for id in ended {
            self.drop_session(id, EndReason::Remote, &mut out);
        }
        for x in &s.sessions {
            self.add_session(x, &mut out);
        }
        out
    }

    /// One event from the service.
    pub fn on_event(&mut self, ev: GuiEvent) -> Vec<MirrorOut> {
        let mut out = Vec::new();
        match ev {
            GuiEvent::Status(s) => return self.on_status(&s),
            GuiEvent::IncomingRequest(r) => self.add_request(&r, &mut out),
            GuiEvent::RequestCancelled { id } => self.drop_request(id, &mut out),
            GuiEvent::SessionStarted(s) => self.add_session(&s, &mut out),
            GuiEvent::SessionEnded { id } => self.drop_session(id, EndReason::Remote, &mut out),
            GuiEvent::CodeChanged { code } => out.push(MirrorOut::Code(code)),
            GuiEvent::App(bytes) => {
                if let Some(ev) = self.on_app(&bytes) {
                    out.push(MirrorOut::Event(ev));
                }
            }
            GuiEvent::Desktop(_) => {}
        }
        out
    }

    /// The service went away: nothing it showed is valid any more.
    pub fn on_disconnected(&mut self) -> Vec<MirrorOut> {
        let mut out = Vec::new();
        for id in self.requests.iter().map(|(_, id)| *id).collect::<Vec<_>>() {
            self.drop_request(id, &mut out);
        }
        for id in self.sessions.iter().map(|(_, id)| *id).collect::<Vec<_>>() {
            self.drop_session(id, EndReason::Error("service stopped".into()), &mut out);
        }
        out.push(MirrorOut::Event(BackendEvent::ServiceHosting(false)));
        out
    }
}

#[cfg(windows)]
pub use link::{ServiceLink, ServiceSink};

#[cfg(windows)]
mod link {
    use std::sync::{Arc, Mutex, MutexGuard, Weak};
    use std::time::Duration;

    use tlp_desk_host::service::ipc::{GuiCommand, GuiReply};
    use tlp_desk_host::service::windows::{gui_command, gui_subscribe};

    use super::{service_transfer_id, MirrorOut, ServiceMirror};
    use crate::backend::chat::{ChatLine, ChatSendError};
    use crate::backend::files::{ConflictResolution, TransferId};
    use crate::backend::{BackendEvent, RequestId, SessionId};
    use crate::service_host::wire::{self, UiCommand, WireResolution};

    /// Where the link delivers what it learns (implemented by the real backend).
    pub trait ServiceSink: Send + Sync {
        fn apply(&self, out: MirrorOut);
    }

    /// Talks to the installed service over the GUI pipe.
    #[derive(Debug)]
    pub struct ServiceLink {
        mirror: Mutex<ServiceMirror>,
    }

    fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|p| p.into_inner())
    }

    impl ServiceLink {
        /// Starts mirroring the service into `sink` (reconnecting while the service restarts).
        pub fn start(sink: Weak<dyn ServiceSink>) -> Arc<Self> {
            let link = Arc::new(Self {
                mirror: Mutex::default(),
            });
            let me = Arc::downgrade(&link);
            let spawned = std::thread::Builder::new()
                .name("tlp-desk-service-link".into())
                .spawn(move || run(me, sink));
            if let Err(e) = spawned {
                tracing::error!(%e, "cannot start the service link");
            }
            link
        }

        /// Answers a prompt if it came from the service; `false` if it is not ours.
        pub fn respond(&self, request: RequestId, accept: bool) -> bool {
            let Some(id) = lock(&self.mirror).take_request(request) else {
                return false;
            };
            send(GuiCommand::Respond {
                request: id,
                accept,
            });
            true
        }

        /// Whether `request` came from the service (accepting it grants the service defaults).
        pub fn has_request(&self, request: RequestId) -> bool {
            lock(&self.mirror).has_request(request)
        }

        /// Ends a service-hosted session; `false` if it is not ours.
        pub fn disconnect(&self, session: SessionId) -> bool {
            let Some(id) = lock(&self.mirror).session(session) else {
                return false;
            };
            send(GuiCommand::Disconnect { session: id });
            true
        }

        /// Rotates the service's one-time code.
        pub fn rotate_code(&self) {
            send(GuiCommand::RotateCode);
        }

        /// Tells the service to re-read the permanent password and reconnect tickets (after
        /// the GUI saved the unattended password; audit F5).
        pub fn reload_credentials(&self) {
            send(GuiCommand::ReloadCredentials);
        }

        /// Whether `session` is hosted by the service.
        pub fn owns_session(&self, session: SessionId) -> bool {
            lock(&self.mirror).session(session).is_some()
        }

        /// Sends a chat line to the viewer of a service-hosted session; `None` if the
        /// session is not the service's. The line is shown at once; delivery is logged.
        pub fn send_chat(
            &self,
            session: SessionId,
            text: &str,
        ) -> Option<Result<ChatLine, ChatSendError>> {
            let id = lock(&self.mirror).session(session)?;
            if text.trim().is_empty() {
                return Some(Err(ChatSendError::Empty));
            }
            let max = crate::backend::chat::max_chat_chars();
            if text.chars().count() > max {
                return Some(Err(ChatSendError::TooLong { max }));
            }
            send(GuiCommand::App(wire::encode(&UiCommand::SendChat {
                session: id,
                text: text.to_owned(),
            })));
            let at_unix_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
                .unwrap_or(0);
            let line_id = lock(&self.mirror).next_line();
            Some(Ok(ChatLine {
                id: line_id,
                mine: true,
                text: text.to_owned(),
                at_unix_ms,
            }))
        }

        /// Answers an upload question of the service; `false` if the id is not the service's.
        pub fn resolve_conflict(
            &self,
            id: TransferId,
            answer: Option<&ConflictResolution>,
        ) -> bool {
            let Some(id) = service_transfer_id(id) else {
                return false;
            };
            send(GuiCommand::App(wire::encode(&UiCommand::ResolveConflict {
                id,
                answer: answer.map(WireResolution::from),
            })));
            true
        }

        /// Cancels a transfer of the service; `false` if the id is not the service's.
        pub fn cancel_transfer(&self, id: TransferId) -> bool {
            let Some(id) = service_transfer_id(id) else {
                return false;
            };
            send(GuiCommand::App(wire::encode(&UiCommand::CancelTransfer {
                id,
            })));
            true
        }

        /// Hands the user's host settings to the service, which validates, applies and saves
        /// them machine-wide.
        pub fn apply_host_settings(&self, settings: &crate::settings::HostSettings) {
            match serde_json::to_string(settings) {
                Ok(json) => send(GuiCommand::App(wire::encode(&UiCommand::ApplySettings {
                    json,
                }))),
                Err(e) => tracing::warn!(%e, "cannot encode host settings"),
            }
        }
    }

    /// Commands do pipe I/O: never on the UI thread.
    fn send(cmd: GuiCommand) {
        let spawned = std::thread::Builder::new()
            .name("tlp-desk-service-cmd".into())
            .spawn(move || match gui_command(&cmd) {
                Ok(GuiReply::Ok) => {}
                Ok(GuiReply::App(bytes)) => match wire::decode::<wire::UiReply>(&bytes) {
                    Some(wire::UiReply::Error(e)) => {
                        tracing::info!(%e, "service did not apply the command");
                    }
                    Some(_) => {}
                    None => tracing::info!("service sent an unreadable answer"),
                },
                // Never log app commands: they may hold chat text.
                Ok(other) => tracing::info!(?other, "service command not applied"),
                Err(e) => tracing::warn!(%e, "service command failed"),
            });
        if let Err(e) = spawned {
            tracing::warn!(%e, "cannot send a service command");
        }
    }

    fn deliver(sink: &Weak<dyn ServiceSink>, out: Vec<MirrorOut>) -> bool {
        let Some(sink) = sink.upgrade() else {
            return false;
        };
        for o in out {
            sink.apply(o);
        }
        true
    }

    fn run(link: Weak<ServiceLink>, sink: Weak<dyn ServiceSink>) {
        let mut connected = false;
        loop {
            let Some(me) = link.upgrade() else { return };
            match gui_subscribe() {
                Ok((status, mut sub)) => {
                    tracing::info!("connected to the TLP-Desk service");
                    connected = true;
                    let mut out = vec![MirrorOut::Event(BackendEvent::ServiceHosting(true))];
                    out.extend(lock(&me.mirror).on_status(&status));
                    if !deliver(&sink, out) {
                        return;
                    }
                    drop(me);
                    while let Ok(ev) = sub.next_event() {
                        let Some(me) = link.upgrade() else { return };
                        let out = lock(&me.mirror).on_event(ev);
                        if !deliver(&sink, out) {
                            return;
                        }
                    }
                    tracing::info!("TLP-Desk service connection closed");
                    let Some(me) = link.upgrade() else { return };
                    let out = lock(&me.mirror).on_disconnected();
                    if !deliver(&sink, out) {
                        return;
                    }
                }
                Err(e) => {
                    if connected {
                        tracing::debug!(%e, "service not reachable yet");
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    }
}

#[cfg(test)]
mod tests {
    use tlp_desk_host::service::ipc::WireDesktop;
    use tlp_desk_proto::Permissions;

    use super::*;

    fn gpeer(name: &str) -> GuiPeer {
        GuiPeer {
            device_id: 123_456_789,
            name: name.into(),
            address: "192.168.1.5".into(),
        }
    }

    fn status(requests: Vec<u64>, sessions: Vec<u64>) -> ServiceStatus {
        ServiceStatus {
            device_id: 987_654_321,
            device_name: "حاسوب المكتب".into(),
            one_time_code: "123456".into(),
            requests: requests
                .into_iter()
                .map(|id| GuiRequest {
                    id,
                    peer: gpeer("سارة"),
                })
                .collect(),
            sessions: sessions
                .into_iter()
                .map(|id| GuiSession {
                    id,
                    peer: gpeer("Laptop-١"),
                    permissions: Permissions::VIEW,
                })
                .collect(),
            desktop: WireDesktop::default(),
        }
    }

    #[test]
    fn snapshot_becomes_prompts_banner_identity_and_code() {
        let mut m = ServiceMirror::default();
        let out = m.on_status(&status(vec![10], vec![20]));
        assert!(matches!(
            &out[0],
            MirrorOut::Identity { device_id: DeviceId(987_654_321), name } if name == "حاسوب المكتب"
        ));
        assert!(matches!(&out[1], MirrorOut::Code(c) if c == "123456"));
        let MirrorOut::Event(BackendEvent::IncomingRequest { request, peer }) = &out[2] else {
            panic!("{out:?}");
        };
        assert_eq!(peer.name, "سارة");
        assert_eq!(peer.address, "192.168.1.5".parse::<IpAddr>().unwrap());
        assert!(matches!(
            &out[3],
            MirrorOut::Event(BackendEvent::HostSessionStarted { peer, .. }) if peer.name == "Laptop-١"
        ));
        // Answering maps back to the service's id once.
        assert_eq!(m.take_request(*request), Some(10));
        assert_eq!(m.take_request(*request), None);
    }

    #[test]
    fn events_and_resnapshots_are_reconciled() {
        let mut m = ServiceMirror::default();
        m.on_status(&status(vec![], vec![]));
        let out = m.on_event(GuiEvent::IncomingRequest(GuiRequest {
            id: 5,
            peer: gpeer("x"),
        }));
        assert_eq!(out.len(), 1);
        // A duplicate (e.g. after a re-snapshot) is not shown twice.
        assert!(m
            .on_event(GuiEvent::IncomingRequest(GuiRequest {
                id: 5,
                peer: gpeer("x"),
            }))
            .is_empty());
        let out = m.on_event(GuiEvent::RequestCancelled { id: 5 });
        assert!(matches!(
            out[..],
            [MirrorOut::Event(
                BackendEvent::IncomingRequestCancelled { .. }
            )]
        ));
        let out = m.on_event(GuiEvent::SessionStarted(GuiSession {
            id: 9,
            peer: gpeer("y"),
            permissions: Permissions::VIEW,
        }));
        let MirrorOut::Event(BackendEvent::HostSessionStarted { session, .. }) = out[0] else {
            panic!();
        };
        assert_eq!(m.session(session), Some(9));
        // A new snapshot without session 9 ends it in the UI.
        let out = m.on_status(&status(vec![], vec![]));
        assert!(out.iter().any(|o| matches!(
            o,
            MirrorOut::Event(BackendEvent::SessionEnded { session: s, .. }) if *s == session
        )));
        assert_eq!(m.session(session), None);
        let out = m.on_event(GuiEvent::CodeChanged {
            code: "000999".into(),
        });
        assert!(matches!(&out[..], [MirrorOut::Code(c)] if c == "000999"));
    }

    #[test]
    fn losing_the_service_clears_everything() {
        let mut m = ServiceMirror::default();
        m.on_status(&status(vec![1, 2], vec![3]));
        let out = m.on_disconnected();
        let cancelled = out
            .iter()
            .filter(|o| {
                matches!(
                    o,
                    MirrorOut::Event(BackendEvent::IncomingRequestCancelled { .. })
                )
            })
            .count();
        let ended = out
            .iter()
            .filter(|o| matches!(o, MirrorOut::Event(BackendEvent::SessionEnded { .. })))
            .count();
        assert_eq!((cancelled, ended), (2, 1));
        assert!(matches!(
            out.last(),
            Some(MirrorOut::Event(BackendEvent::ServiceHosting(false)))
        ));
    }

    #[test]
    fn mirror_ids_never_collide_with_backend_ids() {
        let mut m = ServiceMirror::default();
        let out = m.on_status(&status(vec![1], vec![]));
        let MirrorOut::Event(BackendEvent::IncomingRequest { request, .. }) = &out[2] else {
            panic!();
        };
        assert!(request.0 > MIRROR_ID_BASE);
    }

    #[test]
    fn app_messages_become_host_side_events_for_known_sessions() {
        use crate::service_host::wire::{
            encode, WireChatLine, WireConflict, WireFileConflict, WireStatus, WireTransfer,
        };
        let mut m = ServiceMirror::default();
        let out = m.on_event(GuiEvent::SessionStarted(GuiSession {
            id: 9,
            peer: gpeer("سارة"),
            permissions: Permissions::VIEW,
        }));
        let MirrorOut::Event(BackendEvent::HostSessionStarted { session, .. }) = out[0] else {
            panic!("{out:?}");
        };
        let transfer = UiEvent::Transfer {
            session: 9,
            view: WireTransfer {
                id: 4,
                sending: false,
                label: "تقرير.pdf".into(),
                status: WireStatus::Running,
                bytes_done: 1,
                total_bytes: 2,
                files_done: 0,
                total_files: 1,
                failed_files: 0,
                bytes_per_sec: 3,
                local_dest: Some(r"C:\Users\تامر\Desktop".into()),
                local: false,
                started_unix_ms: 0,
            },
        };
        let out = m.on_event(GuiEvent::App(encode(&transfer)));
        let [MirrorOut::Event(BackendEvent::Transfer { session: s, view })] = &out[..] else {
            panic!("{out:?}");
        };
        assert_eq!(*s, session);
        assert_eq!(view.label, "تقرير.pdf");
        assert_eq!(service_transfer_id(view.id), Some(4));
        assert!(view.id.0 > MIRROR_ID_BASE);
        // Upload question, then its end.
        let conflict = UiEvent::Conflict {
            session: 9,
            prompt: WireConflict {
                id: 4,
                label: "صور".into(),
                dest_dir: r"C:\Users\تامر\Desktop".into(),
                total: 1,
                files: vec![WireFileConflict {
                    index: 0,
                    rel_path: "صور/١.jpg".into(),
                    existing_size: 1,
                    existing_modified_unix_ms: 0,
                    incoming_size: 2,
                    incoming_modified_unix_ms: 0,
                    partial_bytes: None,
                }],
            },
        };
        let out = m.on_event(GuiEvent::App(encode(&conflict)));
        assert!(matches!(
            &out[..],
            [MirrorOut::Event(BackendEvent::TransferConflict { prompt, .. })]
                if !prompt.local && prompt.files[0].rel_path == "صور/١.jpg"
        ));
        let out = m.on_event(GuiEvent::App(encode(&UiEvent::ConflictClosed { id: 4 })));
        assert!(matches!(
            &out[..],
            [MirrorOut::Event(BackendEvent::TransferConflictClosed { id })]
                if service_transfer_id(*id) == Some(4)
        ));
        // Chat and privacy.
        let chat = UiEvent::Chat {
            session: 9,
            line: WireChatLine {
                id: 1,
                mine: false,
                text: "مرحبا\u{200f}!".into(),
                at_unix_ms: 5,
            },
        };
        let out = m.on_event(GuiEvent::App(encode(&chat)));
        assert!(matches!(
            &out[..],
            [MirrorOut::Event(BackendEvent::ChatReceived { line, .. })] if line.text == "مرحبا\u{200f}!"
        ));
        let privacy = UiEvent::Privacy {
            session: 9,
            active: true,
        };
        assert!(matches!(
            &m.on_event(GuiEvent::App(encode(&privacy)))[..],
            [MirrorOut::Event(BackendEvent::HostPrivacyChanged {
                active: true,
                ..
            })]
        ));
        // Unknown session or garbage: nothing.
        let unknown = UiEvent::ChatAvailability {
            session: 77,
            allowed: true,
        };
        assert!(m.on_event(GuiEvent::App(encode(&unknown))).is_empty());
        assert!(m.on_event(GuiEvent::App(vec![255, 1])).is_empty());
        // Local ids are never mistaken for the service's.
        assert_eq!(service_transfer_id(TransferId(5)), None);
        assert_eq!(service_transfer_id(TransferId(MIRROR_ID_BASE)), None);
    }

    #[test]
    fn bad_peer_address_does_not_panic() {
        let mut p = gpeer("x");
        p.address = "not an ip".into();
        assert_eq!(peer(&p).address, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    }
}
