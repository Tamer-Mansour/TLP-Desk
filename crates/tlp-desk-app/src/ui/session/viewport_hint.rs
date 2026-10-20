//! Tells the host how big the picture is shown, so it streams that size.
//!
//! In "fit to window" the viewer shows the remote screen at the size of its picture area. A
//! 4K host streaming its preset cap (1920x1200, or native 3840x2160 for Quality) into a
//! 1700-pixel-wide window is scaled twice: once on the host, again here, and every pass
//! softens text. With the area's physical size in `SetQuality` (the preset's `max_width` /
//! `max_height`, see `QualityTarget::resolve` on the host) the host's GPU scales the capture
//! once, straight to this size, and the viewer shows it 1:1. It also stops sending pixels
//! nobody sees.
//!
//! A new size reopens the capturer and costs a keyframe, so the hint is sent only after the
//! area has kept one size for [`SETTLE`] (window drags and maximize animations send nothing).

use std::time::{Duration, Instant};

use tlp_desk_proto::QualitySettings;

use crate::settings::QualityChoice;

/// How long the picture area must keep one size before the host is told.
pub const SETTLE: Duration = Duration::from_millis(400);

/// The hint for an area of `w` x `h` physical pixels (rounded down; `None` when empty).
pub fn area_hint(w: f32, h: f32) -> Option<(u32, u32)> {
    let px = |v: f32| (v.is_finite() && v >= 1.0).then(|| v.floor().min(u32::MAX as f32) as u32);
    Some((px(w)?, px(h)?))
}

/// The `SetQuality` request for `quality` with an optional viewport hint.
pub fn request(quality: QualityChoice, hint: Option<(u32, u32)>) -> QualitySettings {
    let (max_width, max_height) = hint.unwrap_or((0, 0));
    QualitySettings {
        max_width,
        max_height,
        ..quality.to_proto()
    }
}

/// Debounces the picture-area size into hints for the host.
#[derive(Debug, Default)]
pub struct ViewportHint {
    /// The hint the host has (`None` inside: no hint, the preset's own cap).
    sent: Option<Option<(u32, u32)>>,
    /// A different wanted hint and since when it has been wanted.
    pending: Option<(Option<(u32, u32)>, Instant)>,
}

impl ViewportHint {
    /// The hint to send with the next quality request (the last one sent, or none yet).
    pub fn current(&self) -> Option<(u32, u32)> {
        self.sent.flatten()
    }

    /// Call every frame with the hint the view wants now (`None` for actual-size view).
    /// Returns the hint to send when it has been stable for [`SETTLE`]; otherwise `None`.
    /// While a change is pending, [`ViewportHint::pending`] says to repaint again.
    pub fn update(&mut self, want: Option<(u32, u32)>, now: Instant) -> Option<Option<(u32, u32)>> {
        if self.sent == Some(want) {
            self.pending = None;
            return None;
        }
        match self.pending {
            Some((p, since)) if p == want => {
                if now.saturating_duration_since(since) >= SETTLE {
                    self.pending = None;
                    self.sent = Some(want);
                    return Some(want);
                }
            }
            _ => self.pending = Some((want, now)),
        }
        None
    }

    /// True while a size change waits for [`SETTLE`] (the UI must repaint to send it).
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn area_hint_rounds_down_and_rejects_empty() {
        assert_eq!(area_hint(1919.6, 1001.2), Some((1919, 1001)));
        assert_eq!(area_hint(0.0, 500.0), None);
        assert_eq!(area_hint(f32::NAN, 500.0), None);
        assert_eq!(area_hint(f32::INFINITY, 500.0), None);
    }

    #[test]
    fn request_carries_the_hint_with_the_preset() {
        let r = request(QualityChoice::Quality, Some((2560, 1369)));
        assert_eq!(r.preset, tlp_desk_proto::QualityPreset::Quality);
        assert_eq!((r.max_width, r.max_height), (2560, 1369));
        assert_eq!((r.fps, r.bitrate_kbps), (0, None));
        let r = request(QualityChoice::Speed, None);
        assert_eq!((r.max_width, r.max_height), (0, 0));
    }

    #[test]
    fn a_size_is_sent_once_after_it_settles() {
        let t0 = Instant::now();
        let mut h = ViewportHint::default();
        assert_eq!(h.update(Some((1900, 1000)), t0), None);
        assert!(h.pending());
        assert_eq!(h.update(Some((1900, 1000)), t0 + SETTLE / 2), None);
        assert_eq!(
            h.update(Some((1900, 1000)), t0 + SETTLE),
            Some(Some((1900, 1000)))
        );
        assert!(!h.pending());
        assert_eq!(h.current(), Some((1900, 1000)));
        // Unchanged: nothing more is sent.
        assert_eq!(h.update(Some((1900, 1000)), t0 + SETTLE * 5), None);
    }

    /// A window being dragged bigger sends nothing until it stops.
    #[test]
    fn a_resize_in_progress_sends_nothing() {
        let t0 = Instant::now();
        let mut h = ViewportHint::default();
        let step = Duration::from_millis(50);
        for i in 0..40u32 {
            let now = t0 + step * i;
            assert_eq!(h.update(Some((1000 + i * 10, 600)), now), None, "frame {i}");
        }
        let end = t0 + step * 40;
        assert_eq!(h.update(Some((1400, 600)), end), None);
        assert_eq!(
            h.update(Some((1400, 600)), end + SETTLE),
            Some(Some((1400, 600)))
        );
    }

    /// Switching to actual size withdraws the hint (the host streams its preset cap again).
    #[test]
    fn actual_size_withdraws_the_hint() {
        let t0 = Instant::now();
        let mut h = ViewportHint::default();
        h.update(Some((1900, 1000)), t0);
        h.update(Some((1900, 1000)), t0 + SETTLE);
        assert_eq!(h.update(None, t0 + SETTLE * 2), None);
        assert_eq!(h.update(None, t0 + SETTLE * 3), Some(None));
        assert_eq!(h.current(), None);
    }
}
