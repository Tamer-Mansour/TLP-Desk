//! Apple API calls of the macOS capturer.

use std::time::{Duration, Instant};

use objc2_core_foundation::CFRetained;
#[allow(deprecated)]
use objc2_core_graphics::{
    CGDataProvider, CGDirectDisplayID, CGDisplayBounds, CGDisplayCreateImage, CGEvent, CGImage,
    CGMainDisplayID,
};

use super::logic::{arrow_cursor, order_displays, points_to_pixels, resample_nearest, RawDisplay};
use crate::{
    fit_within, BgraFrame, CaptureError, CaptureOptions, CursorState, MonitorInfo, ScreenCapturer,
};

/// Upper bound on displays we enumerate.
const MAX_DISPLAYS: u32 = 16;
/// Frame pacing: at most ~60 captures per second.
const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);

// `CGPreflightScreenCaptureAccess` is not wrapped by the safe crates.
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
}

fn raw_displays() -> Result<(Vec<RawDisplay>, u32), CaptureError> {
    let mut ids = [0 as CGDirectDisplayID; MAX_DISPLAYS as usize];
    let mut count = 0u32;
    // SAFETY: `ids` has room for MAX_DISPLAYS entries and `count` is a valid out pointer.
    let err = unsafe {
        objc2_core_graphics::CGGetActiveDisplayList(MAX_DISPLAYS, ids.as_mut_ptr(), &mut count)
    };
    if err != objc2_core_graphics::CGError::Success {
        return Err(CaptureError::Platform(format!(
            "CGGetActiveDisplayList failed: {err:?}"
        )));
    }
    let n = (count as usize).min(ids.len());
    let displays = ids
        .iter()
        .take(n)
        .map(|&id| {
            let b = CGDisplayBounds(id);
            RawDisplay {
                id,
                x: b.origin.x.round() as i32,
                y: b.origin.y.round() as i32,
                width: b.size.width.max(0.0).round() as u32,
                height: b.size.height.max(0.0).round() as u32,
            }
        })
        .collect();
    Ok((displays, CGMainDisplayID()))
}

pub(crate) fn list_monitors() -> Result<Vec<MonitorInfo>, CaptureError> {
    let (displays, main) = raw_displays()?;
    Ok(order_displays(displays, main)
        .into_iter()
        .map(|(_, m)| m)
        .collect())
}

pub(crate) fn open_capturer(
    monitor_index: u32,
    options: CaptureOptions,
) -> Result<Box<dyn ScreenCapturer>, CaptureError> {
    // SAFETY: no arguments, returns a bool.
    if !unsafe { CGPreflightScreenCaptureAccess() } {
        return Err(CaptureError::Platform(
            "Screen Recording permission is required (System Settings > Privacy & Security > \
             Screen Recording)"
                .into(),
        ));
    }
    let (displays, main) = raw_displays()?;
    let ordered = order_displays(displays, main);
    let (id, monitor) = ordered
        .into_iter()
        .nth(monitor_index as usize)
        .ok_or(CaptureError::NoSuchMonitor(monitor_index))?;
    Ok(Box::new(MacCapturer {
        id,
        monitor,
        max_size: options.max_size,
        started: Instant::now(),
        next_due: Instant::now(),
        frame_px: (0, 0),
        cursor_sent: false,
    }))
}

struct MacCapturer {
    id: CGDirectDisplayID,
    monitor: MonitorInfo,
    max_size: Option<(u32, u32)>,
    started: Instant,
    next_due: Instant,
    /// Pixel size of the last captured image (for point to pixel cursor mapping).
    frame_px: (u32, u32),
    cursor_sent: bool,
}

// SAFETY: the capturer holds only plain data (a display id and sizes); CoreGraphics images are
// created, read and dropped inside one `next_frame` call on the calling thread.
unsafe impl Send for MacCapturer {}

impl MacCapturer {
    fn grab(&mut self) -> Result<BgraFrame, CaptureError> {
        #[allow(deprecated)]
        let image: CFRetained<CGImage> = CGDisplayCreateImage(self.id).ok_or_else(|| {
            CaptureError::Platform("CGDisplayCreateImage returned no image".into())
        })?;
        let (w, h) = (
            CGImage::width(Some(&image)) as u32,
            CGImage::height(Some(&image)) as u32,
        );
        let stride = CGImage::bytes_per_row(Some(&image)) as u32;
        if CGImage::bits_per_pixel(Some(&image)) != 32 {
            return Err(CaptureError::Platform(
                "unexpected display pixel format (not 32 bits per pixel)".into(),
            ));
        }
        let provider = CGImage::data_provider(Some(&image))
            .ok_or_else(|| CaptureError::Platform("display image has no pixel data".into()))?;
        let data = CGDataProvider::data(Some(&provider))
            .ok_or_else(|| CaptureError::Platform("cannot read display pixels".into()))?
            .to_vec();
        self.frame_px = (w, h);
        let timestamp_us = u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let (ow, oh) = self.frame_size_for(w, h);
        if (ow, oh) == (w, h) {
            return Ok(BgraFrame {
                width: w,
                height: h,
                stride,
                data,
                timestamp_us,
            });
        }
        let data = resample_nearest(&data, w, h, stride, ow, oh)
            .ok_or_else(|| CaptureError::Platform("inconsistent display image geometry".into()))?;
        Ok(BgraFrame {
            width: ow,
            height: oh,
            stride: ow * 4,
            data,
            timestamp_us,
        })
    }

    fn frame_size_for(&self, w: u32, h: u32) -> (u32, u32) {
        match self.max_size {
            Some(max) => fit_within((w, h), max),
            None => (w, h),
        }
    }
}

impl ScreenCapturer for MacCapturer {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<BgraFrame>, CaptureError> {
        let now = Instant::now();
        let wait = self.next_due.saturating_duration_since(now);
        if wait > timeout {
            std::thread::sleep(timeout);
            return Ok(None);
        }
        std::thread::sleep(wait);
        self.next_due = Instant::now() + FRAME_INTERVAL;
        self.grab().map(Some)
    }

    fn cursor(&mut self) -> Result<CursorState, CaptureError> {
        let loc = CGEvent::location(CGEvent::new(None).as_deref());
        let (fw, fh) = if self.frame_px == (0, 0) {
            (self.monitor.width, self.monitor.height)
        } else {
            self.frame_px
        };
        let rel_x = loc.x - f64::from(self.monitor.x);
        let rel_y = loc.y - f64::from(self.monitor.y);
        let visible = rel_x >= 0.0
            && rel_y >= 0.0
            && rel_x < f64::from(self.monitor.width)
            && rel_y < f64::from(self.monitor.height);
        let shape = if self.cursor_sent {
            None
        } else {
            self.cursor_sent = true;
            Some(arrow_cursor())
        };
        Ok(CursorState {
            x: points_to_pixels(rel_x, self.monitor.width, fw),
            y: points_to_pixels(rel_y, self.monitor.height, fh),
            visible,
            shape,
        })
    }

    fn monitor(&self) -> &MonitorInfo {
        &self.monitor
    }

    fn frame_size(&self) -> (u32, u32) {
        let (w, h) = if self.frame_px == (0, 0) {
            (self.monitor.width, self.monitor.height)
        } else {
            self.frame_px
        };
        self.frame_size_for(w, h)
    }
}
