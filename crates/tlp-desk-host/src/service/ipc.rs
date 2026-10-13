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

use std::io::{ErrorKind, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tlp_desk_proto::{InputEvent, Permissions};

use crate::HostError;

/// Version of the local IPC messages. Both ends must match (they are the same binary, so a
/// mismatch means a half-finished upgrade: the old process is told to exit).
pub const IPC_VERSION: u32 = 1;

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
    /// One-way agent -> service: [`AgentEvent`]s (desktop switches).
    Events,
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
}

/// Service -> GUI answer to a [`GuiCommand`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuiReply {
    Ok,
    /// The request/session no longer exists.
    NotFound,
    Error(WireError),
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
