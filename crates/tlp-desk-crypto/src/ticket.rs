//! Single-use reconnect tickets (unattended restart, protocol v4).
//!
//! When a viewer asks the host to restart with `reconnect: true`, the host issues a random
//! 256-bit token bound to the viewer's TLS fingerprint and sends it to the viewer. After the
//! reboot the viewer presents it with `AuthRequest::ReconnectTicket` and is let in without a
//! prompt, with the permissions it had before.
//!
//! Rules enforced here (pure logic plus an optional file, no network):
//!
//! * the store keeps only `SHA-256(token)`, never the token;
//! * a ticket works once (redeeming removes it, success or not when it was presented by the
//!   wrong peer), only for the fingerprint it was issued to, and only until it expires;
//! * a ticket cannot be redeemed by the same process instance that issued it: the restart is
//!   still in progress then and the old process would swallow the ticket just before it dies.
//!   After a reboot (or an app restart) the store is loaded by a new instance;
//! * one live ticket per viewer; the host revokes them on permission changes and explicit
//!   disconnects ([`TicketStore::revoke_peer`]).

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::{CryptoError, Fingerprint};

/// Length of a reconnect token in bytes (256 bits).
pub const TICKET_LEN: usize = 32;
/// How long a ticket stays valid by default.
pub const DEFAULT_TICKET_TTL: Duration = Duration::from_secs(10 * 60);
/// File name of the persisted store inside the host's data directory.
pub const TICKETS_FILE: &str = "reconnect_tickets";
/// At most this many live tickets are kept; the oldest is dropped beyond it.
const MAX_TICKETS: usize = 64;

/// A freshly issued ticket: send `token` to the viewer, never store or log it.
#[derive(Clone, PartialEq, Eq)]
pub struct IssuedTicket {
    /// The secret the viewer presents after the restart.
    pub token: [u8; TICKET_LEN],
    /// Milliseconds since the Unix epoch after which the ticket is refused.
    pub expires_unix_ms: u64,
}

impl std::fmt::Debug for IssuedTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedTicket")
            .field("expires_unix_ms", &self.expires_unix_ms)
            .finish_non_exhaustive()
    }
}

/// Why [`TicketStore::redeem`] refused a ticket. Callers answer every variant with the same
/// rejection on the wire; the variant is for the local log only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketRejected {
    /// Wrong length, never issued, already used or revoked.
    Unknown,
    /// Issued to another viewer (the ticket is burned).
    WrongPeer,
    /// Past its expiry (the ticket is removed).
    Expired,
    /// Issued by this very process instance: the restart has not happened yet. The ticket is
    /// kept for the next instance.
    NotRestartedYet,
}

#[derive(Clone)]
struct Entry {
    hash: [u8; 32],
    peer: Fingerprint,
    expires_unix_ms: u64,
    grant: u16,
    /// `0` = loaded from disk (issued by an earlier process instance).
    instance: u64,
}

struct State {
    path: Option<PathBuf>,
    ttl: Duration,
    instance: u64,
    entries: Vec<Entry>,
}

/// The host's reconnect tickets. Cheap to clone; clones share state (the host's auth path and
/// the restart feature hold the same store).
#[derive(Clone)]
pub struct TicketStore {
    inner: Arc<Mutex<State>>,
}

impl std::fmt::Debug for TicketStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.lock();
        f.debug_struct("TicketStore")
            .field("path", &s.path)
            .field("live", &s.entries.len())
            .finish_non_exhaustive()
    }
}

impl Default for TicketStore {
    fn default() -> Self {
        Self::in_memory(DEFAULT_TICKET_TTL)
    }
}

/// Milliseconds since the Unix epoch (0 if the clock is before 1970).
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn new_instance() -> u64 {
    // Never 0: that value marks entries loaded from disk.
    OsRng.next_u64().max(1)
}

fn hash(token: &[u8]) -> [u8; 32] {
    Sha256::digest(token).into()
}

