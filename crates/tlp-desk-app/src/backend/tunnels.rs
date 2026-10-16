//! Port forwarding (TLP-tunnel): the UI-facing types and the mapping from the host/viewer
//! tunnel features' events to [`BackendEvent`]s.

use std::net::SocketAddr;

use tlp_desk_host::{HostTunnelEvent, HostTunnelInfo};
use tlp_desk_net::FeatureEvent;
use tlp_desk_viewer::{TunnelCloseReason, TunnelEvent, TunnelStatus, ViewerError};

use super::{BackendEvent, SessionId};

/// A forwarding rule the user wants to add (viewer side).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelRequest {
    /// Local port (0 = any free port).
    pub local_port: u16,
    /// Listen on every interface instead of loopback only. The UI must have shown the
    /// warning that other devices can then use the tunnel.
    pub lan_bind: bool,
    /// IP address as seen from the host; `None` = the host itself.
    pub target_host: Option<String>,
    pub target_port: u16,
}

/// One active forwarding rule (viewer side).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelRow {
    pub id: u64,
    /// The real local listening address.
    pub local: SocketAddr,
    /// `host:port` as seen from the host (`localhost` = the host itself).
    pub target: String,
    /// Connections open now.
    pub active: usize,
    /// Connections accepted so far.
    pub connections: u64,
    /// Connections that failed (target unreachable on the host, limits).
    pub failed: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
}

impl From<&TunnelStatus> for TunnelRow {
    fn from(s: &TunnelStatus) -> Self {
        Self {
            id: s.id.0,
            local: s.local,
            target: s.target_display(),
            active: s.active,
            connections: s.connections,
            failed: s.failed,
            bytes_sent: s.bytes_sent,
            bytes_received: s.bytes_received,
        }
    }
}

/// Why a rule could not start or was ended by the host. The UI translates it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelProblem {
    /// The host has not allowed port forwarding.
    NotAllowed,
    /// The host revoked port forwarding; every rule was closed.
    Revoked,
    /// The local port is in use or cannot be opened.
    LocalPort(String),
    /// The target is not on the host's local network.
    TargetNotAllowed,
    /// The target must be an IP address and a port.
    BadTarget,
    /// Too many rules.
    TooMany,
    /// The host did not answer.
    NoAnswer,
    /// The session ended.
    SessionEnded,
    /// Anything else (display-ready detail, may be Arabic).
    Other(String),
}

impl From<&TunnelCloseReason> for TunnelProblem {
    fn from(r: &TunnelCloseReason) -> Self {
        match r {
            TunnelCloseReason::TargetNotAllowed => Self::TargetNotAllowed,
            TunnelCloseReason::BadTarget => Self::BadTarget,
            TunnelCloseReason::TooManyTunnels => Self::TooMany,
            TunnelCloseReason::PermissionDenied => Self::NotAllowed,
            TunnelCloseReason::PermissionRevoked => Self::Revoked,
            TunnelCloseReason::NoAnswer => Self::NoAnswer,
            other => Self::Other(other.to_string()),
        }
    }
}

impl From<&ViewerError> for TunnelProblem {
    fn from(e: &ViewerError) -> Self {
        match e {
            ViewerError::TunnelNotAllowed => Self::NotAllowed,
            ViewerError::TunnelRefused(r) => r.into(),
            ViewerError::TunnelBind { reason, .. } => Self::LocalPort(reason.clone()),
            ViewerError::TunnelLimit => Self::TooMany,
            ViewerError::TunnelSpec(_) => Self::BadTarget,
            ViewerError::SessionEnded => Self::SessionEnded,
            other => Self::Other(other.to_string()),
        }
    }
}

/// One open tunnel into this device (host side, for the banner).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostTunnelRow {
    pub id: u64,
    pub target: SocketAddr,
    pub active: usize,
    pub connections: u64,
}

impl From<&HostTunnelInfo> for HostTunnelRow {
    fn from(t: &HostTunnelInfo) -> Self {
        Self {
            id: t.id,
            target: t.target,
            active: t.active,
            connections: t.connections,
        }
    }
}

/// Maps a viewer-side tunnel feature event.
pub fn viewer_event(session: SessionId, event: &FeatureEvent) -> Option<BackendEvent> {
    match event.payload::<TunnelEvent>()? {
        TunnelEvent::Changed(list) => Some(BackendEvent::Tunnels {
            session,
            rules: list.iter().map(TunnelRow::from).collect(),
        }),
        TunnelEvent::Closed { reason, .. } => Some(BackendEvent::TunnelProblem {
            session,
            problem: reason.into(),
        }),
    }
}

/// Maps a host-side tunnel feature event; `allowed` is whether the session currently holds
/// `TUNNEL`.
pub fn host_event(session: SessionId, allowed: bool, event: &FeatureEvent) -> Option<BackendEvent> {
    match event.payload::<HostTunnelEvent>()? {
        HostTunnelEvent::Changed(list) => Some(BackendEvent::HostTunnels {
            session,
            allowed,
            tunnels: list.iter().map(HostTunnelRow::from).collect(),
        }),
        HostTunnelEvent::Refused { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tlp_desk_viewer::TunnelId;

    #[test]
    fn viewer_events_map_to_backend_events() {
        let status = TunnelStatus {
            id: TunnelId(3),
            local: "127.0.0.1:8080".parse().unwrap(),
            target_host: Some("192.168.1.5".into()),
            target_port: 80,
            active: 1,
            connections: 4,
            failed: 1,
            bytes_sent: 10,
            bytes_received: 20,
        };
        let ev = FeatureEvent::new("tunnel", 1, TunnelEvent::Changed(vec![status]));
        match viewer_event(SessionId(9), &ev) {
            Some(BackendEvent::Tunnels { session, rules }) => {
                assert_eq!(session, SessionId(9));
                assert_eq!(rules[0].target, "192.168.1.5:80");
                assert_eq!((rules[0].id, rules[0].connections), (3, 4));
            }
            other => panic!("{other:?}"),
        }
        let ev = FeatureEvent::new(
            "tunnel",
            1,
            TunnelEvent::Closed {
                id: TunnelId(3),
                reason: TunnelCloseReason::PermissionRevoked,
            },
        );
        assert!(matches!(
            viewer_event(SessionId(9), &ev),
            Some(BackendEvent::TunnelProblem {
                problem: TunnelProblem::Revoked,
                ..
            })
        ));
        // Other features' payloads are ignored.
        assert!(viewer_event(SessionId(9), &FeatureEvent::new("chat", 1, 5u32)).is_none());
    }

    #[test]
    fn errors_map_to_problems() {
        assert_eq!(
            TunnelProblem::from(&ViewerError::TunnelNotAllowed),
            TunnelProblem::NotAllowed
        );
        assert_eq!(
            TunnelProblem::from(&ViewerError::TunnelRefused(
                TunnelCloseReason::TargetNotAllowed
            )),
            TunnelProblem::TargetNotAllowed
        );
        assert_eq!(
            TunnelProblem::from(&TunnelCloseReason::Other("خطأ".into())),
            TunnelProblem::Other("خطأ".into())
        );
    }
}
