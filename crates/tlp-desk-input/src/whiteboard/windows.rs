//! Windows whiteboard overlay: a topmost, click-through, layered window covering the streamed
//! monitor, plus a small clickable "clear" badge, both on their own thread with their own
//! message loop.
//!
//! * The canvas window (`WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST |
//!   WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`) never takes input. Its pixels live in a 32-bpp DIB
//!   section that the renderer draws into directly (BGRA, premultiplied); only the damaged
//!   rectangle is redrawn and handed to `UpdateLayeredWindowIndirect` (`prcDirty`), at most
//!   about 60 times per second.
//! * The clear badge is layered and topmost but not transparent: a click calls the
//!   `on_clear` callback; `WM_MOUSEACTIVATE` returns `MA_NOACTIVATE`, so clicking it never
//!   steals the focus from the host user's application.
//! * Both windows are hidden while the board is empty and are excluded from screen capture
//!   (`SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`, Windows 10 2004+): the viewer draws
//!   the strokes itself.
//!
//! Built from the Microsoft Learn pages for `CreateWindowExW`, extended window styles,
//! `UpdateLayeredWindowIndirect`, `UPDATELAYEREDWINDOWINFO`, `SetWindowDisplayAffinity`,
//! `CreateDIBSection`, `WM_MOUSEACTIVATE`, `GetDpiForMonitor` and `SetTimer`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, COLORREF, ERROR_CLASS_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, POINT,
    RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, MonitorFromPoint,
    SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION,
    DIB_RGB_COLORS, HDC, HGDIOBJ, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, KillTimer,
    LoadCursorW, PeekMessageW, PostMessageW, PostThreadMessageW, RegisterClassExW, SetCursor,
    SetTimer, SetWindowDisplayAffinity, SetWindowPos, ShowWindow, UpdateLayeredWindowIndirect,
    HTTRANSPARENT, HWND_TOPMOST, IDC_HAND, MA_NOACTIVATE, MSG, PM_NOREMOVE, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA, UPDATELAYEREDWINDOWINFO,
    WDA_EXCLUDEFROMCAPTURE, WM_APP, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_NCHITTEST, WM_QUIT,
    WM_SETCURSOR, WM_TIMER, WM_USER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use super::overlay::{
    clear_button_image, lock_board, overlay_size_ok, ClearRequestFn, SharedBoard, WhiteboardOverlay,
};
use super::{blit, damage_region, render_region, ChannelOrder, PxRect};
use crate::{InputError, MonitorRect};

const CANVAS_CLASS: PCWSTR = w!("TlpDeskWhiteboardCanvas");
const BUTTON_CLASS: PCWSTR = w!("TlpDeskWhiteboardClear");
const WM_WB_DIRTY: u32 = WM_APP + 11;
const WM_WB_TARGET: u32 = WM_APP + 12;
const WM_WB_CLICK: u32 = WM_APP + 13;
const TIMER_RENDER: usize = 1;
/// Repaints are coalesced to about 60 per second.
const RENDER_MS: u32 = 16;
const TIMER_TOPMOST: usize = 2;
/// Re-asserting `HWND_TOPMOST` about once a second keeps the overlay above windows that
/// became topmost later.
const TOPMOST_MS: u32 = 1000;
/// Gap between the top edge of the monitor and the clear badge, at 96 DPI.
const BUTTON_MARGIN: f32 = 12.0;

struct Shared {
    target: Mutex<Option<MonitorRect>>,
    /// A `WM_WB_DIRTY` is already queued (coalesces bursts of strokes into one message).
    dirty_posted: AtomicBool,
}

/// The overlay windows (see the module docs). Dropping it destroys them and joins the thread.
pub(crate) struct OverlayWindow {
    shared: Arc<Shared>,
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
    excluded: bool,
}

struct Ready {
    thread_id: u32,
    excluded: bool,
}

impl OverlayWindow {
    /// Creates the hidden windows on a new thread.
    pub(crate) fn start(board: SharedBoard, on_clear: ClearRequestFn) -> Result<Self, InputError> {
        let shared = Arc::new(Shared {
            target: Mutex::new(None),
            dirty_posted: AtomicBool::new(false),
        });
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("tlp-desk-whiteboard".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || window_thread(shared, board, on_clear, ready_tx)
            })
            .map_err(|e| InputError::Platform(format!("spawn whiteboard thread: {e}")))?;
        match ready_rx.recv() {
            Ok(Ok(r)) => Ok(Self {
                shared,
                thread_id: r.thread_id,
                thread: Some(thread),
                excluded: r.excluded,
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(InputError::Platform(e))
            }
            Err(_) => {
                let _ = thread.join();
                Err(InputError::Platform("whiteboard thread died".into()))
            }
        }
    }

