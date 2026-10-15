//! Viewer keyboard grab: while the session view is focused (and may send keys), system keys
//! such as Win, Alt+Tab, Alt+Esc or Ctrl+Esc go to the remote PC instead of the local one.
//!
//! [`SystemKeyGrab`] wraps [`tlp_desk_input::KeyboardGrab`] (a low-level keyboard hook on its
//! own thread) and forwards every captured key straight to [`Backend::send_input`] from a
//! small forwarding thread, so keys are not delayed until the next egui frame. The hook
//! swallows every key it forwards, so the session view needs no de-duplication: a key that
//! still reaches the window while the grab is on was *not* captured and is forwarded from the
//! window path instead (see `ui::key_forward`), and the grab is asked to reinstall its hook.

use std::sync::Weak;
use std::time::{Duration, Instant};

use tlp_desk_input::KeyboardGrab;

use crate::backend::{Backend, SessionId};

/// What the session view needs from a keyboard grab (a trait so tests can use a fake).
pub trait KeyGrab: Send {
    /// Turn the grab on (session view focused, keys allowed) or off. Only called on changes.
    fn set_active(&mut self, active: bool);

    /// A key reached the window although the grab is on, i.e. the grab is not capturing
    /// (Windows silently removes a low-level hook that answers too slowly). The session view
    /// already forwards such keys itself; the grab may repair itself.
    fn report_leak(&mut self) {}
}

/// Minimum time between two hook reinstalls.
const REINSTALL_COOLDOWN: Duration = Duration::from_secs(5);

/// The real, OS-level grab for one viewer session.
pub struct SystemKeyGrab {
    grab: KeyboardGrab,
    backend: Weak<dyn Backend>,
    session: SessionId,
    /// Raw `HWND` of the app window; the grab only applies while it is in the foreground.
    hwnd: Option<isize>,
    active: bool,
    last_reinstall: Option<Instant>,
}

/// Installs a hook and the thread that forwards its keys to `session`.
fn spawn_grab(backend: &Weak<dyn Backend>, session: SessionId) -> Option<KeyboardGrab> {
    let (grab, rx) = match KeyboardGrab::start() {
        Ok(g) => g,
        Err(e) => {
            tracing::warn!(%e, "keyboard grab unavailable; system keys stay local");
            return None;
        }
    };
    let backend = backend.clone();
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
    Some(grab)
}

impl SystemKeyGrab {
    /// Install the (inactive) hook and the forwarding thread. Returns `None` (logged) where
    /// the platform has no grab; the session then falls back to egui key events.
    pub fn start(
        backend: Weak<dyn Backend>,
        session: SessionId,
        hwnd: Option<isize>,
    ) -> Option<Self> {
        let grab = spawn_grab(&backend, session)?;
        Some(Self {
            grab,
            backend,
            session,
            hwnd,
            active: false,
            last_reinstall: None,
        })
    }

    fn apply(&self) {
        match (self.active, self.hwnd) {
            (true, Some(hwnd)) => self.grab.set_active_for_window(hwnd),
            (true, None) => self.grab.set_active(true),
            // Emits ReleaseAll to the host if anything was forwarded.
            (false, _) => self.grab.set_active(false),
        }
    }
}

impl KeyGrab for SystemKeyGrab {
    fn set_active(&mut self, active: bool) {
        self.active = active;
        self.apply();
    }

    fn report_leak(&mut self) {
        if self
            .last_reinstall
            .is_some_and(|t| t.elapsed() < REINSTALL_COOLDOWN)
        {
            return;
        }
        self.last_reinstall = Some(Instant::now());
        // Replace the hook: Windows may have dropped the old one (callback too slow).
        if let Some(new) = spawn_grab(&self.backend, self.session) {
            tracing::warn!("keyboard grab reinstalled after it stopped capturing keys");
            self.grab = new; // drops the old hook; its ReleaseAll is harmless
            self.apply();
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
