//! macOS screen capture over Quartz Display Services.
//!
//! Frames come from `CGDisplayCreateImage` (polled at up to 60 fps) and are copied out as BGRA.
//! Apple marks that call as superseded by ScreenCaptureKit; it still works on current macOS and
//! needs no Objective-C block/delegate plumbing, which makes it the smallest correct first
//! implementation. Swapping in `SCStream` later only changes this module (the
//! [`ScreenCapturer`] trait stays). See `docs/src/dev/platforms.md`.
//!
//! The Screen Recording permission is required: without it macOS returns the desktop wallpaper
//! only, so [`open_capturer`] fails early with a message pointing at the setting.
//!
//! `MonitorInfo` is reported in display **points** (what pointer injection uses);
//! [`ScreenCapturer::frame_size`] reports the real pixel size (2x on Retina).

mod logic;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod platform;

#[cfg(target_os = "macos")]
pub(crate) use platform::{list_monitors, open_capturer};
