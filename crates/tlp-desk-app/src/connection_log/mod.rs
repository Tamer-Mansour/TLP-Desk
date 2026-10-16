//! Local connection log (M4): who connected, when, from which address, how they were
//! authorized, how long it lasted, which permissions were used and how it ended.
//!
//! * [`LogStore`]: `%APPDATA%\TLP-Desk\connections.log`, JSON lines, append-only, rotated by
//!   size. Nothing ever leaves this machine.
//! * [`Recorder`]: the backend tells it when sessions start, change permissions and end; it
//!   writes one line per finished session (and per refused attempt).
//! * [`export`]: CSV export for the viewer screen (`ui::connection_log`).

pub mod export;
pub mod record;
pub mod store;

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tlp_desk_proto::Permissions;

pub use record::{Direction, LogAuth, LogEnd, LogEntry};
pub use store::LogStore;

use crate::settings::PermissionSet;

/// Milliseconds since the Unix epoch for `t` (0 before 1970).
pub fn unix_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// The peer side of a log entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogPeer {
    pub device_id: u32,
    pub name: String,
    /// Hex key fingerprint.
    pub fingerprint: String,
    pub address: String,
}

impl From<&tlp_desk_net::PeerInfo> for LogPeer {
    fn from(p: &tlp_desk_net::PeerInfo) -> Self {
        Self {
            device_id: p.device_id.as_u32(),
            name: p.name.clone(),
            fingerprint: p.fingerprint.to_hex(),
            address: tlp_desk_net::normalize(p.addr.ip()).to_string(),
        }
    }
}

/// Maps the net crate's auth kind to the log's.
pub fn log_auth(kind: tlp_desk_net::AuthKind) -> LogAuth {
    match kind {
        tlp_desk_net::AuthKind::AcceptPrompt => LogAuth::AcceptPrompt,
        tlp_desk_net::AuthKind::OneTimeCode => LogAuth::OneTimeCode,
        tlp_desk_net::AuthKind::Password => LogAuth::Password,
    }
}

struct Open {
    entry: LogEntry,
    started: Instant,
}

/// Tracks running sessions and writes their log lines. All methods are cheap and never fail
/// (write errors are logged with `tracing`); the log is a record, never a reason to break a
/// session.
pub struct Recorder {
    store: Option<LogStore>,
    open: Mutex<HashMap<(Direction, u64), Open>>,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder")
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

impl Recorder {
    /// Record into `store`; `None` records nothing (tests, or no `APPDATA`).
    pub fn new(store: Option<LogStore>) -> Self {
        Self {
            store,
            open: Mutex::default(),
        }
    }

    fn open(&self) -> std::sync::MutexGuard<'_, HashMap<(Direction, u64), Open>> {
        self.open.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn write(&self, entry: LogEntry) {
        if let Some(store) = &self.store {
            if let Err(e) = store.append(&entry.capped()) {
                tracing::warn!(%e, path = %store.path().display(), "cannot write the connection log");
            }
        }
    }

    /// A session started. `key` identifies it within its direction.
    pub fn begin(
        &self,
        direction: Direction,
        key: u64,
        peer: LogPeer,
        auth: LogAuth,
        permissions: Permissions,
    ) {
        let entry = LogEntry {
            started_ms: unix_ms(SystemTime::now()),
            duration_ms: 0,
            direction,
            device_id: peer.device_id,
            name: peer.name,
            fingerprint: peer.fingerprint,
            address: peer.address,
            auth,
            permissions: PermissionSet(permissions),
            end: LogEnd::Unknown,
            detail: None,
        };
        self.open().insert(
            (direction, key),
            Open {
                entry,
                started: Instant::now(),
            },
        );
    }

    /// The session's permissions changed; the log keeps every permission it ever held.
    pub fn permissions(&self, direction: Direction, key: u64, permissions: Permissions) {
        if let Some(o) = self.open().get_mut(&(direction, key)) {
            o.entry.permissions = PermissionSet(o.entry.permissions.0.union(permissions));
        }
    }

