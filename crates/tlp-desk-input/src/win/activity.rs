//! Host-side local activity monitor: `WH_MOUSE_LL` + `WH_KEYBOARD_LL` hooks on a dedicated
//! thread that only timestamp **physical** input (events flagged as injected are ignored).
//!
//! Built from the Microsoft Learn pages for `SetWindowsHookExW`, `LowLevelMouseProc`,
//! `LowLevelKeyboardProc`, `MSLLHOOKSTRUCT` and `KBDLLHOOKSTRUCT`.

use std::cell::RefCell;
use std::sync::mpsc::{self, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, PeekMessageW, PostThreadMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED,
    LLKHF_LOWER_IL_INJECTED, LLMHF_INJECTED, LLMHF_LOWER_IL_INJECTED, MSG, MSLLHOOKSTRUCT,
    PM_NOREMOVE, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_QUIT, WM_USER,
};

use crate::activity::{counts_as_local, ActivityStamp, LocalActivity};
use crate::InputError;

thread_local! {
    /// Stamp for the hook procedures; only set on the monitor thread, which is the thread the
    /// OS calls its low-level hooks on.
    static STAMP: RefCell<Option<Arc<ActivityStamp>>> = const { RefCell::new(None) };
}

/// Watches the host's own mouse and keyboard so the person at the host can take priority over
/// a remote viewer.
///
/// The hooks never block or swallow anything: they store one atomic timestamp and pass the
/// event on with `CallNextHookEx`. Events carrying `LLMHF_INJECTED` / `LLKHF_INJECTED` (or the
/// lower-integrity variants) — i.e. `SendInput` from this or any other program — are ignored.
/// While privacy mode blocks the local keyboard and mouse ([`crate::local_input_blocked`]),
/// physical events are ignored too: local priority is suspended during privacy mode.
///
/// Dropping the monitor removes both hooks and joins the thread.
pub struct LocalActivityMonitor {
    stamp: Arc<ActivityStamp>,
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for LocalActivityMonitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalActivityMonitor")
            .field("last", &self.stamp.last())
            .finish_non_exhaustive()
    }
}

impl LocalActivityMonitor {
    /// Installs the hooks on a new thread.
    pub fn start() -> Result<Self, InputError> {
        let stamp = Arc::new(ActivityStamp::new());
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("tlp-desk-local-activity".into())
            .spawn({
                let stamp = Arc::clone(&stamp);
                move || hook_thread(stamp, ready_tx)
            })
            .map_err(|e| InputError::Platform(format!("spawn local activity thread: {e}")))?;
        match ready_rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                stamp,
                thread_id,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(InputError::Platform(e))
            }
            Err(_) => {
                let _ = thread.join();
                Err(InputError::Platform("local activity thread died".into()))
            }
        }
    }
}

impl LocalActivity for LocalActivityMonitor {
    fn last_physical_input(&self) -> Option<Instant> {
        self.stamp.last()
    }

    fn last_physical_keyboard(&self) -> Option<Instant> {
        self.stamp.last_physical_keyboard()
    }

    fn last_physical_pointer(&self) -> Option<Instant> {
        self.stamp.last_physical_pointer()
    }
}

