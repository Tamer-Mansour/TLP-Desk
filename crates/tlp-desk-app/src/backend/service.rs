//! TLP-service lane, GUI side of unattended access.
//!
//! * **Viewer**: [`DesktopStateFeature`] turns the host's `DesktopState` into
//!   [`BackendEvent::RemoteDesktopState`] (locked / secure desktop notice, Ctrl+Alt+Del).
//! * **Host under the service**: when the installed service owns UDP 47400, this GUI cannot
//!   host. [`ServiceMirror`] mirrors the service's status (ID, code, Accept prompts, who is
//!   connected) into the usual backend events, and the user's answers go back as commands.
//!   [`ServiceLink`] runs the pipe I/O on its own thread (Windows only).

use std::net::{IpAddr, Ipv4Addr};

use tlp_desk_host::service::ipc::{GuiEvent, GuiPeer, GuiRequest, GuiSession, ServiceStatus};
use tlp_desk_net::{Feature, FeatureCtx};
use tlp_desk_proto::Message;
use tokio::sync::mpsc;

use super::{BackendEvent, DeviceId, EndReason, PeerInfo, RequestId, SessionId};

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
/// own counter, so the two never collide.
const MIRROR_ID_BASE: u64 = 1 << 48;

/// Maps the service's GUI events to backend events and remembers which UI ids belong to the
/// service.
#[derive(Debug)]
pub struct ServiceMirror {
    next: u64,
    requests: Vec<(RequestId, u64)>,
    sessions: Vec<(SessionId, u64)>,
}

impl Default for ServiceMirror {
    fn default() -> Self {
        Self {
            next: MIRROR_ID_BASE,
            requests: Vec::new(),
            sessions: Vec::new(),
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
    fn id(&mut self) -> u64 {
        self.next = self.next.wrapping_add(1);
        self.next
    }

    /// The service's request id for a UI request, removing it (the user answered).
    pub fn take_request(&mut self, request: RequestId) -> Option<u64> {
        let i = self.requests.iter().position(|(r, _)| *r == request)?;
        Some(self.requests.swap_remove(i).1)
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

    use super::{MirrorOut, ServiceMirror};
    use crate::backend::{BackendEvent, RequestId, SessionId};

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
    }

    /// Commands do pipe I/O: never on the UI thread.
    fn send(cmd: GuiCommand) {
        let spawned = std::thread::Builder::new()
            .name("tlp-desk-service-cmd".into())
            .spawn(move || match gui_command(&cmd) {
                Ok(GuiReply::Ok) => {}
                Ok(other) => tracing::info!(?cmd, ?other, "service command not applied"),
                Err(e) => tracing::warn!(%e, ?cmd, "service command failed"),
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
    fn bad_peer_address_does_not_panic() {
        let mut p = gpeer("x");
        p.address = "not an ip".into();
        assert_eq!(peer(&p).address, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    }
}
