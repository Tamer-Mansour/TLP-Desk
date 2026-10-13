use serde::{Deserialize, Serialize};

/// Top-level envelope for everything sent on a tlp-desk stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Message {
    Hello(Hello),
    AuthRequest(AuthRequest),
    AuthResult(AuthResult),
    VideoFrame(VideoFrame),
    InputEvent(InputEvent),
    ClipboardData(ClipboardData),
    FileChunk(FileChunk),
    Permissions(Permissions),
    Bye(Bye),
    /// Viewer -> host on the control stream: the decoder lost sync (a frame was dropped or
    /// arrived out of order); the host should encode the next frame as a keyframe.
    /// Added in protocol v2.
    RequestKeyframe,
    /// Host -> viewer: whether the user sitting at the host currently has input priority
    /// (they touched their own mouse/keyboard recently). While `local_active` is true the host
    /// drops the viewer's mouse and keyboard events. Sent only when the state changes.
    /// Added in protocol v3.
    HostInputPriority {
        local_active: bool,
    },
    /// Host -> viewer: the host's monitors and which one is being streamed. Sent after
    /// `Accepted` and whenever the list or the active monitor changes. Added in protocol v3.
    MonitorList {
        monitors: Vec<MonitorDesc>,
        active: u32,
    },
    /// Viewer -> host: stream (and control) the monitor with this index instead. Unknown
    /// indices are ignored. Added in protocol v3.
    SelectMonitor {
        index: u32,
    },

    // ---------------------------------------------------------------------------------------
    // Protocol v4. Append-only: never reorder or insert above, postcard encodes the variant
    // index. Direction and the permission the host requires are documented per message; see
    // `Message::required_permissions` and docs/protocol.md.
    // ---------------------------------------------------------------------------------------
    // --- remote file system (FILES) ---
    /// Either direction (normally viewer -> host): one file-system operation on the receiver's
    /// disks. Answered by exactly one [`Message::FsResponse`] with the same `req_id`. (v4)
    FsRequest {
        req_id: u64,
        op: FsOp,
    },
    /// Answer to [`Message::FsRequest`]. (v4)
    FsResponse {
        req_id: u64,
        result: Result<FsReply, FsError>,
    },

    // --- file transfer (FILES); bulk bytes travel on `StreamKind::FileData` streams ---
    /// Viewer -> host: start a transfer (see [`TransferOffer`]). (v4)
    TransferOffer(TransferOffer),
    /// Host -> viewer: the offer is accepted. For uploads `resume` lists `(item index, byte
    /// offset)` pairs the host already has (partial files); the sender starts there. (v4)
    TransferAccept {
        id: u64,
        resume: Vec<(u32, u64)>,
    },
    /// Host -> viewer: the offer is refused (user declined, no permission, path error). (v4)
    TransferReject {
        id: u64,
        error: FsError,
    },
    /// Receiver -> sender: bytes durably written so far (at most ~4 per second). (v4)
    TransferProgress {
        id: u64,
        bytes_done: u64,
    },
    /// Either direction: abort the transfer; both sides stop and reset its data streams. (v4)
    TransferCancel {
        id: u64,
    },
    /// Sender -> receiver: every byte of item `item` was written to its data stream(s);
    /// `blake3` is the hash of the complete file for end-to-end verification. (v4)
    TransferDone {
        id: u64,
        item: u32,
        blake3: [u8; 32],
    },
    /// Either direction: the transfer (`item: None`) or one item of it failed. (v4)
    TransferError {
        id: u64,
        item: Option<u32>,
        error: FsError,
    },

    // --- clipboard (CLIPBOARD; files also need FILES) ---
    /// Either direction: files were copied (e.g. Ctrl+C in Explorer). The list is announced
    /// under `transfer_id`; when the user pastes on the other side, the bytes are pulled with a
    /// [`TransferOffer`] carrying the same id (direction from the viewer's point of view). (v4)
    ClipboardFiles {
        transfer_id: u64,
        items: Vec<TransferItem>,
    },
    /// Either direction: HTML clipboard content (the `CF_HTML` fragment, UTF-8) plus its
    /// plain-text alternative. (v4)
    ClipboardHtml {
        html: String,
        alt_text: String,
    },

    // --- chat (CHAT) ---
    /// Either direction: one chat message (UTF-8; Arabic, RTL marks and joiners preserved
    /// exactly). `id` is unique per sender and session. (v4)
    ChatMessage {
        id: u64,
        text: String,
        sent_unix_ms: u64,
    },

    // --- audio (AUDIO); Opus packets travel on a `StreamKind::Audio` stream ---
    /// Viewer -> host: start/stop system-audio streaming with this format. Host -> viewer: the
    /// format actually used (sent before the first audio stream opens). (v4)
    AudioControl {
        enabled: bool,
        sample_rate: u32,
        channels: u8,
    },

    // --- session / system (M3) ---
    /// Viewer -> host: press Ctrl+Alt+Del (`SendSAS`, needs the service). Needs `SYSTEM`. (v4)
    SendCtrlAltDel,
    /// Viewer -> host: restart the host PC. With `reconnect` the host first sends a
    /// [`Message::ReconnectTicket`]. Needs `RESTART`. (v4)
    RestartRemote {
        reconnect: bool,
    },
    /// Host -> viewer: a single-use ticket that lets this viewer (bound to its TLS fingerprint
    /// on the host) re-authorize with `AuthRequest::ReconnectTicket` until `expires_unix_ms`,
    /// without a new prompt, e.g. after the host rebooted. (v4)
    ReconnectTicket {
        token: Vec<u8>,
        expires_unix_ms: u64,
    },
    /// Viewer -> host: blank the host's physical screen and block local input (or undo).
    /// Host -> viewer: the actual state. Needs `PRIVACY_MODE`. (v4)
    PrivacyMode {
        enabled: bool,
    },
    /// Host -> viewer: the host shows the secure desktop (UAC, login or Ctrl+Alt+Del screen)
    /// and/or is locked. Sent on change. (v4)
    DesktopState {
        secure_desktop: bool,
        locked: bool,
    },
    /// Viewer -> host: lock the host workstation. Needs `SYSTEM`. (v4)
    LockWorkstation,

    // --- quality / performance ---
    /// Viewer -> host: change capture/encode parameters. (v4)
    SetQuality(QualitySettings),
    /// Viewer -> host, about once per second: decoder health for adaptive bitrate. (v4)
    ViewerFeedback {
        /// Average decode time per frame over the last second, microseconds.
        decode_us: u32,
        /// Frames the viewer dropped (queue overflow, decode errors) in the last second.
        frames_dropped: u32,
        /// QUIC smoothed RTT as seen by the viewer, microseconds.
        rtt_us: u32,
    },
    /// Viewer -> host: latency probe, answered by [`Message::LatencyEcho`]. (v4)
    LatencyProbe {
        /// Viewer clock (microseconds since the viewer's session start).
        t_viewer_us: u64,
    },
    /// Host -> viewer: echo of [`Message::LatencyProbe`] plus the capture timestamp (same clock
    /// as `VideoFrame::timestamp_us`) of the newest frame captured when the probe arrived. (v4)
    LatencyEcho {
        t_viewer_us: u64,
        t_host_capture_us: u64,
    },
    /// Viewer -> host: stream all monitors as one picture of the whole virtual desktop.
    /// `SelectMonitor` returns to one monitor; `MonitorList::active` is `u32::MAX` while all
    /// monitors are shown. (v4)
    SelectAllMonitors,

    // --- power features (M4) ---
    /// Viewer -> host: forward TCP connections to `target_host:target_port` as seen from the
    /// host (`None` = the host itself, 127.0.0.1). Each forwarded connection is one
    /// `StreamKind::Tunnel` bidi stream, opened by the viewer, with this `id`. Needs `TUNNEL`.
    /// (v4)
    TunnelOpen {
        id: u64,
        target_port: u16,
        target_host: Option<String>,
    },
    /// Host -> viewer: the tunnel is allowed; the viewer may open its streams. (v4)
    TunnelOpened {
        id: u64,
    },
    /// Either direction: close the tunnel and all its streams; `error` explains a refusal or
    /// failure (e.g. connection refused on the host). (v4)
    TunnelClose {
        id: u64,
        error: Option<String>,
    },

    // --- whiteboard ---
    /// Either direction: one whiteboard stroke over the streamed monitor. Points are normalized
    /// to `0..=65535` like `InputEvent::MouseMove`. Needs `WHITEBOARD`. (v4)
    WhiteboardStroke {
        id: u64,
        points: Vec<(u16, u16)>,
        /// `0xRRGGBBAA`.
        color_rgba: u32,
        /// Line width in 1/10000 of the monitor height.
        width: u16,
    },
    /// Either direction: erase every stroke. Needs `WHITEBOARD`. (v4)
    WhiteboardClear,

    // --- device info ---
    /// Viewer -> host: ask for [`Message::DeviceInfo`]. Needs `VIEW`. (v4)
    DeviceInfoRequest,
    /// Host -> viewer: system information for the device-info panel. (v4)
    DeviceInfo(DeviceInfo),
    // FEATURE HOOK: new messages go here, at the very end only (bump PROTOCOL_VERSION).
}

