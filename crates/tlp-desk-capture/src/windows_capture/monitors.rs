//! Monitor enumeration (`EnumDisplayMonitors` + `GetMonitorInfoW`) in physical pixels.

use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;

use crate::{CaptureError, MonitorInfo};

/// Makes the current thread per-monitor DPI aware for its lifetime so that monitor rectangles and
/// cursor positions are reported in physical pixels, then restores the previous context.
pub(super) struct PhysicalDpiScope(DPI_AWARENESS_CONTEXT);

impl PhysicalDpiScope {
    pub(super) fn enter() -> Self {
        // SAFETY: SetThreadDpiAwarenessContext only changes the calling thread's DPI mode; the
        // constant is a valid predefined context. It returns the previous context (or null on
        // failure, which we then simply do not restore).
        let prev =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        Self(prev)
    }
}

impl Drop for PhysicalDpiScope {
    fn drop(&mut self) {
        if !self.0 .0.is_null() {
            // SAFETY: restores the context value previously returned by the OS for this thread.
            unsafe {
                SetThreadDpiAwarenessContext(self.0);
            }
        }
    }
}

/// A monitor resolved to its OS handle. Not `Send` (holds an HMONITOR); only used while opening.
#[derive(Clone)]
pub(super) struct Target {
    pub info: MonitorInfo,
    pub hmonitor: HMONITOR,
}

unsafe extern "system" fn collect_monitor(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: `data` is the address of the `Vec<HMONITOR>` passed by `enumerate`, which outlives
    // the synchronous EnumDisplayMonitors call and is not aliased elsewhere during it.
    let list = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
    list.push(hmonitor);
    BOOL(1)
}

fn enumerate() -> Result<Vec<Target>, CaptureError> {
    let _dpi = PhysicalDpiScope::enter();
    let mut handles: Vec<HMONITOR> = Vec::new();
    // SAFETY: the callback matches MONITORENUMPROC and only touches `handles` through the LPARAM,
    // which stays valid for the duration of this synchronous call.
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&mut handles as *mut Vec<HMONITOR> as isize),
        )
    };
    if !ok.as_bool() {
        return Err(CaptureError::Platform("EnumDisplayMonitors failed".into()));
    }

    let mut targets = Vec::with_capacity(handles.len());
    for hmonitor in handles {
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: `mi` is a properly sized MONITORINFOEXW with cbSize set, which
        // GetMonitorInfoW accepts through a MONITORINFO pointer.
        let ok = unsafe {
            GetMonitorInfoW(hmonitor, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO)
        };
        if !ok.as_bool() {
            continue;
        }
        let r = mi.monitorInfo.rcMonitor;
        let name_len = mi
            .szDevice
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(mi.szDevice.len());
        targets.push(Target {
            info: MonitorInfo {
                index: 0,
                name: String::from_utf16_lossy(&mi.szDevice[..name_len]),
                width: (r.right - r.left).max(0) as u32,
                height: (r.bottom - r.top).max(0) as u32,
                primary: mi.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                x: r.left,
                y: r.top,
            },
            hmonitor,
        });
    }
    // Primary first, then left-to-right, top-to-bottom: stable indices across calls.
    targets.sort_by_key(|t| (!t.info.primary, t.info.x, t.info.y));
    for (i, t) in targets.iter_mut().enumerate() {
        t.info.index = i as u32;
    }
    Ok(targets)
}

pub(crate) fn list_monitors() -> Result<Vec<MonitorInfo>, CaptureError> {
    Ok(enumerate()?.into_iter().map(|t| t.info).collect())
}

pub(super) fn find_monitor(index: u32) -> Result<Target, CaptureError> {
    enumerate()?
        .into_iter()
        .nth(index as usize)
        .ok_or(CaptureError::NoSuchMonitor(index))
}
