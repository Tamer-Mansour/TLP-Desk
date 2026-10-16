//! Green "remote pointer" marker on the host's own screen.
//!
//! A topmost, click-through, non-activating layered popup (`WS_EX_LAYERED |
//! WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`) on its own
//! thread with its own message loop. While visible, a ~60 Hz timer keeps it at the cursor
//! position. The image comes from [`MarkerImage`] (per-pixel alpha via
//! `UpdateLayeredWindow`) and is re-rendered when the cursor moves to a monitor with a
//! different DPI. `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` keeps it out of screen
//! capture, so the viewer never sees it in the video.
//!
//! Built from the Microsoft Learn pages for `CreateWindowExW`, extended window styles,
//! `UpdateLayeredWindow`, `SetWindowDisplayAffinity`, `CreateDIBSection`, `DrawTextW`,
//! `GetDpiForMonitor` and `SetTimer`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, COLORREF, ERROR_CLASS_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, POINT,
    RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW, GdiFlush,
    MonitorFromPoint, SelectObject, SetBkMode, SetTextColor, AC_SRC_ALPHA, AC_SRC_OVER,
    ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CALCRECT, DT_NOPREFIX, DT_RTLREADING, DT_SINGLELINE,
    FW_SEMIBOLD, HDC, HGDIOBJ, MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    KillTimer, PeekMessageW, PostThreadMessageW, RegisterClassExW, SetTimer,
    SetWindowDisplayAffinity, SetWindowPos, ShowWindow, UpdateLayeredWindow, HTTRANSPARENT,
    HWND_TOPMOST, MSG, PM_NOREMOVE, SWP_NOACTIVATE, SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE,
    ULW_ALPHA, WDA_EXCLUDEFROMCAPTURE, WM_APP, WM_NCHITTEST, WM_QUIT, WM_TIMER, WM_USER,
    WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::marker_shape::{MarkerImage, TextMask};
use crate::InputError;

const CLASS_NAME: PCWSTR = w!("TlpDeskRemotePointerMarker");
const WM_MARKER_SYNC: u32 = WM_APP + 1;
const WM_MARKER_LABEL: u32 = WM_APP + 2;
const TIMER_ID: usize = 1;
/// ~60 Hz cursor following.
const TIMER_MS: u32 = 16;
/// Longest label (characters) drawn under the arrow.
const MAX_LABEL_CHARS: usize = 32;
/// Label size caps in 96-DPI pixels.
const MAX_LABEL_WIDTH: f32 = 220.0;
const MAX_LABEL_HEIGHT: f32 = 24.0;

struct Shared {
    visible: AtomicBool,
    /// Label requested by [`RemotePointerMarker::set_label`], picked up on `WM_MARKER_LABEL`.
    label: Mutex<String>,
}

/// The overlay window. [`set_visible`](Self::set_visible) is cheap and non-blocking (an
/// atomic store plus a posted message). Dropping it destroys the window and joins its thread.
pub struct RemotePointerMarker {
    shared: Arc<Shared>,
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
    excluded_from_capture: bool,
}

impl std::fmt::Debug for RemotePointerMarker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemotePointerMarker")
            .field("visible", &self.shared.visible.load(Ordering::Relaxed))
            .field("excluded_from_capture", &self.excluded_from_capture)
            .finish_non_exhaustive()
    }
}

struct Ready {
    thread_id: u32,
    excluded: bool,
}

impl RemotePointerMarker {
    /// Creates the (hidden) overlay window on a new thread. `label` (e.g. the viewer's name)
    /// is drawn under the arrow; empty = arrow only.
    pub fn start(label: &str) -> Result<Self, InputError> {
        Self::start_with(label, true)
    }

