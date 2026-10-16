//! Local IPC between the service, its session agent and the GUI: length-prefixed postcard
//! frames over named pipes (or any `Read + Write` in tests).
//!
//! # Framing
//!
//! ```text
//! [u32 LE header length][postcard header]            -- every message
//! [raw payload bytes, length announced in the header] -- only frames (video) carry one
//! ```
//!
//! Video frames are megabytes; serializing them through serde would visit every byte, so the
//! header announces `payload_len` and the pixels follow raw ([`write_payload`] /
//! [`read_payload_into`]). Every length is checked against a cap **before** allocating.
//!
//! This is a *local* protocol between processes of the same binary (version checked in the
//! first message of every connection with [`IPC_VERSION`]); it never crosses the network: the
//! pipes reject remote clients and are ACL-restricted (see `pipe_security`).
//!
//! # Channels
//!
//! * service -> agent: capture, input, commands, desktop events (v1), plus the **user-session
//!   features** the agent owns for the service (v2): [`AgentChannel::Feature`] (clipboard,
//!   Explorer paste, whiteboard overlay, pointer marker, local-input hooks, privacy shield) and
//!   [`AgentChannel::Audio`] (loopback sound). Their events ride the existing events channel
//!   ([`AgentEvent`]).
//! * GUI -> service: status, Accept prompts and session control (v1), plus opaque **app
//!   messages** (v2: [`GuiEvent::App`], [`GuiCommand::App`], [`GuiReply::App`]) whose types
//!   the app defines (transfers, chat, conflict prompts, settings, connection log), so this
//!   crate stays free of file-transfer and UI types.

use std::io::{ErrorKind, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tlp_desk_proto::{InputEvent, Permissions, TransferItem};

use crate::HostError;

/// Version of the local IPC messages. Both ends must match (they are the same binary, so a
/// mismatch means a half-finished upgrade: the old process is told to exit).
///
/// * v1: capture, input, commands, desktop events, GUI status and prompts.
/// * v2: user-session features in the agent, loopback audio, app messages on the GUI pipe.
pub const IPC_VERSION: u32 = 2;

/// Largest postcard header (control messages are tiny; a monitor list or status with Arabic
/// names is a few KB).
pub const MAX_HEADER: usize = 256 * 1024;

/// Largest raw payload: one native 8K BGRA frame (7680 x 4320 x 4 = 132.7 MB) is refused; the
/// service caps capture at 1920x1200 (9.2 MB) and a native 4K frame (33.2 MB) still fits.
pub const MAX_PAYLOAD: usize = 64 * 1024 * 1024;

/// Writes one message (header only).
pub fn write_msg<W: Write + ?Sized, T: Serialize>(w: &mut W, msg: &T) -> Result<(), HostError> {
    let bytes = postcard::to_stdvec(msg).map_err(HostError::IpcEncode)?;
    write_header(w, &bytes)?;
    w.flush()?;
    Ok(())
}

/// Writes one message whose header announced `payload.len()` raw bytes, then the payload.
pub fn write_msg_with_payload<W: Write + ?Sized, T: Serialize>(
    w: &mut W,
    msg: &T,
    payload: &[u8],
) -> Result<(), HostError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(HostError::IpcTooLarge {
            len: payload.len(),
            max: MAX_PAYLOAD,
        });
    }
    let bytes = postcard::to_stdvec(msg).map_err(HostError::IpcEncode)?;
    write_header(w, &bytes)?;
    write_payload(w, payload)?;
    w.flush()?;
    Ok(())
}

fn write_header<W: Write + ?Sized>(w: &mut W, bytes: &[u8]) -> Result<(), HostError> {
    if bytes.len() > MAX_HEADER {
        return Err(HostError::IpcTooLarge {
            len: bytes.len(),
            max: MAX_HEADER,
        });
    }
    let len = u32::try_from(bytes.len()).map_err(|_| HostError::IpcTooLarge {
        len: bytes.len(),
        max: MAX_HEADER,
    })?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(bytes)?;
    Ok(())
}

/// Writes raw payload bytes (after a header that announced their length).
pub fn write_payload<W: Write + ?Sized>(w: &mut W, payload: &[u8]) -> Result<(), HostError> {
    w.write_all(payload)?;
    Ok(())
}

