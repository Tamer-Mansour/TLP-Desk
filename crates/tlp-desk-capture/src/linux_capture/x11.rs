//! X11 capture: `GetImage` of the root window per monitor, RandR monitor enumeration and the
//! XFixes cursor image.
//!
//! References: the X11 core protocol (`GetImage`, ZPixmap format), RandR 1.5 `GetMonitors`
//! and XFixes 4 `GetCursorImage`.
//!
//! The root window is read in pipelined horizontal bands, so no single reply is large and the
//! server works while earlier bands are already being copied. A MIT-SHM transport would avoid
//! one socket copy per frame; it needs `shmget`/`shmat` (unsafe) and is a possible follow-up,
//! the protocol-level format handled here is identical.

use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xfixes::ConnectionExt as _;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, ImageOrder, Window};
use x11rb::rust_connection::RustConnection;

use super::layout;
use crate::{
    fit_within, BgraFrame, CaptureError, CaptureOptions, CursorShape, CursorState, MonitorInfo,
    ScreenCapturer,
};

fn plat(context: &str, e: impl std::fmt::Display) -> CaptureError {
    CaptureError::Platform(format!("{context}: {e}"))
}

/// A connection plus the facts every operation needs.
struct XConn {
    conn: RustConnection,
    root: Window,
    root_size: (u32, u32),
}

impl XConn {
    fn open() -> Result<Self, CaptureError> {
        let (conn, screen_num) =
            RustConnection::connect(None).map_err(|e| plat("connect to the X server", e))?;
        let setup = conn.setup();
        let screen = setup
            .roots
            .get(screen_num)
            .ok_or_else(|| CaptureError::Platform("X server reported no screen".into()))?;
        let root = screen.root;
        let root_size = (
            u32::from(screen.width_in_pixels),
            u32::from(screen.height_in_pixels),
        );

        // Only the 32-bit little-endian BGRX layout is handled (every desktop X server).
        let depth = screen.root_depth;
        let bpp = setup
            .pixmap_formats
            .iter()
            .find(|f| f.depth == depth)
            .map_or(0, |f| f.bits_per_pixel);
        let visual = screen
            .allowed_depths
            .iter()
            .flat_map(|d| d.visuals.iter())
            .find(|v| v.visual_id == screen.root_visual);
        let ok = visual.is_some_and(|v| {
            layout::is_bgrx32(
                bpp,
                setup.image_byte_order == ImageOrder::LSB_FIRST,
                v.red_mask,
                v.green_mask,
                v.blue_mask,
            )
        });
        if !ok {
            return Err(CaptureError::Platform(format!(
                "unsupported X pixel format (depth {depth}, {bpp} bits per pixel); \
                 a 24/32-bit TrueColor little-endian display is required"
            )));
        }
        Ok(Self {
            conn,
            root,
            root_size,
        })
    }

    /// Monitors from RandR 1.5, or the whole screen as one monitor when RandR is too old.
    fn monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError> {
        let mut out = Vec::new();
        let randr15 = self
            .conn
            .randr_query_version(1, 5)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|v| (v.major_version, v.minor_version) >= (1, 5));
        if randr15 {
            if let Ok(reply) = self
                .conn
                .randr_get_monitors(self.root, true)
                .map_err(|e| plat("RandR GetMonitors", e))?
                .reply()
            {
                for (i, m) in reply.monitors.iter().enumerate() {
                    if m.width == 0 || m.height == 0 {
                        continue;
                    }
                    let name = self
                        .conn
                        .get_atom_name(m.name)
                        .ok()
                        .and_then(|c| c.reply().ok())
                        .map(|r| String::from_utf8_lossy(&r.name).into_owned())
                        .unwrap_or_else(|| format!("monitor{i}"));
                    out.push(MonitorInfo {
                        index: 0,
                        name,
                        width: u32::from(m.width),
                        height: u32::from(m.height),
                        primary: m.primary,
                        x: i32::from(m.x),
                        y: i32::from(m.y),
                    });
                }
            }
        }
        if out.is_empty() {
            out.push(MonitorInfo {
                index: 0,
                name: "X11".into(),
                width: self.root_size.0,
                height: self.root_size.1,
                primary: true,
                x: 0,
                y: 0,
            });
        }
        Ok(layout::order_monitors(out))
    }
}

pub(crate) fn list_monitors() -> Result<Vec<MonitorInfo>, CaptureError> {
    XConn::open()?.monitors()
}

pub(crate) fn open_capturer(
    monitor_index: u32,
    options: CaptureOptions,
) -> Result<Box<dyn ScreenCapturer>, CaptureError> {
    let x = XConn::open()?;
    let monitor = x
        .monitors()?
        .into_iter()
        .find(|m| m.index == monitor_index)
        .ok_or(CaptureError::NoSuchMonitor(monitor_index))?;
    Ok(Box::new(X11Capturer::new(x, monitor, options)?))
}

/// Captures one monitor of the X root window.
struct X11Capturer {
    x: XConn,
    monitor: MonitorInfo,
    /// Size of delivered frames (the monitor size unless `max_size` shrank it).
    out_size: (u32, u32),
    started: Instant,
    last_capture: Option<Instant>,
    /// Native-size staging frame, only used when the output is downscaled.
    native: Option<BgraFrame>,
    pool: Vec<Vec<u8>>,
    /// Serial of the cursor image last sent to the host.
    cursor_serial: Option<u32>,
    xfixes: bool,
}

