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

mod glue;
pub mod launcher;
pub mod protocol;
pub mod run;
#[cfg(windows)]
mod windows;

pub use launcher::{AcceptPopups, PopupResult, PromptHandle, PromptPoll, PromptSpawner};
pub use protocol::{Outcome, PromptAuth, PromptDecision, PromptRequest, PromptTheme};
pub use run::{dispatch, ACCEPT_PROMPT_ARG};

/// Platform helpers with no-op fallbacks off Windows.
pub(crate) mod platform {
    /// Primary work area `[left, top, right, bottom]` in points.
    pub fn work_area_points() -> Option<[f32; 4]> {
        #[cfg(windows)]
        return super::windows::work_area_points();
        #[cfg(not(windows))]
        None
    }

    pub fn round_corners(window: isize) {
        #[cfg(windows)]
        super::windows::round_corners(window);
        #[cfg(not(windows))]
        let _ = window;
    }

    pub fn bring_to_front(window: isize) -> bool {
        #[cfg(windows)]
        return super::windows::bring_to_front(window);
        #[cfg(not(windows))]
        {
            let _ = window;
            false
        }
    }

    pub fn allow_foreground(pid: u32) {
        #[cfg(windows)]
        super::windows::allow_foreground(pid);
        #[cfg(not(windows))]
        let _ = pid;
    }

    pub fn notification_sound() {
        #[cfg(windows)]
        super::windows::notification_sound();
    }
}
