//! Windows privacy shield: overlay windows ([`overlay`]) and low-level hooks ([`hooks`]) on
//! one dedicated thread with its own message loop.
//!
//! Life cycle of one engagement (one thread):
//! 1. create the overlays (must be excluded from capture), check the input desktop is the
//!    normal one, install the hooks, show the overlays, report ready;
//! 2. every [`TICK_MS`]: re-assert top-most, poll the emergency chord, check the input
//!    desktop;
//! 3. on stop / emergency chord / secure desktop / failure: hide the overlays and stop
//!    blocking at once (local priority resumes), finish swallowed keystrokes for at most
//!    [`DRAIN_LIMIT`], remove the hooks, destroy the windows, report the signal, exit.
//!
//! The windows and hooks belong to this thread: if the thread or the whole process dies,
//! Windows removes both. Built from the Microsoft Learn pages for `GetMessageW`,
//! `PostThreadMessageW`, `SetTimer`, `OpenInputDesktop` and `GetUserObjectInformationW`.

mod hooks;
mod overlay;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HANDLE, LPARAM, WPARAM};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, GetUserObjectInformationW, OpenInputDesktop, DESKTOP_CONTROL_FLAGS,
    DESKTOP_READOBJECTS, UOI_NAME,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, KillTimer, PeekMessageW, PostThreadMessageW, SetTimer,
    TranslateMessage, MSG, PM_NOREMOVE, WM_APP, WM_TIMER, WM_USER,
};

use self::hooks::Hooks;
use self::overlay::Overlays;
use crate::privacy::{
    is_normal_desktop, PrivacyScreenText, PrivacyShield, ShieldSignal, ShieldSignalFn,
};
use crate::InputError;

/// Stop and tear down (sent by [`WindowsPrivacyShield::release`]).
const WM_SHIELD_STOP: u32 = WM_APP + 0x50;
/// The overlay text changed.
const WM_SHIELD_TEXT: u32 = WM_APP + 0x51;
/// Monitor layout changed (posted by the overlay window procedure).
const WM_SHIELD_REBUILD: u32 = WM_APP + 0x52;
/// The emergency chord fired (posted by the keyboard hook).
const WM_SHIELD_TRIPPED: u32 = WM_APP + 0x53;
/// The last swallowed keystroke finished while draining (posted by the hooks).
const WM_SHIELD_DRAINED: u32 = WM_APP + 0x54;

/// Housekeeping period of the shield thread.
const TICK_MS: u32 = 250;
/// Longest time the hooks stay after privacy mode ended, to finish swallowed keystrokes.
const DRAIN_LIMIT: Duration = Duration::from_secs(2);
/// Longest wait for the shield thread to come up.
const START_TIMEOUT: Duration = Duration::from_secs(5);

struct Worker {
    thread_id: u32,
    thread: JoinHandle<()>,
    /// Overlay shown and physical input blocked (false once draining or gone).
    active: Arc<AtomicBool>,
}

impl Worker {
    fn post(&self, msg: u32) {
        // SAFETY: posts a pointer-free message to the shield thread's queue, which exists
        // before the worker is handed out.
        let _ = unsafe { PostThreadMessageW(self.thread_id, msg, WPARAM(0), LPARAM(0)) };
    }

    fn stop(self) {
        self.post(WM_SHIELD_STOP);
        let _ = self.thread.join();
    }
}

/// The Windows [`PrivacyShield`]. Idle until engaged; dropping it releases everything.
pub(crate) struct WindowsPrivacyShield {
    on_signal: ShieldSignalFn,
    text: Arc<Mutex<PrivacyScreenText>>,
    worker: Option<Worker>,
}

impl WindowsPrivacyShield {
    pub(crate) fn new(on_signal: ShieldSignalFn) -> Self {
        Self {
            on_signal,
            text: Arc::default(),
            worker: None,
        }
    }

    fn store_text(&self, text: &PrivacyScreenText) {
        *self.text.lock().unwrap_or_else(PoisonError::into_inner) = text.sanitized();
    }
}

