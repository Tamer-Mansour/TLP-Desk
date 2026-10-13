use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tlp_desk_crypto::{DeviceId, Fingerprint};
use tlp_desk_proto::{
    encode_frame, Bye, ByeReason, FrameDecoder, Message, Permissions, Role, StreamHeader,
    StreamKind, VideoFrame, MAX_FRAME_LEN, STREAM_HEADER_LEN,
};
use tokio::sync::{mpsc, Mutex};

use crate::NetError;

/// QUIC application close codes.
pub(crate) const CLOSE_OK: u32 = 0;
pub(crate) const CLOSE_REJECTED: u32 = 1;
pub(crate) const CLOSE_PROTOCOL: u32 = 2;

/// Stream error code (`STOP_SENDING`) for a stream the receiver will not read: unknown or
/// unexpected kind, malformed header, or nobody accepting that kind.
pub const STREAM_REFUSED: u32 = 1;

/// How long a peer has to send the stream header after opening a stream.
const STREAM_HEADER_TIMEOUT: Duration = Duration::from_secs(10);

/// Default number of video frames that may be unacknowledged at once before non-key frames
/// are dropped (see [`Session::send_video`]).
pub const DEFAULT_MAX_VIDEO_IN_FLIGHT: usize = 3;

/// Frames the viewer buffers between the network and the decoder before dropping.
const VIEWER_VIDEO_QUEUE: usize = 4;

/// Who is on the other end of a session or pending session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    /// Remote UDP address (IPv4-mapped IPv6 normalized to IPv4).
    pub addr: SocketAddr,
    /// Derived from the key the peer proved in the TLS handshake (authentic).
    pub device_id: DeviceId,
    /// Full SHA-256 of the peer's public key: store it in `KnownHosts` for TOFU.
    pub fingerprint: Fingerprint,
    /// Self-reported display name from `Hello` (not authenticated).
    pub name: String,
}

/// Result of [`Session::send_video`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoSendStatus {
    /// Handed to QUIC.
    Sent,
    /// Dropped because the viewer is behind. The encoder **must** produce a keyframe next;
    /// until a keyframe is offered, all non-key frames are dropped.
    Dropped,
}

/// Reader of length-prefixed [`Message`]s from a QUIC receive stream.
pub(crate) struct FramedRecv {
    recv: quinn::RecvStream,
    dec: FrameDecoder,
    buf: Vec<u8>,
}

impl FramedRecv {
    pub(crate) fn new(recv: quinn::RecvStream) -> Self {
        Self {
            recv,
            dec: FrameDecoder::new(),
            buf: vec![0u8; 64 * 1024],
        }
    }

    pub(crate) async fn next(&mut self) -> Result<Message, NetError> {
        loop {
            if let Some(m) = self.dec.next_message()? {
                return Ok(m);
            }
            match self.recv.read(&mut self.buf).await? {
                Some(n) => self.dec.push(&self.buf[..n]),
                None => return Err(NetError::Closed),
            }
        }
    }
}

pub(crate) async fn write_msg(send: &mut quinn::SendStream, msg: &Message) -> Result<(), NetError> {
    send.write_all(&encode_frame(msg)?).await?;
    Ok(())
}

struct VideoTx {
    in_flight: Arc<AtomicUsize>,
    max_in_flight: usize,
    need_keyframe: AtomicBool,
}

struct VideoRx {
    rx: Mutex<mpsc::Receiver<VideoFrame>>,
    /// Set by the reader task when it had to drop a frame.
    gap: Arc<AtomicBool>,
    last_seq: AtomicU64,
    have_last: AtomicBool,
    waiting_keyframe: AtomicBool,
    last_kf_request: std::sync::Mutex<Option<Instant>>,
}