/// Constant-time comparison of two equal-length byte strings.
fn ct_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl TicketStore {
    /// A store that lives only in this process (tickets do not survive a reboot).
    pub fn in_memory(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(State {
                path: None,
                ttl,
                instance: new_instance(),
                entries: Vec::new(),
            })),
        }
    }

    /// Loads the store persisted at `path` (missing file = empty). Expired and malformed lines
    /// are dropped (malformed ones logged). Every change is written back to `path`.
    ///
    /// # Errors
    /// The file exists but cannot be read.
    pub fn load(path: &Path, ttl: Duration) -> Result<Self, CryptoError> {
        let text = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e.into()),
        };
        let now = now_unix_ms();
        let mut entries = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match parse_line(line) {
                Some(e) if e.expires_unix_ms > now => entries.push(e),
                Some(_) => {}
                None => tracing::warn!(line = n + 1, "ignoring malformed reconnect ticket line"),
            }
            if entries.len() >= MAX_TICKETS {
                break;
            }
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(State {
                path: Some(path.to_owned()),
                ttl,
                instance: new_instance(),
                entries,
            })),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Issues a ticket for `peer` that grants `grant` (opaque permission bits; the host stores
    /// `Permissions.0`). Replaces any earlier ticket of the same peer. The store is saved before
    /// this returns, so the ticket survives an immediate restart.
    ///
    /// # Errors
    /// The store could not be written (the ticket is not issued then).
    pub fn issue(
        &self,
        peer: &Fingerprint,
        grant: u16,
        now_unix_ms: u64,
    ) -> Result<IssuedTicket, CryptoError> {
        let mut token = [0u8; TICKET_LEN];
        OsRng.fill_bytes(&mut token);
        let mut s = self.lock();
        let ttl_ms = u64::try_from(s.ttl.as_millis()).unwrap_or(u64::MAX);
        let expires_unix_ms = now_unix_ms.saturating_add(ttl_ms);
        let before = s.entries.clone();
        s.entries
            .retain(|e| e.peer != *peer && e.expires_unix_ms > now_unix_ms);
        if s.entries.len() >= MAX_TICKETS {
            s.entries.remove(0);
        }
        let instance = s.instance;
        s.entries.push(Entry {
            hash: hash(&token),
            peer: *peer,
            expires_unix_ms,
            grant,
            instance,
        });
        if let Err(e) = save(&s) {
            s.entries = before;
            return Err(e);
        }
        Ok(IssuedTicket {
            token,
            expires_unix_ms,
        })
    }

    /// Checks and consumes a ticket presented by `peer`. Returns the granted permission bits.
    ///
    /// # Errors
    /// [`TicketRejected`] says why (for the log; tell the peer nothing more than "rejected").
    pub fn redeem(
        &self,
        token: &[u8],
        peer: &Fingerprint,
        now_unix_ms: u64,
    ) -> Result<u16, TicketRejected> {
        if token.len() != TICKET_LEN {
            return Err(TicketRejected::Unknown);
        }
        let wanted = hash(token);
        let mut s = self.lock();
        let Some(pos) = s.entries.iter().position(|e| ct_eq(&e.hash, &wanted)) else {
            return Err(TicketRejected::Unknown);
        };
        let Some(entry) = s.entries.get(pos).cloned() else {
            return Err(TicketRejected::Unknown);
        };
        if entry.instance == s.instance && entry.peer == *peer {
            return Err(TicketRejected::NotRestartedYet);
        }
        s.entries.remove(pos);
        if let Err(e) = save(&s) {
            // Consumed in memory anyway: a ticket must never work twice.
            tracing::warn!(%e, "could not persist the consumed reconnect ticket");
        }
        if entry.peer != *peer {
            return Err(TicketRejected::WrongPeer);
        }
        if entry.expires_unix_ms <= now_unix_ms {
            return Err(TicketRejected::Expired);
        }
        Ok(entry.grant)
    }

    /// Revokes every ticket issued to `peer` (permission change, explicit disconnect, failed
    /// restart). Returns how many were removed.
    pub fn revoke_peer(&self, peer: &Fingerprint) -> usize {
        let mut s = self.lock();
        let before = s.entries.len();
        s.entries.retain(|e| e.peer != *peer);
        let removed = before - s.entries.len();
        if removed > 0 {
            if let Err(e) = save(&s) {
                tracing::warn!(%e, "could not persist revoked reconnect tickets");
            }
        }
        removed
    }

    /// Revokes every ticket.
    pub fn revoke_all(&self) {
        let mut s = self.lock();
        if !s.entries.is_empty() {
            s.entries.clear();
            if let Err(e) = save(&s) {
                tracing::warn!(%e, "could not persist revoked reconnect tickets");
            }
        }
    }

    /// Whether a live (unexpired) ticket exists for `peer`.
    pub fn has_ticket(&self, peer: &Fingerprint, now_unix_ms: u64) -> bool {
        self.lock()
            .entries
            .iter()
            .any(|e| e.peer == *peer && e.expires_unix_ms > now_unix_ms)
    }

    /// Number of stored tickets (including expired ones not yet cleaned up).
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    /// Whether no ticket is stored.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The same tickets as seen by a fresh process instance (as after a reboot). For tests of
    /// the in-memory store; a persistent store gets this by being loaded again.
    #[doc(hidden)]
    pub fn simulate_restart(&self) -> Self {
        let s = self.lock();
        let mut entries = s.entries.clone();
        for e in &mut entries {
            e.instance = 0;
        }
        Self {
            inner: Arc::new(Mutex::new(State {
                path: s.path.clone(),
                ttl: s.ttl,
                instance: new_instance(),
                entries,
            })),
        }
    }
}

