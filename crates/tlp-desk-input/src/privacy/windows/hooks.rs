//! `WH_KEYBOARD_LL` / `WH_MOUSE_LL` hooks of the privacy shield: swallow physical input,
//! let injected input through, watch for the emergency chord.
//!
//! Built from the Microsoft Learn pages for `SetWindowsHookExW`, `LowLevelKeyboardProc`,
//! `LowLevelMouseProc`, `KBDLLHOOKSTRUCT` and `MSLLHOOKSTRUCT`. A low-level hook procedure
//! that returns a non-zero value without calling `CallNextHookEx` prevents the system from
//! passing the event to the rest of the hook chain and to the target window.

use std::cell::RefCell;
use std::time::Instant;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION, HHOOK,
    KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, LLKHF_LOWER_IL_INJECTED, LLKHF_UP,
    LLMHF_INJECTED, LLMHF_LOWER_IL_INJECTED, MSLLHOOKSTRUCT, WH_KEYBOARD_LL, WH_MOUSE_LL,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN, WM_XBUTTONUP, XBUTTON1,
};

use super::{WM_SHIELD_DRAINED, WM_SHIELD_TRIPPED};
use crate::privacy::{
    BlockingGuard, EmergencyExit, HookButton, HookEvent, PhysicalFilter, Verdict,
};

/// State shared by the two hook procedures. Lives in a thread-local of the shield thread,
/// which is the thread the OS calls its low-level hooks on.
struct HookState {
    filter: PhysicalFilter,
    exit: EmergencyExit,
    /// Present while physical input is blocked (suspends local input priority).
    blocking: Option<BlockingGuard>,
    /// The emergency chord fired (reported once to the message loop).
    tripped: bool,
}

thread_local! {
    static STATE: RefCell<Option<HookState>> = const { RefCell::new(None) };
}

/// Both hooks, removed on drop (on the installing thread).
pub(super) struct Hooks {
    keyboard: HHOOK,
    mouse: HHOOK,
}

impl Hooks {
    /// Installs both hooks on the calling thread, which must run a message loop.
    pub(super) fn install() -> Result<Self, String> {
        STATE.with(|s| {
            *s.borrow_mut() = Some(HookState {
                filter: PhysicalFilter::new(true),
                exit: EmergencyExit::new(),
                blocking: Some(BlockingGuard::new()),
                tripped: false,
            })
        });
        // SAFETY: a null module name returns the handle of the running executable.
        let hmod = unsafe { GetModuleHandleW(PCWSTR::null()) }
            .ok()
            .map(|m| HINSTANCE(m.0));
        // SAFETY: `ll_keyboard` matches HOOKPROC and lives for the whole program; the hook is
        // global (thread id 0) as WH_KEYBOARD_LL requires and is removed in Drop on this
        // thread.
        let keyboard =
            match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard), hmod, 0) } {
                Ok(h) => h,
                Err(e) => {
                    clear_state();
                    return Err(format!("SetWindowsHookExW(WH_KEYBOARD_LL): {e}"));
                }
            };
        // SAFETY: as above, for WH_MOUSE_LL with `ll_mouse`.
        let mouse = match unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(ll_mouse), hmod, 0) } {
            Ok(h) => h,
            Err(e) => {
                unhook(keyboard);
                clear_state();
                return Err(format!("SetWindowsHookExW(WH_MOUSE_LL): {e}"));
            }
        };
        Ok(Self { keyboard, mouse })
    }

    /// Stops blocking new input; keystrokes already swallowed are finished (see
    /// [`PhysicalFilter::start_draining`]). Local input priority resumes immediately.
    pub(super) fn start_draining(&self) {
        with_state(|s| {
            s.filter.start_draining();
            s.blocking = None;
        });
    }

    /// No swallowed key is still held.
    pub(super) fn is_drained(&self) -> bool {
        with_state(|s| s.filter.is_drained()).unwrap_or(true)
    }

    /// Timer check of the emergency chord (keys do not auto-repeat on every keyboard).
    /// Returns `true` once when it fires.
    pub(super) fn poll_emergency(&self, now: Instant) -> bool {
        with_state(|s| {
            if s.exit.poll(now) {
                trip(s);
                true
            } else {
                false
            }
        })
        .unwrap_or(false)
    }
}

impl Drop for Hooks {
    fn drop(&mut self) {
        unhook(self.mouse);
        unhook(self.keyboard);
        clear_state();
    }
}

