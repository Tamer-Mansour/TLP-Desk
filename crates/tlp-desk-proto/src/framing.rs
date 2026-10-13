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
    let mut out = Vec::with_capacity(LEN_PREFIX + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Incremental decoder: feed arbitrary byte chunks, pull out complete messages.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Returns the next complete message, `Ok(None)` if more bytes are needed.
    pub fn next_message(&mut self) -> Result<Option<Message>, ProtoError> {
        let Some(prefix) = self.buf.first_chunk::<LEN_PREFIX>() else {
            return Ok(None);
        };
        let len = u32::from_le_bytes(*prefix) as usize;
        if len > MAX_FRAME_LEN {
            return Err(ProtoError::FrameTooLarge(len));
        }
        if self.buf.len() < LEN_PREFIX + len {
            return Ok(None);
        }
        let msg = decode_message(&self.buf[LEN_PREFIX..LEN_PREFIX + len]);
        self.buf.drain(..LEN_PREFIX + len);
        msg.map(Some)
    }
}
