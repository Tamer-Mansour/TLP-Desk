//! Headers that start every QUIC stream other than the control stream.
//!
//! Since protocol v4, each unidirectional stream and each bidirectional stream after the
//! control stream begins with a 9-byte [`StreamHeader`]: `kind: u8`, then `id: u64`
//! little-endian. The receiver reads it first and routes the stream by kind (video frames,
//! file data, tunnels, audio, large clipboard payloads). Unknown kinds are refused.

use crate::ProtoError;

/// What a QUIC stream carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum StreamKind {
    /// Host -> viewer uni stream with exactly one framed `Message::VideoFrame`; `id` = `seq`.
    Video = 0,
    /// File transfer bytes (uni). `id` = transfer id; the body starts with a
    /// [`FileDataHeader`] followed by raw file bytes up to the end of the stream.
    FileData = 1,
    /// One forwarded TCP connection (bidi, opened by the viewer). `id` = tunnel id from
    /// `Message::TunnelOpen`; raw bytes both ways.
    Tunnel = 2,
    /// System audio (uni, host -> viewer). `id` = audio stream generation; the body is a
    /// sequence of `u16` little-endian length + Opus packet.
    Audio = 3,
    /// A clipboard payload too large for the control stream (uni). `id` = clipboard
    /// sequence number; the body is one framed `Message` (`ClipboardData` / `ClipboardHtml`).
    Clipboard = 4,
}

impl StreamKind {
    /// Every kind, in wire order.
    pub const ALL: [StreamKind; 5] = [
        StreamKind::Video,
        StreamKind::FileData,
        StreamKind::Tunnel,
        StreamKind::Audio,
        StreamKind::Clipboard,
    ];

    /// The kinds features open with `Session::open_*` (everything except video).
    pub const SIDE: [StreamKind; 4] = [
        StreamKind::FileData,
        StreamKind::Tunnel,
        StreamKind::Audio,
        StreamKind::Clipboard,
    ];

    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Video),
            1 => Some(Self::FileData),
            2 => Some(Self::Tunnel),
            3 => Some(Self::Audio),
            4 => Some(Self::Clipboard),
            _ => None,
        }
    }
}

/// Length of an encoded [`StreamHeader`].
pub const STREAM_HEADER_LEN: usize = 9;

/// First bytes of every non-control stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamHeader {
    pub kind: StreamKind,
    pub id: u64,
}

impl StreamHeader {
    pub const fn new(kind: StreamKind, id: u64) -> Self {
        Self { kind, id }
    }

    pub fn encode(&self) -> [u8; STREAM_HEADER_LEN] {
        let mut b = [0u8; STREAM_HEADER_LEN];
        b[0] = self.kind as u8;
        b[1..].copy_from_slice(&self.id.to_le_bytes());
        b
    }

    /// Fails with [`ProtoError::UnknownStreamKind`] for a kind this version does not know.
    pub fn decode(b: &[u8; STREAM_HEADER_LEN]) -> Result<Self, ProtoError> {
        let kind = StreamKind::from_u8(b[0]).ok_or(ProtoError::UnknownStreamKind(b[0]))?;
        let mut id = [0u8; 8];
        id.copy_from_slice(&b[1..]);
        Ok(Self {
            kind,
            id: u64::from_le_bytes(id),
        })
    }
}

/// Length of an encoded [`FileDataHeader`].
pub const FILE_DATA_HEADER_LEN: usize = 12;

/// Start of a [`StreamKind::FileData`] stream body (after the [`StreamHeader`]): which item of
/// the transfer and at which byte offset the following raw bytes start. One item may be split
/// over several streams (ranges) to use more parallelism; ranges never overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileDataHeader {
    /// Index into `TransferOffer::items`.
    pub item: u32,
    /// Byte offset in the file.
    pub offset: u64,
}

impl FileDataHeader {
    pub fn encode(&self) -> [u8; FILE_DATA_HEADER_LEN] {
        let mut b = [0u8; FILE_DATA_HEADER_LEN];
        b[..4].copy_from_slice(&self.item.to_le_bytes());
        b[4..].copy_from_slice(&self.offset.to_le_bytes());
        b
    }

    pub fn decode(b: &[u8; FILE_DATA_HEADER_LEN]) -> Self {
        let mut item = [0u8; 4];
        item.copy_from_slice(&b[..4]);
        let mut offset = [0u8; 8];
        offset.copy_from_slice(&b[4..]);
        Self {
            item: u32::from_le_bytes(item),
            offset: u64::from_le_bytes(offset),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_header_roundtrips_every_kind() {
        for kind in StreamKind::ALL {
            let h = StreamHeader::new(kind, 0x0102_0304_0506_0708);
            let b = h.encode();
            assert_eq!(b[0], kind as u8);
            assert_eq!(&b[1..], &[8, 7, 6, 5, 4, 3, 2, 1]);
            assert_eq!(StreamHeader::decode(&b).unwrap(), h);
        }
    }

    #[test]
    fn unknown_stream_kind_is_rejected() {
        for k in [5u8, 0x7F, 0xFF] {
            let mut b = [0u8; STREAM_HEADER_LEN];
            b[0] = k;
            assert!(matches!(
                StreamHeader::decode(&b),
                Err(ProtoError::UnknownStreamKind(x)) if x == k
            ));
        }
    }

    #[test]
    fn kind_values_are_stable() {
        let v: Vec<u8> = StreamKind::ALL.iter().map(|k| *k as u8).collect();
        assert_eq!(v, [0, 1, 2, 3, 4]);
        assert!(!StreamKind::SIDE.contains(&StreamKind::Video));
    }

    #[test]
    fn file_data_header_roundtrips() {
        let h = FileDataHeader {
            item: 7,
            offset: (5 << 32) + 3,
        };
        assert_eq!(FileDataHeader::decode(&h.encode()), h);
    }
}