/// Reads one message header. A clean end of stream before the first byte is
/// [`HostError::IpcClosed`]; a stream that ends inside a message is an I/O error.
pub fn read_msg<R: Read + ?Sized, T: DeserializeOwned>(r: &mut R) -> Result<T, HostError> {
    let mut len = [0u8; 4];
    read_exact_or_closed(r, &mut len)?;
    let len = usize::try_from(u32::from_le_bytes(len)).unwrap_or(usize::MAX);
    if len > MAX_HEADER {
        return Err(HostError::IpcTooLarge {
            len,
            max: MAX_HEADER,
        });
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    postcard::from_bytes(&buf).map_err(HostError::IpcDecode)
}

/// Reads a `len`-byte payload into `buf` (reusing its allocation). `len` comes from the peer
/// and is checked against [`MAX_PAYLOAD`] before anything is allocated.
pub fn read_payload_into<R: Read + ?Sized>(
    r: &mut R,
    len: u64,
    buf: &mut Vec<u8>,
) -> Result<(), HostError> {
    let len = usize::try_from(len).unwrap_or(usize::MAX);
    if len > MAX_PAYLOAD {
        return Err(HostError::IpcTooLarge {
            len,
            max: MAX_PAYLOAD,
        });
    }
    buf.clear();
    buf.resize(len, 0);
    r.read_exact(buf)?;
    Ok(())
}

fn read_exact_or_closed<R: Read + ?Sized>(r: &mut R, buf: &mut [u8]) -> Result<(), HostError> {
    let mut filled = 0;
    while filled < buf.len() {
        let rest = buf.get_mut(filled..).unwrap_or_default();
        match r.read(rest) {
            Ok(0) if filled == 0 => return Err(HostError::IpcClosed),
            Ok(0) => return Err(std::io::Error::from(ErrorKind::UnexpectedEof).into()),
            Ok(n) => filled += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            // A named pipe whose other end went away reports "broken pipe": same as closed.
            Err(e) if filled == 0 && e.kind() == ErrorKind::BrokenPipe => {
                return Err(HostError::IpcClosed)
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

// ----- service <-> session agent ---------------------------------------------------------------

/// First message of every connection the service opens to the agent: which channel it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHello {
    pub version: u32,
    pub channel: AgentChannel,
}

/// The four kinds of service -> agent connection. Each connection carries one kind only and is
/// strictly request/reply or one-way: synchronous pipe handles serialize reads and writes, so a
/// connection never needs a reader and a writer at the same time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentChannel {
    /// Request/reply: [`CaptureRequest`] -> [`CaptureReply`]. One per host video pipeline.
    Capture {
        monitor: u32,
        /// Frames are downscaled to fit (0 = native).
        max_width: u32,
        max_height: u32,
    },
    /// One-way service -> agent: [`InputCommand`]s. One per host session.
    Input { rect: WireRect },
    /// Request/reply: [`AgentCommand`] -> [`CommandReply`].
    Command,
    /// One-way agent -> service: [`AgentEvent`]s (desktop switches, feature events).
    Events,
    /// Request/reply: [`FeatureRequest`] -> [`FeatureReply`]. The user-session features the
    /// agent runs on behalf of the service (v2).
    Feature,
    /// One-way agent -> service: [`AudioPacket`]s of the session's system sound (loopback),
    /// until the service closes the connection (v2).
    Audio,
}

/// Agent's answer to [`AgentHello`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentWelcome {
    Ok {
        version: u32,
        /// Windows session the agent runs in.
        session_id: u32,
    },
    /// Version mismatch or a channel that could not be opened.
    Refused { version: u32, error: WireError },
}

/// Service -> agent on a [`AgentChannel::Capture`] connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureRequest {
    /// Wait up to `timeout_ms` for a changed frame.
    NextFrame { timeout_ms: u32 },
}

/// Agent -> service on a capture connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureReply {
    /// Sent once after the welcome: what was opened.
    Opened {
        monitor: WireMonitor,
        frame_width: u32,
        frame_height: u32,
    },
    /// A frame; `payload_len` raw BGRA bytes follow.
    Frame {
        width: u32,
        height: u32,
        stride: u32,
        timestamp_us: u64,
        payload_len: u64,
        cursor: WireCursor,
    },
    /// The screen did not change within the timeout.
    NoChange {
        cursor: WireCursor,
    },
    Error(WireError),
}

/// Service -> agent on an input connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputCommand {
    Event(InputEvent),
    /// Retarget pointer mapping (the viewer switched monitors).
    SetMonitor(WireRect),
}

