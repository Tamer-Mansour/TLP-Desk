//! Starts a verified installer elevated (UAC prompt) in silent mode. The Inno Setup installer
//! stops the running service and app itself and restarts the service afterwards.

use std::path::Path;

use super::UpdateError;

/// Inno Setup switches: no wizard pages, no message boxes, never reboot on its own.
pub const INSTALLER_ARGS: &str = "/SILENT /SUPPRESSMSGBOXES /NORESTART";

/// Runs `installer` with [`INSTALLER_ARGS`] via `ShellExecuteW("runas")`. Only call this with
/// a path that [`super::reverify`] just accepted.
#[cfg(windows)]
pub fn launch_installer(installer: &Path) -> Result<(), UpdateError> {
    windows_impl::run_as(installer, INSTALLER_ARGS)
}

/// Other platforms update through their package manager.
#[cfg(not(windows))]
pub fn launch_installer(_installer: &Path) -> Result<(), UpdateError> {
    Err(UpdateError::Launch("not supported on this platform".into()))
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_impl {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    use super::UpdateError;

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    pub(super) fn run_as(file: &Path, args: &str) -> Result<(), UpdateError> {
        let verb = wide("runas".as_ref());
        let file_w = wide(file.as_os_str());
        let args_w = wide(args.as_ref());
        // SAFETY: every pointer is a NUL-terminated UTF-16 buffer that outlives the call;
        // a null owner window and directory are allowed by ShellExecuteW.
        let h = unsafe {
            ShellExecuteW(
                None,
                PCWSTR(verb.as_ptr()),
                PCWSTR(file_w.as_ptr()),
                PCWSTR(args_w.as_ptr()),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        // Per the docs, values greater than 32 mean success; the HINSTANCE is only a code.
        let code = h.0 as usize;
        if code > 32 {
            Ok(())
        } else {
            Err(UpdateError::Launch(format!("ShellExecute code {code}")))
        }
    }
}
