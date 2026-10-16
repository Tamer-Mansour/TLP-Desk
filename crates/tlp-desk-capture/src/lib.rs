//! Screen capture trait and platform implementations.
//!
//! On Windows, [`open_capturer`] tries Windows.Graphics.Capture first and falls back to DXGI
//! Desktop Duplication. On Linux it reads the X11 root window (RandR monitors, XFixes cursor);
//! Wayland sessions report a clear error until the ScreenCast portal path exists. Other
//! platforms currently return [`CaptureError::Unsupported`].
//!
//! Platform independent helpers:
//! * [`DirtyTracker`] tells which tiles of a frame changed, so the host skips encoding frames
//!   identical to the previous one.
//! * [`AllMonitorsCapturer`] composes one capturer per monitor into a picture of the whole
//!   virtual desktop ("all monitors" view), scaled to a size cap.
//!
//! `unsafe` is denied crate-wide and only allowed inside the Windows platform module, where every
//! block carries a `// SAFETY:` comment.
#![deny(unsafe_code)]

use std::time::Duration;

mod compose;
mod dirty;
#[cfg(any(target_os = "linux", test))]
mod linux_capture;
mod scale;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_capture;

pub use compose::{
    blit, virtual_bounds, AllMonitorsCapturer, ComposeLayout, OpenMonitor, Placement, ALL_MONITORS,
    ALL_MONITORS_NAME,
};
pub use dirty::{DirtyRect, DirtyTracker, FrameChange, DEFAULT_TILE};
pub use scale::{fit_within, mip_level_for};

/// One captured frame in BGRA8 layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BgraFrame {
    pub width: u32,
    pub height: u32,
    /// Bytes per row; may exceed `width * 4` because of GPU row padding.
    pub stride: u32,
    pub data: Vec<u8>,
    /// Capture time in microseconds since the capturer started.
    pub timestamp_us: u64,
}

impl BgraFrame {
    /// Allocates a zeroed (transparent black) frame with a tight stride (`width * 4`).
    pub fn new_tight(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            stride: width * 4,
            data: vec![0; width as usize * height as usize * 4],
            timestamp_us: 0,
        }
    }

    /// The `width * 4` visible bytes of row `y` (stride padding excluded).
    ///
    /// # Panics
    /// Panics if `y >= height` or `data` is shorter than the declared geometry.
    pub fn row(&self, y: u32) -> &[u8] {
        let start = y as usize * self.stride as usize;
        &self.data[start..start + self.width as usize * 4]
    }

    /// Mutable variant of [`BgraFrame::row`].
    pub fn row_mut(&mut self, y: u32) -> &mut [u8] {
        let start = y as usize * self.stride as usize;
        let w = self.width as usize * 4;
        &mut self.data[start..start + w]
    }

    /// True when `data` is long enough for `width`, `height` and `stride`.
    pub fn is_well_formed(&self) -> bool {
        if self.width == 0 || self.height == 0 {
            return self.data.is_empty() || self.stride >= self.width * 4;
        }
        let need = (self.height as usize - 1) * self.stride as usize + self.width as usize * 4;
        self.stride >= self.width * 4 && self.data.len() >= need
    }
}

/// Cursor state, sent separately from video so it stays sharp and low-latency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorState {
    /// Position of the cursor hotspot relative to the monitor's top-left corner, in physical pixels.
    pub x: i32,
    pub y: i32,
    /// False when the cursor is hidden or currently outside this monitor.
    pub visible: bool,
    /// New cursor image when the shape changed since the last call, otherwise `None`.
    pub shape: Option<CursorShape>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorShape {
    pub width: u32,
    pub height: u32,
    pub hotspot_x: u32,
    pub hotspot_y: u32,
    /// Tightly packed RGBA8 pixels.
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorInfo {
    /// Index as used by [`open_capturer`]. Index 0 is always the primary monitor.
    pub index: u32,
    /// OS device name, e.g. `\\.\DISPLAY1` on Windows.
    pub name: String,
    /// Size in physical pixels.
    pub width: u32,
    pub height: u32,
    pub primary: bool,
    /// Top-left corner in virtual-desktop physical pixels (the primary monitor is at 0,0).
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("capture is not supported on this platform")]
    Unsupported,
    #[error("monitor {0} not found")]
    NoSuchMonitor(u32),
    /// Desktop switched (lock screen, UAC) or display mode changed; recreate the capturer.
    #[error("capture surface lost, capturer must be recreated")]
    SurfaceLost,
    #[error("platform error: {0}")]
    Platform(String),
}

