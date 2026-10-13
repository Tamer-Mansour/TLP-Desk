//! Viewer keyboard grab: while the session view is focused (and may send keys), system keys
//! such as Win, Alt+Tab, Alt+Esc or Ctrl+Esc go to the remote PC instead of the local one.
//!
//! [`SystemKeyGrab`] wraps [`tlp_desk_input::KeyboardGrab`] (a low-level keyboard hook on its
//! own thread) and forwards every captured key straight to [`Backend::send_input`] from a
//! small forwarding thread, so keys are not delayed until the next egui frame. While the grab
//! is active the session view ignores egui's own key events (the hook swallows the physical
//! keys anyway; this avoids duplicates for anything that still slips through).

use std::sync::Weak;

use tlp_desk_input::KeyboardGrab;

use crate::backend::{Backend, SessionId};

/// What the session view needs from a keyboard grab (a trait so tests can use a fake).
pub trait KeyGrab: Send {
    /// Turn the grab on (session view focused, keys allowed) or off. Only called on changes.
    fn set_active(&mut self, active: bool);
}

/// The real, OS-level grab for one viewer session.
pub struct SystemKeyGrab {
    grab: KeyboardGrab,
    /// Raw `HWND` of the app window; the grab only applies while it is in the foreground.
    hwnd: Option<isize>,
}

impl SystemKeyGrab {
    /// Install the (inactive) hook and the forwarding thread. Returns `None` (logged) where
    /// the platform has no grab; the session then falls back to egui key events.
    pub fn start(
        backend: Weak<dyn Backend>,
        session: SessionId,
        hwnd: Option<isize>,
    ) -> Option<Self> {
        let (grab, rx) = match KeyboardGrab::start() {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!(%e, "keyboard grab unavailable; system keys stay local");
                return None;
            }
        };
        let spawned = std::thread::Builder::new()
            .name("tlp-desk-grab-forward".into())
            .spawn(move || {
                // Ends when the grab is dropped (its sender goes away) or the app exits.
                while let Ok(ev) = rx.recv() {
                    let Some(b) = backend.upgrade() else { break };
                    b.send_input(session, ev);
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(%e, "cannot spawn keyboard grab forwarder");
            return None;
        }
        Some(Self { grab, hwnd })
    }
}

impl KeyGrab for SystemKeyGrab {
    fn set_active(&mut self, active: bool) {
        match (active, self.hwnd) {
            (true, Some(hwnd)) => self.grab.set_active_for_window(hwnd),
            (true, None) => self.grab.set_active(true),
            // Emits ReleaseAll to the host if anything was forwarded.
            (false, _) => self.grab.set_active(false),
        }
    }
}

/// Raw `HWND` of an eframe window (Windows only), for [`SystemKeyGrab`].
pub fn window_handle(w: &impl raw_window_handle::HasWindowHandle) -> Option<isize> {
    match w.window_handle().ok()?.as_raw() {
        raw_window_handle::RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}
