//! Restart the remote PC and get back in automatically (M3, viewer side).
//!
//! * [`ReconnectTickets`] keeps the single-use ticket a host sends (`Message::ReconnectTicket`)
//!   after `RestartRemote { reconnect: true }`. Register [`ReconnectTickets::feature`] in
//!   `ViewerConfig::features`; it reports [`TicketReceived`] through `ViewerEvent::Feature`.
//! * [`Viewer::reconnect_with_ticket`] waits for the host to come back and logs in with the
//!   ticket: it dials the last known address and, if that fails, the device ID through LAN
//!   discovery, always pinned to the host's full key fingerprint. It retries with growing
//!   delays (kept under the host's 5-attempts-per-minute limit) until the ticket expires or
//!   [`ReconnectPolicy::give_up_after`], whichever comes first.
//!
//! Getting back in before anyone logs in needs the TLP-Desk service on the host; without it
//! the host answers once a user logs in and TLP-Desk starts.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tlp_desk_crypto::{now_unix_ms, DeviceId, Fingerprint};
use tlp_desk_net::{AuthMethod, Feature, FeatureCtx, Pin};
use tlp_desk_proto::{Message, RejectReason};
use tokio::sync::mpsc;

use crate::{map_net_error, Viewer, ViewerError, ViewerEvents, ViewerSession};

/// [`Feature::name`] of the ticket keeper.
pub const RECONNECT_FEATURE: &str = "reconnect";

/// Feature event payload: the host sent a reconnect ticket for this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TicketReceived {
    /// Milliseconds since the Unix epoch after which the host refuses it.
    pub expires_unix_ms: u64,
}

/// A reconnect ticket and where it came from. The token is secret: never logged.
#[derive(Clone, PartialEq, Eq)]
pub struct HeldTicket {
    /// The host's device ID (authenticated).
    pub host: DeviceId,
    /// The host's full key fingerprint: the reconnect is pinned to it.
    pub host_fp: Fingerprint,
    /// Where the host was reached last.
    pub last_addr: SocketAddr,
    /// Milliseconds since the Unix epoch after which the host refuses it.
    pub expires_unix_ms: u64,
    token: Vec<u8>,
}

impl std::fmt::Debug for HeldTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeldTicket")
            .field("host", &self.host)
            .field("last_addr", &self.last_addr)
            .field("expires_unix_ms", &self.expires_unix_ms)
            .finish_non_exhaustive()
    }
}

/// Largest token accepted from a host (the host sends 32 bytes).
const MAX_TOKEN: usize = 256;
/// At most this many tickets are held (one per session that asked for a restart).
const MAX_HELD: usize = 32;

/// Tickets received per session (`ViewerSession::feature_key`). Cheap to clone.
#[derive(Clone, Default)]
pub struct ReconnectTickets {
    inner: Arc<Mutex<HashMap<u64, HeldTicket>>>,
}

impl std::fmt::Debug for ReconnectTickets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReconnectTickets")
            .field("held", &self.lock().len())
            .finish()
    }
}

impl ReconnectTickets {
    /// An empty keeper.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<u64, HeldTicket>> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The feature to register in `ViewerConfig::features`.
    pub fn feature(&self) -> Arc<dyn Feature> {
        Arc::new(TicketFeature(self.clone()))
    }

    /// Removes and returns the ticket of session `session_key`, if one arrived and has not
    /// expired.
    pub fn take(&self, session_key: u64) -> Option<HeldTicket> {
        self.lock()
            .remove(&session_key)
            .filter(|t| t.expires_unix_ms > now_unix_ms())
    }

    /// Whether session `session_key` holds an unexpired ticket.
    pub fn has(&self, session_key: u64) -> bool {
        self.lock()
            .get(&session_key)
            .is_some_and(|t| t.expires_unix_ms > now_unix_ms())
    }

    /// Forgets the ticket of `session_key` (the user gave up).
    pub fn discard(&self, session_key: u64) {
        self.lock().remove(&session_key);
    }

    fn store(&self, session_key: u64, ticket: HeldTicket) {
        let mut m = self.lock();
        let now = now_unix_ms();
        m.retain(|_, t| t.expires_unix_ms > now);
        if m.len() >= MAX_HELD && !m.contains_key(&session_key) {
            if let Some(oldest) = m
                .iter()
                .min_by_key(|(_, t)| t.expires_unix_ms)
                .map(|(k, _)| *k)
            {
                m.remove(&oldest);
            }
        }
        m.insert(session_key, ticket);
    }
}

struct TicketFeature(ReconnectTickets);

