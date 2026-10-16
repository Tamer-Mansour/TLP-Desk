//! Win32 helpers for the popup window: work area, rounded corners, coming to the foreground
//! and the notification sound. Every function is best effort and never fails loudly: the
//! popup is always-on-top anyway, these only make it nicer and more noticeable.
//!
//! Windows only lets a process take the foreground when it has a reason to (it received the
//! last input, or the foreground process allowed it). So: the GUI that launches the popup
//! calls [`allow_foreground`] for the child first; the popup itself attaches its input
//! queue to the current foreground thread for the duration of `SetForegroundWindow` (the
//! documented `AttachThreadInput` behaviour), and if all of that is refused it flashes its
//! taskbar button instead.
#![allow(unsafe_code)]

use std::ffi::c_void;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, BringWindowToTop, FlashWindowEx, GetForegroundWindow,
    GetWindowThreadProcessId, SetForegroundWindow, SystemParametersInfoW, FLASHWINFO, FLASHW_ALL,
    FLASHW_TIMERNOFG, MB_ICONINFORMATION, SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

fn hwnd(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}

/// The primary monitor's work area (screen minus taskbar) in logical points, as
/// `[left, top, right, bottom]`, using the system DPI.
pub fn work_area_points() -> Option<[f32; 4]> {
    let mut rect = RECT::default();
    // SAFETY: SPI_GETWORKAREA writes one RECT into the buffer we pass; `rect` lives for the
    // whole call and has exactly that type.
    unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(std::ptr::from_mut(&mut rect).cast::<c_void>()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .ok()?;
    // SAFETY: no arguments; returns the DPI of the primary monitor at sign-in.
    let dpi = unsafe { GetDpiForSystem() };
    let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
    Some([
        rect.left as f32 / scale,
        rect.top as f32 / scale,
        rect.right as f32 / scale,
        rect.bottom as f32 / scale,
    ])
}

/// Ask DWM for rounded corners (Windows 11; ignored on Windows 10).
pub fn round_corners(window: isize) {
    let pref = DWMWCP_ROUND;
    // SAFETY: the attribute expects a DWM_WINDOW_CORNER_PREFERENCE; we pass a pointer to one
    // that outlives the call and its exact size. An invalid window only makes the call fail.
    let r = unsafe {
        DwmSetWindowAttribute(
            hwnd(window),
            DWMWA_WINDOW_CORNER_PREFERENCE,
            std::ptr::from_ref(&pref).cast::<c_void>(),
            std::mem::size_of_val(&pref) as u32,
        )
    };
    if let Err(e) = r {
        tracing::debug!(%e, "rounded corners not available");
    }
}

/// Bring `window` to the foreground; flash its taskbar button if Windows refuses.
/// Returns whether it is now the foreground window.
pub fn bring_to_front(window: isize) -> bool {
    let target = hwnd(window);
    // SAFETY: plain Win32 calls on window handles; a stale handle makes them fail, nothing
    // more. AttachThreadInput is undone before returning, whatever happened in between.
    unsafe {
        let fg = GetForegroundWindow();
        if fg == target {
            return true;
        }
        let me = GetCurrentThreadId();
        let other = if fg.0.is_null() {
            0
        } else {
            GetWindowThreadProcessId(fg, None)
        };
        let attached = other != 0 && other != me && AttachThreadInput(me, other, true).as_bool();
        let _ = BringWindowToTop(target);
        let ok = SetForegroundWindow(target).as_bool();
        if attached {
            let _ = AttachThreadInput(me, other, false);
        }
        if ok && GetForegroundWindow() == target {
            return true;
        }
        let info = FLASHWINFO {
            cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
            hwnd: target,
            dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
            uCount: 0,
            dwTimeout: 0,
        };
        let _ = FlashWindowEx(&info);
        false
    }
}

/// Let process `pid` (the popup we just started) take the foreground. Only works while this
/// process may set the foreground itself; harmless otherwise.
pub fn allow_foreground(pid: u32) {
    // SAFETY: no pointers involved; fails harmlessly when we lack the right.
    if let Err(e) = unsafe { AllowSetForegroundWindow(pid) } {
        tracing::debug!(%e, "AllowSetForegroundWindow refused");
    }
}

/// The user's "Asterisk / information" system sound (respects the Windows sound scheme and
/// mute; plays asynchronously).
pub fn notification_sound() {
    // SAFETY: no pointers involved.
    if let Err(e) = unsafe { MessageBeep(MB_ICONINFORMATION) } {
        tracing::debug!(%e, "MessageBeep failed");
    }
}