    /// Like [`start`](Self::start); `exclude_from_capture = false` leaves the marker visible
    /// to screen capture (for screenshots/documentation only; sessions always exclude it).
    pub fn start_with(label: &str, exclude_from_capture: bool) -> Result<Self, InputError> {
        let label: String = label.trim().chars().take(MAX_LABEL_CHARS).collect();
        let shared = Arc::new(Shared {
            visible: AtomicBool::new(false),
            label: Mutex::new(label.clone()),
        });
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("tlp-desk-pointer-marker".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || window_thread(shared, label, exclude_from_capture, ready_tx)
            })
            .map_err(|e| InputError::Platform(format!("spawn marker thread: {e}")))?;
        match ready_rx.recv() {
            Ok(Ok(r)) => Ok(Self {
                shared,
                thread_id: r.thread_id,
                thread: Some(thread),
                excluded_from_capture: r.excluded,
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(InputError::Platform(e))
            }
            Err(_) => {
                let _ = thread.join();
                Err(InputError::Platform("marker thread died".into()))
            }
        }
    }

    /// Shows the marker at the cursor (following it) or hides it.
    pub fn set_visible(&self, visible: bool) {
        if self.shared.visible.swap(visible, Ordering::AcqRel) != visible {
            self.post(WM_MARKER_SYNC);
        }
    }

    /// Changes the label under the arrow (cheap; the window redraws on its own thread).
    pub fn set_label(&self, label: &str) {
        let label: String = label.trim().chars().take(MAX_LABEL_CHARS).collect();
        {
            let mut cur = self.shared.label.lock().unwrap_or_else(|p| p.into_inner());
            if *cur == label {
                return;
            }
            *cur = label;
        }
        self.post(WM_MARKER_LABEL);
    }

    pub fn is_visible(&self) -> bool {
        self.shared.visible.load(Ordering::Acquire)
    }

    /// Whether `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` succeeded (Windows 10 2004+).
    /// If not, the marker may appear in the captured video.
    pub fn excluded_from_capture(&self) -> bool {
        self.excluded_from_capture
    }

    fn post(&self, msg: u32) {
        // SAFETY: posts a message without pointers to the marker thread's queue, which exists
        // before `start` returns.
        let _ = unsafe { PostThreadMessageW(self.thread_id, msg, WPARAM(0), LPARAM(0)) };
    }
}

impl Drop for RemotePointerMarker {
    fn drop(&mut self) {
        self.post(WM_QUIT);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCHITTEST {
        // Belt and braces next to WS_EX_TRANSPARENT: never take mouse input.
        return LRESULT(HTTRANSPARENT as isize);
    }
    // SAFETY: default processing with the arguments the system passed us.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn hinstance() -> Option<HINSTANCE> {
    // SAFETY: a null module name returns the handle of the running executable.
    unsafe { GetModuleHandleW(PCWSTR::null()) }
        .ok()
        .map(|m| HINSTANCE(m.0))
}

fn register_class(hinst: Option<HINSTANCE>) -> Result<(), String> {
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wnd_proc),
        hInstance: hinst.unwrap_or_default(),
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    // SAFETY: `wc` is fully initialized, its class name is a static wide string and the
    // window procedure lives for the whole program.
    if unsafe { RegisterClassExW(&wc) } == 0 {
        // SAFETY: reads the calling thread's last-error value.
        let err = unsafe { GetLastError() };
        if err != ERROR_CLASS_ALREADY_EXISTS {
            return Err(format!("RegisterClassExW: {err:?}"));
        }
    }
    Ok(())
}

