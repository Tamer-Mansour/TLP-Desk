//! Cursor position and shape via `GetCursorInfo` / `GetIconInfo` / `GetDIBits`.

use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorInfo, GetIconInfo, CURSORINFO, CURSOR_SHOWING, HICON, ICONINFO,
};

use super::monitors::PhysicalDpiScope;
use super::platform_err;
use crate::{CaptureError, CursorShape, CursorState, MonitorInfo};

/// Tracks the cursor relative to one monitor and reports the shape only when it changes.
pub(super) struct CursorTracker {
    origin: (i32, i32),
    size: (u32, u32),
    /// Raw HCURSOR of the last reported shape (0 = none yet).
    last_handle: isize,
}

impl CursorTracker {
    pub(super) fn new(monitor: &MonitorInfo) -> Self {
        Self {
            origin: (monitor.x, monitor.y),
            size: (monitor.width, monitor.height),
            last_handle: 0,
        }
    }

    pub(super) fn poll(&mut self) -> Result<CursorState, CaptureError> {
        let _dpi = PhysicalDpiScope::enter();
        let mut ci = CURSORINFO {
            cbSize: std::mem::size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: `ci` is a CURSORINFO with cbSize set, as the API requires.
        unsafe { GetCursorInfo(&mut ci) }.map_err(|e| platform_err("GetCursorInfo", e))?;

        let x = ci.ptScreenPos.x - self.origin.0;
        let y = ci.ptScreenPos.y - self.origin.1;
        let showing = ci.flags.0 & CURSOR_SHOWING.0 != 0 && !ci.hCursor.0.is_null();
        let inside = x >= 0 && y >= 0 && (x as u32) < self.size.0 && (y as u32) < self.size.1;

        let mut shape = None;
        let handle = ci.hCursor.0 as isize;
        if showing && handle != self.last_handle {
            // A failed shape read is not fatal: keep the old shape and retry next poll.
            if let Some(s) = read_cursor_shape(HICON(ci.hCursor.0)) {
                shape = Some(s);
                self.last_handle = handle;
            }
        }
        Ok(CursorState {
            x,
            y,
            visible: showing && inside,
            shape,
        })
    }
}

/// Deletes the bitmaps that `GetIconInfo` hands to the caller.
struct IconBitmaps(ICONINFO);

impl Drop for IconBitmaps {
    fn drop(&mut self) {
        for bm in [self.0.hbmColor, self.0.hbmMask] {
            if !bm.0.is_null() {
                // SAFETY: GetIconInfo transfers ownership of these bitmaps to the caller, and
                // each is deleted exactly once here.
                unsafe {
                    let _ = DeleteObject(bm.into());
                }
            }
        }
    }
}

struct ScreenDc(HDC);

impl Drop for ScreenDc {
    fn drop(&mut self) {
        // SAFETY: releases the screen DC obtained with GetDC(None) in `read_cursor_shape`.
        unsafe {
            ReleaseDC(None, self.0);
        }
    }
}

/// Reads a bitmap as top-down 32-bit BGRA. Monochrome bitmaps come back as 0x000000 / 0xFFFFFF.
fn read_bitmap(dc: HDC, bm: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
    let mut info = BITMAP::default();
    // SAFETY: `info` is a BITMAP-sized buffer and `bm` is a live bitmap handle.
    let n = unsafe {
        GetObjectW(
            bm.into(),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut info as *mut BITMAP as *mut _),
        )
    };
    if n == 0 || info.bmWidth <= 0 || info.bmHeight <= 0 {
        return None;
    }
    let (w, h) = (info.bmWidth as u32, info.bmHeight as u32);
    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32), // negative = top-down rows
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut buf = vec![0u8; w as usize * h as usize * 4];
    // SAFETY: `buf` holds exactly w*h 32-bit pixels as described by `bmi`; `dc` is a valid
    // screen DC and `bm` is not selected into any DC.
    let lines = unsafe {
        GetDIBits(
            dc,
            bm,
            0,
            h,
            Some(buf.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        )
    };
    if lines as u32 != h {
        return None;
    }
    Some((w, h, buf))
}

fn read_cursor_shape(cursor: HICON) -> Option<CursorShape> {
    let mut ii = ICONINFO::default();
    // SAFETY: `cursor` is the live HCURSOR from GetCursorInfo; `ii` is a valid out-struct.
    unsafe { GetIconInfo(cursor, &mut ii) }.ok()?;
    let bitmaps = IconBitmaps(ii);
    // SAFETY: GetDC(None) returns the screen DC; released by `ScreenDc`'s Drop.
    let dc = ScreenDc(unsafe { GetDC(None) });
    if dc.0.is_invalid() {
        return None;
    }

    let info = &bitmaps.0;
    let (width, height, rgba) = if !info.hbmColor.0.is_null() {
        let (w, h, mut px) = read_bitmap(dc.0, info.hbmColor)?;
        let has_alpha = px.chunks_exact(4).any(|p| p[3] != 0);
        let mask = if has_alpha {
            None
        } else {
            read_bitmap(dc.0, info.hbmMask).filter(|(mw, mh, _)| *mw == w && *mh >= h)
        };
        for (i, p) in px.chunks_exact_mut(4).enumerate() {
            p.swap(0, 2); // BGRA -> RGBA
            if !has_alpha {
                // Old-style colour cursor: AND mask white = transparent.
                let transparent = mask.as_ref().is_some_and(|(_, _, m)| m[i * 4] != 0);
                p[3] = if transparent { 0 } else { 255 };
            }
        }
        (w, h, px)
    } else {
        // Monochrome cursor: mask is double height, AND plane on top, XOR plane below.
        let (w, h2, m) = read_bitmap(dc.0, info.hbmMask)?;
        let h = h2 / 2;
        let plane = w as usize * h as usize * 4;
        let mut px = vec![0u8; plane];
        for i in 0..(w as usize * h as usize) {
            let and = m[i * 4] != 0;
            let xor = m[plane + i * 4] != 0;
            let out = &mut px[i * 4..i * 4 + 4];
            match (and, xor) {
                (false, false) => out.copy_from_slice(&[0, 0, 0, 255]),
                (false, true) => out.copy_from_slice(&[255, 255, 255, 255]),
                (true, false) => out.copy_from_slice(&[0, 0, 0, 0]),
                // "Invert screen" pixels cannot be expressed in RGBA; draw them black.
                (true, true) => out.copy_from_slice(&[0, 0, 0, 255]),
            }
        }
        (w, h, px)
    };

    Some(CursorShape {
        width,
        height,
        hotspot_x: info.xHotspot.min(width.saturating_sub(1)),
        hotspot_y: info.yHotspot.min(height.saturating_sub(1)),
        rgba,
    })
}
