//! Input injection (host side) and keyboard capture (viewer side).
//!
//! # Host
//! [`WindowsInjector`] (or the portable [`injector_for_monitor`]) turns wire
//! [`InputEvent`]s into `SendInput` calls on the captured monitor. Keys are injected by
//! hardware scancode, so the host's own keyboard layout (Arabic, English, ...) decides which
//! character a key produces: the viewer never needs to know the host layout.
//!
//! The host process should be per-monitor DPI aware (call [`ensure_dpi_aware`] once at
//! startup, before creating windows). The injector also switches its own thread to
//! per-monitor-v2 awareness around every call, so pointer mapping stays correct even if the
//! process awareness could not be set.
//!
//! # Local priority and the remote pointer marker (host)
//! [`LocalActivityMonitor`] timestamps the host user's own physical mouse/keyboard input
//! (low-level hooks, injected events ignored) so the host can give the local user priority.
//! [`RemotePointerMarker`] shows a green arrow at the pointer on the host screen while the
//! remote viewer is moving it; it is excluded from screen capture.
//!
//! # Privacy mode (host)
//! [`privacy`]: a [`PrivacyShield`] blanks the host's physical screens (overlay excluded from
//! capture) and swallows physical keyboard/mouse input while the viewer's injected input
//! passes, with a local emergency exit. See the module docs for the safety rules.
//!
//! # Viewer
//! [`ViewRect`] maps a pointer position inside the displayed (scaled, letterboxed) video to
//! the normalized wire coordinates. [`KeyboardGrab`] captures system keys (Win, Alt+Tab,
//! Alt+Esc, Ctrl+Esc, ...) while the session window is focused.
#![deny(unsafe_code)]

pub use tlp_desk_proto::{InputEvent, MouseButton};

mod activity;
mod grab_logic;
mod keys;
mod mapping;
mod marker_shape;
pub mod privacy;
mod tracker;

#[cfg(windows)]
#[allow(unsafe_code)]
mod win;

#[cfg(not(windows))]
mod stub;

pub use activity::{ActivityStamp, LocalActivity};
pub use keys::{normalize_scancode, ScanKey};
pub use mapping::{normalize_pixel, pixel_to_virtual_absolute, MonitorRect, ViewRect};
pub use marker_shape::{MarkerImage, TextMask, MARKER_DARK, MARKER_GREEN};
pub use privacy::{
    local_input_blocked, start_privacy_shield, PrivacyScreenText, PrivacyShield, ShieldSignal,
    ShieldSignalFn,
};

#[cfg(windows)]
pub use win::{
    ensure_dpi_aware, virtual_desktop_rect, vk_to_scancode, KeyboardGrab, LocalActivityMonitor,
    RemotePointerMarker, WindowsInjector,
};

#[cfg(not(windows))]
pub use stub::{
    ensure_dpi_aware, virtual_desktop_rect, vk_to_scancode, KeyboardGrab, LocalActivityMonitor,
    RemotePointerMarker,
};

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("input injection is not supported on this platform")]
    Unsupported,
    /// Target desktop is not accessible (secure desktop, UIPI); needs the service agent.
    #[error("input blocked by the OS: {0}")]
    Blocked(String),
    #[error("platform error: {0}")]
    Platform(String),
}

/// Injects viewer input into the host's active desktop.
pub trait InputInjector: Send {
    /// Applies one event. Pointer coordinates are normalized to `0..=u16::MAX` over the
    /// captured monitor; the implementation maps them to physical desktop pixels.
    fn inject(&mut self, event: &InputEvent) -> Result<(), InputError>;

    /// Releases every key and button this injector pressed.
    fn release_all(&mut self) -> Result<(), InputError> {
        self.inject(&InputEvent::ReleaseAll)
    }

    /// Retargets pointer mapping to another monitor (e.g. the viewer switched screens),
    /// keeping held keys/buttons. Returns `false` if unsupported: the caller then creates a
    /// new injector for the new monitor.
    fn set_monitor(&mut self, monitor: MonitorRect) -> bool {
        let _ = monitor;
        false
    }
}

/// Shows/hides the "remote pointer" marker on the host screen.
pub trait PointerMarker: Send {
    fn set_visible(&mut self, visible: bool);

    /// Whether the marker is kept out of screen capture (so the viewer never sees it).
    fn excluded_from_capture(&self) -> bool {
        true
    }
}

impl PointerMarker for RemotePointerMarker {
    fn set_visible(&mut self, visible: bool) {
        RemotePointerMarker::set_visible(self, visible);
    }

    fn excluded_from_capture(&self) -> bool {
        RemotePointerMarker::excluded_from_capture(self)
    }
}

/// Starts the platform [`LocalActivityMonitor`] ([`InputError::Unsupported`] elsewhere).
pub fn start_local_activity_monitor() -> Result<std::sync::Arc<dyn LocalActivity>, InputError> {
    Ok(std::sync::Arc::new(LocalActivityMonitor::start()?))
}

/// Creates the platform [`RemotePointerMarker`] ([`InputError::Unsupported`] elsewhere).
pub fn start_pointer_marker(label: &str) -> Result<Box<dyn PointerMarker>, InputError> {
    Ok(Box::new(RemotePointerMarker::start(label)?))
}

/// Creates the platform injector targeting `monitor` (physical virtual-desktop pixels).
///
/// Returns [`InputError::Unsupported`] on platforms without an implementation.
pub fn injector_for_monitor(monitor: MonitorRect) -> Result<Box<dyn InputInjector>, InputError> {
    #[cfg(windows)]
    {
        Ok(Box::new(WindowsInjector::new(monitor)?))
    }
    #[cfg(not(windows))]
    {
        let _ = monitor;
        Err(InputError::Unsupported)
    }
}

/// Maps a normalized coordinate (`0..=u16::MAX`) to a pixel in `0..extent`.
pub fn denormalize(v: u16, extent: u32) -> u32 {
    if extent == 0 {
        return 0;
    }
    ((u64::from(v) * u64::from(extent - 1)) / u64::from(u16::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::denormalize;

    #[test]
    fn denormalize_hits_edges() {
        assert_eq!(denormalize(0, 1920), 0);
        assert_eq!(denormalize(u16::MAX, 1920), 1919);
        assert_eq!(denormalize(u16::MAX / 2, 1081), 539);
        assert_eq!(denormalize(123, 0), 0);
    }
}
