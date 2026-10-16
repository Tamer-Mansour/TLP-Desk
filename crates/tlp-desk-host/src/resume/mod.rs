//! Session resume after a network blip: get back into a running session without a new
//! Accept click.
//!
//! * At session start, and again every [`renew_every`], the host issues a single-use ticket
//!   from an in-memory [`TicketStore::session_resume`] store, bound to the viewer's TLS
//!   fingerprint and granting the session's **current** permissions, and sends it with
//!   `Message::ReconnectTicket` (the v4 message the restart-and-reconnect flow uses; the
//!   viewer keeps the newest one). A permission change re-issues it at once, so a reconnect
//!   never restores more than the host user allows right now.
//! * When the connection drops without a goodbye, the ticket stays valid until it expires
//!   ([`HostConfig::session_resume`], default [`RESUME_TICKET_TTL`]); the viewer reconnects
//!   with it and is let in with that grant. Any deliberate end (host user disconnects or
//!   revokes viewing, the viewer closes, the host shuts down) revokes it.
//! * Once the viewer asks for `RestartRemote { reconnect: true }`, resume tickets stop for the
//!   session: the restart ticket must stay the one the viewer holds.
//!
//! [`HostConfig::session_resume`]: crate::HostConfig::session_resume

use std::time::Duration;

use tlp_desk_crypto::{now_unix_ms, TicketRejected, TicketStore};
use tlp_desk_net::{NetError, PeerInfo, PendingSession, Session};
use tlp_desk_proto::{ByeReason, Message, Permissions, RejectReason};

use crate::unattended::verdict;
use crate::EndReason;

/// Default lifetime of a resume ticket. Renewed every [`renew_every`], so after a drop the
/// viewer has at least two minutes (TTL minus one renewal period) to get back in.
pub const RESUME_TICKET_TTL: Duration = Duration::from_secs(150);

/// How often a session's resume ticket is renewed: a fifth of the TTL, at least 5 s.
pub(crate) fn renew_every(ttl: Duration) -> Duration {
    (ttl / 5).max(Duration::from_secs(5))
}

/// Issues a fresh ticket for the session's peer granting `perms` and sends it. A send error
/// means the session is gone; an issue error (in-memory store: practically impossible) is
/// logged and the session continues without resume.
pub(crate) async fn send_ticket(
    store: &TicketStore,
    session: &Session,
    perms: Permissions,
) -> Result<(), NetError> {
    let peer = session.peer();
    match store.issue(&peer.fingerprint, perms.0, now_unix_ms()) {
        Ok(t) => {
            tracing::debug!(peer = %peer.addr, device_id = %peer.device_id, expires_unix_ms = t.expires_unix_ms, "session resume ticket issued");
            session
                .send(&Message::ReconnectTicket {
                    token: t.token.to_vec(),
                    expires_unix_ms: t.expires_unix_ms,
                })
                .await
        }
        Err(e) => {
            tracing::warn!(%e, "could not issue a session resume ticket");
            Ok(())
        }
    }
}

/// Authorizes a `ReconnectTicket` login: a session-resume ticket first, then (if the token is
/// not one of those) a restart ticket from `restart`. `None` stores refuse.
pub(crate) async fn redeem(
    resume: Option<&TicketStore>,
    restart: Option<&TicketStore>,
    p: PendingSession,
) -> Result<Session, NetError> {
    p.redeem_ticket(|token, fp| {
        let now = now_unix_ms();
        if let Some(store) = resume {
            match store.redeem(token, fp, now) {
                Err(TicketRejected::Unknown) => {}
                r => {
                    if let Err(why) = r {
                        tracing::warn!(?why, "session resume ticket rejected");
                    } else {
                        tracing::info!("session resumed with a ticket");
                    }
                    return verdict(r);
                }
            }
        }
        match restart {
            Some(store) => {
                let r = store.redeem(token, fp, now);
                if let Err(why) = r {
                    tracing::warn!(?why, "reconnect ticket rejected");
                }
                verdict(r)
            }
            None if resume.is_some() => Err(RejectReason::WrongCredentials),
            None => Err(RejectReason::NotAllowed),
        }
    })
    .await
}

/// Whether a session end keeps the peer's resume ticket: only a connection that dropped
/// without anybody deciding to end it (network loss, idle timeout) may be resumed.
pub(crate) fn keeps_ticket(reason: &EndReason, host_shutting_down: bool) -> bool {
    !host_shutting_down
        && matches!(
            reason,
            EndReason::ConnectionLost(_) | EndReason::Remote(ByeReason::Timeout)
        )
}

/// Revokes the peer's resume ticket unless the session may be resumed (see [`keeps_ticket`]).
pub(crate) fn after_session(
    store: Option<&TicketStore>,
    peer: &PeerInfo,
    reason: &EndReason,
    host_shutting_down: bool,
) {
    let Some(store) = store else { return };
    if keeps_ticket(reason, host_shutting_down) {
        tracing::info!(peer = %peer.addr, device_id = %peer.device_id, "connection lost; the viewer may resume with its ticket");
    } else {
        store.revoke_peer(&peer.fingerprint);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tlp_desk_crypto::Fingerprint;

    fn peer() -> PeerInfo {
        let fingerprint = Fingerprint([4; 32]);
        PeerInfo {
            addr: "192.168.1.20:47400".parse().unwrap(),
            device_id: fingerprint.device_id(),
            fingerprint,
            name: "حاسوب الزميل".into(),
        }
    }

    #[test]
    fn renewal_is_well_inside_the_ttl() {
        assert_eq!(renew_every(RESUME_TICKET_TTL), Duration::from_secs(30));
        assert_eq!(renew_every(Duration::from_secs(1)), Duration::from_secs(5));
        assert!(RESUME_TICKET_TTL - renew_every(RESUME_TICKET_TTL) >= Duration::from_secs(120));
    }

    #[test]
    fn only_dropped_connections_keep_the_ticket() {
        let store = TicketStore::session_resume(RESUME_TICKET_TTL);
        let p = peer();
        for (reason, shutting_down, kept) in [
            (EndReason::ConnectionLost("gone".into()), false, true),
            (EndReason::Remote(ByeReason::Timeout), false, true),
            (EndReason::ConnectionLost("gone".into()), true, false),
            (EndReason::Local, false, false),
            (EndReason::PermissionRevoked, false, false),
            (EndReason::Remote(ByeReason::UserClosed), false, false),
            (EndReason::Error("capture".into()), false, false),
        ] {
            let now = now_unix_ms();
            store.issue(&p.fingerprint, 1, now).unwrap();
            after_session(Some(&store), &p, &reason, shutting_down);
            assert_eq!(
                store.has_ticket(&p.fingerprint, now),
                kept,
                "{reason:?} shutting_down={shutting_down}"
            );
        }
    }
}
