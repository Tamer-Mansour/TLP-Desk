//! Stable process exit codes, so scripts and agents can branch on *why* a command failed.
//!
//! The numbers are part of the CLI's public contract: never renumber, only append. The same
//! table is printed by `--help` ([`HELP_TABLE`]) and documented in `docs/cli.md`.

use tlp_desk_net::NetError;
use tlp_desk_proto::RejectReason;
use tlp_desk_viewer::{TunnelCloseReason, ViewerError};

/// Why the CLI stopped. `as u8` is the process exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[allow(
    dead_code,
    reason = "a published table: some codes are produced by std/clap or by later commands"
)]
pub enum Exit {
    /// The command did what was asked.
    Ok = 0,
    /// Any failure not listed below (I/O, bad file, internal error).
    Failure = 1,
    /// Bad command line (unknown command or flag, missing argument). Emitted by clap.
    Usage = 2,
    /// The device ID / name was not found on the LAN.
    NotFound = 3,
    /// The host could not be reached, or the address is outside the LAN allow-list.
    Unreachable = 4,
    /// The host refused the session: rejected prompt, wrong code or password, lockout.
    AuthRejected = 5,
    /// The host's key does not match the pinned/dialed identity (possible man-in-the-middle).
    KeyMismatch = 6,
    /// The session lacks the permission this command needs.
    PermissionDenied = 7,
    /// No answer in time.
    Timeout = 8,
    /// The peer speaks another protocol version.
    VersionMismatch = 9,
    /// Interrupted with Ctrl+C.
    Interrupted = 130,
}

impl Exit {
    /// The numeric exit code.
    pub fn code(self) -> u8 {
        self as u8
    }

    /// Stable machine name used in JSON error objects.
    pub fn name(self) -> &'static str {
        match self {
            Exit::Ok => "ok",
            Exit::Failure => "failure",
            Exit::Usage => "usage",
            Exit::NotFound => "not_found",
            Exit::Unreachable => "unreachable",
            Exit::AuthRejected => "auth_rejected",
            Exit::KeyMismatch => "key_mismatch",
            Exit::PermissionDenied => "permission_denied",
            Exit::Timeout => "timeout",
            Exit::VersionMismatch => "version_mismatch",
            Exit::Interrupted => "interrupted",
        }
    }

    /// Every code, in table order.
    #[cfg(test)]
    pub const ALL: [Exit; 11] = [
        Exit::Ok,
        Exit::Failure,
        Exit::Usage,
        Exit::NotFound,
        Exit::Unreachable,
        Exit::AuthRejected,
        Exit::KeyMismatch,
        Exit::PermissionDenied,
        Exit::Timeout,
        Exit::VersionMismatch,
        Exit::Interrupted,
    ];

    /// The exit code an error maps to (looks through the whole `anyhow` cause chain).
    pub fn of(err: &anyhow::Error) -> Exit {
        for cause in err.chain() {
            if let Some(e) = cause.downcast_ref::<CliError>() {
                return e.exit();
            }
            if let Some(e) = cause.downcast_ref::<tlp_desk_files::fs::FsClientError>() {
                return of_fs(e);
            }
            if let Some(e) = cause.downcast_ref::<ViewerError>() {
                return of_viewer(e);
            }
            if let Some(e) = cause.downcast_ref::<NetError>() {
                if let Some(x) = of_net(e) {
                    return x;
                }
            }
            if let Some(e) = cause.downcast_ref::<tlp_desk_files::FilesError>() {
                return of_files(e);
            }
        }
        Exit::Failure
    }
}

fn of_viewer(e: &ViewerError) -> Exit {
    match e {
        ViewerError::NotFound(_) => Exit::NotFound,
        ViewerError::NotAllowed(_) | ViewerError::Unreachable(_) => Exit::Unreachable,
        ViewerError::Rejected(r) => of_reject(*r),
        ViewerError::HostAuthFailed => Exit::AuthRejected,
        ViewerError::IdentityMismatch { .. } | ViewerError::HostKeyChanged { .. } => {
            Exit::KeyMismatch
        }
        ViewerError::VersionMismatch { .. } => Exit::VersionMismatch,
        ViewerError::Net(n) => of_net(n).unwrap_or(Exit::Failure),
        // --- tunnels (TLP-tunnel) ---
        ViewerError::TunnelNotAllowed => Exit::PermissionDenied,
        ViewerError::TunnelRefused(r) => match r {
            TunnelCloseReason::TargetNotAllowed
            | TunnelCloseReason::PermissionDenied
            | TunnelCloseReason::PermissionRevoked => Exit::PermissionDenied,
            TunnelCloseReason::NoAnswer => Exit::Timeout,
            _ => Exit::Failure,
        },
        ViewerError::TunnelSpec(_) | ViewerError::TunnelLanBind(_) => Exit::Usage,
        ViewerError::TunnelBind { .. } | ViewerError::TunnelLimit => Exit::Failure,
        ViewerError::SessionEnded => Exit::Unreachable,
        ViewerError::KnownHosts(_)
        | ViewerError::RecordingIo(_)
        | ViewerError::DiskFull { .. }
        | ViewerError::BadRecording(_)
        | ViewerError::BadVideo(_)
        | ViewerError::Decoder(_) => Exit::Failure,
    }
}

