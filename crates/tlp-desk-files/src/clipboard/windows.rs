//! Windows clipboard change notifications without polling.
//!
//! A dedicated thread owns a message-only window registered with
//! `AddClipboardFormatListener`; Windows sends it `WM_CLIPBOARDUPDATE` on every clipboard change
//! and the thread publishes `GetClipboardSequenceNumber` into a `watch` channel. The thread
//! sleeps in `GetMessageW` in between, so an idle clipboard costs no CPU.
//!
//! References: Microsoft Learn, "Using the Clipboard" / "Monitoring Clipboard Contents",
//! `AddClipboardFormatListener`, `GetClipboardSequenceNumber`, "Message-Only Windows".

use std::cell::RefCell;
use std::sync::mpsc;
use std::thread::JoinHandle;

use tokio::sync::watch;
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, GetClipboardSequenceNumber, RemoveClipboardFormatListener,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    PostThreadMessageW, RegisterClassW, HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_CLIPBOARDUPDATE, WM_QUIT, WNDCLASSW,
};

use super::error::ClipboardError;

thread_local! {
    /// The listener thread's channel; only touched on that thread (by its window procedure).
    static SENDER: RefCell<Option<watch::Sender<u64>>> = const { RefCell::new(None) };
}

/// The clipboard's current change sequence number (0 if the caller lacks access to the
/// window station).
pub(crate) fn sequence() -> u64 {
    // SAFETY: no arguments, no pointers; the function only reads a per-window-station counter.
    u64::from(unsafe { GetClipboardSequenceNumber() })
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_CLIPBOARDUPDATE {
        let seq = sequence();
        SENDER.with(|s| {
            if let Some(tx) = s.borrow().as_ref() {
                tx.send_replace(seq);
            }
        });
        return LRESULT(0);
    }
    // SAFETY: forwarding the unmodified arguments Windows passed to this window procedure.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Runs the listener until `WM_QUIT`. Reports the thread id (or the setup error) on `ready`.
fn run(tx: watch::Sender<u64>, ready: mpsc::Sender<Result<u32, String>>) {
    SENDER.with(|s| *s.borrow_mut() = Some(tx));
    let class = w!("TLPDeskClipboardListener");
    // SAFETY: `None` asks for the handle of the current executable; nothing is borrowed.
    let instance = match unsafe { GetModuleHandleW(None) } {
        Ok(m) => m.into(),
        Err(e) => {
            let _ = ready.send(Err(e.to_string()));
            return;
        }
    };
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: instance,
        lpszClassName: class,
        ..Default::default()
    };
    // SAFETY: `wc` is a fully initialized WNDCLASSW that outlives the call; the class name is a
    // static wide string. A second registration (another listener) fails harmlessly with
    // ERROR_CLASS_ALREADY_EXISTS and the existing class is used.
    unsafe { RegisterClassW(&wc) };
    // SAFETY: creates a message-only window (parent HWND_MESSAGE) of the class registered
    // above; all pointer arguments are static strings or None.
    let hwnd = match unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!(""),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance),
            None,
        )
    } {
        Ok(h) => h,
        Err(e) => {
            let _ = ready.send(Err(e.to_string()));
            return;
        }
    };
    // SAFETY: `hwnd` is the valid window created above, owned by this thread.
    if let Err(e) = unsafe { AddClipboardFormatListener(hwnd) } {
        // SAFETY: destroying the window this thread created.
        let _ = unsafe { DestroyWindow(hwnd) };
        let _ = ready.send(Err(e.to_string()));
        return;
    }
    // SAFETY: no arguments; returns this thread's id (its message queue exists already).
    let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));
    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is a valid, writable MSG; `None` receives messages for every window of
        // this thread and thread messages (WM_QUIT).
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 <= 0 {
            break; // WM_QUIT (0) or error (-1)
        }
        // SAFETY: `msg` was filled by GetMessageW just above.
        unsafe { DispatchMessageW(&msg) };
    }
    // SAFETY: `hwnd` is still our valid window; unregister and destroy it on its own thread.
    unsafe {
        let _ = RemoveClipboardFormatListener(hwnd);
        let _ = DestroyWindow(hwnd);
    }
    SENDER.with(|s| *s.borrow_mut() = None);
}

/// A running listener thread; dropping it stops the thread.
#[derive(Debug)]
pub(crate) struct Listener {
    thread_id: u32,
    join: Option<JoinHandle<()>>,
}

impl Listener {
    /// Starts the listener; `tx` gets the sequence number on every change.
    pub(crate) fn start(tx: watch::Sender<u64>) -> Result<Self, ClipboardError> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let join = std::thread::Builder::new()
            .name("tlp-clipboard-listener".into())
            .spawn(move || run(tx, ready_tx))
            .map_err(|e| ClipboardError::Clipboard(e.to_string()))?;
        match ready_rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                thread_id,
                join: Some(join),
            }),
            Ok(Err(e)) => {
                let _ = join.join();
                Err(ClipboardError::Clipboard(e))
            }
            Err(_) => Err(ClipboardError::Clipboard("listener thread ended".into())),
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        // SAFETY: posting WM_QUIT (no pointers) to the listener thread's queue.
        let posted =
            unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) }.is_ok();
        if posted {
            if let Some(j) = self.join.take() {
                let _ = j.join();
            }
        }
    }
}
