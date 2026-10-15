//! The boundary between the GUI and everything that does real work (networking, discovery,
//! auth, capture, codec, input injection).
//!
//! # Contract
//!
//! * **The UI never blocks.** Every [`Backend`] method must return immediately. Long-running work
//!   (QUIC handshakes, mDNS, decoding) runs on the backend's own tokio tasks / threads.
//! * **Results flow back as [`BackendEvent`]s** over the [`EventReceiver`] returned by the
//!   backend's constructor. The UI drains it with `try_recv()` once per egui frame.
//! * **Video frames do not go through the event channel.** Each session gets a [`FrameSlot`]
//!   (a single-slot "latest frame wins" mailbox). The decoder overwrites it; the UI takes the
//!   newest frame when it paints. Frames the UI did not get to are dropped, never queued, so
//!   latency cannot grow when the viewer falls behind.
//! * **Wake-ups.** Whenever the backend sends an event or publishes a frame it must call the
//!   [`RepaintFn`] it was given, otherwise egui may sleep until the next user input.
//!
//! Implementations: [`real::RealBackend`] (QUIC host + viewer on one endpoint) and
//! [`fake::FakeBackend`] (in-process simulation for development and screenshots). Both are
//! constructed like
//! `fn start(rt: &tokio::runtime::Handle, repaint: RepaintFn) -> (Arc<dyn Backend>, EventReceiver)`.

mod clipboard;
pub mod fake;
pub mod files;
pub mod files_bridge;
pub mod files_fake;
pub mod real;

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use std::path::PathBuf;

use tlp_desk_proto::{InputEvent, Permissions, RejectReason};

pub use tlp_desk_proto::MonitorDesc;

/// Callback the backend calls (from any thread) to make the UI repaint.
pub type RepaintFn = Arc<dyn Fn() + Send + Sync>;

/// Receiving end of the backend → UI event stream.
pub type EventReceiver = tokio::sync::mpsc::UnboundedReceiver<BackendEvent>;

/// Sending end of the backend → UI event stream (for backend implementations).
pub type EventSender = tokio::sync::mpsc::UnboundedSender<BackendEvent>;

/// 9-digit device ID (`0..=999_999_999`) derived from the device's public key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceId(pub u32);

impl DeviceId {
    pub const MAX: u32 = 999_999_999;

    /// Parse a 9-digit ID, ignoring spaces, dashes and dots used as group separators.
    pub fn parse(s: &str) -> Option<Self> {
        let digits: String = s
            .chars()
            .filter(|c| !matches!(c, ' ' | '-' | '\u{a0}'))
            .collect();
        if digits.len() == 9 && digits.chars().all(|c| c.is_ascii_digit()) {
            digits.parse().ok().map(DeviceId)
        } else {
            None
        }
    }
}

impl fmt::Display for DeviceId {
    /// Formats as three groups: `123 456 789`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = format!("{:09}", self.0);
        write!(f, "{} {} {}", &s[0..3], &s[3..6], &s[6..9])
    }
}

/// Snapshot of this device, shown on the home screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalInfo {
    pub device_id: DeviceId,
    pub device_name: String,
    /// LAN addresses this host listens on (RFC1918 / link-local).
    pub addresses: Vec<IpAddr>,
    /// Current one-time 6-digit code (rotates after each use or on request).
    pub one_time_code: String,
}

/// A device found on the LAN (mDNS / broadcast) or remembered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredDevice {
    pub device_id: DeviceId,
    pub name: String,
    pub address: IpAddr,
    pub online: bool,
}

/// What the user typed or clicked to connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectTarget {
    /// Dial by device ID; the backend must pin the peer key to this ID.
    Id(DeviceId),
    /// Dial an address directly (trust-on-first-use). Port defaults to 47400.
    Address(SocketAddr),
    /// Resolve a device name through discovery.
    Name(String),
}

impl ConnectTarget {
    pub const DEFAULT_PORT: u16 = 47400;