impl Message {
    /// Permissions a viewer must currently hold for the host to act on this message when it
    /// arrives from the viewer. `Permissions::NONE` = always allowed (protocol housekeeping).
    /// Input events are additionally gated per event by the host (`CONTROL` / `KEYBOARD`);
    /// `CONTROL` here is only the coarse requirement. Messages that only flow host -> viewer
    /// return `NONE`; the viewer does not gate them.
    pub fn required_permissions(&self) -> Permissions {
        use Message as M;
        match self {
            M::Hello(_)
            | M::AuthRequest(_)
            | M::AuthResult(_)
            | M::Bye(_)
            | M::Permissions(_)
            | M::HostInputPriority { .. }
            | M::MonitorList { .. }
            | M::VideoFrame(_) => Permissions::NONE,
            M::RequestKeyframe | M::SelectMonitor { .. } => Permissions::VIEW,
            M::InputEvent(_) => Permissions::CONTROL,
            M::ClipboardData(_) => Permissions::CLIPBOARD,
            M::FileChunk(_)
            | M::FsRequest { .. }
            | M::FsResponse { .. }
            | M::TransferOffer(_)
            | M::TransferAccept { .. }
            | M::TransferReject { .. }
            | M::TransferProgress { .. }
            | M::TransferCancel { .. }
            | M::TransferDone { .. }
            | M::TransferError { .. } => Permissions::FILES,
            M::ClipboardHtml { .. } => Permissions::CLIPBOARD,
            M::ClipboardFiles { .. } => Permissions::CLIPBOARD.union(Permissions::FILES),
            M::ChatMessage { .. } => Permissions::CHAT,
            M::AudioControl { .. } => Permissions::AUDIO,
            M::ReconnectTicket { .. } | M::DesktopState { .. } => Permissions::NONE,
            M::SendCtrlAltDel | M::LockWorkstation => Permissions::SYSTEM,
            M::RestartRemote { .. } => Permissions::RESTART,
            M::PrivacyMode { .. } => Permissions::PRIVACY_MODE,
            M::LatencyEcho { .. } => Permissions::NONE,
            M::SetQuality(_)
            | M::ViewerFeedback { .. }
            | M::LatencyProbe { .. }
            | M::SelectAllMonitors => Permissions::VIEW,
            M::TunnelOpened { .. } => Permissions::NONE,
            M::TunnelOpen { .. } | M::TunnelClose { .. } => Permissions::TUNNEL,
            M::WhiteboardStroke { .. } | M::WhiteboardClear => Permissions::WHITEBOARD,
            M::DeviceInfo(_) => Permissions::NONE,
            M::DeviceInfoRequest => Permissions::VIEW,
            // FEATURE HOOK: permission of newly appended messages (the match is exhaustive).
        }
    }
}