    fn post(&self, msg: u32) {
        // SAFETY: posts a message without pointers to the overlay thread's queue, which exists
        // before `start` returns.
        let _ = unsafe { PostThreadMessageW(self.thread_id, msg, WPARAM(0), LPARAM(0)) };
    }
}

impl WhiteboardOverlay for OverlayWindow {
    fn set_target(&mut self, rect: MonitorRect) {
        *self
            .shared
            .target
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(rect);
        self.post(WM_WB_TARGET);
    }

    fn changed(&mut self) {
        if !self.shared.dirty_posted.swap(true, Ordering::AcqRel) {
            self.post(WM_WB_DIRTY);
        }
    }

    fn excluded_from_capture(&self) -> bool {
        self.excluded
    }
}

impl Drop for OverlayWindow {
    fn drop(&mut self) {
        self.post(WM_QUIT);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

unsafe extern "system" fn canvas_proc(
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

unsafe extern "system" fn button_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // Clicking the badge must not activate it (the host user keeps their focus).
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_SETCURSOR => {
            // SAFETY: loads a shared system cursor (never freed) and sets it for this thread.
            unsafe {
                let hand = LoadCursorW(None, IDC_HAND).ok();
                SetCursor(hand);
            }
            LRESULT(1)
        }
        WM_LBUTTONUP => {
            // Handled by the message loop, outside of this window procedure.
            // SAFETY: posts a message without pointers to our own window.
            let _ = unsafe { PostMessageW(Some(hwnd), WM_WB_CLICK, WPARAM(0), LPARAM(0)) };
            LRESULT(0)
        }
        // SAFETY: default processing with the arguments the system passed us.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn hinstance() -> Option<HINSTANCE> {
    // SAFETY: a null module name returns the handle of the running executable.
    unsafe { GetModuleHandleW(PCWSTR::null()) }
        .ok()
        .map(|m| HINSTANCE(m.0))
}

fn register_class(
    hinst: Option<HINSTANCE>,
    name: PCWSTR,
    proc_: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
) -> Result<(), String> {
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(proc_),
        hInstance: hinst.unwrap_or_default(),
        lpszClassName: name,
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

fn create_popup(
    hinst: Option<HINSTANCE>,
    class: PCWSTR,
    click_through: bool,
) -> Result<HWND, String> {
    let mut ex = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW;
    if click_through {
        ex |= WS_EX_TRANSPARENT;
    }
    // SAFETY: the class was registered by this thread; all other arguments are plain values.
    unsafe {
        CreateWindowExW(
            ex,
            class,
            w!("TLP-Desk whiteboard"),
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
    }
    .map_err(|e| format!("CreateWindowExW: {e}"))
}

fn exclude_from_capture(hwnd: HWND) -> bool {
    // SAFETY: `hwnd` is a top-level window created by this thread.
    unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) }.is_ok()
}

fn destroy(hwnd: HWND) {
    // SAFETY: destroys a window created by this thread.
    let _ = unsafe { DestroyWindow(hwnd) };
}

fn window_thread(
    shared: Arc<Shared>,
    board: SharedBoard,
    on_clear: ClearRequestFn,
    ready: SyncSender<Result<Ready, String>>,
) {
    // Physical pixels for window placement, matching the capture's monitor rectangles.
    // SAFETY: takes a predefined pseudo-handle constant; affects only this thread.
    unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let mut msg = MSG::default();
    // SAFETY: `msg` is a valid, writable MSG; creates this thread's message queue.
    let _ = unsafe { PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE) };

    let hinst = hinstance();
    let created = register_class(hinst, CANVAS_CLASS, canvas_proc)
        .and_then(|()| register_class(hinst, BUTTON_CLASS, button_proc))
        .and_then(|()| create_popup(hinst, CANVAS_CLASS, true))
        .and_then(|canvas| match create_popup(hinst, BUTTON_CLASS, false) {
            Ok(button) => Ok((canvas, button)),
            Err(e) => {
                destroy(canvas);
                Err(e)
            }
        });
    let (canvas, button) = match created {
        Ok(w) => w,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let excluded = exclude_from_capture(canvas) & exclude_from_capture(button);
    // SAFETY: no arguments.
    let thread_id = unsafe { GetCurrentThreadId() };
    let _ = ready.send(Ok(Ready {
        thread_id,
        excluded,
    }));

    let mut state = Overlay {
        canvas,
        button,
        board,
        target: None,
        dib: None,
        button_dib: None,
        shown: false,
        render_pending: false,
    };
    // SAFETY: `msg` is a valid, writable MSG for each call.
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
        match msg.message {
            WM_WB_DIRTY => {
                shared.dirty_posted.store(false, Ordering::Release);
                state.schedule();
            }
            WM_WB_TARGET => {
                let target = *shared.target.lock().unwrap_or_else(PoisonError::into_inner);
                state.retarget(target);
            }
            WM_WB_CLICK if msg.hwnd == button => on_clear(),
            WM_TIMER if msg.hwnd == canvas && msg.wParam.0 == TIMER_RENDER => state.render(),
            WM_TIMER if msg.hwnd == canvas && msg.wParam.0 == TIMER_TOPMOST => state.raise(),
            _ => {
                // SAFETY: dispatches a message retrieved by GetMessageW.
                unsafe { DispatchMessageW(&msg) };
            }
        }
    }
    state.hide();
    destroy(button);
    destroy(canvas);
}

struct Overlay {
    canvas: HWND,
    button: HWND,
    board: SharedBoard,
    target: Option<MonitorRect>,
    /// The canvas pixels, the size of `target`.
    dib: Option<DibDc>,
    button_dib: Option<(DibDc, u32)>,
    shown: bool,
    render_pending: bool,
}

impl Overlay {
    /// Renders within one timer period (coalescing bursts of changes).
    fn schedule(&mut self) {
        if !self.render_pending {
            self.render_pending = true;
            // SAFETY: timer on our own window; WM_TIMER is handled in the message loop.
            unsafe { SetTimer(Some(self.canvas), TIMER_RENDER, RENDER_MS, None) };
        }
    }