/// Which OS capture API backs a [`ScreenCapturer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaptureBackend {
    /// Windows.Graphics.Capture (Windows 10 1903+). Preferred.
    WindowsGraphicsCapture,
    /// DXGI Desktop Duplication (Windows 8+). Fallback.
    DxgiDuplication,
}

/// A source of screen frames for one monitor.
pub trait ScreenCapturer: Send {
    /// Waits up to `timeout` for the next changed frame. `Ok(None)` means the screen did not
    /// change, which callers use to send nothing.
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<BgraFrame>, CaptureError>;

    /// Current cursor position (in monitor pixels) and, if changed, its shape.
    fn cursor(&mut self) -> Result<CursorState, CaptureError>;

    fn monitor(&self) -> &MonitorInfo;

    /// Hands a frame's buffer back so the next capture can reuse it instead of allocating.
    /// Optional; dropping frames is always fine.
    fn recycle(&mut self, frame: BgraFrame) {
        drop(frame);
    }

    /// The OS API in use, if this capturer is backed by one.
    fn backend(&self) -> Option<CaptureBackend> {
        None
    }

    /// Size of the frames [`ScreenCapturer::next_frame`] delivers. Equals the monitor size unless
    /// the capturer was opened with [`CaptureOptions::max_size`] and scales on the GPU.
    /// [`ScreenCapturer::monitor`] always reports the real monitor size (input mapping uses it).
    fn frame_size(&self) -> (u32, u32) {
        let m = self.monitor();
        (m.width, m.height)
    }
}

/// Options for [`open_capturer_opts`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptureOptions {
    /// Force one backend (no fallback). `None` tries Windows.Graphics.Capture, then DXGI.
    pub backend: Option<CaptureBackend>,
    /// Deliver frames no larger than this (aspect preserved, see [`fit_within`]). The image is
    /// downscaled on the GPU before readback, so a 3840x2400 monitor capped at 1920x1200 copies
    /// 9 MB per frame to the CPU instead of 36 MB. `None` = native resolution.
    ///
    /// If GPU scaling cannot be set up (very old GPU / driver), frames silently come at native
    /// resolution: always size the encoder from the frames or [`ScreenCapturer::frame_size`].
    pub max_size: Option<(u32, u32)>,
}

impl CaptureOptions {
    /// Automatic backend, frames capped at `width` x `height`.
    pub fn max_size(width: u32, height: u32) -> Self {
        Self {
            backend: None,
            max_size: Some((width, height)),
        }
    }
}

/// Lists the monitors attached to the local desktop. Index 0 is the primary monitor; the rest are
/// ordered left-to-right, then top-to-bottom.
pub fn list_monitors() -> Result<Vec<MonitorInfo>, CaptureError> {
    #[cfg(windows)]
    {
        windows_capture::list_monitors()
    }
    #[cfg(target_os = "linux")]
    {
        linux_capture::list_monitors()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Err(CaptureError::Unsupported)
    }
}

/// Opens a capturer for `monitor_index` (see [`list_monitors`]; 0 = primary), trying
/// Windows.Graphics.Capture first and DXGI Desktop Duplication second.
pub fn open_capturer(monitor_index: u32) -> Result<Box<dyn ScreenCapturer>, CaptureError> {
    #[cfg(windows)]
    {
        windows_capture::open_capturer(monitor_index, CaptureOptions::default())
    }
    #[cfg(target_os = "linux")]
    {
        linux_capture::open_capturer(monitor_index, CaptureOptions::default())
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = monitor_index;
        Err(CaptureError::Unsupported)
    }
}

