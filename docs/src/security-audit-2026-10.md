# Service and IPC audit (2026-10)

A review of the TLP-Desk Windows service (SYSTEM, session 0), its session agent, the two
local named pipes, the data directory and the parts of `tlp-desk-crypto` they use. It was done
before 1.0.0. The fixes ship in the same release; each one has a regression test.

| ID | Severity | Finding | Status |
|---|---|---|---|
| F1 | Medium | Data directory hardened only when first created | Fixed |
| F2 | Low–Medium | GUI pipe trusts any process in the console session | Fixed |
| F3 | Low | Input-desktop handle leaked per capture/input thread | Fixed |
| F4 | Low | Ticket redemption succeeds when the store cannot be saved | Fixed |
| F5 | Low | Credentials changed while the service runs are not reloaded | Fixed |
| F6 | Info | Broad default grant and un-gated secure-desktop interaction | Accepted, documented |
| F7 | Low (DoS) | Predictable agent pipe name can be squatted | Accepted, follow-up |

## F1: data directory hardened only on creation (Medium)

`%ProgramData%\TLP-Desk` holds the identity key (the device ID), the permanent-password
verifier and the reconnect tickets. `Identity::save` and `write_private` ran `harden_dir` only
when they created the folder. A folder that already existed kept its ACL. That folder could
come from an older build, from someone creating it by hand, or from a non-admin user:
`%ProgramData%` lets every user create sub-folders, and the owner can still change the ACL.

**Fix.** `ensure_private_dir` creates the folder if needed and re-applies the private ACL
before every secret write (identity, password, tickets). On every start the service runs
`secure_service_dir`. It sets the owner of the folder and everything in it to
Administrators (`takeown /A`, `icacls /setowner`), drops explicit ACEs (`icacls /reset /T`),
then turns inheritance off with SYSTEM + Administrators (+ OWNER RIGHTS) only. The service
does not read the owner first. It sets it on every start, which gives the same result. When
the service is installed, the installer creates `{commonappdata}\TLP-Desk` (`[Dirs]`) and
runs the same steps. When the service is not installed the installer leaves the folder alone,
so an attended install keeps its own identity.
Tests: `existing_dir_is_rehardened_before_a_secret_is_written`,
`service_dir_repair_steps_are_ordered_and_admin_owned`.

## F2: GUI pipe trusts the whole console session (Low–Medium)

The GUI pipe (`\\.\pipe\TLP-Desk-gui`) accepts privileged commands: answering Accept prompts,
changing a session's permissions and rotating the one-time code. The status snapshot it sends
also contains that code. The service checked only that the client ran in the active console
session. Any process there could use the pipe, including a sandboxed Low-integrity one.

**Fix.** The service reads the hello first, then checks the client before it sends anything:

- the integrity level of the client's token must be Medium or higher
  (`ImpersonateNamedPipeClient` → `OpenThreadToken` → `GetTokenInformation(TokenIntegrityLevel)`
  → `RevertToSelf`);