    fn retarget(&mut self, target: Option<MonitorRect>) {
        let target = target.filter(|r| overlay_size_ok(*r));
        if target == self.target {
            return;
        }
        self.hide();
        self.target = target;
        self.dib = target.and_then(|r| DibDc::new(r.width, r.height));
        self.button_dib = target.and_then(|r| {
            let scale = monitor_dpi(center(r)) as f32 / 96.0;
            let img = clear_button_image(scale, ChannelOrder::Bgra)?;
            let side = img.width();
            let mut dib = DibDc::new(side, side)?;
            dib.pixels().copy_from_slice(img.data());
            Some((dib, side))
        });
        lock_board(&self.board).invalidate();
        self.schedule();
    }

    fn render(&mut self) {
        self.render_pending = false;
        // SAFETY: kills the render timer set by `schedule` (harmless if none).
        let _ = unsafe { KillTimer(Some(self.canvas), TIMER_RENDER) };
        let (Some(rect), Some(dib)) = (self.target, self.dib.as_mut()) else {
            return;
        };
        let (empty, dirty) = {
            let mut board = lock_board(&self.board);
            let damage = board.take_damage();
            let empty = board.is_empty();
            let region = if empty {
                None
            } else {
                damage_region(&board, &damage, rect.width, rect.height)
            };
            if let Some(region) = region {
                if let Some(pix) =
                    render_region(&board, rect.width, rect.height, region, ChannelOrder::Bgra)
                {
                    blit(dib.pixels(), rect.width, region, &pix);
                }
            }
            (empty, region)
        };
        if empty {
            dib.pixels().fill(0);
            self.hide();
            return;
        }
        if self.shown {
            if let Some(region) = dirty {
                let _ = update_layered(self.canvas, dib, rect, Some(region));
            }
        } else if update_layered(self.canvas, dib, rect, None).is_ok() {
            self.show(rect);
        }
    }

    fn show(&mut self, rect: MonitorRect) {
        if let Some((dib, side)) = self.button_dib.as_mut() {
            let scale = monitor_dpi(center(rect)) as f32 / 96.0;
            let half = i32::try_from(*side / 2).unwrap_or(0);
            let x = rect
                .x
                .saturating_add(i32::try_from(rect.width / 2).unwrap_or(0))
                .saturating_sub(half);
            let y = rect
                .y
                .saturating_add((BUTTON_MARGIN * scale).round() as i32);
            let at = MonitorRect::new(x, y, *side, *side);
            let _ = update_layered(self.button, dib, at, None);
        }
        // SAFETY: shows our own windows without activating them; the badge goes above the
        // canvas.
        unsafe {
            let _ = ShowWindow(self.canvas, SW_SHOWNOACTIVATE);
            if self.button_dib.is_some() {
                let _ = ShowWindow(self.button, SW_SHOWNOACTIVATE);
            }
            SetTimer(Some(self.canvas), TIMER_TOPMOST, TOPMOST_MS, None);
        }
        self.shown = true;
        self.raise();
    }

    fn hide(&mut self) {
        if !self.shown {
            return;
        }
        // SAFETY: hides our own windows and stops the topmost timer (harmless if none).
        unsafe {
            let _ = KillTimer(Some(self.canvas), TIMER_TOPMOST);
            let _ = ShowWindow(self.button, SW_HIDE);
            let _ = ShowWindow(self.canvas, SW_HIDE);
        }
        self.shown = false;
    }

