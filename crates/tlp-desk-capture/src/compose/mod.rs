//! "All monitors" capture: every monitor composed into one picture of the virtual desktop.
//!
//! [`AllMonitorsCapturer`] opens one ordinary capturer per monitor, each asked (through
//! [`CaptureOptions::max_size`]) for exactly the size its monitor occupies in the composed
//! picture, so the expensive downscale still happens on the GPU and the CPU only copies rows
//! into place. The composed picture is the bounding box of all monitors (origins may be
//! negative), scaled as a whole to fit the size cap; gaps between monitors stay black.
//!
//! [`ScreenCapturer::monitor`] reports that bounding box (x, y, width, height in physical
//! virtual-desktop pixels), so the host maps the viewer's normalized pointer coordinates across
//! all monitors with the same code it uses for one monitor.

use std::time::{Duration, Instant};

use crate::{
    fit_within, BgraFrame, CaptureBackend, CaptureError, CaptureOptions, CursorShape, CursorState,
    MonitorInfo, ScreenCapturer,
};

/// Monitor index meaning "all monitors" (`MonitorList::active` while the whole virtual
/// desktop is streamed).
pub const ALL_MONITORS: u32 = u32::MAX;

/// Name reported by [`AllMonitorsCapturer`]'s [`MonitorInfo`].
pub const ALL_MONITORS_NAME: &str = "ALL";

/// Opens the capturer of one monitor with the given options.
pub type OpenMonitor<'a> =
    dyn FnMut(u32, CaptureOptions) -> Result<Box<dyn ScreenCapturer>, CaptureError> + 'a;

/// The bounding box of `monitors` on the virtual desktop, as a [`MonitorInfo`] with index
/// [`ALL_MONITORS`]. `None` for an empty list or a degenerate (zero-sized) layout.
pub fn virtual_bounds(monitors: &[MonitorInfo]) -> Option<MonitorInfo> {
    let mut it = monitors.iter().filter(|m| m.width > 0 && m.height > 0);
    let first = it.next()?;
    let edges = |m: &MonitorInfo| {
        let (x, y) = (i64::from(m.x), i64::from(m.y));
        (x, y, x + i64::from(m.width), y + i64::from(m.height))
    };
    let (mut x0, mut y0, mut x1, mut y1) = edges(first);
    for m in it {
        let (a, b, c, d) = edges(m);
        x0 = x0.min(a);
        y0 = y0.min(b);
        x1 = x1.max(c);
        y1 = y1.max(d);
    }
    Some(MonitorInfo {
        index: ALL_MONITORS,
        name: ALL_MONITORS_NAME.into(),
        width: u32::try_from(x1 - x0).ok()?,
        height: u32::try_from(y1 - y0).ok()?,
        primary: false,
        x: i32::try_from(x0).ok()?,
        y: i32::try_from(y0).ok()?,
    })
}

/// Where one monitor lands in the composed picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// Monitor index (as used by the capturer factory).
    pub index: u32,
    /// Left edge in the composed picture, pixels.
    pub x: u32,
    /// Top edge in the composed picture, pixels.
    pub y: u32,
    /// Width in the composed picture, pixels.
    pub width: u32,
    /// Height in the composed picture, pixels.
    pub height: u32,
}

/// Pure layout of the composed picture: canvas size and each monitor's placement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeLayout {
    /// The virtual desktop bounding box (index [`ALL_MONITORS`]).
    pub bounds: MonitorInfo,
    /// Size of the composed picture: the bounding box scaled to fit the cap.
    pub canvas: (u32, u32),
    /// One entry per (non-empty) monitor, in the order given.
    pub parts: Vec<Placement>,
}

/// Maps a virtual-desktop offset `off` (0..=`extent`) to canvas pixels.
fn scale_edge(off: i64, extent: u32, canvas: u32) -> u32 {
    if extent == 0 {
        return 0;
    }
    let v = off.clamp(0, i64::from(extent)) * i64::from(canvas) / i64::from(extent);
    u32::try_from(v).unwrap_or(canvas)
}