/// Opens a capturer using one specific backend, with no fallback.
pub fn open_capturer_with(
    monitor_index: u32,
    backend: CaptureBackend,
) -> Result<Box<dyn ScreenCapturer>, CaptureError> {
    #[cfg(windows)]
    {
        windows_capture::open_capturer(
            monitor_index,
            CaptureOptions {
                backend: Some(backend),
                max_size: None,
            },
        )
    }
    // X11 has a single backend; the request is honoured by ignoring it.
    #[cfg(target_os = "linux")]
    {
        let _ = backend;
        linux_capture::open_capturer(monitor_index, CaptureOptions::default())
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (monitor_index, backend);
        Err(CaptureError::Unsupported)
    }
}

/// Opens a capturer with explicit [`CaptureOptions`] (backend choice and GPU downscaling).
///
/// ```no_run
/// use tlp_desk_capture::{open_capturer_opts, CaptureOptions};
/// // 3840x2400 monitor -> 1920x1200 frames, scaled on the GPU.
/// let cap = open_capturer_opts(0, CaptureOptions::max_size(1920, 1200))?;
/// let (w, h) = cap.frame_size();
/// # Ok::<(), tlp_desk_capture::CaptureError>(())
/// ```
pub fn open_capturer_opts(
    monitor_index: u32,
    options: CaptureOptions,
) -> Result<Box<dyn ScreenCapturer>, CaptureError> {
    #[cfg(windows)]
    {
        windows_capture::open_capturer(monitor_index, options)
    }
    #[cfg(target_os = "linux")]
    {
        linux_capture::open_capturer(monitor_index, options)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (monitor_index, options);
        Err(CaptureError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_rows_respect_stride() {
        let mut f = BgraFrame {
            width: 3,
            height: 2,
            stride: 16,
            data: vec![0; 32],
            timestamp_us: 0,
        };
        assert!(f.is_well_formed());
        f.row_mut(1).copy_from_slice(&[7; 12]);
        assert_eq!(f.row(0), &[0; 12]);
        assert_eq!(f.row(1), &[7; 12]);
        assert_eq!(&f.data[16..28], &[7; 12]);
        assert_eq!(&f.data[28..32], &[0; 4]);
    }

    #[test]
    fn malformed_frame_detected() {
        let f = BgraFrame {
            width: 4,
            height: 2,
            stride: 8,
            data: vec![0; 32],
            timestamp_us: 0,
        };
        assert!(!f.is_well_formed());
        assert!(BgraFrame::new_tight(5, 3).is_well_formed());
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    #[test]
    fn unsupported_off_windows() {
        assert!(matches!(open_capturer(0), Err(CaptureError::Unsupported)));
    }

    /// Needs a real interactive desktop.
    #[cfg(windows)]
    #[test]
    #[ignore = "needs a real interactive desktop session"]
    fn lists_primary_monitor_first() {
        let monitors = list_monitors().expect("list monitors");
        assert!(!monitors.is_empty());
        assert!(monitors[0].primary);
        assert_eq!(monitors[0].index, 0);
        assert!(monitors[0].width > 0 && monitors[0].height > 0);
        for (i, m) in monitors.iter().enumerate() {
            assert_eq!(m.index as usize, i);
        }
    }

    /// Needs an X server, e.g. `xvfb-run -a cargo test -p tlp-desk-capture -- --ignored x11`.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "needs an X server (run under Xvfb)"]
    fn x11_captures_primary_monitor_scaled_and_native() {
        let monitors = list_monitors().expect("list monitors");
        assert!(monitors[0].primary && monitors[0].index == 0);
        let mon = monitors[0].clone();
        let mut cap = open_capturer(0).expect("open");
        let f = cap
            .next_frame(Duration::from_millis(50))
            .expect("frame")
            .expect("some frame");
        assert_eq!((f.width, f.height), (mon.width, mon.height));
        assert!(f.is_well_formed());
        assert!(f.data.chunks_exact(4).all(|p| p[3] == 255));
        cap.recycle(f);
        let c = cap.cursor().expect("cursor");
        assert!(c.shape.is_some(), "first cursor call returns the shape");
        let mut small =
            open_capturer_opts(0, CaptureOptions::max_size(mon.width / 2, mon.height / 2))
                .expect("open scaled");
        let want = fit_within((mon.width, mon.height), (mon.width / 2, mon.height / 2));
        assert_eq!(small.frame_size(), want);
        let f = small
            .next_frame(Duration::from_millis(50))
            .expect("frame")
            .expect("some frame");
        assert_eq!((f.width, f.height), want);
    }

    #[cfg(windows)]
    fn check_capture(backend: Option<CaptureBackend>) {
        let mut cap = match backend {
            Some(b) => open_capturer_with(0, b).expect("open capturer"),
            None => open_capturer(0).expect("open capturer"),
        };
        if let Some(b) = backend {
            assert_eq!(cap.backend(), Some(b));
        }
        let mon = cap.monitor().clone();
        // The first frame after start is always delivered.
        let mut got = None;
        for _ in 0..20 {
            if let Some(f) = cap
                .next_frame(Duration::from_millis(250))
                .expect("next_frame")
            {
                got = Some(f);
                break;
            }
        }
        let frame = got.expect("no frame within 5 s");
        assert_eq!((frame.width, frame.height), (mon.width, mon.height));
        assert!(frame.is_well_formed());
        // A real desktop is not one flat colour.
        let first = frame.row(frame.height / 2)[..4].to_vec();
        let varied = (0..frame.height)
            .step_by(7)
            .any(|y| frame.row(y).chunks(4).any(|p| p != first.as_slice()));
        assert!(varied, "captured frame is uniform");
        cap.recycle(frame);
        let cursor = cap.cursor().expect("cursor");
        if cursor.visible {
            let shape = cursor.shape.expect("first cursor call returns the shape");
            assert_eq!(
                shape.rgba.len(),
                shape.width as usize * shape.height as usize * 4
            );
            assert!(cap.cursor().expect("cursor").shape.is_none());
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "needs a real interactive desktop session"]
    fn wgc_captures_primary_monitor() {
        check_capture(Some(CaptureBackend::WindowsGraphicsCapture));
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "needs a real interactive desktop session"]
    fn dxgi_captures_primary_monitor() {
        check_capture(Some(CaptureBackend::DxgiDuplication));
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "needs a real interactive desktop session"]
    fn factory_captures_primary_monitor() {
        check_capture(None);
    }

    #[cfg(windows)]
    fn first_frame(cap: &mut dyn ScreenCapturer) -> BgraFrame {
        for _ in 0..20 {
            if let Some(f) = cap
                .next_frame(Duration::from_millis(250))
                .expect("next_frame")
            {
                return f;
            }
        }
        panic!("no frame within 5 s");
    }

    /// CPU reference: area-average of the source rectangle each output pixel covers.
    #[cfg(windows)]
    fn area_downscale(src: &BgraFrame, w: u32, h: u32) -> BgraFrame {
        let mut out = BgraFrame::new_tight(w, h);
        for oy in 0..h {
            let y0 = (oy as u64 * src.height as u64 / h as u64) as u32;
            let y1 = (((oy + 1) as u64 * src.height as u64).div_ceil(h as u64) as u32).max(y0 + 1);
            for ox in 0..w {
                let x0 = (ox as u64 * src.width as u64 / w as u64) as u32;
                let x1 =
                    (((ox + 1) as u64 * src.width as u64).div_ceil(w as u64) as u32).max(x0 + 1);
                let mut sum = [0u64; 4];
                for y in y0..y1.min(src.height) {
                    let row = src.row(y);
                    for x in x0..x1.min(src.width) {
                        for c in 0..4 {
                            sum[c] += row[x as usize * 4 + c] as u64;
                        }
                    }
                }
                let n = ((y1.min(src.height) - y0) * (x1.min(src.width) - x0)) as u64;
                let px = &mut out.row_mut(oy)[ox as usize * 4..ox as usize * 4 + 4];
                for c in 0..4 {
                    px[c] = (sum[c] / n) as u8;
                }
            }
        }
        out
    }

    /// Mean absolute difference over B, G, R.
    #[cfg(windows)]
    fn mean_abs_diff(a: &BgraFrame, b: &BgraFrame) -> f64 {
        assert_eq!((a.width, a.height), (b.width, b.height));
        let mut total = 0u64;
        for y in 0..a.height {
            for (p, q) in a.row(y).chunks_exact(4).zip(b.row(y).chunks_exact(4)) {
                for c in 0..3 {
                    total += p[c].abs_diff(q[c]) as u64;
                }
            }
        }
        total as f64 / (a.width as f64 * a.height as f64 * 3.0)
    }

    /// Opens `monitor` natively and scaled to fit `divisor`-times-smaller, and checks the scaled
    /// frame matches a CPU area-average of the native one (catches flips, offsets, bad filters).
    #[cfg(windows)]
    fn check_scaled_capture(backend: CaptureBackend, monitor: u32, divisor: u32) {
        let Some(mon) = list_monitors()
            .expect("list")
            .into_iter()
            .nth(monitor as usize)
        else {
            eprintln!("monitor {monitor} not present; skipping");
            return;
        };
        let max = (mon.width / divisor, mon.height / divisor);
        let opts = CaptureOptions {
            backend: Some(backend),
            max_size: Some(max),
        };
        let want = fit_within((mon.width, mon.height), max);
        // The desktop may change between the two captures (a clock, a scrolling terminal), so
        // take the best of a few attempts; a real scaling bug is wrong every time.
        let mut best = f64::MAX;
        let mut scaled_cap: Option<Box<dyn ScreenCapturer>> = None;
        let mut scaled = None;
        for _ in 0..4 {
            // One at a time: DXGI refuses a second duplication of the same output in one process.
            drop(scaled_cap.take());
            let native = {
                let mut native_cap = open_capturer_with(monitor, backend).expect("open native");
                assert_eq!(native_cap.frame_size(), (mon.width, mon.height));
                first_frame(native_cap.as_mut())
            };
            let mut cap = open_capturer_opts(monitor, opts).expect("open scaled");
            // MonitorInfo keeps the real size; frames are scaled.
            assert_eq!(
                (cap.monitor().width, cap.monitor().height),
                (mon.width, mon.height)
            );
            assert_eq!(cap.frame_size(), want, "GPU scaler not active");
            let frame = first_frame(cap.as_mut());
            assert_eq!((frame.width, frame.height), want);
            assert!(frame.is_well_formed());
            let diff = mean_abs_diff(&frame, &area_downscale(&native, want.0, want.1));
            eprintln!(
                "{backend:?} monitor {monitor} {}x{} -> {}x{}: mean abs diff vs CPU area average {diff:.2}",
                mon.width, mon.height, want.0, want.1
            );
            best = best.min(diff);
            scaled_cap = Some(cap);
            scaled = Some(frame);
            if best < 2.0 {
                break;
            }
        }
        // Identical content gives ~0.03 for 2x (rounding) and ~1 for the mip + bilinear path.
        // A flipped or shifted image is far above this on a real desktop.
        assert!(best < 2.0, "scaled frame differs from reference: {best:.2}");
        let (mut scaled_cap, scaled) = (scaled_cap.unwrap(), scaled.unwrap());

        // Steady state reuses buffers: recycle and capture again (may be None if static).
        scaled_cap.recycle(scaled);
        if let Some(f) = scaled_cap
            .next_frame(Duration::from_millis(100))
            .expect("next")
        {
            assert_eq!((f.width, f.height), want);
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "needs a real interactive desktop session"]
    fn wgc_scaled_half_matches_cpu_reference() {
        check_scaled_capture(CaptureBackend::WindowsGraphicsCapture, 0, 2);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "needs a real interactive desktop session"]
    fn dxgi_scaled_half_matches_cpu_reference() {
        check_scaled_capture(CaptureBackend::DxgiDuplication, 0, 2);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "needs a real interactive desktop session"]
    fn wgc_scaled_third_uses_mips_and_matches_cpu_reference() {
        check_scaled_capture(CaptureBackend::WindowsGraphicsCapture, 0, 3);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "needs a real interactive desktop session"]
    fn wgc_scaled_second_monitor_matches_cpu_reference() {
        check_scaled_capture(CaptureBackend::WindowsGraphicsCapture, 1, 2);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "needs a real interactive desktop session"]
    fn max_size_larger_than_monitor_is_native() {
        let mon = list_monitors().expect("list")[0].clone();
        let mut cap = open_capturer_opts(0, CaptureOptions::max_size(mon.width * 2, mon.height))
            .expect("open");
        assert_eq!(cap.frame_size(), (mon.width, mon.height));
        let f = first_frame(cap.as_mut());
        assert_eq!((f.width, f.height), (mon.width, mon.height));
    }
}
