//! Viewer-side keyboard grab via a `WH_KEYBOARD_LL` hook on a dedicated thread.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{MapVirtualKeyW, MAPVK_VK_TO_VSC_EX};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetAncestor, GetForegroundWindow, GetMessageW, PeekMessageW,
    PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, GA_ROOTOWNER, HC_ACTION,
    KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, LLKHF_UP, MSG, PM_NOREMOVE, WH_KEYBOARD_LL,
    WM_QUIT, WM_USER,
};

use crate::grab_logic::GrabLogic;
use crate::{InputError, InputEvent};

#[derive(Default)]
struct Shared {
    active: AtomicBool,
    /// HWND (as integer) that must be in the foreground for the grab to apply; 0 = any.
    target: AtomicIsize,
    logic: Mutex<GrabLogic>,
}

impl Shared {
    fn logic(&self) -> MutexGuard<'_, GrabLogic> {
        // The logic has no invariants a panic could break; recover from poisoning.
        self.logic.lock().unwrap_or_else(|p| p.into_inner())
    }
}

struct HookCtx {
    tx: Sender<InputEvent>,
    shared: Arc<Shared>,
}

thread_local! {
    /// State for the hook procedure; only set on the grab thread, which is the thread the
    /// OS calls a low-level hook on.
    static HOOK_CTX: RefCell<Option<HookCtx>> = const { RefCell::new(None) };
}

/// Captures the local keyboard for a viewer session window, including keys Windows normally
/// handles itself: Win, Alt+Tab, Alt+Esc, Ctrl+Esc, Alt+F4, Win+R, PrintScreen, ...
///
/// While active (see [`set_active`](Self::set_active)) and the target window is in the
/// foreground, every physical key event is swallowed locally and sent on the channel as
/// [`InputEvent::Key`] with its hardware scancode. Key-ups of keys that were already held
/// when the grab started are let through locally so nothing sticks on the viewer PC.
/// Events synthesized by software (`LLKHF_INJECTED`) are never grabbed.
///
/// Not interceptable, by OS design: **Ctrl+Alt+Del** (the secure attention sequence is
/// handled by Winlogon before any hook) and **Win+L**. When they switch to the secure desktop
/// the grab notices the foreground change and emits [`InputEvent::ReleaseAll`]. Use a toolbar
/// action to send Ctrl+Alt+Del to the host (`SendSAS` in the host service, M3).
///
/// While the grab is active the session window receives no keyboard input itself; local text
/// fields (e.g. a chat box) need the grab deactivated.
///
/// Dropping the grab deactivates it (emitting `ReleaseAll` if it was active), removes the
/// hook and joins its thread.
pub struct KeyboardGrab {
    shared: Arc<Shared>,
    tx: Sender<InputEvent>,
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl KeyboardGrab {
    /// Installs the hook (inactive). Returns the grab and the receiver of captured events.
    pub fn start() -> Result<(KeyboardGrab, Receiver<InputEvent>), InputError> {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared::default());
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("tlp-desk-keyboard-grab".into())
            .spawn({
                let tx = tx.clone();
                let shared = Arc::clone(&shared);
                move || hook_thread(tx, shared, ready_tx)
            })
            .map_err(|e| InputError::Platform(format!("spawn keyboard grab thread: {e}")))?;
        match ready_rx.recv() {
            Ok(Ok(thread_id)) => Ok((
                KeyboardGrab {
                    shared,
                    tx,
                    thread_id,
                    thread: Some(thread),
                },
                rx,
            )),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(InputError::Platform(e))
            }
            Err(_) => {
                let _ = thread.join();
                Err(InputError::Platform("keyboard grab thread died".into()))
            }
        }
    }

    /// Turns the grab on or off. Call with `true` when the session window gains focus (the
    /// current foreground window becomes the grab target) and `false` when it loses focus or
    /// the session switches to view-only. Turning it off emits [`InputEvent::ReleaseAll`].
    pub fn set_active(&self, active: bool) {
        if active {
            // SAFETY: no arguments; returns the foreground window handle or null.
            let fg = unsafe { GetForegroundWindow() };
            self.activate(fg.0 as isize);
        } else {
            self.deactivate();
        }
    }

    /// Like `set_active(true)` but targets an explicit window handle (raw `HWND` value, e.g.
    /// from `raw-window-handle`), so the grab applies only while that window is foreground.
    pub fn set_active_for_window(&self, hwnd: isize) {
        self.activate(hwnd);
    }

    pub fn is_active(&self) -> bool {
        self.shared.active.load(Ordering::SeqCst)
    }

    fn activate(&self, target: isize) {
        self.shared.logic().reset();
        self.shared.target.store(target, Ordering::SeqCst);
        self.shared.active.store(true, Ordering::SeqCst);
    }

    fn deactivate(&self) {
        if self.shared.active.swap(false, Ordering::SeqCst) {
            self.shared.logic().reset();
            let _ = self.tx.send(InputEvent::ReleaseAll);
        }
    }
}