// ------------------------------------------------------------------------------------------
// v4 payload types
// ------------------------------------------------------------------------------------------

/// A file-system operation. Paths are UTF-8 `String`s in the receiver's native form (e.g.
/// `C:\Users\مستخدم\Desktop`); Windows converts them to UTF-16 losslessly, so Arabic names
/// survive exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FsOp {
    /// Entries of a directory (not recursive). Reply: [`FsReply::Entries`].
    ListDir { path: String },
    /// One entry. Reply: [`FsReply::Entry`].
    Stat { path: String },
    /// Drives and known folders (Desktop, Documents, Downloads, ...). Reply: [`FsReply::Roots`].
    Roots,
    /// Create a directory (and missing parents). Reply: [`FsReply::Done`].
    MakeDir { path: String },
    /// Rename or move within one volume. Reply: [`FsReply::Done`].
    Rename { from: String, to: String },
    /// Delete a file, or a directory (`recursive` required if not empty). Reply:
    /// [`FsReply::Done`].
    Delete { path: String, recursive: bool },
}

/// Successful answer to an [`FsOp`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FsReply {
    Entries(Vec<FsEntry>),
    Entry(FsEntry),
    Roots(Vec<FsRoot>),
    Done,
}

/// One directory entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsEntry {
    /// File name only (no directory), UTF-8.
    pub name: String,
    pub is_dir: bool,
    /// Bytes (0 for directories).
    pub size: u64,
    /// Last modification, milliseconds since the Unix epoch (negative before 1970).
    pub modified_unix_ms: i64,
    pub readonly: bool,
    pub hidden: bool,
}