    /// Interpret the address box: 9-digit ID, IP (optionally with port), or a device name.
    pub fn parse(input: &str) -> Option<Self> {
        let s = input.trim();
        if s.is_empty() {
            return None;
        }
        if let Some(id) = DeviceId::parse(s) {
            return Some(Self::Id(id));
        }
        if let Ok(sa) = s.parse::<SocketAddr>() {
            return Some(Self::Address(sa));
        }
        if let Ok(ip) = s.parse::<IpAddr>() {
            return Some(Self::Address(SocketAddr::new(ip, Self::DEFAULT_PORT)));
        }
        // Reject things that look like a mistyped ID or IP rather than a name.
        if s.chars()
            .all(|c| c.is_ascii_digit() || c == ' ' || c == '.')
        {
            return None;
        }
        Some(Self::Name(s.to_owned()))
    }
}

impl fmt::Display for ConnectTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Id(id) => write!(f, "{id}"),
            Self::Address(sa) if sa.port() == Self::DEFAULT_PORT => write!(f, "{}", sa.ip()),
            Self::Address(sa) => write!(f, "{sa}"),
            Self::Name(n) => f.write_str(n),
        }
    }
}

/// Identifies one connection attempt started with [`Backend::connect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AttemptId(pub u64);

/// Identifies an established session (viewer or host side).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(pub u64);

/// Identifies an incoming request awaiting the local user's decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId(pub u64);

/// The other side of a session or request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    pub device_id: DeviceId,
    pub name: String,
    pub address: IpAddr,
}

/// Why a connection attempt failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectError {
    /// Name/ID could not be resolved on the LAN.
    NotFound,
    /// The host answered with a rejection.
    Rejected(RejectReason),
    /// No route / no answer from the address.
    Unreachable,
    /// The peer's certificate does not hash to the dialed ID, or the host failed to prove it
    /// knows the one-time code.
    IdentityMismatch,
    /// The host's key differs from the one remembered from earlier connections (possible
    /// man-in-the-middle, or the host was reinstalled). The UI must warn loudly; only
    /// [`Backend::forget_host_key`] for this attempt clears it. Fingerprints are short,
    /// display-ready hex groups.
    HostKeyChanged {
        previous: String,
        presented: String,
    },
    /// The address is outside the local network (refused before any packet was sent).
    NotLocalNetwork,
    Timeout,
    Other(String),
}

/// Why a session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndReason {
    /// The local user disconnected.
    Local,
    /// The peer closed the session or revoked it.
    Remote,
    Error(String),
}

/// Decoded remote screen image.
///
/// Pixels are tightly packed **RGBA8, opaque** (alpha = 255), row-major, `width * height * 4`
/// bytes. Opaque pixels are identical premultiplied and unmultiplied, so the UI can upload them
/// without per-pixel conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Monotonic frame number, for diagnostics.
    pub seq: u64,
}

/// Gives a consumed frame buffer back to the producer (e.g. the decoder) for reuse.
pub type RecycleFn = Box<dyn Fn(Vec<u8>) + Send + Sync>;

/// Single-slot mailbox for the newest decoded frame of one session.
#[derive(Default)]
pub struct FrameSlot {
    latest: Mutex<Option<VideoImage>>,
    recycler: Option<RecycleFn>,
}

impl fmt::Debug for FrameSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameSlot")
            .field("recycles", &self.recycler.is_some())
            .finish_non_exhaustive()
    }
}

impl FrameSlot {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// A slot whose dropped / consumed buffers go back to the producer through `recycle`.
    pub fn with_recycler(recycle: RecycleFn) -> Arc<Self> {
        Arc::new(Self {
            latest: Mutex::default(),
            recycler: Some(recycle),
        })
    }

    /// Replace the pending frame (an unconsumed older frame is dropped and its buffer
    /// recycled). Call the [`RepaintFn`] afterwards.
    pub fn publish(&self, image: VideoImage) {
        let old = self
            .latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(image);
        if let Some(old) = old {
            self.recycle(old.rgba);
        }
    }

