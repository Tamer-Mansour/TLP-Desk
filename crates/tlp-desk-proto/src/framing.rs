use crate::{Message, ProtoError};

/// Largest accepted frame body. Bounds memory use against hostile peers.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

const LEN_PREFIX: usize = 4;

/// Serializes a message body without a length prefix.
pub fn encode_message(msg: &Message) -> Result<Vec<u8>, ProtoError> {
    Ok(postcard::to_stdvec(msg)?)
}

/// Deserializes a message body (no length prefix).
pub fn decode_message(body: &[u8]) -> Result<Message, ProtoError> {
    Ok(postcard::from_bytes(body)?)
}

/// Serializes a message as `u32 LE length || postcard body`.
pub fn encode_frame(msg: &Message) -> Result<Vec<u8>, ProtoError> {
    let body = encode_message(msg)?;
    if body.len() > MAX_FRAME_LEN {
        return Err(ProtoError::FrameTooLarge(body.len()));
    }
    let len = u32::try_from(body.len()).map_err(|_| ProtoError::FrameTooLarge(body.len()))?;
    let mut out = Vec::with_capacity(LEN_PREFIX + body.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Incremental decoder: feed arbitrary byte chunks, pull out complete messages.
#[derive(Debug)]
pub struct FrameDecoder {
    buf: Vec<u8>,
    max_len: usize,
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self {
            buf: Vec::new(),
            max_len: MAX_FRAME_LEN,
        }
    }
}

impl FrameDecoder {
    /// An empty decoder accepting frames up to [`MAX_FRAME_LEN`].
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty decoder accepting frame bodies up to `max_len` bytes (at most
    /// [`MAX_FRAME_LEN`]), e.g. a small limit before the peer is authenticated.
    pub fn with_max_len(max_len: usize) -> Self {
        Self {
            buf: Vec::new(),
            max_len: max_len.min(MAX_FRAME_LEN),
        }
    }

    /// Changes the body limit for the frames that follow (at most [`MAX_FRAME_LEN`]).
    pub fn set_max_len(&mut self, max_len: usize) {
        self.max_len = max_len.min(MAX_FRAME_LEN);
    }

    /// The current body limit.
    pub fn max_len(&self) -> usize {
        self.max_len
    }

    /// Appends received bytes. Call [`FrameDecoder::next_message`] until it returns `Ok(None)`
    /// after every push: then at most one incomplete frame (`max_len + 4` bytes) plus the
    /// latest chunk is ever buffered.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Bytes received but not yet returned as messages.
    pub fn buffered_len(&self) -> usize {
        self.buf.len()
    }

    /// Returns the next complete message, `Ok(None)` if more bytes are needed.
    ///
    /// A malformed body is consumed and reported as an error; an oversized length prefix is
    /// reported as [`ProtoError::FrameTooLarge`] before any of the body is buffered.
    pub fn next_message(&mut self) -> Result<Option<Message>, ProtoError> {
        let Some(prefix) = self.buf.first_chunk::<LEN_PREFIX>() else {
            return Ok(None);
        };
        let len = u32::from_le_bytes(*prefix) as usize;
        if len > self.max_len {
            return Err(ProtoError::FrameTooLarge(len));
        }
        let end = LEN_PREFIX + len;
        let Some(body) = self.buf.get(LEN_PREFIX..end) else {
            return Ok(None);
        };
        let msg = decode_message(body);
        self.buf.drain(..end);
        msg.map(Some)
    }
}