impl Drop for KeyboardGrab {
    fn drop(&mut self) {
        self.deactivate();
        // SAFETY: posts WM_QUIT to the grab thread's queue (created before `start` returned);
        // no pointers are passed.
        let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn hook_thread(
    tx: Sender<InputEvent>,
    shared: Arc<Shared>,
    ready: SyncSender<Result<u32, String>>,
) {
    let mut msg = MSG::default();
    // Force creation of this thread's message queue so PostThreadMessageW cannot fail.
    // SAFETY: `msg` is a valid, writable MSG; PM_NOREMOVE leaves the queue untouched.
    let _ = unsafe { PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE) };

    HOOK_CTX.with(|c| *c.borrow_mut() = Some(HookCtx { tx, shared }));

    // SAFETY: a null module name returns the handle of the running executable; nothing is
    // retained.
    let hmod = unsafe { GetModuleHandleW(PCWSTR::null()) }
        .ok()
        .map(|m| HINSTANCE(m.0));
    // SAFETY: `ll_keyboard_proc` matches HOOKPROC and lives for the whole program; the hook
    // is global (thread id 0) as WH_KEYBOARD_LL requires and is removed below on this thread.
    let hook = match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard_proc), hmod, 0) } {
        Ok(h) => h,
        Err(e) => {
            HOOK_CTX.with(|c| c.borrow_mut().take());
            let _ = ready.send(Err(format!("SetWindowsHookExW(WH_KEYBOARD_LL): {e}")));
            return;
        }
    };
    // SAFETY: no arguments.
    let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));

    // The OS calls the low-level hook from inside GetMessageW on this thread.
    // SAFETY: `msg` is a valid, writable MSG for each call.
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {}

    // SAFETY: `hook` was installed by this thread and is removed exactly once.
    let _ = unsafe { UnhookWindowsHookEx(hook) };
    HOOK_CTX.with(|c| c.borrow_mut().take());
}

/// `LowLevelKeyboardProc`. Must return quickly (the OS drops slow hooks), so it only takes a
/// short lock and pushes onto an unbounded channel.
unsafe extern "system" fn ll_keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam.0 != 0 {
        // SAFETY: for WH_KEYBOARD_LL with HC_ACTION, lParam points to a KBDLLHOOKSTRUCT that
        // is valid for the duration of this call; we copy it out immediately.
        let info = unsafe { *(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if handle_key(&info) {
            // Non-zero: swallow the key so the local system never sees it.
            return LRESULT(1);
        }
    }
    // SAFETY: forwards the unmodified arguments we received, as the hook contract requires.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Whether `target` (a window handle, possibly a child or owned window of the real top-level
/// window) is, or belongs to, the foreground window. winit may hand out a handle that is not
/// exactly the one `GetForegroundWindow` reports; comparing the root windows avoids a grab
/// that never applies.
fn is_target_foreground(target: isize) -> bool {
    // SAFETY: no arguments; returns the foreground window handle or null.
    let fg = unsafe { GetForegroundWindow() };
    if fg.0 as isize == target {
        return true;
    }
    if fg.0.is_null() {
        return false;
    }
    let target = HWND(target as *mut core::ffi::c_void);
    // SAFETY: both handles are plain window handles; a stale one makes the call return null.
    let (a, b) = unsafe {
        (
            GetAncestor(fg, GA_ROOTOWNER),
            GetAncestor(target, GA_ROOTOWNER),
        )
    };
    !a.0.is_null() && a == b
}

/// Returns true when the key must be suppressed locally.
fn handle_key(info: &KBDLLHOOKSTRUCT) -> bool {
    HOOK_CTX.with(|c| {
        let ctx = c.borrow();
        let Some(ctx) = ctx.as_ref() else {
            return false;
        };
        let grab_now = ctx.shared.active.load(Ordering::SeqCst) && {
            let target = ctx.shared.target.load(Ordering::SeqCst);
            target == 0 || is_target_foreground(target)
        };
        let injected = info.flags.contains(LLKHF_INJECTED);
        let extended = info.flags.contains(LLKHF_EXTENDED);
        let down = !info.flags.contains(LLKHF_UP);
        let mut scancode = (info.scanCode & 0xFF) as u16;
        if scancode == 0 {
            // Some keys (certain media/OEM keys) arrive without a scancode.
            // SAFETY: plain integer-in/integer-out call.
            let sc = unsafe { MapVirtualKeyW(info.vkCode, MAPVK_VK_TO_VSC_EX) };
            scancode = sc as u16;
        }
        let decision = ctx
            .shared
            .logic()
            .on_key(grab_now, injected, scancode, extended, down);
        for ev in decision.emit {
            let _ = ctx.tx.send(ev);
        }
        decision.suppress
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn grab_starts_toggles_and_stops() {
        let (grab, rx) = KeyboardGrab::start().expect("install hook");
        assert!(!grab.is_active());
        grab.set_active_for_window(12345);
        assert!(grab.is_active());
        grab.set_active(false);
        assert!(!grab.is_active());
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            InputEvent::ReleaseAll
        );
        // Deactivating twice emits nothing more.
        grab.set_active(false);
        grab.set_active(true);
        drop(grab);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            InputEvent::ReleaseAll
        );
        // The channel closes once the hook thread exits.
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_err());
    }
}