    /// Hand a consumed frame's buffer back (after uploading it) to avoid an allocation in the
    /// producer. Without a recycler the buffer is simply dropped.
    pub fn recycle(&self, buf: Vec<u8>) {
        if let Some(r) = &self.recycler {
            r(buf);
        }
    }

    /// Take the newest frame, if a new one arrived since the last call.
    pub fn take(&self) -> Option<VideoImage> {
        self.latest.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// Live session statistics for the overlay.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SessionStats {
    /// Frames per second received/decoded.
    pub fps: f32,
    /// Estimated glass-to-glass latency in milliseconds.
    pub latency_ms: f32,
    /// Video bitrate in kilobits per second.
    pub bitrate_kbps: u32,
}

/// Events from the backend to the UI.
#[derive(Debug)]
pub enum BackendEvent {
    /// This device's info changed (new one-time code, address change, rename).
    LocalInfo(LocalInfo),
    /// Full replacement of the discovered devices list.
    Devices(Vec<DiscoveredDevice>),

    /// The attempt reached the host and is waiting for the host user to click Accept.
    AwaitingAccept { attempt: AttemptId },
    /// The attempt failed. (A cancelled attempt sends nothing further.)
    ConnectFailed {
        attempt: AttemptId,
        error: ConnectError,
    },
    /// Viewer session established. `frames` delivers decoded remote frames.
    SessionStarted {
        attempt: AttemptId,
        session: SessionId,
        peer: PeerInfo,
        permissions: Permissions,
        frames: Arc<FrameSlot>,
    },
    /// The host changed what this viewer may do (live revoke/grant).
    PermissionsChanged {
        session: SessionId,
        permissions: Permissions,
    },
    /// Periodic statistics (about once per second).
    Stats {
        session: SessionId,
        stats: SessionStats,
    },
    /// The user at the host is using their own mouse/keyboard (`true`); our input is ignored
    /// until it switches back to `false`. Show a notice in the session view.
    HostInputPriority {
        session: SessionId,
        local_active: bool,
    },
    /// The host's monitor list or the streamed monitor changed (monitor picker).
    MonitorsChanged {
        session: SessionId,
        monitors: Vec<MonitorDesc>,
        active: u32,
    },
    /// A viewer or host session ended.
    SessionEnded {
        session: SessionId,
        reason: EndReason,
    },

    /// Host side: a peer asks to connect; the UI must show Accept / Decline.
    IncomingRequest { request: RequestId, peer: PeerInfo },
    /// Host side: the peer gave up before the user answered.
    IncomingRequestCancelled { request: RequestId },
    /// Host side: a viewer is now connected to this device (show the banner).
    HostSessionStarted { session: SessionId, peer: PeerInfo },
    /// Host side: the OS refused to inject this viewer's input (elevated window in front).
    /// Rate limited by the host; the UI shows a notice for a while.
    HostInputBlocked { session: SessionId },
    /// This device cannot accept incoming connections (connecting to others may still work).
    /// Sent at startup; the UI shows it until the app restarts.
    HostUnavailable(HostProblem),
    // =======================================================================================
    // FEATURE HOOK: feature lanes append their variants ONLY inside their own section below,
    // so parallel branches never touch the same lines. Keep payload types in your own module
    // (e.g. `backend/files.rs`) and map `HostEvent::Feature` / `ViewerEvent::Feature` to these
    // in real.rs.
    // =======================================================================================
    // --- files: remote browser, transfers, Explorer copy/paste (TLP-files) ---
    /// Answer to [Backend::fs_request].
    FsResult {
        side: files::FsSide,
        request: files::FsRequestId,
        result: Result<files::FsReply, files::FsFailure>,
    },
    /// The remote PC's clipboard now holds these copied files (None: nothing to paste).
    RemoteClipboardFiles {
        session: SessionId,
        files: Option<files::RemoteClipboardFiles>,
    },
    /// Outcome of [Backend::fs_transfer] worth telling the user.
    FilesNotice {
        session: SessionId,
        notice: files::FilesNotice,
    },
    /// Viewer side: a file transfer was created or changed (progress arrives about four times a
    /// second). The UI keeps the newest view per `id`.
    Transfer {
        session: SessionId,
        view: files::TransferView,
    },
    /// Viewer side: a transfer could not start.
    TransferNotStarted {
        session: SessionId,
        error: files::TransferStartError,
    },
    // --- clipboard: text/image/HTML sync (TLP-clipboard) ---
    /// Clipboard content was synced (or refused) in a viewer or host session. Kinds and sizes
    /// only, never contents.
    Clipboard {
        session: SessionId,
        event: tlp_desk_files::ClipboardEvent,
    },
    // --- chat (TLP-ux) ---

