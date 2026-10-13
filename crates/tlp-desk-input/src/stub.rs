//! Non-Windows placeholders so the workspace builds everywhere.

use std::sync::mpsc::Receiver;
use std::time::Instant;

use crate::keys::ScanKey;
use crate::{InputError, InputEvent, MonitorRect};

/// No-op off Windows.
pub fn ensure_dpi_aware() -> Result<(), InputError> {
    Ok(())
}

pub fn virtual_desktop_rect() -> Option<MonitorRect> {
    None
}

pub fn vk_to_scancode(_vk: u16) -> Option<ScanKey> {
    None
}

/// Keyboard grab placeholder; [`KeyboardGrab::start`] returns [`InputError::Unsupported`].
pub struct KeyboardGrab {
    _private: (),
}

impl KeyboardGrab {
    pub fn start() -> Result<(KeyboardGrab, Receiver<InputEvent>), InputError> {
        Err(InputError::Unsupported)
    }

    pub fn set_active(&self, _active: bool) {}

    pub fn set_active_for_window(&self, _hwnd: isize) {}

    pub fn is_active(&self) -> bool {
        false
    }
}

/// Local activity monitor placeholder; [`LocalActivityMonitor::start`] is unsupported.
#[derive(Debug)]
pub struct LocalActivityMonitor {
    _private: (),
}

impl LocalActivityMonitor {
    pub fn start() -> Result<Self, InputError> {
        Err(InputError::Unsupported)
    }
}

impl crate::LocalActivity for LocalActivityMonitor {
    fn last_physical_input(&self) -> Option<Instant> {
        None
    }
}

/// Remote pointer marker placeholder; [`RemotePointerMarker::start`] is unsupported.
#[derive(Debug)]
pub struct RemotePointerMarker {
    _private: (),
}

impl RemotePointerMarker {
    pub fn start(_label: &str) -> Result<Self, InputError> {
        Err(InputError::Unsupported)
    }

    pub fn start_with(_label: &str, _exclude_from_capture: bool) -> Result<Self, InputError> {
        Err(InputError::Unsupported)
    }

    pub fn set_visible(&self, _visible: bool) {}

    pub fn is_visible(&self) -> bool {
        false
    }

    pub fn excluded_from_capture(&self) -> bool {
        false
    }
}