fn parse_line(line: &str) -> Option<Entry> {
    let mut parts = line.split_whitespace();
    let hash = Fingerprint::from_hex(parts.next()?).ok()?.0;
    let peer = Fingerprint::from_hex(parts.next()?).ok()?;
    let expires_unix_ms = parts.next()?.parse().ok()?;
    let grant = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(Entry {
        hash,
        peer,
        expires_unix_ms,
        grant,
        instance: 0,
    })
}

/// Writes the store atomically (temp file + rename). No-op for an in-memory store.
fn save(s: &State) -> Result<(), CryptoError> {
    let Some(path) = &s.path else {
        return Ok(());
    };
    let mut text = String::from("# TLP-Desk reconnect tickets: sha256(token) peer expires grant\n");
    for e in &s.entries {
        text.push_str(&format!(
            "{} {} {} {}\n",
            Fingerprint(e.hash).to_hex(),
            e.peer.to_hex(),
            e.expires_unix_ms,
            e.grant
        ));
    }
    write_private(path, text.as_bytes())
}

/// Atomically writes a small private file, creating (and hardening) its directory if needed.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), CryptoError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    if !dir.as_os_str().is_empty() && !dir.exists() {
        fs::create_dir_all(dir)?;
        crate::harden_dir(dir);
    }
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_800_000_000_000;

    fn fp(n: u8) -> Fingerprint {
        Fingerprint([n; 32])
    }

    #[test]
    fn ticket_works_once_after_restart() {
        let store = TicketStore::in_memory(DEFAULT_TICKET_TTL);
        let t = store.issue(&fp(1), 0x0fff, T0).unwrap();
        assert_eq!(t.expires_unix_ms, T0 + 600_000);
        // The issuing instance refuses it and keeps it.
        assert_eq!(
            store.redeem(&t.token, &fp(1), T0 + 1),
            Err(TicketRejected::NotRestartedYet)
        );
        let after = store.simulate_restart();
        assert_eq!(after.redeem(&t.token, &fp(1), T0 + 1_000), Ok(0x0fff));
        // single use
        assert_eq!(
            after.redeem(&t.token, &fp(1), T0 + 2_000),
            Err(TicketRejected::Unknown)
        );
    }

    #[test]
    fn ticket_expires() {
        let store = TicketStore::in_memory(Duration::from_secs(60)).simulate_restart();
        let t = store.issue(&fp(1), 1, T0).unwrap();
        let after = store.simulate_restart();
        assert_eq!(
            after.redeem(&t.token, &fp(1), T0 + 60_000),
            Err(TicketRejected::Expired)
        );
        // removed, not just refused
        assert!(after.is_empty());
    }

    #[test]
    fn ticket_is_bound_to_the_fingerprint_and_burned_on_misuse() {
        let store = TicketStore::in_memory(DEFAULT_TICKET_TTL);
        let t = store.issue(&fp(1), 1, T0).unwrap();
        let after = store.simulate_restart();
        assert_eq!(
            after.redeem(&t.token, &fp(2), T0 + 1),
            Err(TicketRejected::WrongPeer)
        );
        // Burned: the rightful viewer cannot use a leaked ticket either.
        assert_eq!(
            after.redeem(&t.token, &fp(1), T0 + 2),
            Err(TicketRejected::Unknown)
        );
    }

    #[test]
    fn garbage_and_wrong_length_tokens_are_unknown() {
        let store = TicketStore::in_memory(DEFAULT_TICKET_TTL);
        store.issue(&fp(1), 1, T0).unwrap();
        let after = store.simulate_restart();
        for token in [vec![], vec![0u8; 31], vec![0u8; 33], vec![7u8; 32]] {
            assert_eq!(
                after.redeem(&token, &fp(1), T0 + 1),
                Err(TicketRejected::Unknown)
            );
        }
        assert_eq!(after.len(), 1);
    }

    #[test]
    fn one_ticket_per_peer_and_revocation() {
        let store = TicketStore::in_memory(DEFAULT_TICKET_TTL);
        let old = store.issue(&fp(1), 1, T0).unwrap();
        let new = store.issue(&fp(1), 2, T0 + 5).unwrap();
        assert_ne!(old.token, new.token);
        store.issue(&fp(2), 3, T0).unwrap();
        assert_eq!(store.len(), 2);
        assert!(store.has_ticket(&fp(1), T0 + 10));
        let after = store.simulate_restart();
        assert_eq!(
            after.redeem(&old.token, &fp(1), T0 + 10),
            Err(TicketRejected::Unknown)
        );
        assert_eq!(after.revoke_peer(&fp(2)), 1);
        assert_eq!(after.redeem(&new.token, &fp(1), T0 + 10), Ok(2));
        assert!(after.is_empty());
    }

    #[test]
    fn persisted_store_survives_a_restart_and_keeps_only_hashes() {
        let dir = tempfile::tempdir().unwrap();
        // Arabic folder name: paths must round-trip exactly.
        let path = dir.path().join("بيانات").join(TICKETS_FILE);
        let store = TicketStore::load(&path, DEFAULT_TICKET_TTL).unwrap();
        let now = now_unix_ms();
        let t = store.issue(&fp(9), 0x41, now).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let token_hex = Fingerprint(t.token).to_hex();
        assert!(!text.contains(&token_hex), "token stored in clear");
        assert!(text.contains(&Fingerprint(hash(&t.token)).to_hex()));

        let rebooted = TicketStore::load(&path, DEFAULT_TICKET_TTL).unwrap();
        assert_eq!(rebooted.redeem(&t.token, &fp(9), now + 1), Ok(0x41));
        // consumption is persisted too
        let again = TicketStore::load(&path, DEFAULT_TICKET_TTL).unwrap();
        assert!(again.is_empty());
    }

    #[test]
    fn malformed_and_expired_lines_are_dropped_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(TICKETS_FILE);
        let good = format!(
            "{} {} {} 5",
            Fingerprint([1; 32]).to_hex(),
            Fingerprint([2; 32]).to_hex(),
            u64::MAX
        );
        let expired = format!(
            "{} {} 1 5",
            Fingerprint([3; 32]).to_hex(),
            Fingerprint([2; 32]).to_hex()
        );
        fs::write(
            &path,
            format!("# c\n{good}\n{expired}\nnot a ticket\nzz zz zz zz\n{good} extra\n"),
        )
        .unwrap();
        let store = TicketStore::load(&path, DEFAULT_TICKET_TTL).unwrap();
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn debug_never_shows_the_token() {
        let store = TicketStore::in_memory(DEFAULT_TICKET_TTL);
        let t = store.issue(&fp(1), 1, T0).unwrap();
        let shown = format!("{t:?} {store:?}");
        assert!(!shown.contains(&Fingerprint(t.token).to_hex()));
        assert!(!shown.contains(&format!("{:?}", t.token)));
    }
}
