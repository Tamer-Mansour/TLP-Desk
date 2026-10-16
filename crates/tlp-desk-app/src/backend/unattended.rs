//! Backend types for unattended access (TLP-unattended): the permanent password, the remote
//! "System" actions (restart, lock, Ctrl+Alt+Del) and restart-and-reconnect progress.

use std::path::Path;
use std::time::Duration;

/// What the Settings screen shows about the permanent password.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PasswordState {
    /// A permanent password is set (password logins are accepted).
    pub set: bool,
    /// Stored in the machine-wide `%ProgramData%\TLP-Desk\` (hardened ACL) rather than the
    /// per-user fallback (`false` when TLP-Desk could not write there, e.g. not elevated).
    pub machine_wide: bool,
    /// This process runs elevated (local administrator): changing or removing the password
    /// does not need the current one.
    pub admin: bool,
}

/// Successful outcome of [`super::Backend::set_unattended_password`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordChange {
    Set,
    Removed,
}

/// Why a password change was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordError {
    /// A password is set and the current one given was wrong (and we are not elevated).
    WrongCurrent,
    /// Shorter than `tlp_desk_crypto::MIN_PASSWORD_CHARS` characters.
    TooShort,
    /// Hashing or writing failed (detail for the message).
    Save(String),
    /// This device cannot accept connections at all.
    Unavailable,
}

/// A remote system action from the session toolbar's "System" menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemAction {
    /// Restart the remote PC and reconnect automatically when it is back (`RESTART`).
    Restart,
    /// Lock the remote workstation (`SYSTEM`).
    Lock,
    /// Press Ctrl+Alt+Del on the remote PC (`SYSTEM`; needs the TLP-Desk service there).
    CtrlAltDel,
}

impl SystemAction {
    /// The permission the host requires for this action.
    pub fn required(self) -> tlp_desk_proto::Permissions {
        use tlp_desk_proto::Permissions;
        match self {
            SystemAction::Restart => Permissions::RESTART,
            SystemAction::Lock | SystemAction::CtrlAltDel => Permissions::SYSTEM,
        }
    }

    /// The v4 message that carries it.
    pub fn message(self) -> tlp_desk_proto::Message {
        use tlp_desk_proto::Message;
        match self {
            SystemAction::Restart => Message::RestartRemote { reconnect: true },
            SystemAction::Lock => Message::LockWorkstation,
            SystemAction::CtrlAltDel => Message::SendCtrlAltDel,
        }
    }
}

/// How long a peer is locked out after too many wrong codes/passwords (the host's default; the
/// wire does not carry the remaining time), in whole minutes for the error message.
pub fn lockout_minutes() -> u64 {
    let lockout: Duration = tlp_desk_crypto::RateLimitConfig::default().lockout;
    lockout.as_secs().div_ceil(60).max(1)
}

/// Whether `dir` is the machine-wide data folder (under `%ProgramData%`).
pub fn is_machine_wide(dir: &Path) -> bool {
    std::env::var_os("ProgramData")
        .map(std::path::PathBuf::from)
        .is_some_and(|pd| {
            let pd = pd.to_string_lossy().to_lowercase();
            dir.to_string_lossy().to_lowercase().starts_with(&pd)
        })
}

/// Whether this process runs elevated (High or System mandatory integrity level). Asks the
/// built-in `whoami.exe` (by absolute path) so the app stays free of `unsafe`; blocking, call
/// it off the UI thread. `false` on any failure and on other OSes.
pub fn is_elevated() -> bool {
    if !cfg!(windows) {
        return false;
    }
    let root = std::env::var_os("SystemRoot").map_or_else(
        || std::path::PathBuf::from(r"C:\Windows"),
        std::path::PathBuf::from,
    );
    let out = std::process::Command::new(root.join("System32").join("whoami.exe"))
        .arg("/groups")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            // Mandatory Label\High Mandatory Level / System Mandatory Level
            text.contains("S-1-16-12288") || text.contains("S-1-16-16384")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tlp_desk_proto::{Message, Permissions};

    #[test]
    fn actions_need_their_permission_and_map_to_v4_messages() {
        assert_eq!(SystemAction::Restart.required(), Permissions::RESTART);
        assert_eq!(SystemAction::Lock.required(), Permissions::SYSTEM);
        assert_eq!(SystemAction::CtrlAltDel.required(), Permissions::SYSTEM);
        assert_eq!(
            SystemAction::Restart.message(),
            Message::RestartRemote { reconnect: true }
        );
        assert_eq!(SystemAction::Lock.message(), Message::LockWorkstation);
        assert_eq!(SystemAction::CtrlAltDel.message(), Message::SendCtrlAltDel);
    }

    #[test]
    fn lockout_is_ten_minutes_by_default() {
        assert_eq!(lockout_minutes(), 10);
    }

    #[test]
    fn machine_wide_detection() {
        if let Some(pd) = std::env::var_os("ProgramData") {
            let p = std::path::PathBuf::from(pd).join("TLP-Desk");
            assert!(is_machine_wide(&p));
        }
        assert!(!is_machine_wide(Path::new(r"Z:\elsewhere\مجلد")));
    }
}