impl PrivacyShield for WindowsPrivacyShield {
    fn engage(&mut self, text: &PrivacyScreenText) -> Result<(), InputError> {
        self.store_text(text);
        if self.is_engaged() {
            if let Some(w) = &self.worker {
                w.post(WM_SHIELD_TEXT);
            }
            return Ok(());
        }
        // A previous engagement may still be finishing (draining) or be gone.
        if let Some(old) = self.worker.take() {
            old.stop();
        }
        let active = Arc::new(AtomicBool::new(false));
        // Rendezvous: the thread only goes live if `engage` actually took its answer, so a
        // timed-out engage can never leave an unowned shield behind.
        let (ready_tx, ready_rx) = mpsc::sync_channel(0);
        let thread = std::thread::Builder::new()
            .name("tlp-desk-privacy".into())
            .spawn({
                let text = Arc::clone(&self.text);
                let on_signal = Arc::clone(&self.on_signal);
                let active = Arc::clone(&active);
                move || shield_thread(text, on_signal, active, ready_tx)
            })
            .map_err(|e| InputError::Platform(format!("spawn privacy thread: {e}")))?;
        match ready_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(thread_id)) => {
                self.worker = Some(Worker {
                    thread_id,
                    thread,
                    active,
                });
                Ok(())
            }
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(InputError::Platform(e))
            }
            Err(RecvTimeoutError::Timeout) => {
                // Never leave a half-started shield behind: with the receiver gone its ready
                // message cannot be delivered, and it tears itself down.
                drop(ready_rx);
                Err(InputError::Platform(
                    "privacy thread did not start in time".into(),
                ))
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = thread.join();
                Err(InputError::Platform("privacy thread died".into()))
            }
        }
    }

    fn set_text(&mut self, text: &PrivacyScreenText) {
        self.store_text(text);
        if let Some(w) = &self.worker {
            w.post(WM_SHIELD_TEXT);
        }
    }

    fn release(&mut self) {
        if let Some(w) = self.worker.take() {
            w.stop();
        }
    }

    fn is_engaged(&self) -> bool {
        self.worker
            .as_ref()
            .is_some_and(|w| w.active.load(Ordering::Acquire) && !w.thread.is_finished())
    }
}

impl Drop for WindowsPrivacyShield {
    fn drop(&mut self) {
        self.release();
    }
}

/// Name of the desktop that currently receives input, `None` if it cannot be opened (which is
/// what a normal process gets while the secure desktop is shown).
fn input_desktop_name() -> Option<String> {
    // SAFETY: plain flags; the handle is closed below.
    let desk =
        unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS) }.ok()?;
    let mut buf = [0u16; 64];
    let mut needed = 0u32;
    let len = u32::try_from(std::mem::size_of_val(&buf)).unwrap_or(0);
    // SAFETY: `buf` is a writable buffer of `len` bytes, `needed` a writable u32; the desktop
    // handle is valid until CloseDesktop.
    let ok = unsafe {
        GetUserObjectInformationW(
            HANDLE(desk.0),
            UOI_NAME,
            Some(buf.as_mut_ptr().cast()),
            len,
            Some(&mut needed),
        )
    }
    .is_ok();
    // SAFETY: closes the handle opened above.
    let _ = unsafe { CloseDesktop(desk) };
    if !ok {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(buf.get(..end).unwrap_or(&[])))
}

fn on_normal_desktop() -> bool {
    is_normal_desktop(input_desktop_name().as_deref())
}

