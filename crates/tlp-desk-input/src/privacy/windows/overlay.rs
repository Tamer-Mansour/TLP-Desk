//! The black privacy screens: one top-most, click-through, non-activating window per monitor,
//! excluded from screen capture.
//!
//! Built from the Microsoft Learn pages for `EnumDisplayMonitors`, `CreateWindowExW`,
//! extended window styles (`WS_EX_LAYERED | WS_EX_TRANSPARENT` make a window click-through),
//! `SetLayeredWindowAttributes`, `SetWindowDisplayAffinity` (`WDA_EXCLUDEFROMCAPTURE`, Windows
//! 10 version 2004 and later), `BeginPaint`, `DrawTextW` and `GetDpiForWindow`.

use std::cell::RefCell;

use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, COLORREF, ERROR_CLASS_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, RECT,
    WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, DeleteObject, DrawTextW, EndPaint, EnumDisplayMonitors, FillRect,
    GetStockObject, InvalidateRect, SelectObject, SetBkMode, SetTextColor, BLACK_BRUSH,
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DRAW_TEXT_FORMAT, DT_CALCRECT,
    DT_CENTER, DT_NOPREFIX, DT_RTLREADING, DT_WORDBREAK, FW_NORMAL, FW_SEMIBOLD, HBRUSH, HDC,
    HGDIOBJ, HMONITOR, OUT_DEFAULT_PRECIS, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, PostThreadMessageW,
    RegisterClassExW, SetLayeredWindowAttributes, SetWindowDisplayAffinity, SetWindowPos,
    ShowWindow, HTTRANSPARENT, HWND_TOPMOST, LWA_ALPHA, MA_NOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE, WDA_EXCLUDEFROMCAPTURE, WM_DISPLAYCHANGE,
    WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_NCHITTEST, WM_PAINT, WNDCLASSEXW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use super::WM_SHIELD_REBUILD;
use crate::privacy::PrivacyScreenText;

const CLASS_NAME: PCWSTR = w!("TlpDeskPrivacyScreen");
/// Most monitors covered (a hostile or broken display configuration cannot make us create
/// thousands of windows).
const MAX_MONITORS: usize = 16;
/// Title / body font heights at 96 DPI.
const TITLE_PX: f32 = 34.0;
const BODY_PX: f32 = 19.0;
const TITLE_COLOR: COLORREF = COLORREF(0x00F0_F0F0);
const BODY_COLOR: COLORREF = COLORREF(0x00B4_B4B4);

thread_local! {
    /// What the windows of this (shield) thread paint.
    static TEXT: RefCell<PrivacyScreenText> = RefCell::new(PrivacyScreenText::default());
}

/// The overlay windows of the shield thread. Destroyed on drop.
pub(super) struct Overlays {
    hinst: Option<HINSTANCE>,
    windows: Vec<HWND>,
}

impl Overlays {
    /// Creates (hidden) overlays for every monitor. Fails if any cannot be excluded from
    /// capture: the viewer would then only see black.
    pub(super) fn create(text: &PrivacyScreenText) -> Result<Self, String> {
        set_text(text);
        // SAFETY: a null module name returns the handle of the running executable.
        let hinst = unsafe { GetModuleHandleW(PCWSTR::null()) }
            .ok()
            .map(|m| HINSTANCE(m.0));
        register_class(hinst)?;
        let mut me = Self {
            hinst,
            windows: Vec::new(),
        };
        me.build()?;
        Ok(me)
    }

    fn build(&mut self) -> Result<(), String> {
        let rects = monitor_rects();
        if rects.is_empty() {
            return Err("no monitors".into());
        }
        for r in rects {
            let hwnd = create_window(self.hinst, r)?;
            self.windows.push(hwnd);
            // SAFETY: `hwnd` is a top-level window this thread just created.
            if let Err(e) = unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) } {
                return Err(format!(
                    "SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE) failed ({e}); \
                     Windows 10 version 2004 or later is required"
                ));
            }
        }
        Ok(())
    }

    /// Shows every overlay without activating it.
    pub(super) fn show(&self) {
        for &hwnd in &self.windows {
            // SAFETY: shows our own window without activating it.
            let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
        }
        self.raise();
    }

    /// Hides every overlay (privacy mode ended; the windows go with the thread).
    pub(super) fn hide(&self) {
        for &hwnd in &self.windows {
            // SAFETY: hides our own window.
            let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
        }
    }

    /// Re-asserts `HWND_TOPMOST` (windows that became top-most later would otherwise cover
    /// the overlay).
    pub(super) fn raise(&self) {
        for &hwnd in &self.windows {
            // SAFETY: changes only the z-order of our own window; no move, no activation.
            let _ = unsafe {
                SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                )
            };
        }
    }

    /// New text: repaint.
    pub(super) fn set_text(&self, text: &PrivacyScreenText) {
        set_text(text);
        for &hwnd in &self.windows {
            // SAFETY: invalidates our own window's client area; WM_PAINT follows.
            let _ = unsafe { InvalidateRect(Some(hwnd), None, true) };
        }
    }

    /// Monitors were added, removed or resized: cover the new layout.
    pub(super) fn rebuild(&mut self) -> Result<(), String> {
        self.destroy();
        self.build()?;
        self.show();
        Ok(())
    }

    fn destroy(&mut self) {
        for hwnd in self.windows.drain(..) {
            // SAFETY: destroys a window this thread created.
            let _ = unsafe { DestroyWindow(hwnd) };
        }
    }
}