fn window_thread(
    shared: Arc<Shared>,
    label: String,
    exclude_from_capture: bool,
    ready: SyncSender<Result<Ready, String>>,
) {
    // Physical pixels for the cursor position, window placement and per-monitor DPI.
    // SAFETY: takes a predefined pseudo-handle constant; affects only this thread.
    unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let mut msg = MSG::default();
    // SAFETY: `msg` is a valid, writable MSG; creates this thread's message queue.
    let _ = unsafe { PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE) };

    let hinst = hinstance();
    if let Err(e) = register_class(hinst) {
        let _ = ready.send(Err(e));
        return;
    }
    // SAFETY: the class was registered above; all other arguments are plain values.
    let hwnd = match unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            CLASS_NAME,
            w!("tlp-desk remote pointer"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            hinst,
            None,
        )
    } {
        Ok(h) => h,
        Err(e) => {
            let _ = ready.send(Err(format!("CreateWindowExW: {e}")));
            return;
        }
    };
    // SAFETY: `hwnd` is the top-level window this thread just created.
    let excluded = exclude_from_capture
        && unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) }.is_ok();
    // SAFETY: no arguments.
    let thread_id = unsafe { GetCurrentThreadId() };
    let _ = ready.send(Ok(Ready {
        thread_id,
        excluded,
    }));

    let mut state = Overlay {
        hwnd,
        label,
        dpi: 0,
        hotspot: (0, 0),
        pos: None,
        shown: false,
        ticks: 0,
    };
    // SAFETY: `msg` is a valid, writable MSG for each call.
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
        match msg.message {
            WM_MARKER_SYNC => state.sync(shared.visible.load(Ordering::Acquire)),
            WM_MARKER_LABEL => {
                state.label = shared
                    .label
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone();
                // Force a re-render with the new text at the next cursor update.
                state.dpi = 0;
                state.follow();
            }
            WM_TIMER if msg.hwnd == hwnd => state.follow(),
            _ => {
                // SAFETY: dispatches a message retrieved by GetMessageW.
                unsafe { DispatchMessageW(&msg) };
            }
        }
    }
    state.sync(false);
    // SAFETY: destroys the window created by this thread.
    let _ = unsafe { DestroyWindow(hwnd) };
}

struct Overlay {
    hwnd: HWND,
    label: String,
    /// DPI the current image was rendered for (0 = none yet).
    dpi: u32,
    hotspot: (i32, i32),
    pos: Option<(i32, i32)>,
    shown: bool,
    ticks: u32,
}

impl Overlay {
    fn sync(&mut self, visible: bool) {
        if visible == self.shown {
            return;
        }
        self.shown = visible;
        if visible {
            self.pos = None;
            self.follow();
            // SAFETY: timer on our own window; WM_TIMER is handled in the message loop.
            unsafe { SetTimer(Some(self.hwnd), TIMER_ID, TIMER_MS, None) };
            // SAFETY: shows our own window without activating it.
            let _ = unsafe { ShowWindow(self.hwnd, SW_SHOWNOACTIVATE) };
        } else {
            // SAFETY: kills the timer set above (harmless if none) and hides our window.
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_ID);
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }

    /// Moves the window to the cursor; re-renders when the monitor DPI changed.
    fn follow(&mut self) {
        if !self.shown {
            return;
        }
        let mut pt = POINT::default();
        // SAFETY: `pt` is a valid, writable POINT.
        if unsafe { GetCursorPos(&mut pt) }.is_err() {
            return;
        }
        let dpi = monitor_dpi(pt);
        if dpi != self.dpi {
            self.dpi = dpi;
            self.upload(pt, dpi);
        } else if self.pos != Some((pt.x, pt.y)) || self.ticks % 60 == 0 {
            // Re-asserting HWND_TOPMOST about once a second keeps us above windows that
            // became topmost later.
            // SAFETY: repositions our own window; no activation, no resize.
            let _ = unsafe {
                SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOPMOST),
                    pt.x - self.hotspot.0,
                    pt.y - self.hotspot.1,
                    0,
                    0,
                    SWP_NOACTIVATE | SWP_NOSIZE,
                )
            };
        }
        self.ticks = self.ticks.wrapping_add(1);
        self.pos = Some((pt.x, pt.y));
    }

    fn upload(&mut self, pt: POINT, dpi: u32) {
        let scale = dpi as f32 / 96.0;
        let mask = if self.label.is_empty() {
            None
        } else {
            text_mask(&self.label, scale)
        };
        let img = MarkerImage::render(scale, mask.as_ref());
        debug_assert!(img.width <= MarkerImage::max_extent(scale));
        self.hotspot = img.hotspot;
        let dst = POINT {
            x: pt.x - img.hotspot.0,
            y: pt.y - img.hotspot.1,
        };
        if let Err(e) = update_layered(self.hwnd, &img, dst) {
            // Nothing else to do from inside the message loop; the window just stays empty.
            let _ = e;
        }
    }
}