impl Feature for TicketFeature {
    fn name(&self) -> &'static str {
        RECONNECT_FEATURE
    }

    fn handles(&self, msg: &Message) -> bool {
        matches!(msg, Message::ReconnectTicket { .. })
    }

    fn start(&self, ctx: FeatureCtx, mut inbox: mpsc::UnboundedReceiver<Message>) {
        let tickets = self.0.clone();
        tokio::spawn(async move {
            while let Some(msg) = inbox.recv().await {
                let Message::ReconnectTicket {
                    token,
                    expires_unix_ms,
                } = msg
                else {
                    continue;
                };
                if token.is_empty() || token.len() > MAX_TOKEN {
                    tracing::warn!(len = token.len(), "ignoring a malformed reconnect ticket");
                    continue;
                }
                let peer = ctx.session.peer();
                tracing::info!(host = %peer.device_id, expires_unix_ms, "reconnect ticket received");
                tickets.store(
                    ctx.session_key,
                    HeldTicket {
                        host: peer.device_id,
                        host_fp: peer.fingerprint,
                        last_addr: peer.addr,
                        expires_unix_ms,
                        token,
                    },
                );
                ctx.emit(RECONNECT_FEATURE, TicketReceived { expires_unix_ms });
            }
            // The ticket outlives the session on purpose: it is used after the host is gone.
        });
    }
}

/// How long and how often [`Viewer::reconnect_with_ticket`] tries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectPolicy {
    /// Stop after this long (also stops when the ticket expires). Default 5 minutes.
    pub give_up_after: Duration,
    /// Wait before the first try (the host needs time to go down). Default 5 s.
    pub first_delay: Duration,
    /// Longest wait between tries. Default 30 s.
    pub max_delay: Duration,
    /// Limit for one connection try. Default 15 s.
    pub attempt_timeout: Duration,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            give_up_after: Duration::from_secs(5 * 60),
            first_delay: Duration::from_secs(5),
            max_delay: Duration::from_secs(30),
            attempt_timeout: Duration::from_secs(15),
        }
    }
}

impl ReconnectPolicy {
    /// Delay before try number `n` (0-based): `first_delay`, then growing by half each time,
    /// never below 12 s after the first so the host's 5-per-minute limit is not hit, capped at
    /// `max_delay`.
    pub fn delay(&self, n: u32) -> Duration {
        if n == 0 {
            return self.first_delay;
        }
        let floor = Duration::from_secs(12).min(self.max_delay);
        let grown = self
            .first_delay
            .mul_f64(1.5f64.powi(i32::try_from(n).unwrap_or(i32::MAX)));
        grown.max(floor).min(self.max_delay)
    }
}

/// Progress callback of [`Viewer::reconnect_with_ticket`]: called with the try number (1-based)
/// just before each try.
pub type AttemptFn = Arc<dyn Fn(u32) + Send + Sync>;

/// Whether a failed try is worth repeating.
fn retryable(e: &ViewerError) -> bool {
    match e {
        ViewerError::Unreachable(_) | ViewerError::NotFound(_) | ViewerError::Net(_) => true,
        // The old host process has not gone down yet.
        ViewerError::Rejected(RejectReason::NotAllowed) => true,
        _ => false,
    }
}