/// What kind of place an [`FsRoot`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FsRootKind {
    /// A drive / volume (`C:\`).
    Drive,
    Home,
    Desktop,
    Documents,
    Downloads,
    Pictures,
    Music,
    Videos,
    Other,
}

/// A drive or well-known folder offered as a starting point for browsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsRoot {
    /// Absolute path (`C:\`, `C:\Users\name\Desktop`).
    pub path: String,
    /// Display label (volume label or localized folder name), may be empty.
    pub label: String,
    pub kind: FsRootKind,
    /// Volume size and free space when known.
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
}

/// Why a file-system operation or transfer failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FsError {
    NotFound,
    /// The OS refused access, or the session lacks `FILES`.
    PermissionDenied,
    AlreadyExists,
    /// Directory not empty (use `recursive`).
    NotEmpty,
    /// Path is malformed, escapes the transfer root, or names a device.
    InvalidPath,
    /// Disk full.
    NoSpace,
    /// File in use by another process.
    Busy,
    /// The user at the other side declined.
    Declined,
    Cancelled,
    /// Data did not match its `blake3` hash.
    HashMismatch,
    /// Anything else, with the OS message (UTF-8, may be localized).
    Other(String),
}

/// One host monitor, as listed in [`Message::MonitorList`]. Position and size are physical
/// pixels on the host's virtual desktop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorDesc {
    pub index: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
    pub x: i32,
    pub y: i32,
}

/// What a peer wants to do in this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Host,
    Viewer,
}

/// First message on the control stream, sent by both sides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol_version: u32,
    /// 9-digit device ID derived from the peer's public key.
    pub device_id: u32,
    pub device_name: String,
    pub role: Role,
}

/// How the viewer asks the host to authorize the session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthRequest {
    /// Ask the user sitting at the host to click Accept.
    AcceptPrompt,
    /// SPAKE2 message keyed by the host's one-time 6-digit code.
    OneTimeCode { spake2_msg: Vec<u8> },
    /// SPAKE2 message keyed by the host's unattended-access password.
    Password { spake2_msg: Vec<u8> },
    /// SPAKE2 key confirmation from the viewer (sent after `AuthResult::Spake2Reply`):
    /// `HMAC-SHA256(K, "tlp-desk/kc/viewer" || transcript)`. Added in protocol v2.
    KeyConfirm { mac: Vec<u8> },
}