/// Service -> agent on a command connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentCommand {
    ListMonitors,
    /// `LockWorkStation` must run on the interactive desktop of the user's session.
    LockWorkstation,
    /// Exit the agent process (service stopping or retargeting).
    Exit,
}

/// Agent -> service on a command connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandReply {
    Ok,
    Monitors(Vec<WireMonitor>),
    Error(WireError),
}

/// Agent -> service on the events connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentEvent {
    /// The input desktop changed (`Default` <-> `Winlogon` for lock/login/UAC/Ctrl+Alt+Del).
    Desktop { name: String, secure: bool },
    /// The user's clipboard changed; `seq` is its change sequence number (v2).
    ClipboardChanged { seq: u64 },
    /// The user at the machine used their own keyboard and/or mouse (physical input, while
    /// the local-input hooks run; rate limited by the agent) (v2).
    LocalInput { keyboard: bool, pointer: bool },
    /// A privacy shield turned itself off (v2).
    Shield { id: u64, signal: WireShieldSignal },
    /// The user clicked the clear badge of a whiteboard overlay (v2).
    OverlayClear { id: u64 },
    /// Explorer started pasting the virtual files of `offer`: start pulling them (v2).
    StageBegin { offer: u64 },
    /// The virtual files of `offer` are gone from the clipboard: stop pulling (v2).
    StageCancel { offer: u64 },
}

// ----- user-session features (v2) -------------------------------------------------------------

/// Service -> agent on a [`AgentChannel::Feature`] connection. Ids (`id`, `offer`) are chosen
/// by the service; an unknown id is ignored (the object is gone).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeatureRequest {
    /// Read the clipboard: answered with [`FeatureReply::Clip`].
    ClipboardRead,
    /// Replace the clipboard; the [`WireClip`] payload follows. Answered with
    /// [`FeatureReply::ClipboardSeq`].
    ClipboardWrite(WireClip),
    /// The files copied in Explorer: answered with [`FeatureReply::Files`].
    ClipboardFiles,
    /// Put the peer's announced files on the clipboard as virtual files staged in `dir`.
    ShellOffer {
        offer: u64,
        dir: String,
        items: Vec<TransferItem>,
    },
    /// How far the pull of `offer` has come.
    ShellStage {
        offer: u64,
        state: WireStage,
    },
    /// Take `offer` off the clipboard.
    ShellClear {
        offer: u64,
    },
    /// Create a (hidden) whiteboard overlay.
    OverlayOpen {
        id: u64,
    },
    /// Place the overlay over `rect`.
    OverlayTarget {
        id: u64,
        rect: WireRect,
    },
    /// New strokes or points for the overlay (`reset`: erase the board first). Chunks with the
    /// id of a stroke already on the board extend it.
    OverlayStrokes {
        id: u64,
        reset: bool,
        strokes: Vec<WireStroke>,
    },
    OverlayClose {
        id: u64,
    },
    /// Create the remote-pointer marker labelled `label`.
    MarkerOpen {
        id: u64,
        label: String,
    },
    MarkerVisible {
        id: u64,
        visible: bool,
    },
    MarkerLabel {
        id: u64,
        label: String,
    },
    MarkerClose {
        id: u64,
    },
    /// Start reporting physical local input ([`AgentEvent::LocalInput`]).
    ActivityStart,
    /// Stop the local-input hooks.
    ActivityStop,
    /// Create a privacy shield and engage it with `lines`.
    ShieldEngage {
        id: u64,
        lines: Vec<WireScreenLine>,
    },
    ShieldText {
        id: u64,
        lines: Vec<WireScreenLine>,
    },
    ShieldRelease {
        id: u64,
    },
}

/// Agent -> service answer to a [`FeatureRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeatureReply {
    Ok,
    /// The clipboard content (`None`: empty or nothing we sync); a payload follows.
    Clip(Option<WireClip>),
    /// The clipboard's change sequence number right after a write.
    ClipboardSeq(u64),
    /// Files copied in Explorer (`None`: no files on the clipboard).
    Files(Option<Vec<String>>),
    Error(WireError),
}

/// Clipboard content header; the bytes follow as the message payload (big images and texts
/// would not fit in a header).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireClip {
    /// UTF-8 text, `len` bytes.
    Text { len: u64 },
    /// UTF-8 HTML fragment (`html_len` bytes) then its plain-text alternative (`alt_len`).
    Html { html_len: u64, alt_len: u64 },
    /// RGBA8, `width * height * 4` bytes.
    Image { width: u32, height: u32 },
}