fn monitor_dpi(pt: POINT) -> u32 {
    // SAFETY: plain value arguments; returns a monitor handle (never null with NEAREST).
    let mon = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
    let (mut x, mut y) = (96u32, 96u32);
    // SAFETY: `x` and `y` are valid, writable u32s.
    if unsafe { GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut x, &mut y) }.is_err() {
        return 96;
    }
    x.max(48)
}

/// A memory DC with a top-down 32-bpp DIB section selected into it.
struct DibDc {
    dc: HDC,
    bitmap: HGDIOBJ,
    old: HGDIOBJ,
    bits: *mut u8,
    len: usize,
}

impl DibDc {
    fn new(width: u32, height: u32) -> Option<Self> {
        if width == 0 || height == 0 {
            return None;
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                // Negative height = top-down rows.
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        // SAFETY: creates a memory DC compatible with the screen; freed in Drop.
        let dc = unsafe { CreateCompatibleDC(None) };
        if dc.is_invalid() {
            return None;
        }
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: `info` describes a 32-bpp top-down DIB; `bits` receives its pixel pointer,
        // which stays valid until the bitmap is deleted in Drop.
        let bitmap = match unsafe {
            CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
        } {
            Ok(b) if !bits.is_null() => b,
            _ => {
                // SAFETY: deletes the DC created above.
                let _ = unsafe { DeleteDC(dc) };
                return None;
            }
        };
        let bitmap = HGDIOBJ(bitmap.0);
        // SAFETY: selects our bitmap into our DC; the previous object is restored in Drop.
        let old = unsafe { SelectObject(dc, bitmap) };
        Some(Self {
            dc,
            bitmap,
            old,
            bits: bits.cast(),
            len: (width * height * 4) as usize,
        })
    }

    fn pixels(&mut self) -> &mut [u8] {
        // SAFETY: `bits` points to the DIB's `len` bytes, owned by `bitmap`, which lives as
        // long as `self`; GDI drawing is flushed before the slice is used.
        unsafe {
            let _ = GdiFlush();
            std::slice::from_raw_parts_mut(self.bits, self.len)
        }
    }
}

impl Drop for DibDc {
    fn drop(&mut self) {
        // SAFETY: restores the DC's original bitmap, then frees the objects we created.
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap);
            let _ = DeleteDC(self.dc);
        }
    }
}

fn update_layered(hwnd: HWND, img: &MarkerImage, dst: POINT) -> Result<(), String> {
    let mut dib = DibDc::new(img.width, img.height).ok_or("CreateDIBSection failed")?;
    dib.pixels().copy_from_slice(&img.bgra);
    let size = SIZE {
        cx: img.width as i32,
        cy: img.height as i32,
    };
    let src = POINT::default();
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    // SAFETY: all pointers refer to locals that outlive the call; the source DC holds a
    // premultiplied 32-bpp bitmap of `size`; the system copies it.
    unsafe {
        UpdateLayeredWindow(
            hwnd,
            None,
            Some(&dst),
            Some(&size),
            Some(dib.dc),
            Some(&src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        )
    }
    .map_err(|e| format!("UpdateLayeredWindow: {e}"))?;
    // SAFETY: repositions/raises our own window without activating it.
    let _ = unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            dst.x,
            dst.y,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOSIZE,
        )
    };
    Ok(())
}

fn has_rtl(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(c as u32,
            0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF)
    })
}

