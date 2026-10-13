//! The real [`SystemPower`]: restarts and locks this machine.
//!
//! Uses the OS's own tools by absolute path (`%SystemRoot%\System32\shutdown.exe` and
//! `rundll32.exe user32.dll,LockWorkStation`) so this crate stays free of `unsafe`, the same
//! approach `tlp_desk_crypto::harden_dir` takes with `icacls.exe`. Never call it from tests.
//!
//! * Restart: `shutdown.exe /r /t 5 /d p:4:1` = restart in 5 seconds (time for the reconnect
//!   ticket to reach the viewer), reason "Application: Maintenance (Planned)". A non-zero
//!   timeout makes Windows close running applications without asking.
//! * Lock: works when the host runs in the interactive user's session (the GUI app). From the
//!   service (session 0) it cannot reach the user's desktop; the service lane provides its own
//!   implementation there.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use super::SystemPower;
use crate::HostError;

/// Restarts / locks the real machine (Windows). On other OSes every call fails.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsPower;

/// `%SystemRoot%\System32\<exe>`; never resolved through `PATH`.
fn system32(exe: &str) -> PathBuf {
    let root =
        std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    root.join("System32").join(exe)
}

fn run(exe: &str, args: &[&str]) -> Result<(), HostError> {
    if !cfg!(windows) {
        return Err(HostError::System(format!(
            "{exe} is only available on Windows"
        )));
    }
    let status = Command::new(system32(exe))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| HostError::System(format!("cannot run {exe}: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(HostError::System(format!("{exe} failed with {status}")))
    }
}

impl SystemPower for WindowsPower {
    fn restart(&self) -> Result<(), HostError> {
        run("shutdown.exe", &["/r", "/t", "5", "/d", "p:4:1"])
    }

    fn lock_workstation(&self) -> Result<(), HostError> {
        run("rundll32.exe", &["user32.dll,LockWorkStation"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_are_resolved_under_system32() {
        // Path construction only: nothing is executed.
        let p = system32("shutdown.exe");
        assert!(p.ends_with(r"System32\shutdown.exe") || p.ends_with("System32/shutdown.exe"));
        assert!(p.is_absolute() || std::env::var_os("SystemRoot").is_none());
    }
}
