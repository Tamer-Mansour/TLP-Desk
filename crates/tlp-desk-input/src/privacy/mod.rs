//! Privacy mode, host side: blank the host's physical screens and block its physical keyboard
//! and mouse while a remote viewer keeps seeing and controlling the real desktop.
//!
//! # Pieces
//! * [`PrivacyShield`] is the OS seam. The Windows implementation (`start_privacy_shield`)
//!   shows one black, top-most, click-through window per monitor that is **excluded from
//!   screen capture** (`SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`, Windows 10 2004+),
//!   so the viewer's video still shows the desktop, and installs low-level keyboard and mouse
//!   hooks that swallow **physical** events while injected ones (the viewer's `SendInput`)
//!   pass. Tests use fakes; nothing in the default test run touches the real screen.
//! * [`PhysicalFilter`] (which events to swallow) and [`EmergencyExit`] (the local escape
//!   chord) are pure logic, unit-tested without any OS call.
//!
//! # Safety rules (the host user must always be able to get their PC back)
//! * **Owned by the process.** The overlay windows and hooks belong to a thread of the host
//!   process. If the process exits or crashes, Windows destroys the windows and removes the
//!   hooks: the blanking disappears and input blocking can never outlive the process.
//!   Dropping the shield (session end, disconnect, permission revoked) does the same.
//! * **Emergency exit.** Holding Ctrl+Alt+Shift+P for [`EMERGENCY_HOLD`] on the physical
//!   keyboard turns blocking off and hides the overlay at once (inside the hook thread, before
//!   anyone else reacts) and reports [`ShieldSignal::EmergencyExit`].
//! * **Ctrl+Alt+Del always works**: Windows never passes it to hooks. Its secure desktop (and
//!   the lock screen, UAC prompts, the screen saver) cannot be covered by any window, so as
//!   soon as the input desktop is not the normal one the shield turns itself off and reports
//!   [`ShieldSignal::SecureDesktop`]. Privacy is never silently claimed when it cannot hold.
//! * **Local input priority is suspended** while input is blocked
//!   ([`local_input_blocked`]): the swallowed physical events must not count as "the host user
//!   is active", or the viewer would lose control exactly when privacy mode is on.
//! * If the OS refuses capture exclusion, the shield refuses to start: the viewer would only
//!   see the black overlay, so claiming privacy mode would be a lie either way.
//!
//! # Known limits (documented for users)
//! * The mouse pointer itself is drawn by Windows above every window, so people at the host
//!   still see it move while the viewer works.
//! * System surfaces that live above all top-most windows (the Start menu, some toasts and
//!   the volume flyout) may show briefly when the viewer opens them.
//! * Windows silently removes a low-level hook that stops answering (`LowLevelHooksTimeout`);
//!   the hook thread does nothing slow, but if it ever happened the local input would simply
//!   work again (fail-open, never fail-closed).

mod escape;
mod filter;

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub use escape::{EmergencyExit, EMERGENCY_HOLD};
pub use filter::{HookButton, HookEvent, PhysicalFilter, Verdict};

use crate::InputError;

/// Longest line drawn on the privacy screen, in characters (peer names are self-reported).
pub const MAX_LINE_CHARS: usize = 160;
/// Most lines drawn on the privacy screen (title not included).
pub const MAX_LINES: usize = 12;

/// Text drawn on the privacy screen, already localized by the caller (the input crate has no
/// translation table). The title is drawn large, the lines below it in a smaller font.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrivacyScreenText {
    /// E.g. "This PC is being used remotely".
    pub title: String,
    /// Who is connected, how to end privacy mode locally, ...
    pub lines: Vec<String>,
    /// Lay the text out right-to-left (Arabic UI).
    pub rtl: bool,
}

impl PrivacyScreenText {
    /// A copy that is safe to draw: control characters and bidi overrides removed (a peer's
    /// device name must not be able to reorder the sentence around it), each line capped at
    /// [`MAX_LINE_CHARS`], at most [`MAX_LINES`] lines, empty lines dropped.
    pub fn sanitized(&self) -> Self {
        Self {
            title: clean_line(&self.title),
            lines: self
                .lines
                .iter()
                .map(|l| clean_line(l))
                .filter(|l| !l.is_empty())
                .take(MAX_LINES)
                .collect(),
            rtl: self.rtl,
        }
    }
}

/// Removes control and bidi-override characters, trims and caps one line.
pub fn clean_line(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() && !is_bidi_control(*c))
        .take(MAX_LINE_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Explicit embedding/override/isolate controls (U+202A..U+202E, U+2066..U+2069). Implicit
/// marks (LRM/RLM/ALM) and ZWJ/ZWNJ are kept: they are needed for correct Arabic text.
fn is_bidi_control(c: char) -> bool {
    matches!(c as u32, 0x202A..=0x202E | 0x2066..=0x2069)
}

/// Why a shield turned itself off without being asked to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShieldSignal {
    /// The host user held the emergency chord (Ctrl+Alt+Shift+P).
    EmergencyExit,
    /// The input desktop is no longer the normal one (Ctrl+Alt+Del screen, lock screen, UAC,
    /// screen saver): no window can cover it, so privacy mode cannot hold.
    SecureDesktop,
    /// The shield's thread stopped unexpectedly.
    Failed,
}