impl ComposeLayout {
    /// Lays out `monitors` scaled to fit `cap` (`None` = native size). `None` if there is
    /// nothing to show.
    pub fn new(monitors: &[MonitorInfo], cap: Option<(u32, u32)>) -> Option<Self> {
        let bounds = virtual_bounds(monitors)?;
        let full = (bounds.width, bounds.height);
        let canvas = cap.map_or(full, |c| fit_within(full, c));
        let parts = monitors
            .iter()
            .filter(|m| m.width > 0 && m.height > 0)
            .filter_map(|m| {
                let dx = i64::from(m.x) - i64::from(bounds.x);
                let dy = i64::from(m.y) - i64::from(bounds.y);
                let x = scale_edge(dx, bounds.width, canvas.0);
                let y = scale_edge(dy, bounds.height, canvas.1);
                let r = scale_edge(dx + i64::from(m.width), bounds.width, canvas.0);
                let b = scale_edge(dy + i64::from(m.height), bounds.height, canvas.1);
                (r > x && b > y).then_some(Placement {
                    index: m.index,
                    x,
                    y,
                    width: r - x,
                    height: b - y,
                })
            })
            .collect();
        Some(Self {
            bounds,
            canvas,
            parts,
        })
    }
}

/// Copies `src` into `dst` at `place`. Same size (within 2 px of rounding): row copies of the
/// overlap. Otherwise nearest-neighbour scaling (only when the GPU scaler was unavailable).
/// Never panics: everything is clipped to both buffers.
pub fn blit(dst: &mut BgraFrame, src: &BgraFrame, place: Placement) {
    if !dst.is_well_formed() || !src.is_well_formed() || src.width == 0 || src.height == 0 {
        return;
    }
    let w = place.width.min(dst.width.saturating_sub(place.x));
    let h = place.height.min(dst.height.saturating_sub(place.y));
    if w == 0 || h == 0 {
        return;
    }
    let near = |a: u32, b: u32| a.abs_diff(b) <= 2;
    let same = near(src.width, place.width) && near(src.height, place.height);
    let dst_x = place.x as usize * 4;
    for row in 0..h {
        let sy = if same {
            row
        } else {
            (u64::from(row) * u64::from(src.height) / u64::from(place.height)) as u32
        };
        if sy >= src.height {
            // Source smaller than the slot (same-size path): leave the rest black.
            break;
        }
        let s_start = sy as usize * src.stride as usize;
        let Some(s_row) = src.data.get(s_start..s_start + src.width as usize * 4) else {
            return;
        };
        let d_start = (place.y + row) as usize * dst.stride as usize + dst_x;
        let Some(d_row) = dst.data.get_mut(d_start..d_start + w as usize * 4) else {
            return;
        };
        if same {
            let n = d_row.len().min(s_row.len());
            if let (Some(d), Some(s)) = (d_row.get_mut(..n), s_row.get(..n)) {
                d.copy_from_slice(s);
            }
        } else {
            for (col, px) in d_row.chunks_exact_mut(4).enumerate() {
                let sx = (col as u64 * u64::from(src.width) / u64::from(place.width)) as usize;
                if let Some(s) = s_row.get(sx * 4..sx * 4 + 4) {
                    px.copy_from_slice(s);
                }
            }
        }
    }
}

struct Part {
    place: Placement,
    monitor: MonitorInfo,
    cap: Box<dyn ScreenCapturer>,
    shape: Option<CursorShape>,
}

/// Captures all monitors into one composed frame (see the module docs).
pub struct AllMonitorsCapturer {
    layout: ComposeLayout,
    parts: Vec<Part>,
    canvas: BgraFrame,
    spare: Vec<BgraFrame>,
    started: Instant,
    /// Part that showed the cursor last time.
    cursor_owner: Option<usize>,
}

impl std::fmt::Debug for AllMonitorsCapturer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AllMonitorsCapturer")
            .field("layout", &self.layout)
            .finish_non_exhaustive()
    }
}

/// Longest single wait on one monitor while polling all of them, so a change on another
/// monitor is picked up quickly.
const POLL_SLICE: Duration = Duration::from_millis(2);

/// Recycled output buffers kept for reuse.
const MAX_SPARE: usize = 3;