impl WireClip {
    /// Bytes of payload that follow the header (saturating: a hostile size is refused by the
    /// payload cap).
    pub fn payload_len(&self) -> u64 {
        match *self {
            Self::Text { len } => len,
            Self::Html { html_len, alt_len } => html_len.saturating_add(alt_len),
            Self::Image { width, height } => u64::from(width)
                .saturating_mul(u64::from(height))
                .saturating_mul(4),
        }
    }
}

/// Clipboard content in owned form (what the agent's OS clipboard reads and writes).
#[derive(Clone, PartialEq, Eq)]
pub enum ClipData {
    Text(String),
    Html {
        html: String,
        alt_text: String,
    },
    Image {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
}

impl std::fmt::Debug for ClipData {
    /// Kind and size only: clipboard contents are never printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(t) => write!(f, "ClipData::Text({} bytes)", t.len()),
            Self::Html { html, alt_text } => write!(
                f,
                "ClipData::Html({} bytes)",
                html.len().saturating_add(alt_text.len())
            ),
            Self::Image { width, height, .. } => write!(f, "ClipData::Image({width}x{height})"),
        }
    }
}

fn byte_len(s: &str) -> u64 {
    u64::try_from(s.len()).unwrap_or(u64::MAX)
}

impl ClipData {
    /// The header announcing this content.
    pub fn header(&self) -> WireClip {
        match self {
            Self::Text(t) => WireClip::Text { len: byte_len(t) },
            Self::Html { html, alt_text } => WireClip::Html {
                html_len: byte_len(html),
                alt_len: byte_len(alt_text),
            },
            Self::Image { width, height, .. } => WireClip::Image {
                width: *width,
                height: *height,
            },
        }
    }

    /// The payload bytes (HTML then its alternative text, for HTML).
    pub fn payload(&self) -> Vec<u8> {
        match self {
            Self::Text(t) => t.as_bytes().to_vec(),
            Self::Html { html, alt_text } => {
                let mut v = Vec::with_capacity(html.len().saturating_add(alt_text.len()));
                v.extend_from_slice(html.as_bytes());
                v.extend_from_slice(alt_text.as_bytes());
                v
            }
            Self::Image { rgba, .. } => rgba.clone(),
        }
    }

    /// Rebuilds the content from a header and its payload. Invalid UTF-8 or a payload of
    /// the wrong size is an error, never a panic.
    pub fn from_wire(header: WireClip, payload: Vec<u8>) -> Result<Self, HostError> {
        let bad = |what: &str| HostError::IpcRefused(format!("malformed clipboard {what}"));
        if u64::try_from(payload.len()).ok() != Some(header.payload_len()) {
            return Err(bad("size"));
        }
        match header {
            WireClip::Text { .. } => String::from_utf8(payload)
                .map(Self::Text)
                .map_err(|_| bad("text")),
            WireClip::Html { html_len, .. } => {
                let split = usize::try_from(html_len).map_err(|_| bad("html"))?;
                let mut html = payload;
                let alt = html.split_off(split.min(html.len()));
                Ok(Self::Html {
                    html: String::from_utf8(html).map_err(|_| bad("html"))?,
                    alt_text: String::from_utf8(alt).map_err(|_| bad("html"))?,
                })
            }
            WireClip::Image { width, height } => Ok(Self::Image {
                width,
                height,
                rgba: payload,
            }),
        }
    }
}

/// Writes a [`FeatureReply::Clip`] with the payload of `clip` (if any).
pub fn write_clip_reply<W: Write + ?Sized>(
    w: &mut W,
    clip: Option<&ClipData>,
) -> Result<(), HostError> {
    match clip {
        Some(c) => write_msg_with_payload(w, &FeatureReply::Clip(Some(c.header())), &c.payload()),
        None => write_msg(w, &FeatureReply::Clip(None)),
    }
}

/// Reads the payload announced by `header` and rebuilds the content.
pub fn read_clip_payload<R: Read + ?Sized>(
    r: &mut R,
    header: WireClip,
) -> Result<ClipData, HostError> {
    let mut buf = Vec::new();
    read_payload_into(r, header.payload_len(), &mut buf)?;
    ClipData::from_wire(header, buf)
}

/// How far the pull of a virtual-files offer has come (mirrors the files crate's stage
/// state).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireStage {
    Idle,
    Running,
    Finished,
    /// The text is for the log only.
    Failed(String),
}