    /// The session ended: write its line. Unknown keys are ignored.
    pub fn end(&self, direction: Direction, key: u64, end: LogEnd, detail: Option<String>) {
        let Some(open) = self.open().remove(&(direction, key)) else {
            return;
        };
        let mut entry = open.entry;
        entry.duration_ms = u64::try_from(open.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        entry.end = end;
        entry.detail = detail;
        self.write(entry);
    }

    /// An attempt that never became a session (declined, wrong code, refused...).
    pub fn refused(&self, direction: Direction, peer: LogPeer, auth: LogAuth, end: LogEnd) {
        self.write(LogEntry {
            started_ms: unix_ms(SystemTime::now()),
            duration_ms: 0,
            direction,
            device_id: peer.device_id,
            name: peer.name,
            fingerprint: peer.fingerprint,
            address: peer.address,
            auth,
            permissions: PermissionSet(Permissions::NONE),
            end,
            detail: None,
        });
    }

    /// App exit: close every running session with [`LogEnd::AppExit`].
    pub fn close_all(&self) {
        let open: Vec<_> = self.open().drain().collect();
        for (_, o) in open {
            let mut entry = o.entry;
            entry.duration_ms = u64::try_from(o.started.elapsed().as_millis()).unwrap_or(u64::MAX);
            entry.end = LogEnd::AppExit;
            self.write(entry);
        }
    }

    /// Number of sessions currently tracked.
    pub fn open_sessions(&self) -> usize {
        self.open().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(name: &str) -> LogPeer {
        LogPeer {
            device_id: 482_913_057,
            name: name.into(),
            fingerprint: "3f".repeat(32),
            address: "192.168.1.7".into(),
        }
    }

    #[test]
    fn recorder_writes_one_line_per_session_with_used_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let rec = Recorder::new(Some(LogStore::new(dir.path())));
        rec.begin(
            Direction::Incoming,
            7,
            peer("حاسوب سارة"),
            LogAuth::AcceptPrompt,
            Permissions::VIEW | Permissions::CONTROL,
        );
        rec.permissions(Direction::Incoming, 7, Permissions::VIEW);
        rec.permissions(
            Direction::Incoming,
            7,
            Permissions::VIEW | Permissions::FILES,
        );
        // Same key, other direction: a different session.
        rec.begin(
            Direction::Outgoing,
            7,
            peer("Server-1"),
            LogAuth::OneTimeCode,
            Permissions::VIEW,
        );
        assert_eq!(rec.open_sessions(), 2);
        rec.end(Direction::Incoming, 7, LogEnd::ClosedRemotely, None);
        rec.end(Direction::Incoming, 7, LogEnd::ClosedHere, None); // already ended: ignored
        rec.refused(
            Direction::Incoming,
            peer("مجهول"),
            LogAuth::OneTimeCode,
            LogEnd::WrongCredentials,
        );
        rec.close_all();
        assert_eq!(rec.open_sessions(), 0);

        let back = LogStore::new(dir.path()).read_all();
        assert_eq!(back.len(), 3);
        assert_eq!(back[0].name, "حاسوب سارة");
        assert_eq!(back[0].end, LogEnd::ClosedRemotely);
        assert_eq!(
            back[0].permissions.0,
            Permissions::VIEW | Permissions::CONTROL | Permissions::FILES
        );
        assert_eq!(back[0].address, "192.168.1.7");
        assert_eq!(back[1].end, LogEnd::WrongCredentials);
        assert_eq!(back[1].duration_ms, 0);
        assert_eq!(back[2].direction, Direction::Outgoing);
        assert_eq!(back[2].end, LogEnd::AppExit);
    }

    #[test]
    fn recorder_without_store_is_a_no_op() {
        let rec = Recorder::new(None);
        rec.begin(
            Direction::Incoming,
            1,
            peer("x"),
            LogAuth::Password,
            Permissions::VIEW,
        );
        rec.end(Direction::Incoming, 1, LogEnd::Error, Some("boom".into()));
        assert_eq!(rec.open_sessions(), 0);
    }

    #[test]
    fn unix_ms_handles_the_epoch() {
        assert_eq!(unix_ms(UNIX_EPOCH), 0);
        assert!(unix_ms(SystemTime::now()) > 1_700_000_000_000);
    }
}