impl X11Capturer {
    fn new(x: XConn, monitor: MonitorInfo, options: CaptureOptions) -> Result<Self, CaptureError> {
        let native = (monitor.width, monitor.height);
        let out_size = options
            .max_size
            .map_or(native, |max| fit_within(native, max));
        let xfixes = x
            .conn
            .xfixes_query_version(4, 0)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some();
        Ok(Self {
            x,
            monitor,
            out_size,
            started: Instant::now(),
            last_capture: None,
            native: None,
            pool: Vec::new(),
            cursor_serial: None,
            xfixes,
        })
    }

    fn take_buffer(&mut self, len: usize) -> Vec<u8> {
        let mut v = self.pool.pop().unwrap_or_default();
        v.clear();
        v.resize(len, 0);
        v
    }

    /// Reads the monitor rectangle into `dst` (native size).
    fn grab(&self, dst: &mut BgraFrame) -> Result<(), CaptureError> {
        let rows = layout::band_rows(dst.width, dst.height, layout::MAX_BAND_BYTES);
        let width = u16::try_from(dst.width).map_err(|_| CaptureError::SurfaceLost)?;
        let mut pending = Vec::new();
        // Send every request before reading any reply so the server never idles.
        for (first, n) in layout::bands(dst.height, rows) {
            let y = i16::try_from(i64::from(self.monitor.y) + i64::from(first))
                .map_err(|_| CaptureError::SurfaceLost)?;
            let x = i16::try_from(self.monitor.x).map_err(|_| CaptureError::SurfaceLost)?;
            let n16 = u16::try_from(n).map_err(|_| CaptureError::SurfaceLost)?;
            let cookie = self
                .x
                .conn
                .get_image(ImageFormat::Z_PIXMAP, self.x.root, x, y, width, n16, !0)
                .map_err(|e| plat("GetImage", e))?;
            pending.push((first, n, cookie));
        }
        for (first, n, cookie) in pending {
            // A failed GetImage means the geometry changed under us (resolution switch,
            // monitor unplugged): the host recreates the capturer.
            let reply = cookie.reply().map_err(|_| CaptureError::SurfaceLost)?;
            if !layout::copy_band(dst, first, n, &reply.data) {
                return Err(CaptureError::SurfaceLost);
            }
        }
        Ok(())
    }
}

impl ScreenCapturer for X11Capturer {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<BgraFrame>, CaptureError> {
        // X has no damage notification here, so pace like a frame timer: one capture per
        // `timeout`. The host skips frames identical to the previous one.
        if let Some(last) = self.last_capture {
            let wait = (last + timeout).saturating_duration_since(Instant::now());
            if !wait.is_zero() {
                std::thread::sleep(wait);
            }
        }
        self.last_capture = Some(Instant::now());

        let (nw, nh) = (self.monitor.width, self.monitor.height);
        let scaled = self.out_size != (nw, nh);
        let ts = u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX);

        if !scaled {
            let len = nw as usize * nh as usize * 4;
            let data = self.take_buffer(len);
            let mut frame = BgraFrame {
                width: nw,
                height: nh,
                stride: nw * 4,
                data,
                timestamp_us: ts,
            };
            self.grab(&mut frame)?;
            return Ok(Some(frame));
        }

        let mut native = self
            .native
            .take()
            .unwrap_or_else(|| BgraFrame::new_tight(nw, nh));
        let result = self.grab(&mut native);
        let (ow, oh) = self.out_size;
        let mut out = BgraFrame {
            width: ow,
            height: oh,
            stride: ow * 4,
            data: self.take_buffer(ow as usize * oh as usize * 4),
            timestamp_us: ts,
        };
        if result.is_ok() {
            layout::downscale_area(&native, &mut out);
        }
        self.native = Some(native);
        result?;
        Ok(Some(out))
    }

    fn cursor(&mut self) -> Result<CursorState, CaptureError> {
        let hidden = CursorState {
            x: 0,
            y: 0,
            visible: false,
            shape: None,
        };
        if !self.xfixes {
            return Ok(hidden);
        }
        let img = self
            .x
            .conn
            .xfixes_get_cursor_image()
            .map_err(|e| plat("XFixes GetCursorImage", e))?
            .reply()
            .map_err(|e| plat("XFixes GetCursorImage", e))?;
        let shape = (self.cursor_serial != Some(img.cursor_serial)).then(|| {
            self.cursor_serial = Some(img.cursor_serial);
            let (w, h) = (usize::from(img.width), usize::from(img.height));
            let mut rgba = layout::argb_premul_to_rgba(&img.cursor_image);
            rgba.resize(w * h * 4, 0);
            CursorShape {
                width: u32::from(img.width),
                height: u32::from(img.height),
                hotspot_x: u32::from(img.xhot),
                hotspot_y: u32::from(img.yhot),
                rgba,
            }
        });
        // `x`/`y` locate the hotspot in root coordinates.
        let (x, y) = (
            i32::from(img.x) - self.monitor.x,
            i32::from(img.y) - self.monitor.y,
        );
        let inside = x >= 0
            && y >= 0
            && i64::from(x) < i64::from(self.monitor.width)
            && i64::from(y) < i64::from(self.monitor.height);
        Ok(CursorState {
            x,
            y,
            visible: inside,
            shape,
        })
    }

    fn monitor(&self) -> &MonitorInfo {
        &self.monitor
    }

    fn recycle(&mut self, frame: BgraFrame) {
        // Bounded pool: at most a few frames are ever in flight.
        if self.pool.len() < 4 {
            self.pool.push(frame.data);
        }
    }

    fn frame_size(&self) -> (u32, u32) {
        self.out_size
    }
}