    // --- audio (TLP-audio) ---

    // --- service: unattended access, secure desktop, Ctrl+Alt+Del, restart (TLP-service) ---

    // --- privacy mode (TLP-privacy) ---

    // --- settings / ux: address book, tray, banner (TLP-ux) ---

    // --- perf: quality presets, latency overlay, all monitors (TLP-perf) ---

    // --- power: WOL, recording, tunnels, whiteboard, connection log, device info (TLP-power) ---
}

/// Why this device cannot act as a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostProblem {
    /// The UDP port is taken, most likely by another running TLP-Desk.
    PortInUse(u16),
    /// Networking could not start at all (no connections in either direction).
    NoNetwork(String),
    Other(String),
}

/// Commands from the UI to the backend. All methods must return immediately.
pub trait Backend: Send + Sync {
    /// Latest snapshot of this device. Also pushed as [`BackendEvent::LocalInfo`] on change.
    fn local_info(&self) -> LocalInfo;

    /// Rename this device (UTF-8, may be Arabic). The new info arrives as
    /// [`BackendEvent::LocalInfo`].
    fn set_device_name(&self, name: String);

    /// Rotate the one-time code now. The new code arrives as [`BackendEvent::LocalInfo`].
    fn refresh_one_time_code(&self);

    /// Start connecting. `code` is the host's one-time code; `None` asks the host user to
    /// accept. Progress arrives as `AwaitingAccept` (accept prompt only), then
    /// `SessionStarted` or `ConnectFailed` with the returned attempt id.
    fn connect(&self, target: ConnectTarget, code: Option<String>) -> AttemptId;

    /// Abort a pending attempt. No further events are sent for it.
    fn cancel_connect(&self, attempt: AttemptId);

    /// After `ConnectFailed { error: HostKeyChanged }` and an explicit user confirmation:
    /// forget the remembered key that attempt tripped over, so the next connect trusts the
    /// new key on first use. Returns `false` if there is nothing to forget.
    fn forget_host_key(&self, attempt: AttemptId) -> bool;

    /// Host side: answer an [`BackendEvent::IncomingRequest`].
    fn respond_to_request(&self, request: RequestId, accept: bool);

    /// Viewer side: send one input event. The UI already filters by view-only mode and
    /// permissions; the backend/host must still enforce permissions.
    fn send_input(&self, session: SessionId, event: InputEvent);

    /// Viewer side: tell the backend the user toggled view-only (it may e.g. stop sending
    /// cursor updates). Input gating itself happens in the UI.
    fn set_view_only(&self, session: SessionId, view_only: bool);

    /// End a viewer or host session. A `SessionEnded { reason: Local }` follows.
    fn disconnect(&self, session: SessionId);

    /// Viewer side: ask the host to stream (and map input to) another monitor. Confirmed by
    /// [`BackendEvent::MonitorsChanged`].
    fn select_monitor(&self, session: SessionId, index: u32);

    // =======================================================================================
    // FEATURE HOOK: feature lanes append methods ONLY inside their own section below. Give
    // every new method a default body (a no-op or `tracing::debug!`) so `FakeBackend` and
    // `RealBackend` only change where the lane actually implements it.
    // =======================================================================================
    // --- files (TLP-files) ---
    /// Runs one file-system operation on side (this PC or the remote PC of a viewer session)
    /// without blocking; the answer arrives as [BackendEvent::FsResult]. None if this
    /// backend has no file access.
    fn fs_request(&self, side: files::FsSide, op: files::FsOp) -> Option<files::FsRequestId> {
        let _ = (side, op);
        None
    }