/// One whiteboard stroke chunk (mirrors `tlp_desk_input::whiteboard::Stroke`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireStroke {
    pub id: u64,
    /// `0xRRGGBBAA`.
    pub color: u32,
    /// 1/10000 of the monitor height.
    pub width: u16,
    /// Normalized to `0..=65535` over the target rectangle.
    pub points: Vec<(u16, u16)>,
}

/// One line of the privacy screen (mirrors `tlp_desk_input::ScreenLine`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireScreenLine {
    pub text: String,
    pub large: bool,
}

/// Why a privacy shield turned itself off (mirrors `tlp_desk_input::privacy::ShieldSignal`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireShieldSignal {
    EmergencyExit,
    SecureDesktop,
    Failed,
}

/// Agent -> service on an audio connection: interleaved little-endian `f32` samples follow
/// (`payload_len` bytes). `payload_len == 0` is a keep-alive while nothing plays (loopback
/// capture delivers no packets during silence), so the service notices a closed agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioPacket {
    pub rate: u32,
    pub channels: u16,
    pub payload_len: u64,
}

/// An error that crosses the pipe, mapped back to capture/input errors on the service side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireError {
    /// The capture surface is gone (desktop switch): reopen.
    SurfaceLost,
    NoSuchMonitor(u32),
    Unsupported,
    /// Input blocked by the OS (e.g. UIPI).
    Blocked(String),
    VersionMismatch {
        expected: u32,
        got: u32,
    },
    Other(String),
}

/// A monitor as reported by the agent (mirrors `tlp_desk_capture::MonitorInfo`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireMonitor {
    pub index: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
    pub x: i32,
    pub y: i32,
}

/// Monitor rectangle in virtual-desktop physical pixels (mirrors `tlp_desk_input::MonitorRect`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Cursor state sent with every capture reply, so the service needs one round trip per frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireCursor {
    pub x: i32,
    pub y: i32,
    pub visible: bool,
    pub shape: Option<WireCursorShape>,
}

/// A new cursor image (RGBA8, tightly packed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireCursorShape {
    pub width: u32,
    pub height: u32,
    pub hotspot_x: u32,
    pub hotspot_y: u32,
    pub rgba: Vec<u8>,
}

// ----- service <-> GUI -------------------------------------------------------------------------

/// First message of every GUI -> service connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuiHello {
    pub version: u32,
    pub channel: GuiChannel,
}

/// GUI connection kinds (same one-direction-at-a-time rule as [`AgentChannel`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuiChannel {
    /// One-way service -> GUI: a [`ServiceStatus`] snapshot, then [`GuiEvent`]s.
    Events,
    /// Request/reply: one [`GuiCommand`] -> [`GuiReply`], then the connection closes.
    Command,
}

/// What the service tells a GUI that connects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuiWelcome {
    Ok {
        version: u32,
    },
    /// Version mismatch, or the GUI is not in the active console session.
    Refused {
        version: u32,
        error: WireError,
    },
}

/// The host status the GUI shows while the service owns the network endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub device_id: u32,
    /// UTF-8, may be Arabic.
    pub device_name: String,
    /// The current one-time code (shown to the user at the machine only; the pipe is limited
    /// to interactive users and the GUI must be in the active console session).
    pub one_time_code: String,
    pub requests: Vec<GuiRequest>,
    pub sessions: Vec<GuiSession>,
    pub desktop: WireDesktop,
}

/// Secure-desktop / lock state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WireDesktop {
    pub secure_desktop: bool,
    pub locked: bool,
}

/// An Accept prompt waiting for the user at the machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuiRequest {
    pub id: u64,
    pub peer: GuiPeer,
}

/// A connected viewer (for the "who is connected" banner).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuiSession {
    pub id: u64,
    pub peer: GuiPeer,
    pub permissions: Permissions,
}

/// The other side of a request or session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuiPeer {
    pub device_id: u32,
    /// Self-reported, UTF-8 (may be Arabic).
    pub name: String,
    /// Textual IP address.
    pub address: String,
}

/// Service -> GUI on an events connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuiEvent {
    /// Full snapshot (sent first, and whenever the GUI may have missed something).
    Status(ServiceStatus),
    IncomingRequest(GuiRequest),
    RequestCancelled {
        id: u64,
    },
    SessionStarted(GuiSession),
    SessionEnded {
        id: u64,
    },
    CodeChanged {
        code: String,
    },
    Desktop(WireDesktop),
    /// An app-defined event (postcard bytes of the app's own type: transfers, chat, conflict
    /// prompts, ...). The hub forwards it and keeps nothing (v2).
    App(Vec<u8>),
}