    fn raise(&self) {
        if !self.shown {
            return;
        }
        for hwnd in [self.canvas, self.button] {
            // SAFETY: re-asserts the z-order of our own window; no move, resize or
            // activation.
            let _ = unsafe {
                SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
                )
            };
        }
    }
}

fn center(r: MonitorRect) -> POINT {
    POINT {
        x: r.x.saturating_add(i32::try_from(r.width / 2).unwrap_or(0)),
        y: r.y.saturating_add(i32::try_from(r.height / 2).unwrap_or(0)),
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
    x.clamp(48, 480)
}

/// Pushes `dib` (premultiplied BGRA, the size of `at`) to the layered window `hwnd` placed at
/// `at`; with `dirty`, only that rectangle of the DIB changed.
fn update_layered(
    hwnd: HWND,
    dib: &mut DibDc,
    at: MonitorRect,
    dirty: Option<PxRect>,
) -> Result<(), String> {
    dib.flush();
    let dst = POINT { x: at.x, y: at.y };
    let size = SIZE {
        cx: i32::try_from(at.width).map_err(|_| "overlay too wide")?,
        cy: i32::try_from(at.height).map_err(|_| "overlay too tall")?,
    };
    let src = POINT::default();
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let dirty_rect = dirty.map(|d| RECT {
        left: i32::try_from(d.x).unwrap_or(0),
        top: i32::try_from(d.y).unwrap_or(0),
        right: i32::try_from(d.right()).unwrap_or(size.cx),
        bottom: i32::try_from(d.bottom()).unwrap_or(size.cy),
    });
    let info = UPDATELAYEREDWINDOWINFO {
        cbSize: std::mem::size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
        hdcDst: HDC::default(),
        pptDst: &dst,
        psize: &size,
        hdcSrc: dib.dc,
        pptSrc: &src,
        crKey: COLORREF(0),
        pblend: &blend,
        dwFlags: ULW_ALPHA,
        prcDirty: dirty_rect
            .as_ref()
            .map_or(std::ptr::null(), |r| r as *const RECT),
    };
    // SAFETY: every pointer in `info` refers to a local that outlives the call; the source DC
    // holds a premultiplied 32-bpp bitmap of `size`; the system copies what it needs.
    let ok = unsafe { UpdateLayeredWindowIndirect(hwnd, &info) };
    if ok.as_bool() {
        Ok(())
    } else {
        Err("UpdateLayeredWindowIndirect failed".into())
    }
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
        let len = usize::try_from(u64::from(width) * u64::from(height) * 4).ok()?;
        if len == 0 {
            return None;
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: i32::try_from(width).ok()?,
                // Negative height = top-down rows.
                biHeight: -i32::try_from(height).ok()?,
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
        // A fresh DIB section is zero-filled (transparent) by the system.
        Some(Self {
            dc,
            bitmap,
            old,
            bits: bits.cast(),
            len,
        })
    }

    /// Makes sure GDI finished any pending drawing on the bitmap.
    fn flush(&self) {
        // SAFETY: no arguments; flushes this thread's GDI batch.
        let _ = unsafe { GdiFlush() };
    }

    fn pixels(&mut self) -> &mut [u8] {
        self.flush();
        // SAFETY: `bits` points to the DIB's `len` bytes, owned by `bitmap`, which lives as
        // long as `self`; GDI drawing was flushed above and `&mut self` makes the slice
        // exclusive.
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.len) }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::whiteboard::{Rgba, Stroke};

    /// Shows a short scribble on the real desktop for about a second, so it is not part of
    /// the default run. Run it by hand: `cargo test -p tlp-desk-input -- --ignored overlay`.
    #[test]
    #[ignore = "shows a window on the interactive desktop"]
    fn overlay_window_shows_strokes_and_stops() {
        let board: SharedBoard = Arc::default();
        let clicks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c2 = clicks.clone();
        let mut o = OverlayWindow::start(
            board.clone(),
            Arc::new(move || {
                c2.fetch_add(1, Ordering::Relaxed);
            }),
        )
        .expect("create overlay");
        println!("excluded from capture: {}", o.excluded_from_capture());
        o.set_target(MonitorRect::new(0, 0, 800, 600));
        lock_board(&board).add(&Stroke {
            id: 1,
            points: vec![(5000, 5000), (30000, 40000), (60000, 10000)],
            color: Rgba::new(0xE5, 0x39, 0x35, 0xFF),
            width: 80,
        });
        o.changed();
        std::thread::sleep(std::time::Duration::from_millis(800));
        lock_board(&board).clear();
        o.changed();
        std::thread::sleep(std::time::Duration::from_millis(100));
        drop(o);
    }
}