fn unhook(h: HHOOK) {
    // SAFETY: `h` was installed by this thread and is removed exactly once.
    let _ = unsafe { UnhookWindowsHookEx(h) };
}

fn clear_state() {
    STATE.with(|s| s.borrow_mut().take());
}

fn with_state<T>(f: impl FnOnce(&mut HookState) -> T) -> Option<T> {
    STATE.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

fn post_self(msg: u32) {
    // SAFETY: no arguments.
    let tid = unsafe { GetCurrentThreadId() };
    // SAFETY: posts a message without pointers to this thread's own queue.
    let _ = unsafe { PostThreadMessageW(tid, msg, WPARAM(0), LPARAM(0)) };
}

/// The emergency chord fired: stop blocking new input right here, inside the hook, before the
/// message loop (or anyone else) gets a say.
fn trip(s: &mut HookState) {
    if s.tripped {
        return;
    }
    s.tripped = true;
    s.filter.start_draining();
    s.blocking = None;
    post_self(WM_SHIELD_TRIPPED);
}

/// Runs the filter for one event; reports when a drain completes.
fn decide(s: &mut HookState, ev: HookEvent, injected: bool) -> Verdict {
    let was_drained = s.filter.is_drained();
    let v = s.filter.decide(ev, injected);
    if !s.filter.is_blocking() && !was_drained && s.filter.is_drained() {
        post_self(WM_SHIELD_DRAINED);
    }
    v
}

/// `LowLevelKeyboardProc`.
unsafe extern "system" fn ll_keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam.0 != 0 {
        // SAFETY: for WH_KEYBOARD_LL with HC_ACTION, lParam points to a KBDLLHOOKSTRUCT valid
        // for the duration of this call; we copy it out immediately.
        let k = unsafe { *(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let injected =
            k.flags.contains(LLKHF_INJECTED) || k.flags.contains(LLKHF_LOWER_IL_INJECTED);
        let down = !k.flags.contains(LLKHF_UP);
        let extended = k.flags.contains(LLKHF_EXTENDED);
        let scancode = u8::try_from(k.scanCode & 0xFF).unwrap_or(0);
        let now = Instant::now();
        let verdict = with_state(|s| {
            // Decide first: the key that completes the chord is swallowed like its siblings.
            let v = decide(
                s,
                HookEvent::Key {
                    scancode,
                    extended,
                    down,
                },
                injected,
            );
            if !injected && s.exit.key(scancode, extended, down, now) {
                trip(s);
            }
            v
        });
        if verdict == Some(Verdict::Swallow) {
            return LRESULT(1);
        }
    }
    // SAFETY: forwards the unmodified arguments we received, as the hook contract requires.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// `LowLevelMouseProc`.
unsafe extern "system" fn ll_mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam.0 != 0 {
        // SAFETY: for WH_MOUSE_LL with HC_ACTION, lParam points to an MSLLHOOKSTRUCT valid for
        // the duration of this call; we copy it out immediately.
        let m = unsafe { *(lparam.0 as *const MSLLHOOKSTRUCT) };
        let injected = m.flags & (LLMHF_INJECTED | LLMHF_LOWER_IL_INJECTED) != 0;
        let x_button = if (m.mouseData >> 16) as u16 == XBUTTON1 {
            HookButton::X1
        } else {
            HookButton::X2
        };
        let button = |button, down| Some(HookEvent::MouseButton { button, down });
        let event = match u32::try_from(wparam.0).unwrap_or(0) {
            WM_MOUSEMOVE => Some(HookEvent::MouseMove),
            WM_MOUSEWHEEL | WM_MOUSEHWHEEL => Some(HookEvent::Wheel),
            WM_LBUTTONDOWN => button(HookButton::Left, true),
            WM_LBUTTONUP => button(HookButton::Left, false),
            WM_RBUTTONDOWN => button(HookButton::Right, true),
            WM_RBUTTONUP => button(HookButton::Right, false),
            WM_MBUTTONDOWN => button(HookButton::Middle, true),
            WM_MBUTTONUP => button(HookButton::Middle, false),
            WM_XBUTTONDOWN => button(x_button, true),
            WM_XBUTTONUP => button(x_button, false),
            _ => None,
        };
        if let Some(ev) = event {
            if with_state(|s| decide(s, ev, injected)) == Some(Verdict::Swallow) {
                return LRESULT(1);
            }
        }
    }
    // SAFETY: forwards the unmodified arguments we received, as the hook contract requires.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}