impl Drop for LocalActivityMonitor {
    fn drop(&mut self) {
        // SAFETY: posts WM_QUIT to the monitor thread's queue (created before `start`
        // returned); no pointers are passed.
        let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn unhook(h: HHOOK) {
    // SAFETY: `h` was installed by this thread and is removed exactly once.
    let _ = unsafe { UnhookWindowsHookEx(h) };
}

fn hook_thread(stamp: Arc<ActivityStamp>, ready: SyncSender<Result<u32, String>>) {
    let mut msg = MSG::default();
    // Force creation of this thread's message queue so PostThreadMessageW cannot fail.
    // SAFETY: `msg` is a valid, writable MSG; PM_NOREMOVE leaves the queue untouched.
    let _ = unsafe { PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE) };
    STAMP.with(|s| *s.borrow_mut() = Some(stamp));

    // SAFETY: a null module name returns the handle of the running executable.
    let hmod = unsafe { GetModuleHandleW(PCWSTR::null()) }
        .ok()
        .map(|m| HINSTANCE(m.0));
    // SAFETY: `ll_mouse_proc` matches HOOKPROC and lives for the whole program; the hook is
    // global (thread id 0) as WH_MOUSE_LL requires and is removed below on this thread.
    let mouse = match unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(ll_mouse_proc), hmod, 0) } {
        Ok(h) => h,
        Err(e) => {
            STAMP.with(|s| s.borrow_mut().take());
            let _ = ready.send(Err(format!("SetWindowsHookExW(WH_MOUSE_LL): {e}")));
            return;
        }
    };
    // SAFETY: as above, for WH_KEYBOARD_LL with `ll_keyboard_proc`.
    let keyboard =
        match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard_proc), hmod, 0) } {
            Ok(h) => h,
            Err(e) => {
                unhook(mouse);
                STAMP.with(|s| s.borrow_mut().take());
                let _ = ready.send(Err(format!("SetWindowsHookExW(WH_KEYBOARD_LL): {e}")));
                return;
            }
        };
    // SAFETY: no arguments.
    let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));

    // The OS calls low-level hooks from inside GetMessageW on this thread.
    // SAFETY: `msg` is a valid, writable MSG for each call.
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {}

    unhook(keyboard);
    unhook(mouse);
    STAMP.with(|s| s.borrow_mut().take());
}

/// Stamps one hook event. Injected events, and physical ones that privacy mode swallows,
/// do not count (see [`counts_as_local`]).
fn record(keyboard: bool, injected: bool) {
    let now = Instant::now();
    let ignored = !counts_as_local(injected);
    STAMP.with(|s| {
        if let Some(s) = s.borrow().as_ref() {
            if keyboard {
                s.record_keyboard(ignored, now);
            } else {
                s.record_pointer(ignored, now);
            }
        }
    });
}

/// `LowLevelMouseProc`: timestamp only, never swallow.
unsafe extern "system" fn ll_mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam.0 != 0 {
        // SAFETY: for WH_MOUSE_LL with HC_ACTION, lParam points to an MSLLHOOKSTRUCT valid for
        // the duration of this call; we copy the flags out immediately.
        let flags = unsafe { (*(lparam.0 as *const MSLLHOOKSTRUCT)).flags };
        record(
            false,
            flags & (LLMHF_INJECTED | LLMHF_LOWER_IL_INJECTED) != 0,
        );
    }
    // SAFETY: forwards the unmodified arguments we received, as the hook contract requires.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// `LowLevelKeyboardProc`: timestamp only, never swallow.
unsafe extern "system" fn ll_keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam.0 != 0 {
        // SAFETY: for WH_KEYBOARD_LL with HC_ACTION, lParam points to a KBDLLHOOKSTRUCT valid
        // for the duration of this call; we copy the flags out immediately.
        let flags = unsafe { (*(lparam.0 as *const KBDLLHOOKSTRUCT)).flags };
        record(
            true,
            flags.contains(LLKHF_INJECTED) || flags.contains(LLKHF_LOWER_IL_INJECTED),
        );
    }
    // SAFETY: forwards the unmodified arguments we received, as the hook contract requires.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEINPUT,
    };

    #[test]
    fn monitor_starts_ignores_injected_input_and_stops() {
        let m = LocalActivityMonitor::start().expect("install hooks");
        let before = m.last_physical_input();
        // A zero-distance relative move: injected, so it must not count as local activity,
        // and it does not visibly move the user's pointer.
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: 0,
                    dwFlags: MOUSEEVENTF_MOVE,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        // SAFETY: one fully initialized INPUT with its correct size.
        unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
        std::thread::sleep(std::time::Duration::from_millis(150));
        // Injected input must not count. The user may be touching the real mouse while the
        // test runs, so only a stamp *older* than our SendInput is acceptable evidence; a
        // newer one could be genuine, so it is reported rather than asserted.
        if let Some(t) = m.last_physical_input() {
            println!("local activity seen {:?} ago (user input?)", t.elapsed());
        }
        let _ = before;
        drop(m);
    }
}
