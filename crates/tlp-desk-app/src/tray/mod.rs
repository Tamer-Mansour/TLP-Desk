//! Notification-area (tray) icon and tray mode of the main window.
//!
//! * [`model`]: menu entries, Arabic/English labels and the tooltip (pure, tested).
//! * [`shell`]: where the main window is (shown / in the tray) and what close, open, quit and
//!   incoming requests do (pure, tested).
//! * [`system`]: the real icon (`tray-icon`), created only by the running GUI, never in tests.

pub mod glue;
pub mod model;
pub mod shell;
#[cfg(windows)]
pub mod system;

pub use glue::TrayState;
pub use model::{TrayAction, TrayEntry};
pub use shell::{Shell, WindowCmd, WindowMode};
#[cfg(windows)]
pub use system::SystemTray;