- the client's image must be the `TLP-Desk.exe` the service runs from
  (`GetNamedPipeClientProcessId` → `QueryFullProcessImageNameW`, compared case-insensitively,
  with or without the `\\?\` prefix);
- the client must be in the active console session, as before.

If any of these values cannot be read, the client is refused. The Accept popup
(`TLP-Desk.exe --accept-prompt`) is the same executable, so it passes. The decision itself is
the pure function `check_gui_client`. Residual risk: a process running as the same user can
still inject code into a running `TLP-Desk.exe`, and a PID can be reused between the two
checks. An attacker who can do either already controls that user's desktop.
Tests: `gui_client_admission_fails_closed`, `image_paths_compare_like_windows_does`,
`client_facts_identify_the_connected_process` (real local pipe against the test process).

## F3: input-desktop handle leak (Low)

`WinDesktopSource` kept the handle of its last `OpenInputDesktop` and never closed it, so
each capture/input thread leaked one handle when it ended. `Drop` now puts the thread back on
its original desktop (Windows cannot close a desktop that a thread is still assigned to), then
closes the handle. Test (ignored, needs the interactive desktop; run manually):
`dropping_the_source_closes_the_attached_desktop`. It failed before the fix.

## F4: ticket redemption ignored a failed save (Low)

`TicketStore::redeem` removed the ticket in memory and logged a failed save, but still
returned success. The file still listed the ticket, so it would work once more after the next
restart. Redemption now fails closed with `TicketRejected::NotPersisted`, which the viewer sees
as wrong credentials. The ticket stays used up in memory.
Test: `redemption_fails_closed_when_the_consumption_cannot_be_saved` (real failing write).

## F5: credentials not reloaded at run time (Low)

When the GUI set or removed the permanent password while the service was running, the service
kept the old one until it restarted. The new GUI-pipe command `GuiCommand::ReloadCredentials`
fixes this. It is a local IPC command; the network protocol does not change. On this command
the service re-reads the password file and `TicketStore::reload` re-reads the tickets. An
unreadable password file disables the password. Tickets revoked on disk are dropped. The GUI
sends the command after it saves the unattended password.
Tests: `reload_picks_up_password_and_ticket_changes`,
`reload_drops_revoked_tickets_and_keeps_own_instance`, `gui_commands_reach_the_host`.

## F6: default grant breadth and the secure desktop (Info, accepted)

`service_entry.rs` adds FILES, CHAT, WHITEBOARD, RESTART and SYSTEM to the service's default
permissions. So every successful unattended login (one-time code, permanent password, Accept,
reconnect ticket) can by default press Ctrl+Alt+Del, lock the PC and restart it. Separately,
the agent captures and injects input on the Winlogon secure desktop whenever it is the input
desktop. A viewer with only VIEW + CONTROL + KEYBOARD can therefore see and type into the lock
screen, the sign-in screen and UAC prompts. Only SendSAS and LockWorkstation require the SYSTEM
bit (`system.rs`, `decide()`). Unattended access is meant to work this way. Anyone who
can authenticate to the service should be treated as able to operate the whole machine,
including the secure desktop. The host user can still revoke permissions per session at any
time. Follow-up: let the machine-wide host settings choose these defaults.

## F7: predictable agent pipe name (Low, denial of service only)

The agent pipe is named `\\.\pipe\TLP-Desk-agent-<session>-<service PID>-<seq>`, and `seq`
starts at 1. The SYSTEM agent creates it with `FILE_FLAG_FIRST_PIPE_INSTANCE`. A local
unprivileged process can create that name first. The agent then exits, and the supervisor
retries with the next `seq`, which the squatter can also take. This blocks unattended capture
and input. It cannot intercept anything: before sending, the service checks that the pipe
server's PID is the agent it launched (`PipeConnector::connect` → `HostError::PipeImpostor`).
Follow-up: add randomness to the name and raise an alert after repeated first-instance
failures.

## Checked, no issue

- GUI pipe SDDL `D:P(D;;GA;;;NU)(A;;GA;;;SY)(A;;0x120083;;;IU)`: network logons denied,
  SYSTEM full, interactive users data access only (no `FILE_CREATE_PIPE_INSTANCE`, `WRITE_DAC`
  or `WRITE_OWNER`; tests assert this). The agent pipe is SYSTEM-only.
- Both pipes use `PIPE_REJECT_REMOTE_CLIENTS` and `FILE_FLAG_FIRST_PIPE_INSTANCE`. The GUI
  checks that the pipe server runs in session 0. The service checks that the agent pipe server
  PID equals the launched agent. The GUI server refuses clients outside the console session
  before it sends the snapshot.
- The IPC decoder caps every length before it allocates (header 256 KiB, payload 64 MiB) and
  does not panic on hostile input. `RemoteCapturer` bounds frame dimensions (16384) and the
  cursor image size.
- `CreateProcessAsUserW`: fully quoted absolute exe path, `bInheritHandles = FALSE`,
  `CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT`, environment from the token. The agent exe is
  the service's own executable under Program Files. `CREATE_NO_WINDOW` and UIPI limit
  shatter-style attacks.
- SendSAS needs the SYSTEM permission on an authenticated session, is throttled to once a
  second, and no `GuiCommand` exposes it. Writing the `SoftwareSASGeneration` policy keeps its
  existing bits.
- Service install: quoted binary path, LocalSystem (needed for `WTSQueryUserToken`,
  `OpenInputDesktop`, SendSAS), default service DACL. Installer: app folder under Program
  Files, no weakened permissions, firewall rules for the Private and Domain profiles only.
- Reconnect tickets: only SHA-256 hashes stored, `Debug` redacted, single use under one mutex
  with constant-time compare, bound to the peer fingerprint, 10-minute expiry, at most 64,
  saved before `issue` returns, cannot be redeemed by the process that issued them.
- Permanent password: Argon2id verifier only, constant-time compare, atomic private write,
  minimum 8 characters. Rate limit: 5 attempts per minute per peer, 10-minute lockout, old
  entries pruned.
- Identity: ECDSA P-256, only the private key is saved, `Debug` never shows it. No secrets are
  logged anywhere in `service/` or `tlp-desk-crypto`.
- Peer names are sanitized and limited to 64 characters before they reach the GUI pipe.
- `SessionTracker` and `AgentSupervisor` run the agent only in the active console session and
  stop it on `Drop`. The desktop follower treats unknown desktops as secure (fail-safe).
- `unsafe` review of `launch.rs`, `pipe.rs`, `desktop.rs`, `sas.rs` and `scm.rs`: sizes and
  ownership are correct apart from the F3 leak.

## Not audited in this pass

Two planned sub-audits did not finish: the shell clipboard / Explorer file paste
(`shellclip`, clipboard files), and app persistence together with CLI input handling. Those
areas are **not covered** by this report.