fn of_fs(e: &tlp_desk_files::fs::FsClientError) -> Exit {
    use tlp_desk_files::fs::FsClientError as C;
    match e {
        C::NoPermission | C::Remote(tlp_desk_proto::FsError::PermissionDenied) => {
            Exit::PermissionDenied
        }
        C::Timeout => Exit::Timeout,
        _ => Exit::Failure,
    }
}

fn of_files(e: &tlp_desk_files::FilesError) -> Exit {
    use tlp_desk_files::FilesError as F;
    use tlp_desk_proto::FsError;
    match e {
        F::PermissionDenied | F::Remote(FsError::PermissionDenied) => Exit::PermissionDenied,
        F::Remote(FsError::NotFound) => Exit::NotFound,
        F::Cancelled => Exit::Interrupted,
        F::Net(n) => of_net(n).unwrap_or(Exit::Failure),
        _ => Exit::Failure,
    }
}

fn of_reject(r: RejectReason) -> Exit {
    match r {
        RejectReason::VersionMismatch => Exit::VersionMismatch,
        _ => Exit::AuthRejected,
    }
}

fn of_net(e: &NetError) -> Option<Exit> {
    Some(match e {
        NetError::PinMismatch { .. } => Exit::KeyMismatch,
        NetError::NotAllowed(_) | NetError::Connect(_) | NetError::Connection(_) => {
            Exit::Unreachable
        }
        NetError::VersionMismatch { .. } => Exit::VersionMismatch,
        NetError::Rejected(r) => of_reject(*r),
        NetError::AuthFailed | NetError::HostAuthFailed | NetError::RateLimited => {
            Exit::AuthRejected
        }
        NetError::Timeout => Exit::Timeout,
        _ => return None,
    })
}

/// CLI-level failures that carry their own exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    /// Ctrl+C before the command finished.
    Interrupted,
}

impl CliError {
    /// The exit code for this error.
    pub fn exit(&self) -> Exit {
        match self {
            CliError::Interrupted => Exit::Interrupted,
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::Interrupted => f.write_str("interrupted"),
        }
    }
}

impl std::error::Error for CliError {}

/// The exit-code table shown at the end of `--help`.
pub const HELP_TABLE: &str = "\
Exit codes:
  0    ok                 the command did what was asked
  1    failure            any other error (I/O, bad file, internal)
  2    usage              bad command line
  3    not_found          device ID / name / alias not found on the LAN
  4    unreachable        host unreachable or outside the LAN allow-list
  5    auth_rejected      rejected, wrong code/password, or locked out
  6    key_mismatch       host key does not match (possible man-in-the-middle)
  7    permission_denied  the session lacks the permission the command needs
  8    timeout            no answer in time
  9    version_mismatch   the peer speaks another protocol version
  130  interrupted        Ctrl+C

Environment:
  TLP_DESK_PASSWORD  unattended-access password for --password (never on the command line)
  TLP_DESK_LANG      default for --lang (en or ar)";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable() {
        let codes: Vec<(u8, &str)> = Exit::ALL.iter().map(|e| (e.code(), e.name())).collect();
        assert_eq!(
            codes,
            [
                (0, "ok"),
                (1, "failure"),
                (2, "usage"),
                (3, "not_found"),
                (4, "unreachable"),
                (5, "auth_rejected"),
                (6, "key_mismatch"),
                (7, "permission_denied"),
                (8, "timeout"),
                (9, "version_mismatch"),
                (130, "interrupted"),
            ]
        );
    }

    #[test]
    fn help_table_lists_every_code() {
        for e in Exit::ALL {
            let row = format!("  {:<4} {}", e.code(), e.name());
            assert!(HELP_TABLE.contains(&row), "missing {row:?}");
        }
    }

    #[test]
    fn errors_map_to_codes() {
        let cases: Vec<(anyhow::Error, Exit)> = vec![
            (anyhow::anyhow!("disk full"), Exit::Failure),
            (CliError::Interrupted.into(), Exit::Interrupted),
            (ViewerError::NotFound("مكتب".into()).into(), Exit::NotFound),
            (
                ViewerError::Unreachable("10.0.0.9".into()).into(),
                Exit::Unreachable,
            ),
            (
                ViewerError::Rejected(RejectReason::UserDeclined).into(),
                Exit::AuthRejected,
            ),
            (ViewerError::HostAuthFailed.into(), Exit::AuthRejected),
            (
                ViewerError::VersionMismatch { ours: 4, theirs: 3 }.into(),
                Exit::VersionMismatch,
            ),
            (
                ViewerError::Net(NetError::RateLimited).into(),
                Exit::AuthRejected,
            ),
            (NetError::Timeout.into(), Exit::Timeout),
        ];
        for (err, want) in cases {
            assert_eq!(Exit::of(&err), want, "{err:#}");
        }
    }

    #[test]
    fn context_does_not_hide_the_code() {
        let err = anyhow::Error::from(CliError::Interrupted).context("device info");
        assert_eq!(Exit::of(&err), Exit::Interrupted);
    }
}