fn shield_thread(
    text: Arc<Mutex<PrivacyScreenText>>,
    on_signal: ShieldSignalFn,
    active: Arc<AtomicBool>,
    ready: SyncSender<Result<u32, String>>,
) {
    // Physical pixels for monitor rectangles and per-monitor DPI for the fonts.
    // SAFETY: takes a predefined pseudo-handle constant; affects only this thread.
    unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let mut msg = MSG::default();
    // SAFETY: `msg` is a valid, writable MSG; creates this thread's message queue so posted
    // messages cannot get lost.
    let _ = unsafe { PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE) };

    let current = || text.lock().unwrap_or_else(PoisonError::into_inner).clone();
    if !on_normal_desktop() {
        let _ = ready.send(Err(
            "the secure desktop (lock screen, UAC or Ctrl+Alt+Del) is shown; privacy mode \
             cannot cover it"
                .into(),
        ));
        return;
    }
    let overlays = match Overlays::create(&current()) {
        Ok(o) => o,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let hooks = match Hooks::install() {
        Ok(h) => h,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    overlays.show();
    // SAFETY: a thread timer (no window); WM_TIMER arrives in this thread's queue.
    let timer = unsafe { SetTimer(None, 0, TICK_MS, None) };
    active.store(true, Ordering::Release);
    // SAFETY: no arguments.
    let thread_id = unsafe { GetCurrentThreadId() };
    if ready.send(Ok(thread_id)).is_err() {
        // `engage` gave up waiting: undo everything (hooks and windows drop here).
        active.store(false, Ordering::Release);
        // SAFETY: kills the thread timer created above.
        let _ = unsafe { KillTimer(None, timer) };
        return;
    }

    let mut e = Engagement {
        overlays,
        hooks,
        active,
        on_signal,
        drain_until: None,
    };
    loop {
        if e.drain_done() {
            break;
        }
        // SAFETY: `msg` is a valid, writable MSG for each call.
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) }.0;
        if got <= 0 {
            // WM_QUIT (nobody posts it) or an error: tear down as a failure.
            e.begin_drain(Some(ShieldSignal::Failed));
            break;
        }
        match msg.message {
            WM_SHIELD_STOP => e.begin_drain(None),
            WM_SHIELD_TRIPPED => e.begin_drain(Some(ShieldSignal::EmergencyExit)),
            WM_SHIELD_DRAINED => {}
            WM_SHIELD_TEXT => e.overlays.set_text(&current()),
            WM_SHIELD_REBUILD if !e.draining() => {
                if e.overlays.rebuild().is_err() {
                    e.begin_drain(Some(ShieldSignal::Failed));
                }
            }
            WM_TIMER if msg.hwnd.is_invalid() => e.tick(),
            _ => {
                // SAFETY: translates/dispatches a message retrieved by GetMessageW (WM_PAINT
                // and friends for the overlay windows).
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
    e.active.store(false, Ordering::Release);
    // SAFETY: kills the thread timer created above.
    let _ = unsafe { KillTimer(None, timer) };
    // Hooks first: input is never blocked without the overlay explaining why.
    drop(e.hooks);
    drop(e.overlays);
}

/// One engagement on the shield thread.
struct Engagement {
    overlays: Overlays,
    hooks: Hooks,
    active: Arc<AtomicBool>,
    on_signal: ShieldSignalFn,
    /// Set once privacy mode ended: the hooks only finish swallowed keystrokes until then.
    drain_until: Option<Instant>,
}

impl Engagement {
    fn draining(&self) -> bool {
        self.drain_until.is_some()
    }

    fn drain_done(&self) -> bool {
        self.drain_until
            .is_some_and(|t| Instant::now() >= t || self.hooks.is_drained())
    }

    /// Ends privacy mode at once (overlay hidden, new input flows, local priority resumes)
    /// and reports `signal`; the hooks stay only to finish swallowed keystrokes.
    fn begin_drain(&mut self, signal: Option<ShieldSignal>) {
        if self.draining() {
            return;
        }
        self.active.store(false, Ordering::Release);
        self.hooks.start_draining();
        self.overlays.hide();
        self.drain_until = Some(Instant::now() + DRAIN_LIMIT);
        if let Some(s) = signal {
            (self.on_signal)(s);
        }
    }

    /// Periodic housekeeping while engaged.
    fn tick(&mut self) {
        if self.draining() {
            return;
        }
        if self.hooks.poll_emergency(Instant::now()) {
            self.begin_drain(Some(ShieldSignal::EmergencyExit));
        } else if !on_normal_desktop() {
            self.begin_drain(Some(ShieldSignal::SecureDesktop));
        } else {
            self.overlays.raise();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::privacy::{local_input_blocked, ScreenLine};

    #[test]
    fn idle_shield_is_not_engaged_and_release_is_harmless() {
        let mut s = WindowsPrivacyShield::new(Arc::new(|_| {}));
        assert!(!s.is_engaged());
        s.set_text(&PrivacyScreenText::default());
        s.release();
        s.release();
        assert!(!s.is_engaged());
    }

    #[test]
    fn input_desktop_query_is_read_only_and_total() {
        // Read-only: opens the input desktop for reading its name, nothing else. In a service
        // or CI session it may be unavailable; that must simply read as "not normal".
        match input_desktop_name() {
            Some(name) => assert!(!name.is_empty()),
            None => assert!(!on_normal_desktop()),
        }
    }

    /// Blacks out every monitor and blocks the physical mouse and keyboard for ~1.5 s, so it
    /// never runs by default. Run it only on a test PC:
    /// `cargo test -p tlp-desk-input -- --ignored shield_blanks_and_blocks_then_releases`.
    /// Expected: black screens with the Arabic/English text, mouse and keyboard dead, then
    /// everything back; a screenshot tool during the run still shows the desktop.
    #[test]
    #[ignore = "blacks out the screen and blocks local input"]
    fn shield_blanks_and_blocks_then_releases() {
        let signals = Arc::new(Mutex::new(Vec::new()));
        let rec = Arc::clone(&signals);
        let mut s = WindowsPrivacyShield::new(Arc::new(move |sig| {
            rec.lock().unwrap().push(sig);
        }));
        let text = PrivacyScreenText {
            lines: vec![
                ScreenLine::title("هذا الحاسوب قيد الاستخدام عن بُعد"),
                ScreenLine::title("This PC is being used remotely"),
                ScreenLine::body("متصل: مكتب-Office (123 456 789)"),
                ScreenLine::body("Hold Ctrl+Alt+Shift+P for 3 seconds to end privacy mode here."),
            ],
        };
        s.engage(&text).expect("engage");
        assert!(s.is_engaged());
        assert!(local_input_blocked());
        std::thread::sleep(Duration::from_millis(1500));
        s.release();
        assert!(!s.is_engaged());
        assert!(!local_input_blocked());
        assert!(signals.lock().unwrap().is_empty());
    }
}
