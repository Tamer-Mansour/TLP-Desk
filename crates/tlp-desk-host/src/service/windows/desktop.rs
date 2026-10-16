//! The input desktop (Microsoft Learn: `OpenInputDesktop`, `GetUserObjectInformationW` with
//! `UOI_NAME`, `SetThreadDesktop`, "Desktops").

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, GetThreadDesktop, GetUserObjectInformationW, OpenInputDesktop, SetThreadDesktop,
    DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, DESKTOP_CREATEMENU, DESKTOP_CREATEWINDOW,
    DESKTOP_ENUMERATE, DESKTOP_HOOKCONTROL, DESKTOP_JOURNALPLAYBACK, DESKTOP_READOBJECTS,
    DESKTOP_WRITEOBJECTS, HDESK, UOI_NAME,
};

use super::os_err;
use crate::service::DesktopSource;
use crate::HostError;

/// Rights a capture/input thread needs on its desktop: read the screen, send input, create
/// the (invisible) windows some capture backends use.
fn thread_access() -> DESKTOP_ACCESS_FLAGS {
    DESKTOP_ACCESS_FLAGS(
        DESKTOP_READOBJECTS.0
            | DESKTOP_WRITEOBJECTS.0
            | DESKTOP_CREATEWINDOW.0
            | DESKTOP_CREATEMENU.0
            | DESKTOP_HOOKCONTROL.0
            | DESKTOP_JOURNALPLAYBACK.0
            | DESKTOP_ENUMERATE.0,
    )
}

fn open_input(access: DESKTOP_ACCESS_FLAGS) -> Result<HDESK, HostError> {
    // SAFETY: plain call; on success we own the desktop handle and close it with CloseDesktop.
    unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, access) }
        .map_err(|e| os_err("OpenInputDesktop", &e))
}

fn name_of(desk: HDESK) -> Result<String, HostError> {
    let mut buf = [0u16; 256];
    let mut needed = 0u32;
    let bytes = u32::try_from(std::mem::size_of_val(&buf)).unwrap_or(0);
    // SAFETY: `desk` is a valid desktop handle; `buf` is writable for `bytes` bytes and
    // `needed` is a valid out pointer.
    unsafe {
        GetUserObjectInformationW(
            HANDLE(desk.0),
            UOI_NAME,
            Some(buf.as_mut_ptr().cast()),
            bytes,
            Some(&mut needed),
        )
    }
    .map_err(|e| os_err("GetUserObjectInformationW", &e))?;
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Ok(String::from_utf16_lossy(buf.get(..len).unwrap_or_default()))
}

fn close(desk: HDESK) {
    // SAFETY: `desk` is a handle we opened and close once. Closing the desktop a thread is
    // still assigned to fails harmlessly.
    let _ = unsafe { CloseDesktop(desk) };
}

/// [`DesktopSource`] backed by the real input desktop. One per thread (it remembers the
/// desktop handle the thread is attached to; the handle must stay open while assigned).
#[derive(Debug, Default)]
pub struct WinDesktopSource {
    /// The desktop handle assigned to the thread, as an integer so the struct is `Send`.
    attached: Option<isize>,
    /// The attaching thread and the desktop it had before the first attach (restored on drop
    /// so `attached` can be closed).
    original: Option<(u32, isize)>,
}

impl WinDesktopSource {
    pub fn new() -> Self {
        Self::default()
    }
}

impl DesktopSource for WinDesktopSource {
    fn input_desktop(&mut self) -> Result<String, HostError> {
        let desk = open_input(DESKTOP_READOBJECTS)?;
        let name = name_of(desk);
        close(desk);
        name
    }

    fn attach_thread(&mut self) -> Result<String, HostError> {
        let desk = open_input(thread_access())?;
        let name = match name_of(desk) {
            Ok(n) => n,
            Err(e) => {
                close(desk);
                return Err(e);
            }
        };
        // SAFETY: plain calls; GetThreadDesktop returns a handle we must not close.
        let thread = unsafe { GetCurrentThreadId() };
        // SAFETY: as above.
        let original = unsafe { GetThreadDesktop(thread) }.unwrap_or_default();
        // SAFETY: `desk` is a valid desktop handle; SetThreadDesktop affects only the calling
        // thread and fails (no harm) if the thread owns windows or hooks.
        if let Err(e) = unsafe { SetThreadDesktop(desk) } {
            close(desk);
            return Err(os_err("SetThreadDesktop", &e));
        }
        if let Some(old) = self.attached.replace(desk.0 as isize) {
            close(HDESK(old as *mut core::ffi::c_void));
        } else {
            self.original = Some((thread, original.0 as isize));
        }
        Ok(name)
    }
}

impl Drop for WinDesktopSource {
    /// Closes the desktop handle of the last [`DesktopSource::attach_thread`]: without this,
    /// every capture/input thread that ends leaks one `OpenInputDesktop` handle (audit F3).
    /// A desktop still assigned to a thread cannot be closed, so on the attaching thread the
    /// thread's original desktop is restored first.
    fn drop(&mut self) {
        let Some(attached) = self.attached.take() else {
            return;
        };
        if let Some((thread, original)) = self.original.take() {
            // SAFETY: plain call without arguments.
            if unsafe { GetCurrentThreadId() } == thread {
                // SAFETY: `original` came from GetThreadDesktop on this thread; such handles
                // stay valid for the thread's lifetime and are not closed by us.
                let _ = unsafe { SetThreadDesktop(HDESK(original as *mut core::ffi::c_void)) };
            }
        }
        close(HDESK(attached as *mut core::ffi::c_void));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads the current input desktop name. Harmless (no switching), but it depends on the
    /// session the test runs in, so it is ignored in CI.
    #[test]
    #[ignore = "reads the interactive desktop; run manually"]
    fn reads_the_input_desktop_name() {
        let mut s = WinDesktopSource::new();
        assert_eq!(s.input_desktop().unwrap(), "Default");
    }

    fn handle_count() -> u32 {
        use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};
        let mut n = 0u32;
        // SAFETY: the pseudo handle of the current process needs no closing; `n` is a valid
        // out pointer.
        unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut n) }.unwrap();
        n
    }

    /// Regression test for audit F3: attaching fresh threads to the input desktop (the same
    /// desktop they already use, so nothing visible happens) and dropping the source must
    /// not leak the desktop handle. Needs an interactive desktop, so it is ignored in CI.
    #[test]
    #[ignore = "needs the interactive input desktop; run manually"]
    fn dropping_the_source_closes_the_attached_desktop() {
        let attach_and_drop = || {
            std::thread::spawn(|| {
                let mut s = WinDesktopSource::new();
                s.attach_thread().unwrap();
                drop(s);
            })
            .join()
            .unwrap();
        };
        // Warm up (thread pool, loader) before measuring.
        attach_and_drop();
        let before = handle_count();
        for _ in 0..50 {
            attach_and_drop();
        }
        let after = handle_count();
        assert!(
            after < before + 10,
            "desktop handles leak: {before} -> {after}"
        );
    }
}
