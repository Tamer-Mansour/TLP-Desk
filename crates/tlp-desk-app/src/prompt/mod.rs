//! The on-screen Accept window: a small, separate `TLP-Desk.exe --accept-prompt` process that
//! shows one incoming request on top of everything, so the user can answer it even while
//! the main window is minimized, hidden in the notification area, or not running.
//!
//! * [`protocol`]: the JSON lines exchanged with the popup (request in on stdin, one decision
//!   out on stdout, `cancel` to close it early);
//! * [`run`]: the popup process itself ([`dispatch`] is called first thing in `main`);
//! * [`launcher`]: the GUI side: start a popup per request, read its answer without
//!   blocking, cancel it when the viewer gives up. Behind the [`launcher::PromptSpawner`]
//!   trait so tests use a fake process;
//! * `windows`: the Win32 bits (work area, rounded corners, foreground, sound).
//!
//! The card itself is drawn by [`crate::ui::accept_window`], which the main window also uses
//! for its fallback dialog.

pub mod protocol;

pub use protocol::{Outcome, PromptAuth, PromptDecision, PromptRequest, PromptTheme};
