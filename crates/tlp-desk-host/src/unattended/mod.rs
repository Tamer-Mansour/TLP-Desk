//! Unattended access, host side (M3): restart the PC on the viewer's request (optionally with a
//! reconnect ticket), lock the workstation, and authorize reconnect tickets after the reboot.
//!
//! * [`UnattendedFeature`] is a session [`Feature`]: register it in `HostConfig::features`. It
//!   handles `Message::RestartRemote` (needs `RESTART`) and `Message::LockWorkstation` (needs
//!   `SYSTEM`); both permissions are re-checked live when the message is handled.
//! * With `RestartRemote { reconnect: true }` it issues a ticket from the shared
//!   [`TicketStore`] (bound to the viewer's TLS fingerprint, granting the session's current
//!   permissions), sends `Message::ReconnectTicket`, then restarts. A failed restart revokes the
//!   ticket. A permission change revokes it too, and so does an explicit disconnect (see
//!   [`revoke_after_session`]).
//! * After the reboot the viewer presents the ticket; [`redeem`] checks it against the same
//!   store (loaded again from disk by the new process, see `HostConfig::reconnect_tickets`);
//!   the host's auth path does this in `resume::redeem`.
//! * The OS calls sit behind [`SystemPower`]. [`WindowsPower`] restarts and locks the real
//!   machine; tests use a fake and must never use it.
//!
//! Reconnecting before anyone logs in needs the TLP-Desk service (it starts the host at boot);
//! without it the viewer gets back in once a user logs in and TLP-Desk starts.

mod windows;

use std::sync::Arc;

use tlp_desk_crypto::{now_unix_ms, TicketRejected, TicketStore};
use tlp_desk_net::{Feature, FeatureCtx};
use tlp_desk_proto::{ByeReason, Message, Permissions, RejectReason};
use tokio::sync::mpsc;

pub use windows::WindowsPower;

use crate::{EndReason, HostError};

/// Restart / lock the host machine. Implementations must return quickly (the restart itself
/// happens asynchronously in the OS) and are called off the async runtime.
pub trait SystemPower: Send + Sync + 'static {
    /// Starts a restart of the whole machine (running programs are closed).
    ///
    /// # Errors
    /// The OS refused (no privilege, a shutdown already in progress, unsupported OS).
    fn restart(&self) -> Result<(), HostError>;

    /// Locks the interactive workstation.
    ///
    /// # Errors
    /// The OS refused or no interactive session is reachable from this process.
    fn lock_workstation(&self) -> Result<(), HostError>;
}

/// What happened, for the host UI / log ([`crate::HostEvent::Feature`] with
/// `feature == "unattended"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnattendedEvent {
    /// The viewer asked to restart this PC and the restart was started.
    RestartStarted {
        /// A reconnect ticket was sent to the viewer.
        reconnect: bool,
    },
    /// The restart could not be started (the ticket, if any, was revoked).
    RestartFailed(String),
    /// The workstation was locked at the viewer's request.
    Locked,
    /// Locking failed.
    LockFailed(String),
}

/// [`Feature::name`] of [`UnattendedFeature`].
pub const FEATURE_NAME: &str = "unattended";

/// The host-side session feature for restart and lock. See the module docs.
pub struct UnattendedFeature {
    tickets: Option<TicketStore>,
    power: Arc<dyn SystemPower>,
}

impl std::fmt::Debug for UnattendedFeature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnattendedFeature")
            .field("tickets", &self.tickets.is_some())
            .finish_non_exhaustive()
    }
}

impl UnattendedFeature {
    /// `tickets`: the store shared with `HostConfig::reconnect_tickets` (`None` = restart
    /// without reconnect tickets). `power`: [`WindowsPower`] in the app, a fake in tests.
    pub fn new(tickets: Option<TicketStore>, power: Arc<dyn SystemPower>) -> Self {
        Self { tickets, power }
    }
}

