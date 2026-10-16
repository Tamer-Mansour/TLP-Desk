//! macOS input: scancode tables (always compiled and tested) and the `CGEvent` injector
//! (macOS only).
//!
//! * [`keymap`]: wire scancode to macOS virtual key code and modifier flags.
//! * [`pointer`]: click counting and wheel conversion.
//! * `injector`: [`MacInjector`], an [`crate::InputInjector`] over Quartz Event Services.
//!
//! Windows-only features have documented stand-ins on macOS: the privacy shield, the remote
//! pointer marker, the whiteboard overlay, the viewer keyboard grab and the local-activity monitor
//! return [`crate::InputError::Unsupported`] (see `stub.rs`); the host runs without them and
//! simply does not offer those permissions.

pub mod keymap;
pub mod pointer;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod injector;

#[cfg(target_os = "macos")]
pub use injector::MacInjector;