/// An authenticated, authorized session.
///
/// * **Control**: one bidirectional QUIC stream (opened by the viewer) carrying framed
///   [`Message`]s: [`Session::send`] / [`Session::recv`].
/// * **Video**: one unidirectional QUIC stream **per frame**, opened by the host
///   ([`Session::send_video`] / [`Session::recv_video`]). A late frame never blocks newer ones
///   (no head-of-line blocking) and the host drops frames instead of queueing when more than
///   `max_in_flight` are unacknowledged.
///
/// All methods take `&self`; wrap the session in an `Arc` to use it from several tasks.
///
/// Keep the `Session` alive for as long as the session should last: dropping it (the last
/// handle) closes the QUIC connection immediately and discards unsent data. Use
/// [`Session::close`] for a graceful end.
pub struct Session {
    conn: quinn::Connection,
    role: Role,
    peer: PeerInfo,
    permissions: Permissions,
    ctrl_tx: Mutex<quinn::SendStream>,
    ctrl_rx: Mutex<FramedRecv>,
    video_tx: Option<VideoTx>,
    video_rx: Option<VideoRx>,
    rt: tokio::runtime::Handle,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("role", &self.role)
            .field("peer", &self.peer)
            .field("permissions", &self.permissions)
            .finish_non_exhaustive()
    }
}

impl Session {
    pub(crate) fn new(
        conn: quinn::Connection,
        role: Role,
        peer: PeerInfo,
        permissions: Permissions,
        ctrl_tx: quinn::SendStream,
        ctrl_rx: FramedRecv,
    ) -> Self {
        let _ = ctrl_tx.set_priority(10);
        let rt = tokio::runtime::Handle::current();
        let mut router = StreamRouter { video: None };
        let (video_tx, video_rx) = match role {
            Role::Host => (
                Some(VideoTx {
                    in_flight: Arc::new(AtomicUsize::new(0)),
                    max_in_flight: DEFAULT_MAX_VIDEO_IN_FLIGHT,
                    need_keyframe: AtomicBool::new(false),
                }),
                None,
            ),
            Role::Viewer => {
                let (tx, rx) = mpsc::channel(VIEWER_VIDEO_QUEUE);
                let gap = Arc::new(AtomicBool::new(false));
                router.video = Some((tx, gap.clone()));
                (
                    None,
                    Some(VideoRx {
                        rx: Mutex::new(rx),
                        gap,
                        last_seq: AtomicU64::new(0),
                        have_last: AtomicBool::new(false),
                        waiting_keyframe: AtomicBool::new(false),
                        last_kf_request: std::sync::Mutex::new(None),
                    }),
                )
            }
        };
        // The router (with every sender) lives only in the acceptor tasks: when the connection
        // ends they exit and every receiver sees the end of its queue.
        let router = Arc::new(router);
        rt.spawn(accept_uni_loop(conn.clone(), router.clone()));
        rt.spawn(accept_bi_loop(conn.clone(), router));
        Self {
            conn,
            role,
            peer,
            permissions,
            ctrl_tx: Mutex::new(ctrl_tx),
            ctrl_rx: Mutex::new(ctrl_rx),
            video_tx,
            video_rx,
            rt,
        }
    }

    /// Our role in this session.
    pub fn role(&self) -> Role {
        self.role
    }

    pub fn peer(&self) -> &PeerInfo {
        &self.peer
    }

    /// Permissions granted when the session was accepted. Later live changes travel as
    /// `Message::Permissions` on the control stream and are tracked by the host/viewer crates.
    pub fn initial_permissions(&self) -> Permissions {
        self.permissions
    }

    /// Current smoothed round-trip time.
    pub fn rtt(&self) -> Duration {
        self.conn.rtt()
    }

    /// Sends one message on the control stream.
    pub async fn send(&self, msg: &Message) -> Result<(), NetError> {
        let mut tx = self.ctrl_tx.lock().await;
        write_msg(&mut tx, msg).await
    }

    /// Receives the next control message. `Err(NetError::Closed)` once the peer finished the
    /// stream; connection errors otherwise.
    pub async fn recv(&self) -> Result<Message, NetError> {
        self.ctrl_rx.lock().await.next().await
    }