impl Feature for UnattendedFeature {
    fn name(&self) -> &'static str {
        FEATURE_NAME
    }

    fn handles(&self, msg: &Message) -> bool {
        matches!(
            msg,
            Message::RestartRemote { .. } | Message::LockWorkstation
        )
    }

    fn start(&self, ctx: FeatureCtx, inbox: mpsc::UnboundedReceiver<Message>) {
        tokio::spawn(run(
            ctx,
            inbox,
            self.tickets.clone(),
            Arc::clone(&self.power),
        ));
    }
}

async fn run(
    mut ctx: FeatureCtx,
    mut inbox: mpsc::UnboundedReceiver<Message>,
    tickets: Option<TicketStore>,
    power: Arc<dyn SystemPower>,
) {
    let peer = ctx.session.peer().clone();
    let mut perms_open = true;
    loop {
        tokio::select! {
            msg = inbox.recv() => match msg {
                Some(Message::RestartRemote { reconnect }) => {
                    restart(&ctx, reconnect, tickets.as_ref(), &power).await;
                }
                Some(Message::LockWorkstation) => lock(&ctx, &power).await,
                Some(_) => {}
                None => break,
            },
            changed = ctx.permissions.changed(), if perms_open => {
                if changed.is_err() {
                    perms_open = false;
                    continue;
                }
                // Whatever changed, a ticket must not outlive the grant it was issued for.
                if let Some(t) = &tickets {
                    if t.revoke_peer(&peer.fingerprint) > 0 {
                        tracing::info!(peer = %peer.addr, device_id = %peer.device_id, "permissions changed; reconnect ticket revoked");
                    }
                }
            }
        }
    }
}

async fn restart(
    ctx: &FeatureCtx,
    reconnect: bool,
    tickets: Option<&TicketStore>,
    power: &Arc<dyn SystemPower>,
) {
    let peer = ctx.session.peer();
    if !ctx.allowed(Permissions::RESTART) {
        tracing::warn!(peer = %peer.addr, device_id = %peer.device_id, "restart refused: no RESTART permission");
        return;
    }
    let mut ticket_sent = false;
    if reconnect {
        match tickets {
            Some(store) => {
                let grant = *ctx.permissions.borrow();
                match store.issue(&peer.fingerprint, grant.0, now_unix_ms()) {
                    Ok(t) => {
                        let msg = Message::ReconnectTicket {
                            token: t.token.to_vec(),
                            expires_unix_ms: t.expires_unix_ms,
                        };
                        match ctx.session.send(&msg).await {
                            Ok(()) => ticket_sent = true,
                            Err(e) => {
                                tracing::warn!(%e, "could not send the reconnect ticket");
                                store.revoke_peer(&peer.fingerprint);
                            }
                        }
                    }
                    Err(e) => tracing::warn!(%e, "could not issue a reconnect ticket"),
                }
            }
            None => tracing::info!("reconnect tickets are disabled; restarting without one"),
        }
    }
    tracing::warn!(peer = %peer.addr, device_id = %peer.device_id, reconnect = ticket_sent, "restarting this PC at the viewer's request");
    let p = Arc::clone(power);
    let result = match tokio::task::spawn_blocking(move || p.restart()).await {
        Ok(r) => r,
        Err(e) => Err(HostError::System(format!("restart task failed: {e}"))),
    };
    match result {
        Ok(()) => ctx.emit(
            FEATURE_NAME,
            UnattendedEvent::RestartStarted {
                reconnect: ticket_sent,
            },
        ),
        Err(e) => {
            tracing::error!(%e, "restart failed");
            if let Some(store) = tickets {
                store.revoke_peer(&peer.fingerprint);
            }
            ctx.emit(FEATURE_NAME, UnattendedEvent::RestartFailed(e.to_string()));
        }
    }
}

