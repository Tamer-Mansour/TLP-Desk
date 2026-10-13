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