impl Drop for Overlays {
    fn drop(&mut self) {
        self.destroy();
    }
}

fn set_text(text: &PrivacyScreenText) {
    TEXT.with(|t| *t.borrow_mut() = text.sanitized());
}

fn register_class(hinst: Option<HINSTANCE>) -> Result<(), String> {
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wnd_proc),
        hInstance: hinst.unwrap_or_default(),
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    // SAFETY: `wc` is fully initialized, its class name is a static wide string and the window
    // procedure lives for the whole program.
    if unsafe { RegisterClassExW(&wc) } == 0 {
        // SAFETY: reads the calling thread's last-error value.
        let err = unsafe { GetLastError() };
        if err != ERROR_CLASS_ALREADY_EXISTS {
            return Err(format!("RegisterClassExW: {err:?}"));
        }
    }
    Ok(())
}

fn create_window(hinst: Option<HINSTANCE>, r: RECT) -> Result<HWND, String> {
    // SAFETY: the class was registered on this thread's process; plain value arguments.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
            CLASS_NAME,
            w!("TLP-Desk privacy screen"),
            WS_POPUP,
            r.left,
            r.top,
            r.right.saturating_sub(r.left),
            r.bottom.saturating_sub(r.top),
            None,
            None,
            hinst,
            None,
        )
    }
    .map_err(|e| format!("CreateWindowExW: {e}"))?;
    // Fully opaque; the layered style is what makes the window click-through.
    // SAFETY: `hwnd` is our own layered window.
    if let Err(e) = unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA) } {
        // SAFETY: destroys the window created above.
        let _ = unsafe { DestroyWindow(hwnd) };
        return Err(format!("SetLayeredWindowAttributes: {e}"));
    }
    Ok(hwnd)
}

/// Physical-pixel rectangles of all monitors (the shield thread is per-monitor DPI aware).
fn monitor_rects() -> Vec<RECT> {
    let mut rects: Vec<RECT> = Vec::new();
    // SAFETY: the callback only runs during this call and writes through the pointer to our
    // local `rects`, which outlives the call.
    let _ = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&mut rects as *mut Vec<RECT> as isize),
        )
    };
    rects.truncate(MAX_MONITORS);
    rects
}

unsafe extern "system" fn collect_monitor(
    _mon: HMONITOR,
    _hdc: HDC,
    rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    if rect.is_null() || data.0 == 0 {
        return BOOL(1);
    }
    // SAFETY: `data` is the `&mut Vec<RECT>` passed by `monitor_rects` for the duration of
    // EnumDisplayMonitors, and `rect` is a valid RECT supplied by the system.
    let (rects, r) = unsafe { (&mut *(data.0 as *mut Vec<RECT>), *rect) };
    if rects.len() < MAX_MONITORS {
        rects.push(r);
    }
    BOOL(1)
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // Belt and braces next to WS_EX_TRANSPARENT: the viewer's injected clicks go to the
        // windows underneath.
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // Everything is painted in WM_PAINT.
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            // SAFETY: posts a pointer-free message to this (the shield) thread's own queue.
            let _ = unsafe {
                PostThreadMessageW(
                    GetCurrentThreadId(),
                    WM_SHIELD_REBUILD,
                    WPARAM(0),
                    LPARAM(0),
                )
            };
            LRESULT(0)
        }
        // SAFETY: default processing with the arguments the system passed us.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: `hwnd` is our window receiving WM_PAINT; `ps` is a writable PAINTSTRUCT that
    // EndPaint gets back below.
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    if hdc.is_invalid() {
        return;
    }
    let mut rc = RECT::default();
    // SAFETY: `rc` is writable; `hwnd` is our window.
    let _ = unsafe { GetClientRect(hwnd, &mut rc) };
    // SAFETY: stock objects never need freeing; `hdc` and `rc` are valid.
    unsafe {
        let black = HBRUSH(GetStockObject(BLACK_BRUSH).0);
        FillRect(hdc, &rc, black);
    }
    // SAFETY: plain query on our own window.
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(48);
    TEXT.with(|t| draw_text(hdc, rc, dpi as f32 / 96.0, &t.borrow()));
    // SAFETY: matches the BeginPaint above.
    let _ = unsafe { EndPaint(hwnd, &ps) };
}

/// A GDI font, deleted on drop.
struct Font(HGDIOBJ);

impl Font {
    fn new(px: f32, weight: i32) -> Option<Self> {
        let height = -(px.round() as i32).max(8);
        // SAFETY: plain value arguments and a static face name; deleted in Drop.
        let f = unsafe {
            CreateFontW(
                height,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                w!("Segoe UI"),
            )
        };
        (!f.is_invalid()).then(|| Self(HGDIOBJ(f.0)))
    }
}