    /// Starts copying files between this PC and the remote PC of session. Problems (and the
    /// start) are reported as [BackendEvent::FilesNotice].
    fn fs_transfer(&self, session: SessionId, request: files::TransferRequest) {
        tracing::debug!(
            session = session.0,
            ?request,
            "file transfers not supported by this backend"
        );
    }

    /// Viewer side: send local files and folders (recursively) to the host's default folder.
    /// Progress arrives as [`BackendEvent::Transfer`].
    fn send_files(&self, session: SessionId, paths: Vec<PathBuf>) {
        let _ = (session, paths);
        tracing::debug!("send_files: not supported by this backend");
    }

    /// Viewer side: download host files and folders (absolute host paths in one folder) into
    /// the local folder `dest`.
    fn download_files(&self, session: SessionId, remote_paths: Vec<String>, dest: PathBuf) {
        let _ = (session, remote_paths, dest);
        tracing::debug!("download_files: not supported by this backend");
    }

    /// Cancel a running transfer; partial data is kept so a retry resumes.
    fn cancel_transfer(&self, session: SessionId, id: files::TransferId) {
        let _ = (session, id);
    }

    /// Continue a cancelled, paused or failed transfer from its partial data.
    fn retry_transfer(&self, session: SessionId, id: files::TransferId) {
        let _ = (session, id);
    }

    // --- clipboard (TLP-clipboard) ---
    /// Switch clipboard sync on or off for one viewer or host session ("Sync clipboard").
    /// The host's `CLIPBOARD` permission still applies.
    fn set_clipboard_sync(&self, session: SessionId, enabled: bool) {
        tracing::debug!(
            session = session.0,
            enabled,
            "clipboard sync toggle not supported"
        );
    }

    /// Whether sessions started from now on sync the clipboard (settings default).
    fn set_clipboard_default(&self, enabled: bool) {
        tracing::debug!(enabled, "clipboard default not supported");
    }

    // --- chat (TLP-ux) ---

    // --- audio (TLP-audio) ---

    // --- service (TLP-service) ---

    // --- privacy mode (TLP-privacy) ---

    // --- settings / ux (TLP-ux) ---

    // --- perf (TLP-perf) ---

    // --- power (TLP-power) ---
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_formats_in_groups() {
        assert_eq!(DeviceId(123_456_789).to_string(), "123 456 789");
        assert_eq!(DeviceId(7).to_string(), "000 000 007");
    }

    #[test]
    fn connect_target_parsing() {
        assert_eq!(
            ConnectTarget::parse(" 123 456 789 "),
            Some(ConnectTarget::Id(DeviceId(123_456_789)))
        );
        assert_eq!(
            ConnectTarget::parse("123-456-789"),
            Some(ConnectTarget::Id(DeviceId(123_456_789)))
        );
        assert_eq!(
            ConnectTarget::parse("192.168.1.20"),
            Some(ConnectTarget::Address(
                "192.168.1.20:47400".parse().unwrap()
            ))
        );
        assert_eq!(
            ConnectTarget::parse("10.0.0.5:5000"),
            Some(ConnectTarget::Address("10.0.0.5:5000".parse().unwrap()))
        );
        assert_eq!(
            ConnectTarget::parse("office-pc"),
            Some(ConnectTarget::Name("office-pc".into()))
        );
        assert_eq!(ConnectTarget::parse("12345"), None);
        assert_eq!(ConnectTarget::parse("192.168.1"), None);
        assert_eq!(ConnectTarget::parse("   "), None);
    }

    #[test]
    fn frame_slot_keeps_only_latest() {
        let slot = FrameSlot::new();
        for seq in 0..3 {
            slot.publish(VideoImage {
                width: 1,
                height: 1,
                rgba: vec![0; 4],
                seq,
            });
        }
        assert_eq!(slot.take().map(|f| f.seq), Some(2));
        assert!(slot.take().is_none());
    }
}