async fn lock(ctx: &FeatureCtx, power: &Arc<dyn SystemPower>) {
    let peer = ctx.session.peer();
    if !ctx.allowed(Permissions::SYSTEM) {
        tracing::warn!(peer = %peer.addr, device_id = %peer.device_id, "lock refused: no SYSTEM permission");
        return;
    }
    tracing::info!(peer = %peer.addr, device_id = %peer.device_id, "locking the workstation at the viewer's request");
    let p = Arc::clone(power);
    let result = match tokio::task::spawn_blocking(move || p.lock_workstation()).await {
        Ok(r) => r,
        Err(e) => Err(HostError::System(format!("lock task failed: {e}"))),
    };
    match result {
        Ok(()) => ctx.emit(FEATURE_NAME, UnattendedEvent::Locked),
        Err(e) => {
            tracing::warn!(%e, "lock failed");
            ctx.emit(FEATURE_NAME, UnattendedEvent::LockFailed(e.to_string()));
        }
    }
}

/// Maps a ticket store verdict to what the viewer is told. Only "not restarted yet" invites a
/// retry; everything else is a plain wrong credential.
pub(crate) fn verdict(r: Result<u16, TicketRejected>) -> Result<Permissions, RejectReason> {
    match r {
        Ok(bits) => Ok(Permissions(bits & Permissions::ALL.0)),
        Err(TicketRejected::NotRestartedYet) => Err(RejectReason::NotAllowed),
        Err(TicketRejected::Unknown | TicketRejected::WrongPeer | TicketRejected::Expired) => {
            Err(RejectReason::WrongCredentials)
        }
    }
}

/// Revokes the peer's tickets when its session ended on purpose (host user disconnected it,
/// the viewer closed it, or viewing was revoked), but not when the host itself is shutting
/// down for the restart.
pub(crate) fn revoke_after_session(
    store: Option<&TicketStore>,
    peer: &tlp_desk_net::PeerInfo,
    reason: &EndReason,
    host_shutting_down: bool,
) {
    let Some(store) = store else { return };
    let explicit = matches!(
        reason,
        EndReason::Local | EndReason::PermissionRevoked | EndReason::Remote(ByeReason::UserClosed)
    );
    if explicit && !host_shutting_down && store.revoke_peer(&peer.fingerprint) > 0 {
        tracing::info!(peer = %peer.addr, device_id = %peer.device_id, "session ended; reconnect ticket revoked");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tlp_desk_crypto::{Fingerprint, DEFAULT_TICKET_TTL};

    fn peer(fp: u8) -> tlp_desk_net::PeerInfo {
        let fingerprint = Fingerprint([fp; 32]);
        tlp_desk_net::PeerInfo {
            addr: "192.168.1.9:47400".parse().unwrap(),
            device_id: fingerprint.device_id(),
            fingerprint,
            name: "جهاز المكتب".into(),
        }
    }

    #[test]
    fn verdicts_map_to_wire_reasons() {
        assert_eq!(verdict(Ok(0xffff)), Ok(Permissions::ALL));
        assert_eq!(
            verdict(Err(TicketRejected::NotRestartedYet)),
            Err(RejectReason::NotAllowed)
        );
        for r in [
            TicketRejected::Unknown,
            TicketRejected::WrongPeer,
            TicketRejected::Expired,
        ] {
            assert_eq!(verdict(Err(r)), Err(RejectReason::WrongCredentials));
        }
    }

    #[test]
    fn explicit_ends_revoke_tickets_but_host_shutdown_does_not() {
        let store = TicketStore::in_memory(DEFAULT_TICKET_TTL);
        let p = peer(3);
        let now = now_unix_ms();
        for (reason, shutting_down, revoked) in [
            (EndReason::Local, true, false),
            (EndReason::ConnectionLost("gone".into()), false, false),
            (EndReason::Remote(ByeReason::Timeout), false, false),
            (EndReason::Local, false, true),
            (EndReason::PermissionRevoked, false, true),
            (EndReason::Remote(ByeReason::UserClosed), false, true),
        ] {
            store.issue(&p.fingerprint, 1, now).unwrap();
            revoke_after_session(Some(&store), &p, &reason, shutting_down);
            assert_eq!(
                !store.has_ticket(&p.fingerprint, now),
                revoked,
                "{reason:?} shutting_down={shutting_down}"
            );
        }
    }
}