impl Drop for Font {
    fn drop(&mut self) {
        // SAFETY: the font is not selected into any DC any more (callers restore the old one).
        let _ = unsafe { DeleteObject(self.0) };
    }
}

/// Title and lines centered on the monitor. GDI shapes Arabic itself; `DT_RTLREADING` sets
/// the paragraph direction for the Arabic UI.
fn draw_text(hdc: HDC, rc: RECT, scale: f32, text: &PrivacyScreenText) {
    let (Some(title_font), Some(body_font)) = (
        Font::new(TITLE_PX * scale, FW_SEMIBOLD.0 as i32),
        Font::new(BODY_PX * scale, FW_NORMAL.0 as i32),
    ) else {
        return;
    };
    let mut fmt = DT_CENTER | DT_WORDBREAK | DT_NOPREFIX;
    if text.rtl {
        fmt |= DT_RTLREADING;
    }
    let width = rc.right.saturating_sub(rc.left);
    let margin = width / 8;
    let (left, right) = (
        rc.left.saturating_add(margin),
        rc.right.saturating_sub(margin),
    );
    let gap = (BODY_PX * scale * 0.6).round() as i32;

    let mut blocks: Vec<(Vec<u16>, &Font, COLORREF)> = Vec::new();
    if !text.title.is_empty() {
        blocks.push((
            text.title.encode_utf16().collect(),
            &title_font,
            TITLE_COLOR,
        ));
    }
    // Never hand DrawTextW an empty buffer (see `measure`).
    for line in text.lines.iter().filter(|l| !l.is_empty()) {
        blocks.push((line.encode_utf16().collect(), &body_font, BODY_COLOR));
    }
    // Measure every block at the final width, then center the whole stack vertically.
    let heights: Vec<i32> = blocks
        .iter_mut()
        .map(|(buf, font, _)| measure(hdc, buf, font, left, right, fmt))
        .collect();
    let total: i32 = heights.iter().sum::<i32>()
        + gap.saturating_mul(i32::try_from(heights.len().saturating_sub(1)).unwrap_or(0));
    let mut y = rc.top + (rc.bottom.saturating_sub(rc.top).saturating_sub(total)) / 2;
    // SAFETY: `hdc` is the paint DC of our window.
    unsafe {
        SetBkMode(hdc, TRANSPARENT);
    }
    for ((buf, font, color), h) in blocks.iter_mut().zip(heights) {
        let mut r = RECT {
            left,
            top: y,
            right,
            bottom: y.saturating_add(h),
        };
        // SAFETY: `font` is live; the previous font is restored before it is dropped; `buf`
        // and `r` are writable locals.
        unsafe {
            let old = SelectObject(hdc, font.0);
            SetTextColor(hdc, *color);
            DrawTextW(hdc, buf, &mut r, fmt);
            SelectObject(hdc, old);
        }
        y = y.saturating_add(h).saturating_add(gap);
    }
}

fn measure(
    hdc: HDC,
    buf: &mut [u16],
    font: &Font,
    left: i32,
    right: i32,
    fmt: DRAW_TEXT_FORMAT,
) -> i32 {
    if buf.is_empty() {
        // An empty slice has a dangling pointer, which DrawTextW dereferences.
        return 0;
    }
    let mut r = RECT {
        left,
        top: 0,
        right,
        bottom: 0,
    };
    // SAFETY: as in `draw_text`; DT_CALCRECT only measures.
    unsafe {
        let old = SelectObject(hdc, font.0);
        DrawTextW(hdc, buf, &mut r, fmt | DT_CALCRECT);
        SelectObject(hdc, old);
    }
    r.bottom.saturating_sub(r.top).max(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Gdi::{CreateCompatibleDC, DeleteDC};

    /// Measuring needs only a memory DC: no window is shown.
    #[test]
    fn arabic_and_english_text_measure_and_wrap() {
        // SAFETY: a memory DC compatible with the screen, deleted below.
        let dc = unsafe { CreateCompatibleDC(None) };
        assert!(!dc.is_invalid());
        let font = Font::new(BODY_PX * 1.5, FW_NORMAL.0 as i32).expect("font");
        let fmt = DT_CENTER | DT_WORDBREAK | DT_NOPREFIX | DT_RTLREADING;
        let mut ar: Vec<u16> = "هذا الجهاز قيد الاستخدام عن بُعد".encode_utf16().collect();
        let one_line = measure(dc, &mut ar, &font, 0, 4000, fmt);
        assert!(one_line > 10, "{one_line}");
        // Narrow column: the same sentence wraps onto more lines.
        let wrapped = measure(dc, &mut ar, &font, 0, 120, fmt);
        assert!(wrapped > one_line, "{wrapped} vs {one_line}");
        let mut empty: Vec<u16> = Vec::new();
        // DrawTextW must never see an empty buffer (it reads through the dangling pointer).
        assert_eq!(measure(dc, &mut empty, &font, 0, 500, fmt), 0);
        drop(font);
        // SAFETY: deletes the DC created above; no objects of ours are selected into it.
        let _ = unsafe { DeleteDC(dc) };
    }
}
