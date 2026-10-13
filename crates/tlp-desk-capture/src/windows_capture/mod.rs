//! Windows capture backends (Windows.Graphics.Capture + DXGI Desktop Duplication).
//!
//! References: Microsoft Learn — "Screen capture" (Windows.Graphics.Capture),
//! "Desktop Duplication API", `GetCursorInfo`, `GetIconInfo`, `GetDIBits`.

mod cursor;
mod d3d;
mod dxgi;
mod monitors;
mod scaler;
mod wgc;

use crate::{CaptureBackend, CaptureError, CaptureOptions, ScreenCapturer};

pub(crate) use monitors::list_monitors;

fn platform_err(context: &str, e: windows::core::Error) -> CaptureError {
    CaptureError::Platform(format!("{context}: {e}"))
}

pub(crate) fn open_capturer(
    monitor_index: u32,
    options: CaptureOptions,
) -> Result<Box<dyn ScreenCapturer>, CaptureError> {
    let target = monitors::find_monitor(monitor_index)?;
    let max = options.max_size;
    match options.backend {
        Some(CaptureBackend::WindowsGraphicsCapture) => {
            Ok(Box::new(wgc::WgcCapturer::new(target, max)?))
        }
        Some(CaptureBackend::DxgiDuplication) => {
            Ok(Box::new(dxgi::DxgiCapturer::new(target, max)?))
        }
        None => match wgc::WgcCapturer::new(target.clone(), max) {
            Ok(c) => Ok(Box::new(c)),
            Err(wgc_err) => match dxgi::DxgiCapturer::new(target, max) {
                Ok(c) => Ok(Box::new(c)),
                Err(dxgi_err) => Err(CaptureError::Platform(format!(
                    "Windows.Graphics.Capture failed ({wgc_err}); DXGI duplication failed ({dxgi_err})"
                ))),
            },
        },
    }
}