/// Host's answer to an [`AuthRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthResult {
    /// SPAKE2 reply from the host; the viewer must finish the exchange and prove the key.
    Spake2Reply {
        spake2_msg: Vec<u8>,
    },
    Accepted {
        permissions: Permissions,
    },
    Rejected {
        reason: RejectReason,
    },
    /// SPAKE2 key confirmation from the host, sent after the viewer's MAC verified and before
    /// `Accepted`: `HMAC-SHA256(K, "tlp-desk/kc/host" || transcript)`. Added in protocol v2.
    KeyConfirm {
        mac: Vec<u8>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectReason {
    UserDeclined,
    WrongCredentials,
    RateLimited,
    NotAllowed,
    VersionMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Codec {
    H264,
}

/// One encoded video frame, sent on the video uni stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoFrame {
    pub seq: u64,
    /// Host capture time in microseconds since session start.
    pub timestamp_us: u64,
    pub width: u32,
    pub height: u32,
    pub codec: Codec,
    pub keyframe: bool,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// Input from the viewer. Pointer coordinates are normalized to `0..=u16::MAX` across the
/// captured surface so they are independent of resolution and DPI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputEvent {
    MouseMove {
        x: u16,
        y: u16,
    },
    MouseButton {
        button: MouseButton,
        down: bool,
    },
    /// Wheel deltas in 1/120 notch units (Windows `WHEEL_DELTA`).
    MouseWheel {
        dx: i16,
        dy: i16,
    },
    /// Layout-independent key by hardware scancode.
    Key {
        scancode: u16,
        extended: bool,
        down: bool,
    },
    /// Release every pressed key and button (e.g. when the viewer loses focus).
    ReleaseAll,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardData {
    Text(String),
    /// Tightly packed RGBA8 pixels.
    Image {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChunk {
    pub transfer_id: u64,
    pub offset: u64,
    pub data: Vec<u8>,
    pub last: bool,
}

/// Session permission set, as a bitset of the `Permissions::*` flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Permissions(pub u16);

impl Permissions {
    pub const VIEW: Self = Self(1 << 0);
    pub const CONTROL: Self = Self(1 << 1);
    pub const KEYBOARD: Self = Self(1 << 2);
    pub const CLIPBOARD: Self = Self(1 << 3);
    pub const FILES: Self = Self(1 << 4);
    pub const AUDIO: Self = Self(1 << 5);
    pub const RESTART: Self = Self(1 << 6);
    pub const PRIVACY_MODE: Self = Self(1 << 7);
    /// Chat panel messages. (v4)
    pub const CHAT: Self = Self(1 << 8);
    /// TCP port forwarding through the session. (v4)
    pub const TUNNEL: Self = Self(1 << 9);
    /// Draw on the host screen. (v4)
    pub const WHITEBOARD: Self = Self(1 << 10);
    /// Ctrl+Alt+Del and lock workstation (restarting stays `RESTART`). (v4)
    pub const SYSTEM: Self = Self(1 << 11);

    pub const NONE: Self = Self(0);
    pub const ALL: Self = Self((1 << 12) - 1);

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

impl std::ops::BitOr for Permissions {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ByeReason {
    UserClosed,
    PermissionRevoked,
    Timeout,
    Error,
}

/// Graceful session end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bye {
    pub reason: ByeReason,
}

/// Direction of a transfer, from the viewer's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferDirection {
    /// Viewer -> host: the viewer sends the bytes.
    Upload,
    /// Host -> viewer: the host sends the bytes.
    Download,
}

/// One file or directory in a transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferItem {
    /// Path relative to the transfer's `source_dir` / `dest_dir`, `/`-separated, UTF-8. Must
    /// not be absolute or contain `..` (the receiver rejects it with `InvalidPath`).
    pub rel_path: String,
    /// Bytes (0 for directories).
    pub size: u64,
    /// Milliseconds since the Unix epoch; the receiver restores it.
    pub modified_unix_ms: i64,
    pub is_dir: bool,
}

/// A transfer request, always sent by the viewer. Data then flows from the sender (viewer for
/// `Upload`, host for `Download`) on one or more `StreamKind::FileData` uni streams with
/// stream id = the transfer id, each body starting with a [`crate::FileDataHeader`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferOffer {
    /// Unique per session and viewer; also the side-stream id.
    pub id: u64,
    pub direction: TransferDirection,
    /// Directory on the sending side that `items[].rel_path` are relative to.
    pub source_dir: String,
    /// Directory on the receiving side to write into.
    pub dest_dir: String,
    pub items: Vec<TransferItem>,
    /// Sum of `items[].size`.
    pub total_bytes: u64,
    /// `Download` only: `(item index, byte offset)` the viewer already has (resume). For
    /// uploads the host answers with offsets in `Message::TransferAccept::resume` instead.
    pub resume: Vec<(u32, u64)>,
}

/// Encoder preset chosen in the viewer toolbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QualityPreset {
    /// Lowest latency and bandwidth.
    Speed,
    Balanced,
    /// Best picture.
    Quality,
    /// Use the explicit fields of [`QualitySettings`].
    Custom,
}

/// Capture/encode parameters requested by the viewer. Zero means "host default" for
/// `max_width`, `max_height` and `fps`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualitySettings {
    pub preset: QualityPreset,
    pub max_width: u32,
    pub max_height: u32,
    pub fps: u32,
    /// `None` = adaptive bitrate.
    pub bitrate_kbps: Option<u32>,
}

/// One disk in [`DeviceInfo`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskInfo {
    /// Mount point / drive (`C:\`).
    pub mount: String,
    /// Volume label, may be empty.
    pub label: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

/// Host system information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// e.g. "Windows 11 Pro".
    pub os: String,
    /// OS version/build, e.g. "10.0.26100".
    pub version: String,
    /// CPU brand string.
    pub cpu: String,
    /// Logical processors.
    pub cores: u32,
    pub ram_bytes: u64,
    pub disks: Vec<DiskInfo>,
    pub uptime_s: u64,
    pub hostname: String,
}