impl AllMonitorsCapturer {
    /// Opens one capturer per monitor through `open`, each capped to its slot in the composed
    /// picture, which itself fits `cap` (`None` = native).
    ///
    /// # Errors
    /// [`CaptureError::NoSuchMonitor`] for an empty layout, or the first error of `open`.
    pub fn open(
        monitors: &[MonitorInfo],
        cap: Option<(u32, u32)>,
        open: &mut OpenMonitor<'_>,
    ) -> Result<Self, CaptureError> {
        let layout =
            ComposeLayout::new(monitors, cap).ok_or(CaptureError::NoSuchMonitor(ALL_MONITORS))?;
        let mut parts = Vec::with_capacity(layout.parts.len());
        for place in &layout.parts {
            let Some(monitor) = monitors.iter().find(|m| m.index == place.index).cloned() else {
                continue;
            };
            let opts = CaptureOptions {
                backend: None,
                max_size: Some((place.width.max(2), place.height.max(2))),
            };
            let cap = open(place.index, opts)?;
            parts.push(Part {
                place: *place,
                monitor,
                cap,
                shape: None,
            });
        }
        if parts.is_empty() {
            return Err(CaptureError::NoSuchMonitor(ALL_MONITORS));
        }
        let mut canvas = BgraFrame::new_tight(layout.canvas.0, layout.canvas.1);
        for px in canvas.data.chunks_exact_mut(4) {
            px[3] = 255;
        }
        Ok(Self {
            layout,
            parts,
            canvas,
            spare: Vec::new(),
            started: Instant::now(),
            cursor_owner: None,
        })
    }

    /// The layout in use.
    pub fn layout(&self) -> &ComposeLayout {
        &self.layout
    }

    fn output(&mut self) -> BgraFrame {
        let mut out = self
            .spare
            .pop()
            .filter(|f| (f.width, f.height) == (self.canvas.width, self.canvas.height))
            .unwrap_or_else(|| BgraFrame::new_tight(self.canvas.width, self.canvas.height));
        out.stride = self.canvas.stride;
        out.data.clear();
        out.data.extend_from_slice(&self.canvas.data);
        out.timestamp_us = self.started.elapsed().as_micros() as u64;
        out
    }
}

impl ScreenCapturer for AllMonitorsCapturer {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<BgraFrame>, CaptureError> {
        let deadline = Instant::now() + timeout;
        let n = self.parts.len().max(1) as u32;
        let slice = (timeout / n).min(POLL_SLICE).max(Duration::from_millis(1));
        loop {
            let mut changed = false;
            for part in &mut self.parts {
                // After the first change, only collect what is already there.
                let wait = if changed { Duration::ZERO } else { slice };
                if let Some(f) = part.cap.next_frame(wait)? {
                    blit(&mut self.canvas, &f, part.place);
                    part.cap.recycle(f);
                    changed = true;
                }
            }
            if changed {
                return Ok(Some(self.output()));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
        }
    }

    fn cursor(&mut self) -> Result<CursorState, CaptureError> {
        // (part, x, y, the part reported a new shape just now)
        let mut found: Option<(usize, i32, i32, bool)> = None;
        for (i, part) in self.parts.iter_mut().enumerate() {
            let state = part.cap.cursor()?;
            let fresh = state.shape.is_some();
            if let Some(shape) = state.shape {
                part.shape = Some(shape);
            }
            if state.visible && found.is_none() {
                found = Some((i, state.x, state.y, fresh));
            }
        }
        let Some((i, x, y, fresh)) = found else {
            self.cursor_owner = None;
            return Ok(CursorState {
                x: 0,
                y: 0,
                visible: false,
                shape: None,
            });
        };
        let Some(part) = self.parts.get(i) else {
            return Ok(CursorState {
                x: 0,
                y: 0,
                visible: false,
                shape: None,
            });
        };
        let (b, m) = (&self.layout.bounds, &part.monitor);
        let shift = |p: i32, mon: i32, origin: i32| {
            let v = i64::from(p) + i64::from(mon) - i64::from(origin);
            v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
        };
        // Moving onto another monitor re-sends that monitor's shape; otherwise only changes.
        let shape = if fresh || self.cursor_owner != Some(i) {
            part.shape.clone()
        } else {
            None
        };
        self.cursor_owner = Some(i);
        Ok(CursorState {
            x: shift(x, m.x, b.x),
            y: shift(y, m.y, b.y),
            visible: true,
            shape,
        })
    }

    fn monitor(&self) -> &MonitorInfo {
        &self.layout.bounds
    }

    fn recycle(&mut self, frame: BgraFrame) {
        if self.spare.len() < MAX_SPARE {
            self.spare.push(frame);
        }
    }

    fn backend(&self) -> Option<CaptureBackend> {
        self.parts.first().and_then(|p| p.cap.backend())
    }

    fn frame_size(&self) -> (u32, u32) {
        self.layout.canvas
    }
}

#[cfg(test)]
mod tests;