/// Callback for [`ShieldSignal`]s. Called on the shield's own thread: it must not block (send
/// on a channel, spawn a thread) and must not call back into the shield.
pub type ShieldSignalFn = Arc<dyn Fn(ShieldSignal) + Send + Sync>;

/// Blanks the physical screens and blocks physical input (the OS seam of privacy mode).
pub trait PrivacyShield: Send {
    /// Covers every monitor and starts blocking physical input. Idempotent. Fails (and leaves
    /// nothing behind) if the overlay cannot be excluded from capture or the hooks cannot be
    /// installed.
    fn engage(&mut self, text: &PrivacyScreenText) -> Result<(), InputError>;

    /// Replaces the text on the overlay (e.g. a second viewer connected).
    fn set_text(&mut self, text: &PrivacyScreenText);

    /// Removes the overlay and the hooks. Idempotent; also done on drop.
    fn release(&mut self);

    /// Whether the overlay and hooks are currently active.
    fn is_engaged(&self) -> bool;
}

/// Number of shields currently blocking physical input in this process.
static BLOCKING: AtomicUsize = AtomicUsize::new(0);

/// Whether a privacy shield of this process is blocking physical input right now. The local
/// activity monitor ignores physical input while this is true (local priority is suspended
/// during privacy mode).
pub fn local_input_blocked() -> bool {
    BLOCKING.load(Ordering::Acquire) > 0
}

/// Marks physical input as blocked for as long as it lives.
#[derive(Debug)]
pub(crate) struct BlockingGuard(());

impl BlockingGuard {
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn new() -> Self {
        BLOCKING.fetch_add(1, Ordering::AcqRel);
        Self(())
    }
}

impl Drop for BlockingGuard {
    fn drop(&mut self) {
        BLOCKING.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Whether a desktop name (from `GetUserObjectInformationW(UOI_NAME)` on the input desktop)
/// is the normal interactive desktop. `None` (the input desktop could not be opened, which is
/// what a normal process gets while the secure desktop is shown) counts as not normal.
pub fn is_normal_desktop(name: Option<&str>) -> bool {
    name.is_some_and(|n| n.trim_end_matches('\0').eq_ignore_ascii_case("Default"))
}

/// Starts the platform privacy shield (idle until [`PrivacyShield::engage`]).
/// [`InputError::Unsupported`] on platforms without one.
pub fn start_privacy_shield(
    on_signal: ShieldSignalFn,
) -> Result<Box<dyn PrivacyShield>, InputError> {
    #[cfg(windows)]
    {
        Ok(Box::new(windows::WindowsPrivacyShield::new(on_signal)))
    }
    #[cfg(not(windows))]
    {
        let _ = on_signal;
        Err(InputError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_text_is_sanitized() {
        let t = PrivacyScreenText {
            title: "  هذا الجهاز قيد الاستخدام عن بُعد \u{202E}evil ".into(),
            lines: vec![
                "Connected: \u{2066}مكتب-Office\u{2069}\r\n (123 456 789)".into(),
                String::new(),
                "x".repeat(500),
            ],
            rtl: true,
        };
        let s = t.sanitized();
        assert_eq!(s.title, "هذا الجهاز قيد الاستخدام عن بُعد evil");
        assert_eq!(s.lines.len(), 2);
        assert_eq!(s.lines[0], "Connected: مكتب-Office (123 456 789)");
        assert_eq!(s.lines[1].chars().count(), MAX_LINE_CHARS);
        assert!(s.rtl);
        let many = PrivacyScreenText {
            lines: vec!["line".into(); 50],
            ..PrivacyScreenText::default()
        };
        assert_eq!(many.sanitized().lines.len(), MAX_LINES);
    }

    #[test]
    fn arabic_joiners_and_marks_survive() {
        // ZWNJ, ZWJ and RLM are part of correct Arabic text and must stay.
        let s = "مرحبا\u{200C}بك \u{200D}ـ \u{200F}نص";
        assert_eq!(clean_line(s), s);
    }

    #[test]
    fn only_the_default_desktop_is_normal() {
        assert!(is_normal_desktop(Some("Default")));
        assert!(is_normal_desktop(Some("default\0")));
        assert!(!is_normal_desktop(Some("Winlogon")));
        assert!(!is_normal_desktop(Some("Screen-saver")));
        assert!(!is_normal_desktop(None));
    }

    #[test]
    fn blocking_guard_counts() {
        // Other tests in this binary never create guards, so the count is ours.
        let a = BlockingGuard::new();
        assert!(local_input_blocked());
        let b = BlockingGuard::new();
        drop(a);
        assert!(local_input_blocked());
        drop(b);
        assert!(!local_input_blocked());
    }
}
