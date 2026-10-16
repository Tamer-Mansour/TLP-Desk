//! Body of a `StreamKind::Audio` stream (protocol v4): repeated `u16` LE length + one Opus
//! packet. The stream id is the *generation*; packets are numbered by their position in the
//! stream (QUIC delivers a stream reliably and in order), so the sequence number and the
//! timestamp (`seq * 10 ms`) of every packet are implicit and cost no bytes.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::AudioError;
use crate::format::MAX_PACKET;

fn io_err(e: std::io::Error) -> AudioError {
    AudioError::Stream(e.to_string())
}

/// Writes one packet (length prefix and bytes in a single write).
pub(crate) async fn write_packet<W: AsyncWrite + Unpin>(
    w: &mut W,
    packet: &[u8],
    scratch: &mut Vec<u8>,
) -> Result<(), AudioError> {
    let len = u16::try_from(packet.len())
        .ok()
        .filter(|&l| l > 0 && usize::from(l) <= MAX_PACKET)
        .ok_or(AudioError::BadPacket {
            len: packet.len(),
            max: MAX_PACKET,
        })?;
    scratch.clear();
    scratch.extend_from_slice(&len.to_le_bytes());
    scratch.extend_from_slice(packet);
    w.write_all(scratch).await.map_err(io_err)
}

/// Reads the next packet into `buf`. `Ok(false)` when the stream ended cleanly between
/// packets.
///
/// # Errors
/// [`AudioError::BadPacket`] for a zero or oversized length (checked before allocating),
/// [`AudioError::Stream`] for a stream that ends inside a packet or fails.
pub(crate) async fn read_packet<R: AsyncRead + Unpin>(
    r: &mut R,
    buf: &mut Vec<u8>,
) -> Result<bool, AudioError> {
    let mut len = [0u8; 2];
    if r.read(&mut len[..1]).await.map_err(io_err)? == 0 {
        return Ok(false);
    }
    r.read_exact(&mut len[1..]).await.map_err(io_err)?;
    let len = usize::from(u16::from_le_bytes(len));
    if len == 0 || len > MAX_PACKET {
        return Err(AudioError::BadPacket {
            len,
            max: MAX_PACKET,
        });
    }
    buf.resize(len, 0);
    r.read_exact(buf).await.map_err(io_err)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn packets_round_trip_and_end_cleanly() {
        let (mut a, mut b) = tokio::io::duplex(64 * 1024);
        let mut scratch = Vec::new();
        for n in 1..=50u8 {
            write_packet(&mut a, &vec![n; usize::from(n) * 7], &mut scratch)
                .await
                .unwrap();
        }
        drop(a);
        let mut buf = Vec::new();
        for n in 1..=50u8 {
            assert!(read_packet(&mut b, &mut buf).await.unwrap());
            assert_eq!(buf, vec![n; usize::from(n) * 7]);
        }
        assert!(!read_packet(&mut b, &mut buf).await.unwrap());
    }

    #[tokio::test]
    async fn hostile_lengths_are_rejected() {
        let mut scratch = Vec::new();
        let (mut a, _b) = tokio::io::duplex(64);
        assert!(write_packet(&mut a, &[], &mut scratch).await.is_err());
        assert!(write_packet(&mut a, &[0; MAX_PACKET + 1], &mut scratch)
            .await
            .is_err());

        let mut buf = Vec::new();
        for bytes in [&[0u8, 0][..], &[0xff, 0xff], &[0xdd, 0x05]] {
            let mut r = bytes;
            assert!(matches!(
                read_packet(&mut r, &mut buf).await,
                Err(AudioError::BadPacket { .. })
            ));
        }
        // Truncated: a length and fewer bytes than announced, or half a length.
        let mut r = &[4u8, 0, 1, 2][..];
        assert!(matches!(
            read_packet(&mut r, &mut buf).await,
            Err(AudioError::Stream(_))
        ));
        let mut r = &[4u8][..];
        assert!(read_packet(&mut r, &mut buf).await.is_err());
    }
}