/// GUI -> service on a command connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuiCommand {
    /// Answer an Accept prompt.
    Respond {
        request: u64,
        accept: bool,
    },
    RotateCode,
    Disconnect {
        session: u64,
    },
    SetPermissions {
        session: u64,
        permissions: Permissions,
    },
    /// An app-defined command (postcard bytes of the app's own type), answered with
    /// [`GuiReply::App`] (v2).
    App(Vec<u8>),
    /// Re-read the permanent password and the reconnect tickets from the data directory (the
    /// GUI sends it after saving the unattended password; audit F5). Appended last: the
    /// encoding of the other commands is unchanged.
    ReloadCredentials,
}

/// Service -> GUI answer to a [`GuiCommand`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuiReply {
    Ok,
    /// The request/session no longer exists.
    NotFound,
    Error(WireError),
    /// Answer to [`GuiCommand::App`] (postcard bytes of the app's own type).
    App(Vec<u8>),
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn roundtrip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(msg: &T) {
        let mut buf = Vec::new();
        write_msg(&mut buf, msg).unwrap();
        let back: T = read_msg(&mut Cursor::new(buf)).unwrap();
        assert_eq!(&back, msg);
    }

    fn arabic_peer() -> GuiPeer {
        GuiPeer {
            device_id: 123_456_789,
            name: "حاسوب المكتب Office-2".into(),
            address: "192.168.1.20".into(),
        }
    }

    #[test]
    fn every_ipc_message_roundtrips() {
        roundtrip(&AgentHello {
            version: IPC_VERSION,
            channel: AgentChannel::Capture {
                monitor: 1,
                max_width: 1920,
                max_height: 1200,
            },
        });
        roundtrip(&AgentWelcome::Refused {
            version: 9,
            error: WireError::VersionMismatch {
                expected: 1,
                got: 9,
            },
        });
        roundtrip(&CaptureReply::NoChange {
            cursor: WireCursor {
                x: -5,
                y: 7,
                visible: true,
                shape: Some(WireCursorShape {
                    width: 1,
                    height: 1,
                    hotspot_x: 0,
                    hotspot_y: 0,
                    rgba: vec![1, 2, 3, 4],
                }),
            },
        });
        roundtrip(&InputCommand::Event(InputEvent::Key {
            scancode: 0x1e,
            extended: false,
            down: true,
        }));
        roundtrip(&CommandReply::Monitors(vec![WireMonitor {
            index: 0,
            name: r"\\.\DISPLAY1".into(),
            width: 1920,
            height: 1080,
            primary: true,
            x: 0,
            y: 0,
        }]));
        roundtrip(&AgentEvent::Desktop {
            name: "Winlogon".into(),
            secure: true,
        });
        roundtrip(&GuiEvent::Status(ServiceStatus {
            device_id: 1,
            device_name: "جهاز تامر".into(),
            one_time_code: "042917".into(),
            requests: vec![GuiRequest {
                id: 3,
                peer: arabic_peer(),
            }],
            sessions: vec![GuiSession {
                id: 4,
                peer: arabic_peer(),
                permissions: Permissions::VIEW | Permissions::SYSTEM,
            }],
            desktop: WireDesktop {
                secure_desktop: true,
                locked: true,
            },
        }));
        roundtrip(&GuiCommand::SetPermissions {
            session: 4,
            permissions: Permissions::VIEW,
        });
    }

    #[test]
    fn feature_messages_roundtrip_with_arabic() {
        roundtrip(&AgentHello {
            version: IPC_VERSION,
            channel: AgentChannel::Feature,
        });
        roundtrip(&AgentHello {
            version: IPC_VERSION,
            channel: AgentChannel::Audio,
        });
        roundtrip(&FeatureRequest::ShellOffer {
            offer: 3,
            dir: r"C:\ProgramData\TLP-Desk\paste\مجلد 1".into(),
            items: vec![
                TransferItem {
                    rel_path: "تقارير/الربع الأول.xlsx".into(),
                    size: 1234,
                    modified_unix_ms: 1_700_000_000_000,
                    is_dir: false,
                },
                TransferItem {
                    rel_path: "تقارير".into(),
                    size: 0,
                    modified_unix_ms: 0,
                    is_dir: true,
                },
            ],
        });
        roundtrip(&FeatureRequest::ShellStage {
            offer: 3,
            state: WireStage::Failed("انقطع الاتصال".into()),
        });
        roundtrip(&FeatureRequest::OverlayStrokes {
            id: 9,
            reset: true,
            strokes: vec![WireStroke {
                id: 1,
                color: 0xff00_00ff,
                width: 30,
                points: vec![(0, 0), (65_535, 65_535)],
            }],
        });
        roundtrip(&FeatureRequest::OverlayTarget {
            id: 9,
            rect: WireRect {
                x: -1920,
                y: 0,
                width: 1920,
                height: 1080,
            },
        });
        roundtrip(&FeatureRequest::MarkerOpen {
            id: 2,
            label: "حاسوب سارة".into(),
        });
        roundtrip(&FeatureRequest::ShieldEngage {
            id: 4,
            lines: vec![
                WireScreenLine {
                    text: "هذا الجهاز قيد الاستخدام عن بُعد".into(),
                    large: true,
                },
                WireScreenLine {
                    text: "Laptop-١ (123 456 789)".into(),
                    large: false,
                },
            ],
        });
        roundtrip(&FeatureRequest::ClipboardWrite(WireClip::Html {
            html_len: 10,
            alt_len: 4,
        }));
        roundtrip(&FeatureReply::Files(Some(vec![
            r"C:\Users\تامر\ملف.txt".into()
        ])));
        roundtrip(&FeatureReply::Error(WireError::Unsupported));
        roundtrip(&AgentEvent::LocalInput {
            keyboard: true,
            pointer: false,
        });
        roundtrip(&AgentEvent::Shield {
            id: 4,
            signal: WireShieldSignal::EmergencyExit,
        });
        roundtrip(&AgentEvent::StageBegin { offer: 3 });
        roundtrip(&AudioPacket {
            rate: 48_000,
            channels: 2,
            payload_len: 3840,
        });
        roundtrip(&GuiEvent::App("نقل ملف".as_bytes().to_vec()));
        roundtrip(&GuiCommand::App(vec![0, 1, 2]));
        roundtrip(&GuiReply::App(Vec::new()));
    }

    #[test]
    fn clipboard_payloads_roundtrip_byte_exact() {
        let cases = [
            ClipData::Text("مرحبا\r\n\u{200f}عالم\u{200d}ـ".into()),
            ClipData::Html {
                html: "<b dir=\"rtl\">مرحبا</b>".into(),
                alt_text: "مرحبا".into(),
            },
            ClipData::Html {
                html: String::new(),
                alt_text: "only alt".into(),
            },
            ClipData::Image {
                width: 2,
                height: 1,
                rgba: vec![1, 2, 3, 4, 5, 6, 7, 8],
            },
        ];
        for clip in cases {
            let mut buf = Vec::new();
            write_clip_reply(&mut buf, Some(&clip)).unwrap();
            let mut r = Cursor::new(buf);
            let FeatureReply::Clip(Some(header)) = read_msg(&mut r).unwrap() else {
                panic!("expected a clip header");
            };
            assert_eq!(read_clip_payload(&mut r, header).unwrap(), clip);
        }
        let mut buf = Vec::new();
        write_clip_reply(&mut buf, None).unwrap();
        assert_eq!(
            read_msg::<_, FeatureReply>(&mut Cursor::new(buf)).unwrap(),
            FeatureReply::Clip(None)
        );
    }

    #[test]
    fn hostile_clipboard_headers_are_errors() {
        // Wrong size for the image.
        assert!(ClipData::from_wire(
            WireClip::Image {
                width: 2,
                height: 2
            },
            vec![0; 4]
        )
        .is_err());
        // Invalid UTF-8.
        assert!(ClipData::from_wire(WireClip::Text { len: 2 }, vec![0xff, 0xfe]).is_err());
        // An HTML split in the middle of a character.
        let bytes = "مر".as_bytes().to_vec();
        assert!(ClipData::from_wire(
            WireClip::Html {
                html_len: 1,
                alt_len: 3
            },
            bytes
        )
        .is_err());
        // Sizes that overflow saturate and are refused before allocating.
        let huge = WireClip::Image {
            width: u32::MAX,
            height: u32::MAX,
        };
        assert_eq!(huge.payload_len(), u64::MAX);
        assert!(matches!(
            read_clip_payload(&mut Cursor::new(Vec::new()), huge),
            Err(HostError::IpcTooLarge { .. })
        ));
        // Contents are never printed.
        let secret = ClipData::Text("كلمة السر 1234".into());
        assert!(!format!("{secret:?}").contains("1234"));
    }

    #[test]
    fn arabic_names_survive_byte_exact() {
        let name = "مكتب\u{200f}-تامر\u{200d}ـ٣";
        let mut buf = Vec::new();
        write_msg(&mut buf, &arabic_peer()).unwrap();
        let mut p = arabic_peer();
        p.name = name.into();
        buf.clear();
        write_msg(&mut buf, &p).unwrap();
        let back: GuiPeer = read_msg(&mut Cursor::new(buf)).unwrap();
        assert_eq!(back.name.as_bytes(), name.as_bytes());
    }

    #[test]
    fn payload_follows_header_and_reuses_buffer() {
        let pixels: Vec<u8> = (0..=255).collect();
        let mut buf = Vec::new();
        let hdr = CaptureReply::Frame {
            width: 8,
            height: 8,
            stride: 32,
            timestamp_us: 1,
            payload_len: pixels.len() as u64,
            cursor: WireCursor {
                x: 0,
                y: 0,
                visible: false,
                shape: None,
            },
        };
        write_msg_with_payload(&mut buf, &hdr, &pixels).unwrap();
        write_msg(&mut buf, &CaptureRequest::NextFrame { timeout_ms: 5 }).unwrap();
        let mut r = Cursor::new(buf);
        let back: CaptureReply = read_msg(&mut r).unwrap();
        let CaptureReply::Frame { payload_len, .. } = back else {
            panic!("expected a frame, got {back:?}");
        };
        let mut data = Vec::with_capacity(1024);
        read_payload_into(&mut r, payload_len, &mut data).unwrap();
        assert_eq!(data, pixels);
        // The next message is intact after the payload.
        let next: CaptureRequest = read_msg(&mut r).unwrap();
        assert_eq!(next, CaptureRequest::NextFrame { timeout_ms: 5 });
        assert!(matches!(
            read_msg::<_, CaptureRequest>(&mut r),
            Err(HostError::IpcClosed)
        ));
    }

    #[test]
    fn oversized_lengths_are_refused_before_allocating() {
        // Header length beyond the cap.
        let mut bytes = (u32::MAX).to_le_bytes().to_vec();
        bytes.extend_from_slice(&[0; 8]);
        assert!(matches!(
            read_msg::<_, AgentCommand>(&mut Cursor::new(bytes)),
            Err(HostError::IpcTooLarge { .. })
        ));
        // Payload length beyond the cap.
        let mut v = Vec::new();
        assert!(matches!(
            read_payload_into(&mut Cursor::new(Vec::new()), u64::MAX, &mut v),
            Err(HostError::IpcTooLarge { .. })
        ));
        assert!(v.capacity() < 1024);
        // Writing a too-large payload fails instead of sending it.
        let big = vec![0u8; MAX_PAYLOAD + 1];
        assert!(matches!(
            write_msg_with_payload(&mut Vec::new(), &AgentCommand::Exit, &big),
            Err(HostError::IpcTooLarge { .. })
        ));
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        // Truncated header length.
        assert!(read_msg::<_, AgentCommand>(&mut Cursor::new(vec![1, 0])).is_err());
        // Truncated body.
        assert!(read_msg::<_, AgentCommand>(&mut Cursor::new(vec![5, 0, 0, 0, 1])).is_err());
        // Unknown enum tag.
        assert!(matches!(
            read_msg::<_, AgentCommand>(&mut Cursor::new(vec![1, 0, 0, 0, 200])),
            Err(HostError::IpcDecode(_))
        ));
        // Truncated payload.
        let mut v = Vec::new();
        assert!(read_payload_into(&mut Cursor::new(vec![1, 2]), 10, &mut v).is_err());
        // Random bytes never panic.
        let mut seed = 0x1234_5678_u32;
        for _ in 0..500 {
            let mut bytes = Vec::new();
            for _ in 0..(seed % 40) {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                bytes.push((seed & 0xff) as u8);
            }
            let _ = read_msg::<_, GuiEvent>(&mut Cursor::new(bytes.clone()));
            let _ = read_msg::<_, CaptureReply>(&mut Cursor::new(bytes));
        }
    }

    #[test]
    fn empty_stream_is_closed() {
        assert!(matches!(
            read_msg::<_, GuiEvent>(&mut Cursor::new(Vec::new())),
            Err(HostError::IpcClosed)
        ));
    }
}
