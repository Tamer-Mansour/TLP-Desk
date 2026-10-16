//! A synthetic screen.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tlp_desk_capture::{BgraFrame, CaptureError, CursorState, MonitorInfo, ScreenCapturer};
use tlp_desk_host::{CapturerFactory, MonitorLister};

use super::{Alive, AliveGuard, Kind};

/// Width of [`monitor`]`(0)`.
pub const DEFAULT_W: u32 = 320;
/// Height of [`monitor`]`(0)`.
pub const DEFAULT_H: u32 = 240;
/// Frames per second a [`FakeCapturer`] delivers by default.
pub const DEFAULT_FPS: u32 = 60;

/// Fake monitor `index`: monitor 0 is primary and [`DEFAULT_W`] x [`DEFAULT_H`]; the others
/// are smaller-than-HD odd sizes placed to the right, with Arabic in their names.
pub fn monitor(index: u32) -> MonitorInfo {
    if index == 0 {
        return MonitorInfo {
            index,
            name: "FAKE-0".into(),
            width: DEFAULT_W,
            height: DEFAULT_H,
            primary: true,
            x: 0,
            y: 0,
        };
    }
    let width = 480 + 32 * index;
    MonitorInfo {
        index,
        name: format!("شاشة FAKE-{index}"),
        width,
        height: 272,
        primary: false,
        x: i32::try_from(DEFAULT_W + (index - 1) * 512).unwrap_or(i32::MAX),
        y: 0,
    }
}

/// Monitors `0..n` (see [`monitor`]).
pub fn monitors(n: u32) -> Vec<MonitorInfo> {
    (0..n).map(monitor).collect()
}

/// A [`MonitorLister`] returning `list`.
pub fn monitor_lister(list: Vec<MonitorInfo>) -> MonitorLister {
    Arc::new(move || Ok(list.clone()))
}

/// A [`CapturerFactory`] opening a [`FakeCapturer`] on any monitor of `list` (unknown indices
/// fail like a real capturer would).
pub fn capturer_factory(list: Vec<MonitorInfo>, alive: Arc<Alive>) -> CapturerFactory {
    Arc::new(move |index, _options| {
        let mon = list
            .iter()
            .find(|m| m.index == index)
            .cloned()
            .ok_or(CaptureError::NoSuchMonitor(index))?;
        Ok(Box::new(FakeCapturer::new(mon, alive.clone())) as Box<dyn ScreenCapturer>)
    })
}

/// A bright bar moving across a dark gradient at a fixed frame rate. Each frame differs from
/// the previous one, so encoders always have work and frame counts are deterministic.
pub struct FakeCapturer {
    mon: MonitorInfo,
    interval: Duration,
    next_at: Instant,
    frames: Arc<AtomicU64>,
    _alive: AliveGuard,
}

impl std::fmt::Debug for FakeCapturer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeCapturer")
            .field("monitor", &self.mon.index)
            .field("frames", &self.frames())
            .finish_non_exhaustive()
    }
}

impl FakeCapturer {
    /// A capturer for `mon` at [`DEFAULT_FPS`].
    pub fn new(mon: MonitorInfo, alive: Arc<Alive>) -> Self {
        Self::with_fps(mon, DEFAULT_FPS, alive)
    }

    /// A capturer for `mon` delivering `fps` frames per second (at least 1).
    pub fn with_fps(mon: MonitorInfo, fps: u32, alive: Arc<Alive>) -> Self {
        Self {
            mon,
            interval: Duration::from_micros(1_000_000 / u64::from(fps.max(1))),
            next_at: Instant::now(),
            frames: Arc::default(),
            _alive: AliveGuard::new(&alive, Kind::Capturer),
        }
    }

    /// Frames delivered so far.
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Acquire)
    }

    /// Paints frame number `n` of a `w` x `h` screen.
    pub fn paint(w: u32, h: u32, n: u64) -> BgraFrame {
        let mut f = BgraFrame::new_tight(w, h);
        let bar = u32::try_from((n * 8) % u64::from(w.max(1))).unwrap_or(0);
        for y in 0..h {
            let row = f.row_mut(y);
            for (x, px) in (0u32..).zip(row.chunks_exact_mut(4)) {
                let on = x >= bar && x < bar.saturating_add(32);
                let v = if on { 230 } else { (y % 64) as u8 };
                px.copy_from_slice(&[v, v / 2, 255 - v, 255]);
            }
        }
        f
    }
}

impl ScreenCapturer for FakeCapturer {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<BgraFrame>, CaptureError> {
        let now = Instant::now();
        if now < self.next_at {
            std::thread::sleep(timeout.min(self.next_at - now));
            if Instant::now() < self.next_at {
                return Ok(None);
            }
        }
        self.next_at = Instant::now() + self.interval;
        let n = self.frames.fetch_add(1, Ordering::AcqRel) + 1;
        Ok(Some(Self::paint(self.mon.width, self.mon.height, n)))
    }

    fn cursor(&mut self) -> Result<CursorState, CaptureError> {
        Ok(CursorState {
            x: 0,
            y: 0,
            visible: false,
            shape: None,
        })
    }

    fn monitor(&self) -> &MonitorInfo {
        &self.mon
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_have_the_monitor_size_and_change() {
        let alive = Arc::new(Alive::default());
        let mut c = FakeCapturer::with_fps(monitor(1), 1000, alive.clone());
        assert_eq!(alive.capturers(), 1);
        let a = c.next_frame(Duration::from_secs(1)).unwrap().unwrap();
        let b = loop {
            if let Some(f) = c.next_frame(Duration::from_secs(1)).unwrap() {
                break f;
            }
        };
        assert_eq!((a.width, a.height), (monitor(1).width, 272));
        assert!(a.is_well_formed());
        assert_ne!(a.data, b.data);
        assert_eq!(c.frames(), 2);
        drop(c);
        assert_eq!(alive.capturers(), 0);
    }

    #[test]
    fn factory_refuses_unknown_monitors() {
        let f = capturer_factory(monitors(2), Arc::default());
        assert!(f(1, Default::default()).is_ok());
        assert!(f(7, Default::default()).is_err());
        assert!(monitors(3)[2].name.contains("شاشة"));
    }
}
