//! Latest-frame mailbox between the decode thread and the UI.

use std::sync::{Arc, Mutex, MutexGuard};

use tlp_desk_capture::BgraFrame;

/// Pixel layout of [`DecodedFrame::pixels`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PixelFormat {
    /// B, G, R, A bytes (what the decoder produces; no conversion cost).
    #[default]
    Bgra,
    /// R, G, B, A bytes (what most GPU texture uploads and image encoders want). The swizzle
    /// runs on the decode thread, never on the UI thread.
    Rgba,
}

/// One decoded remote screen image. Pixels are tightly packed (`width * 4` bytes per row) and
/// opaque (alpha = 255).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub pixels: Vec<u8>,
    /// Host frame sequence number.
    pub seq: u64,
    /// Host capture time in microseconds since the host session started.
    pub host_timestamp_us: u64,
}

/// Single-slot "newest wins" mailbox: the decoder overwrites it, the UI takes the newest frame
/// when it paints. Frames the UI never took are dropped, so latency cannot build up.
#[derive(Debug, Default)]
pub struct FrameSlot {
    latest: Mutex<Option<DecodedFrame>>,
    spare: Mutex<Option<Vec<u8>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl FrameSlot {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Replaces the pending frame; an unconsumed older frame is dropped (its buffer reused).
    pub fn publish(&self, frame: DecodedFrame) {
        if let Some(old) = lock(&self.latest).replace(frame) {
            self.recycle(old.pixels);
        }
    }

    /// Takes the newest frame if one arrived since the last call.
    pub fn take(&self) -> Option<DecodedFrame> {
        lock(&self.latest).take()
    }

    /// Optionally hand a frame's buffer back after uploading it, to avoid an allocation.
    pub fn recycle(&self, buf: Vec<u8>) {
        *lock(&self.spare) = Some(buf);
    }

    pub(crate) fn take_spare(&self) -> Vec<u8> {
        lock(&self.spare).take().unwrap_or_default()
    }
}

/// Copies `src` into a tight buffer in `format`.
pub(crate) fn convert(src: &BgraFrame, format: PixelFormat, mut out: Vec<u8>) -> Vec<u8> {
    let row_bytes = src.width as usize * 4;
    out.clear();
    out.reserve(row_bytes * src.height as usize);
    for y in 0..src.height {
        let row = src.row(y);
        match format {
            PixelFormat::Bgra => out.extend_from_slice(row),
            PixelFormat::Rgba => {
                for px in row.chunks_exact(4) {
                    out.extend_from_slice(&[px[2], px[1], px[0], 255]);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_keeps_only_newest() {
        let slot = FrameSlot::new();
        for seq in 0..3 {
            slot.publish(DecodedFrame {
                width: 1,
                height: 1,
                format: PixelFormat::Bgra,
                pixels: vec![0; 4],
                seq,
                host_timestamp_us: 0,
            });
        }
        assert_eq!(slot.take().map(|f| f.seq), Some(2));
        assert!(slot.take().is_none());
        assert_eq!(slot.take_spare().len(), 4, "dropped frame buffer is reused");
    }

    #[test]
    fn convert_swizzles_and_drops_stride_padding() {
        let src = BgraFrame {
            width: 1,
            height: 2,
            stride: 8,
            data: vec![1, 2, 3, 4, 9, 9, 9, 9, 5, 6, 7, 8, 9, 9, 9, 9],
            timestamp_us: 0,
        };
        assert_eq!(
            convert(&src, PixelFormat::Rgba, Vec::new()),
            vec![3, 2, 1, 255, 7, 6, 5, 255]
        );
        assert_eq!(
            convert(&src, PixelFormat::Bgra, Vec::new()),
            vec![1, 2, 3, 4, 5, 6, 7, 8]
        );
    }
}