    /// Host only: sends one encoded video frame on its own QUIC stream without waiting.
    ///
    /// Drop-instead-of-queue: if `max_in_flight` frames are still unacknowledged, a non-key
    /// frame is dropped and [`VideoSendStatus::Dropped`] tells the encoder to make the next
    /// frame a keyframe. Keyframes are sent unless twice the limit is in flight.
    /// `frame.seq` must increase by 1 per encoded frame (the viewer uses gaps to detect loss).
    pub fn send_video(&self, frame: VideoFrame) -> Result<VideoSendStatus, NetError> {
        let v = self
            .video_tx
            .as_ref()
            .ok_or_else(|| NetError::Unexpected("only the host sends video".into()))?;
        let in_flight = v.in_flight.load(Ordering::Acquire);
        let over = in_flight >= v.max_in_flight;
        if frame.keyframe {
            if in_flight >= v.max_in_flight * 2 {
                v.need_keyframe.store(true, Ordering::Release);
                return Ok(VideoSendStatus::Dropped);
            }
        } else if over || v.need_keyframe.load(Ordering::Acquire) {
            v.need_keyframe.store(true, Ordering::Release);
            tracing::trace!(seq = frame.seq, in_flight, "video frame dropped");
            return Ok(VideoSendStatus::Dropped);
        }
        let seq = frame.seq;
        let mut bytes = StreamHeader::new(StreamKind::Video, seq).encode().to_vec();
        bytes.extend_from_slice(&encode_frame(&Message::VideoFrame(frame))?);
        if v.need_keyframe.swap(false, Ordering::AcqRel) {
            tracing::debug!(seq, "keyframe resumes video after drops");
        }
        v.in_flight.fetch_add(1, Ordering::AcqRel);
        let counter = v.in_flight.clone();
        let conn = self.conn.clone();
        self.rt.spawn(async move {
            let res: Result<(), NetError> = async {
                let mut s = conn.open_uni().await?;
                let _ = s.set_priority(0);
                s.write_all(&bytes).await?;
                let _ = s.finish();
                // Count the frame as in flight until the viewer acknowledged all of it.
                let _ = s.stopped().await;
                Ok(())
            }
            .await;
            counter.fetch_sub(1, Ordering::AcqRel);
            if let Err(e) = res {
                tracing::debug!(seq, %e, "video frame send failed");
            }
        });
        Ok(VideoSendStatus::Sent)
    }

    /// Host only: number of video frames not yet acknowledged by the viewer.
    pub fn video_in_flight(&self) -> usize {
        self.video_tx
            .as_ref()
            .map_or(0, |v| v.in_flight.load(Ordering::Acquire))
    }

    /// Viewer only: next decodable video frame, in order.
    ///
    /// Stale (out-of-order) frames are discarded. After any loss, non-key frames are skipped
    /// until the next keyframe and a `Message::RequestKeyframe` is sent to the host
    /// automatically (at most every 250 ms).
    pub async fn recv_video(&self) -> Result<VideoFrame, NetError> {
        let v = self
            .video_rx
            .as_ref()
            .ok_or_else(|| NetError::Unexpected("only the viewer receives video".into()))?;
        let mut rx = v.rx.lock().await;
        loop {
            let frame = rx.recv().await.ok_or(NetError::Closed)?;
            let have_last = v.have_last.load(Ordering::Acquire);
            let last = v.last_seq.load(Ordering::Acquire);
            if have_last && frame.seq <= last {
                tracing::trace!(seq = frame.seq, last, "stale video frame discarded");
                continue;
            }
            let lost = v.gap.swap(false, Ordering::AcqRel)
                || (have_last && frame.seq != last + 1)
                || (!have_last && !frame.keyframe);
            if lost {
                v.waiting_keyframe.store(true, Ordering::Release);
            }
            if frame.keyframe {
                v.waiting_keyframe.store(false, Ordering::Release);
            }
            v.have_last.store(true, Ordering::Release);
            v.last_seq.store(frame.seq, Ordering::Release);
            if v.waiting_keyframe.load(Ordering::Acquire) {
                self.request_keyframe_throttled(v).await;
                continue;
            }
            return Ok(frame);
        }
    }

    async fn request_keyframe_throttled(&self, v: &VideoRx) {
        let now = Instant::now();
        let due = {
            let mut last = v.last_kf_request.lock().unwrap_or_else(|p| p.into_inner());
            let due = match *last {
                None => true,
                Some(t) => now.duration_since(t) >= Duration::from_millis(250),
            };
            if due {
                *last = Some(now);
            }
            due
        };
        if due {
            tracing::debug!("video gap: requesting keyframe");
            let _ = self.send(&Message::RequestKeyframe).await;
        }
    }

