//! The host-side overlay seam: a [`WhiteboardOverlay`] shows a [`SharedBoard`] on top of the
//! streamed monitor. The Windows implementation is a layered window ([`super::windows`]);
//! tests use a fake that renders offscreen.

use std::sync::{Arc, Mutex, MutexGuard};

use tiny_skia::{FillRule, LineCap, Paint, PathBuilder, Pixmap, Stroke as SkStroke, Transform};

use super::{Board, ChannelOrder};
use crate::{InputError, MonitorRect};

/// The board the host feature updates and the overlay draws (shared across threads).
pub type SharedBoard = Arc<Mutex<Board>>;

/// Called (on the overlay's own thread) when the host user clicks the overlay's clear button.
/// Must not block.
pub type ClearRequestFn = Arc<dyn Fn() + Send + Sync>;

/// Largest overlay the host creates, in pixels (an 8K monitor, or three 4K monitors side by
/// side). Bigger targets are ignored rather than allocating hundreds of megabytes.
pub const MAX_OVERLAY_PIXELS: u64 = 7680 * 4320;

/// Locks the board, recovering from a poisoned lock (a panicking renderer must not take the
/// session down with it).
pub fn lock_board(board: &SharedBoard) -> MutexGuard<'_, Board> {
    board.lock().unwrap_or_else(|p| p.into_inner())
}

/// Shows whiteboard strokes on the host screen. Every method is cheap and non-blocking;
/// dropping the overlay removes it from the screen.
pub trait WhiteboardOverlay: Send {
    /// Places the overlay over `rect` (physical virtual-desktop pixels): the streamed monitor,
    /// or the bounding box of all monitors. Strokes are re-mapped to the new rectangle.
    fn set_target(&mut self, rect: MonitorRect);

    /// The shared board changed: repaint soon (coalesced to the display rate).
    fn changed(&mut self);

    /// Whether the overlay is kept out of screen capture (the viewer draws the strokes
    /// itself, so they must not also appear in the video).
    fn excluded_from_capture(&self) -> bool {
        true
    }
}

/// Starts the platform overlay window for `board` (hidden until there is something to show).
/// `on_clear` runs when the host user clicks its clear button.
///
/// Returns [`InputError::Unsupported`] on platforms without an implementation.
pub fn start_whiteboard_overlay(
    board: SharedBoard,
    on_clear: ClearRequestFn,
) -> Result<Box<dyn WhiteboardOverlay>, InputError> {
    #[cfg(windows)]
    {
        Ok(Box::new(super::windows::OverlayWindow::start(
            board, on_clear,
        )?))
    }
    #[cfg(not(windows))]
    {
        let _ = (board, on_clear);
        Err(InputError::Unsupported)
    }
}

/// Whether an overlay over `rect` is small enough to create (see [`MAX_OVERLAY_PIXELS`]).
pub fn overlay_size_ok(rect: MonitorRect) -> bool {
    rect.width > 0
        && rect.height > 0
        && u64::from(rect.width) * u64::from(rect.height) <= MAX_OVERLAY_PIXELS
}

/// Side of the clear button in pixels at `scale` (1.0 = 96 DPI).
pub fn clear_button_size(scale: f32) -> u32 {
    let s = if scale.is_finite() { scale } else { 1.0 };
    (40.0 * s.clamp(0.5, 4.0)).round() as u32
}

/// The host's "clear drawings" button: a dark round badge with a white ring and a white
/// cross, premultiplied, `order` bytes, [`clear_button_size`] square. No text, so it needs no
/// translation.
pub fn clear_button_image(scale: f32, order: ChannelOrder) -> Option<Pixmap> {
    let side = clear_button_size(scale);
    let s = side as f32 / 40.0;
    let mut pixmap = Pixmap::new(side, side)?;
    let c = side as f32 / 2.0;
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    let (r, g, b) = (0x1F, 0x24, 0x2B);
    match order {
        ChannelOrder::Rgba => paint.set_color_rgba8(r, g, b, 0xE6),
        ChannelOrder::Bgra => paint.set_color_rgba8(b, g, r, 0xE6),
    }
    let disc = PathBuilder::from_circle(c, c, c - 1.0)?;
    pixmap.fill_path(
        &disc,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
    paint.set_color_rgba8(0xFF, 0xFF, 0xFF, 0xFF);
    let ring = PathBuilder::from_circle(c, c, c - 2.5 * s)?;
    let thin = SkStroke {
        width: 1.5 * s,
        ..SkStroke::default()
    };
    pixmap.stroke_path(&ring, &paint, &thin, Transform::identity(), None);
    let arm = 7.0 * s;
    let mut pb = PathBuilder::new();
    pb.move_to(c - arm, c - arm);
    pb.line_to(c + arm, c + arm);
    pb.move_to(c + arm, c - arm);
    pb.line_to(c - arm, c + arm);
    let cross = pb.finish()?;
    let bold = SkStroke {
        width: 3.0 * s,
        line_cap: LineCap::Round,
        ..SkStroke::default()
    };
    pixmap.stroke_path(&cross, &paint, &bold, Transform::identity(), None);
    Some(pixmap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_size_cap() {
        assert!(overlay_size_ok(MonitorRect::new(-3840, 0, 3 * 3840, 2160)));
        assert!(overlay_size_ok(MonitorRect::new(0, 0, 7680, 4320)));
        assert!(!overlay_size_ok(MonitorRect::new(0, 0, 7681, 4320)));
        assert!(!overlay_size_ok(MonitorRect::new(0, 0, 0, 1080)));
        assert!(!overlay_size_ok(MonitorRect::new(0, 0, u32::MAX, u32::MAX)));
    }

    #[test]
    fn clear_button_has_a_dark_badge_and_a_white_cross() {
        for scale in [1.0f32, 1.5, 2.25] {
            let img = clear_button_image(scale, ChannelOrder::Rgba).expect("button");
            let side = clear_button_size(scale);
            assert_eq!((img.width(), img.height()), (side, side));
            let at = |x: u32, y: u32| {
                let i = ((y * side + x) * 4) as usize;
                img.data()[i..i + 4].to_vec()
            };
            // Center: white cross. Corner: transparent (click-through). Between: dark badge.
            let center = at(side / 2, side / 2);
            assert!(center[0] > 200 && center[3] > 200, "{center:?}");
            assert_eq!(at(0, 0), vec![0, 0, 0, 0]);
            let badge = at(side / 2, side / 2 - side * 3 / 10);
            assert!(badge[0] < 0x40 && badge[3] >= 0xE0, "{badge:?}");
        }
        assert_eq!(clear_button_size(f32::NAN), 40);
        assert_eq!(clear_button_size(100.0), 160);
        let bgra = clear_button_image(1.0, ChannelOrder::Bgra).expect("button");
        let i = ((5 * 40 + 20) * 4) as usize;
        // Dark badge pixel: blue-ish channel first in BGRA.
        assert!(bgra.data()[i] >= bgra.data()[i + 2]);
    }

    #[test]
    fn poisoned_board_lock_recovers() {
        let board: SharedBoard = Arc::default();
        let b2 = board.clone();
        let _ = std::thread::spawn(move || {
            let _g = b2.lock();
            panic!("renderer crashed");
        })
        .join();
        assert!(lock_board(&board).is_empty());
    }
}