/// Renders `label` with GDI (white on black, grayscale anti-aliasing) and returns its
/// coverage. GDI shapes Arabic itself; `DT_RTLREADING` sets the paragraph direction.
fn text_mask(label: &str, scale: f32) -> Option<TextMask> {
    let mut text: Vec<u16> = label.encode_utf16().collect();
    let mut fmt = DT_SINGLELINE | DT_NOPREFIX;
    if has_rtl(label) {
        fmt |= DT_RTLREADING;
    }
    let height = -((12.0 * scale).round() as i32).max(8);
    // SAFETY: plain value arguments and a static face name; the font is deleted below.
    let font = unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            FW_SEMIBOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            0,
            w!("Segoe UI"),
        )
    };
    if font.is_invalid() {
        return None;
    }
    let font = HGDIOBJ(font.0);
    let measure = {
        // SAFETY: temporary memory DC for measuring; deleted right after.
        let dc = unsafe { CreateCompatibleDC(None) };
        let mut rc = RECT::default();
        // SAFETY: `dc` is valid, `font` is a live font, `text` and `rc` are writable locals.
        unsafe {
            let old = SelectObject(dc, font);
            DrawTextW(dc, &mut text, &mut rc, fmt | DT_CALCRECT);
            SelectObject(dc, old);
            let _ = DeleteDC(dc);
        }
        rc
    };
    // Hard caps keep the overlay small whatever GDI reports (DrawTextW clips to the rect).
    let max_w = (MAX_LABEL_WIDTH * scale).round() as i32;
    let max_h = (MAX_LABEL_HEIGHT * scale).round() as i32;
    let (w, h) = (
        (measure.right - measure.left).clamp(0, max_w) as u32,
        (measure.bottom - measure.top).clamp(0, max_h) as u32,
    );
    let result = DibDc::new(w, h).map(|mut dib| {
        dib.pixels().fill(0);
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: w as i32,
            bottom: h as i32,
        };
        // SAFETY: `dib.dc` has our bitmap selected; `font` is live; locals are writable.
        unsafe {
            let old = SelectObject(dib.dc, font);
            SetBkMode(dib.dc, TRANSPARENT);
            SetTextColor(dib.dc, COLORREF(0x00FF_FFFF));
            DrawTextW(dib.dc, &mut text, &mut rc, fmt);
            SelectObject(dib.dc, old);
        }
        let coverage = dib
            .pixels()
            .chunks_exact(4)
            .map(|p| p[0].max(p[1]).max(p[2]))
            .collect();
        TextMask {
            width: w,
            height: h,
            coverage,
        }
    });
    // SAFETY: the font is no longer selected into any DC.
    let _ = unsafe { DeleteObject(font) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flashes the green marker on the real desktop, so it is not part of the default run.
    #[test]
    #[ignore = "shows a window on the interactive desktop"]
    fn marker_window_starts_toggles_and_stops() {
        let m = RemotePointerMarker::start("مكتب viewer").expect("create marker window");
        println!("excluded from capture: {}", m.excluded_from_capture());
        assert!(!m.is_visible());
        m.set_visible(true);
        assert!(m.is_visible());
        std::thread::sleep(std::time::Duration::from_millis(60));
        m.set_visible(false);
        assert!(!m.is_visible());
        drop(m);
    }

    #[test]
    fn label_text_renders_some_ink() {
        let mask = text_mask("viewer-pc", 1.0).expect("text mask");
        assert!(mask.width > 10 && mask.height > 5);
        let long = text_mask(&"W".repeat(MAX_LABEL_CHARS), 2.5).expect("long mask");
        assert!(long.width <= (MAX_LABEL_WIDTH * 2.5).round() as u32);
        assert!(long.height <= (MAX_LABEL_HEIGHT * 2.5).round() as u32);
        let img = MarkerImage::render(2.5, Some(&long));
        assert!(img.width <= MarkerImage::max_extent(2.5));
        assert!(img.height <= MarkerImage::max_extent(2.5));
        assert!(mask.coverage.iter().any(|&c| c > 200));
        let ar = text_mask("مكتب", 1.5).expect("arabic mask");
        assert!(ar.coverage.iter().any(|&c| c > 200));
    }
}