    /// Graceful close: sends `Bye`, finishes the control stream, closes the connection.
    pub async fn close(&self, reason: ByeReason) {
        {
            let mut tx = self.ctrl_tx.lock().await;
            let _ = write_msg(&mut tx, &Message::Bye(Bye { reason })).await;
            let _ = tx.finish();
            let _ = tokio::time::timeout(Duration::from_millis(500), tx.stopped()).await;
        }
        self.conn.close(CLOSE_OK.into(), b"bye");
    }

    /// Resolves when the connection is closed (by either side or by idle timeout).
    pub async fn closed(&self) -> NetError {
        NetError::Connection(self.conn.closed().await)
    }
}

impl Drop for Session {
    /// The acceptor tasks hold connection handles too, so close explicitly: dropping the last
    /// `Session` handle ends the connection (as documented above).
    fn drop(&mut self) {
        self.conn.close(CLOSE_OK.into(), b"session dropped");
    }
}

/// Destinations of incoming non-control streams, by [`StreamKind`].
struct StreamRouter {
    /// Viewer only: decoded-frame queue and the "a frame was lost" flag.
    video: Option<(mpsc::Sender<VideoFrame>, Arc<AtomicBool>)>,
}

/// Reads the [`StreamHeader`] that starts every non-control stream.
async fn read_stream_header(recv: &mut quinn::RecvStream) -> Result<StreamHeader, NetError> {
    let mut b = [0u8; STREAM_HEADER_LEN];
    match tokio::time::timeout(STREAM_HEADER_TIMEOUT, recv.read_exact(&mut b)).await {
        Err(_) => Err(NetError::Timeout),
        Ok(Err(e)) => Err(NetError::Unexpected(format!("stream header: {e}"))),
        Ok(Ok(())) => Ok(StreamHeader::decode(&b)?),
    }
}

fn refuse(recv: &mut quinn::RecvStream, why: &str) {
    tracing::debug!(why, "refusing incoming stream");
    let _ = recv.stop(STREAM_REFUSED.into());
}

async fn accept_uni_loop(conn: quinn::Connection, router: Arc<StreamRouter>) {
    while let Ok(mut recv) = conn.accept_uni().await {
        let router = router.clone();
        tokio::spawn(async move {
            let header = match read_stream_header(&mut recv).await {
                Ok(h) => h,
                Err(e) => {
                    if let Some((_, gap)) = &router.video {
                        // Possibly a video frame we could not read.
                        gap.store(true, Ordering::Release);
                    }
                    return refuse(&mut recv, &e.to_string());
                }
            };
            match header.kind {
                StreamKind::Video => match &router.video {
                    Some((tx, gap)) => read_video_frame(recv, tx, gap).await,
                    None => refuse(&mut recv, "video stream to a host"),
                },
                // FEATURE HOOK: side streams are routed in the next step.
                _ => refuse(&mut recv, "no handler for this stream kind"),
            }
        });
    }
}

async fn accept_bi_loop(conn: quinn::Connection, router: Arc<StreamRouter>) {
    // The control stream was accepted during the handshake; any later bidi stream is a side
    // stream.
    while let Ok((_send, mut recv)) = conn.accept_bi().await {
        let _router = router.clone();
        tokio::spawn(async move {
            match read_stream_header(&mut recv).await {
                Ok(h) => refuse(&mut recv, &format!("no handler for bidi {:?}", h.kind)),
                Err(e) => refuse(&mut recv, &e.to_string()),
            }
        });
    }
}

/// One video frame per stream: read it whole and queue it for the decoder.
async fn read_video_frame(
    mut stream: quinn::RecvStream,
    tx: &mpsc::Sender<VideoFrame>,
    gap: &AtomicBool,
) {
    let bytes = match stream.read_to_end(MAX_FRAME_LEN + 4).await {
        Ok(b) => b,
        Err(e) => {
            tracing::debug!(%e, "video stream read failed");
            gap.store(true, Ordering::Release);
            return;
        }
    };
    let mut dec = FrameDecoder::new();
    dec.push(&bytes);
    match dec.next_message() {
        Ok(Some(Message::VideoFrame(f))) => {
            if tx.try_send(f).is_err() {
                // Decoder is behind: drop rather than queue.
                gap.store(true, Ordering::Release);
            }
        }
        other => tracing::debug!(?other, "unexpected data on video stream"),
    }
}