impl Viewer {
    /// Waits for the host of `ticket` to come back (e.g. after a restart) and logs in with the
    /// ticket. Cancel by dropping the future.
    ///
    /// # Errors
    /// The ticket was refused (`Rejected(WrongCredentials)`: expired, used or revoked), the
    /// host's key changed ([`ViewerError::HostKeyChanged`]: do not retry silently), the peer is
    /// locked out, or the deadline passed (the last transport error).
    pub async fn reconnect_with_ticket(
        &self,
        ticket: HeldTicket,
        policy: ReconnectPolicy,
        on_attempt: Option<AttemptFn>,
    ) -> Result<(ViewerSession, ViewerEvents), ViewerError> {
        let now_ms = now_unix_ms();
        let ticket_left = Duration::from_millis(ticket.expires_unix_ms.saturating_sub(now_ms));
        let deadline = Instant::now() + policy.give_up_after.min(ticket_left);
        let mut last_err = ViewerError::Unreachable("the host did not come back".into());
        for n in 0u32.. {
            let wait = policy.delay(n);
            if Instant::now() + wait >= deadline {
                break;
            }
            tokio::time::sleep(wait).await;
            if let Some(f) = &on_attempt {
                f(n.saturating_add(1));
            }
            let attempt = tokio::time::timeout(policy.attempt_timeout, self.try_ticket(&ticket));
            match attempt.await {
                Ok(Ok(done)) => return Ok(done),
                Ok(Err(e)) if retryable(&e) => {
                    tracing::info!(try_no = n + 1, %e, "host not back yet");
                    last_err = e;
                }
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    tracing::info!(try_no = n + 1, "reconnect try timed out");
                    last_err = ViewerError::Unreachable("timed out".into());
                }
            }
        }
        Err(last_err)
    }

    /// One try: the last address first, then wherever discovery finds the device ID.
    async fn try_ticket(
        &self,
        ticket: &HeldTicket,
    ) -> Result<(ViewerSession, ViewerEvents), ViewerError> {
        let mut addrs = vec![ticket.last_addr];
        let mut err = None;
        let mut tried_discovery = false;
        let mut i = 0;
        while let Some(addr) = addrs.get(i).copied() {
            i += 1;
            match self.dial_ticket(addr, ticket).await {
                Ok(done) => return Ok(done),
                Err(e) if retryable(&e) => err = Some(e),
                Err(e) => return Err(e),
            }
            if i == addrs.len() && !tried_discovery {
                tried_discovery = true;
                if let Some(d) = self.find(|d| d.device_id == ticket.host).await {
                    addrs.extend(d.addrs.into_iter().filter(|a| *a != ticket.last_addr));
                }
            }
        }
        Err(err.unwrap_or_else(|| ViewerError::NotFound(ticket.host.to_string())))
    }

    async fn dial_ticket(
        &self,
        addr: SocketAddr,
        ticket: &HeldTicket,
    ) -> Result<(ViewerSession, ViewerEvents), ViewerError> {
        if !self.cfg.allow_list.is_allowed(addr.ip()) {
            return Err(ViewerError::NotAllowed(addr));
        }
        let pin = Pin::Fingerprint(ticket.host_fp);
        let key = tlp_desk_crypto::HostKey::Id(ticket.host);
        let session = self
            .endpoint
            .connect_pinned(addr, pin, AuthMethod::ReconnectTicket(ticket.token.clone()))
            .await
            .map_err(|e| map_net_error(e, pin, &key))?;
        tracing::info!(host = %ticket.host, %addr, "reconnected with ticket");
        Ok(ViewerSession::start(
            session,
            self.cfg.pixel_format,
            self.cfg.on_frame.clone(),
            &self.cfg.features,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_grow_and_respect_the_rate_limit() {
        let p = ReconnectPolicy::default();
        assert_eq!(p.delay(0), Duration::from_secs(5));
        let mut total = Duration::ZERO;
        let mut in_first_minute = 0;
        for n in 0..40 {
            let d = p.delay(n);
            assert!(d <= p.max_delay);
            if n > 0 {
                assert!(d >= Duration::from_secs(12), "{n}: {d:?}");
                assert!(d >= p.delay(n - 1));
            }
            total += d;
            if total <= Duration::from_secs(60) {
                in_first_minute += 1;
            }
        }
        assert!(in_first_minute <= 5, "{in_first_minute}");
        assert_eq!(p.delay(39), Duration::from_secs(30));
    }

    #[test]
    fn only_transient_errors_are_retried() {
        assert!(retryable(&ViewerError::Unreachable("x".into())));
        assert!(retryable(&ViewerError::Rejected(RejectReason::NotAllowed)));
        assert!(!retryable(&ViewerError::Rejected(
            RejectReason::WrongCredentials
        )));
        assert!(!retryable(&ViewerError::Rejected(
            RejectReason::RateLimited
        )));
        assert!(!retryable(&ViewerError::HostAuthFailed));
    }

    #[test]
    fn keeper_takes_once_and_hides_the_token() {
        let keeper = ReconnectTickets::new();
        let fp = Fingerprint([4; 32]);
        let t = HeldTicket {
            host: fp.device_id(),
            host_fp: fp,
            last_addr: "192.168.1.5:47400".parse().unwrap(),
            expires_unix_ms: now_unix_ms() + 60_000,
            token: vec![0xAB; 32],
        };
        assert!(!format!("{t:?}").contains("171"));
        keeper.store(7, t.clone());
        assert!(keeper.has(7));
        assert_eq!(keeper.take(7), Some(t.clone()));
        assert_eq!(keeper.take(7), None);
        // expired tickets are not handed out
        keeper.store(
            8,
            HeldTicket {
                expires_unix_ms: 1,
                ..t
            },
        );
        assert!(!keeper.has(8));
        assert_eq!(keeper.take(8), None);
    }
}
