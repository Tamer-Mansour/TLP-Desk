//! Starts a verified installer elevated (UAC prompt) in silent mode, and opens release pages
//! in the browser. The Inno Setup installer stops the running service and app itself and
//! restarts the service afterwards.

use std::path::Path;

use super::UpdateError;

/// Inno Setup switches: no wizard pages, no message boxes, never reboot on its own.
pub const INSTALLER_ARGS: &str = "/SILENT /SUPPRESSMSGBOXES /NORESTART";

/// Only pages of this site are opened from the app.
const PAGE_PREFIX: &str = "https://github.com/";

/// Runs `installer` with [`INSTALLER_ARGS`] via `ShellExecuteW("runas")`. Only call this with
/// a path that [`super::reverify`] just accepted.
#[cfg(windows)]
pub fn launch_installer(installer: &Path) -> Result<(), UpdateError> {
    windows_impl::shell_execute("runas", installer.as_os_str(), Some(INSTALLER_ARGS))
}

/// Other platforms update through their package manager.
#[cfg(not(windows))]
pub fn launch_installer(_installer: &Path) -> Result<(), UpdateError> {
    Err(UpdateError::Launch("not supported on this platform".into()))
}

/// Opens a GitHub page (release page) in the default browser. Anything that is not an
/// `https://github.com/` URL is refused, so a hostile API answer cannot launch other things.
pub fn open_release_page(url: &str) -> Result<(), UpdateError> {
    if !url.starts_with(PAGE_PREFIX) || url.chars().any(|c| c.is_whitespace() || c == '"') {
        return Err(UpdateError::InsecureUrl);
    }
    #[cfg(windows)]
    {
        windows_impl::shell_execute("open", url.as_ref(), None)
    }
    #[cfg(not(windows))]
    {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        std::process::Command::new(opener)
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|e| UpdateError::Launch(e.to_string()))
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_impl {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    use super::UpdateError;

    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    pub(super) fn shell_execute(
        verb: &str,
        file: &OsStr,
        args: Option<&str>,
    ) -> Result<(), UpdateError> {
        let verb_w = wide(verb.as_ref());
        let file_w = wide(file);
        let args_w = args.map(|a| wide(a.as_ref()));
        let args_p = args_w
            .as_ref()
            .map_or(PCWSTR::null(), |a| PCWSTR(a.as_ptr()));
        // SAFETY: every pointer is null or a NUL-terminated UTF-16 buffer that outlives the
        // call; a null owner window and directory are allowed by ShellExecuteW.
        let h = unsafe {
            ShellExecuteW(
                None,
                PCWSTR(verb_w.as_ptr()),
                PCWSTR(file_w.as_ptr()),
                args_p,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_github_pages_open() {
        // Refused before anything is launched.
        assert!(open_release_page("https://evil.example/x").is_err());
        assert!(open_release_page("file:///C:/Windows/System32/calc.exe").is_err());
        assert!(open_release_page("https://github.com.evil.example/").is_err());
        assert!(open_release_page("https://github.com/a b").is_err());
    }
}
