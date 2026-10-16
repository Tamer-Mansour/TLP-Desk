//! Starting processes in another Windows session from the service (Microsoft Learn:
//! `WTSGetActiveConsoleSessionId`, `WTSQueryUserToken`, `DuplicateTokenEx`,
//! `SetTokenInformation(TokenSessionId)`, `CreateProcessAsUserW`, `CreateEnvironmentBlock`).
//!
//! * The **agent** runs with a duplicate of the service's own LocalSystem token moved into the
//!   console session: SYSTEM is required to open the `Winlogon` desktop (lock screen, login
//!   screen, UAC), and it works before anyone logs on.
//! * The **GUI** (to show an Accept prompt) runs with the logged-on user's token
//!   (`WTSQueryUserToken`), i.e. as that user, never elevated.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::{Path, PathBuf};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::Security::{
    DuplicateTokenEx, SecurityIdentification, SetTokenInformation, TokenPrimary, TokenSessionId,
    TOKEN_ALL_ACCESS, TOKEN_DUPLICATE, TOKEN_QUERY,
};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::RemoteDesktop::{
    ProcessIdToSessionId, WTSGetActiveConsoleSessionId, WTSQueryUserToken,
};
use windows::Win32::System::Threading::{
    CreateProcessAsUserW, GetCurrentProcess, GetExitCodeProcess, OpenProcess, OpenProcessToken,
    TerminateProcess, WaitForSingleObject, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, INFINITE,
    PROCESS_INFORMATION, PROCESS_SYNCHRONIZE, STARTUPINFOW,
};

use super::os_err;
use crate::service::{agent_pipe_name, console_session, AgentLauncher, AgentProcess, AGENT_ARG};
use crate::HostError;

/// Whose token the new process gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchAs {
    /// The service's LocalSystem token, moved into the target session (the agent).
    System,
    /// The user logged on to the target session (the GUI).
    User,
}

fn owned(h: HANDLE) -> OwnedHandle {
    // SAFETY: callers pass a fresh, valid handle they own exclusively.
    unsafe { OwnedHandle::from_raw_handle(h.0 as RawHandle) }
}

fn raw(h: &OwnedHandle) -> HANDLE {
    HANDLE(h.as_raw_handle())
}

/// The session attached to the physical console, if any.
pub fn active_console_session() -> Option<u32> {
    // SAFETY: no arguments; always safe to call.
    console_session(unsafe { WTSGetActiveConsoleSessionId() })
}

/// Session of the current process.
pub fn current_session_id() -> Result<u32, HostError> {
    let mut id = 0u32;
    // SAFETY: `id` is a valid out pointer.
    unsafe { ProcessIdToSessionId(std::process::id(), &mut id) }
        .map_err(|e| os_err("ProcessIdToSessionId", &e))?;
    Ok(id)
}

/// Whether a user is logged on to `session` (only callable as LocalSystem).
pub fn user_logged_on(session: u32) -> bool {
    user_token(session).is_ok()
}

fn user_token(session: u32) -> Result<OwnedHandle, HostError> {
    let mut token = HANDLE::default();
    // SAFETY: `token` is a valid out pointer; on success we own the returned handle.
    unsafe { WTSQueryUserToken(session, &mut token) }
        .map_err(|e| os_err("WTSQueryUserToken", &e))?;
    Ok(owned(token))
}

fn system_token_in(session: u32) -> Result<OwnedHandle, HostError> {
    let mut own = HANDLE::default();
    // SAFETY: GetCurrentProcess returns a pseudo handle; `own` is a valid out pointer and we
    // own the returned token handle.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_QUERY, &mut own) }
        .map_err(|e| os_err("OpenProcessToken", &e))?;
    let own = owned(own);
    let mut dup = HANDLE::default();
    // SAFETY: `own` is a valid token handle with TOKEN_DUPLICATE; `dup` is a valid out
    // pointer and we own the new primary token.
    unsafe {
        DuplicateTokenEx(
            raw(&own),
            TOKEN_ALL_ACCESS,
            None,
            SecurityIdentification,
            TokenPrimary,
            &mut dup,
        )
    }
    .map_err(|e| os_err("DuplicateTokenEx", &e))?;
    let dup = owned(dup);
    // SAFETY: `dup` is a primary token we own; the buffer is a u32 of the stated size.
    // Changing the session id needs SeTcbPrivilege, which LocalSystem holds.
    unsafe {
        SetTokenInformation(
            raw(&dup),
            TokenSessionId,
            std::ptr::from_ref(&session).cast(),
            4,
        )
    }
    .map_err(|e| os_err("SetTokenInformation", &e))?;
    Ok(dup)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Quotes one command-line argument for `CommandLineToArgvW` rules (paths may contain spaces
/// and Arabic letters; neither needs more than quotes, but backslashes before a quote do).
fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_owned();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// Builds the command line `"<exe>" args...`.
pub(crate) fn command_line(exe: &Path, args: &[&str]) -> String {
    let mut s = quote(&exe.to_string_lossy());
    for a in args {
        s.push(' ');
        s.push_str(&quote(a));
    }
    s
}

/// A process started by [`launch`].
#[derive(Debug)]
pub struct WinProcess {
    handle: OwnedHandle,
    pid: u32,
}

impl AgentProcess for WinProcess {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn try_wait(&mut self) -> Option<u32> {
        // SAFETY: valid process handle owned by self; zero timeout never blocks.
        let r = unsafe { WaitForSingleObject(raw(&self.handle), 0) };
        if r != WAIT_OBJECT_0 {
            return None;
        }
        let mut code = 0u32;
        // SAFETY: valid process handle; `code` is a valid out pointer.
        match unsafe { GetExitCodeProcess(raw(&self.handle), &mut code) } {
            Ok(()) => Some(code),
            Err(_) => Some(u32::MAX),
        }
    }

    fn terminate(&mut self) {
        // SAFETY: valid process handle with PROCESS_TERMINATE (we created the process).
        if let Err(e) = unsafe { TerminateProcess(raw(&self.handle), 1) } {
            tracing::debug!(%e, pid = self.pid, "TerminateProcess failed (already gone?)");
        }
    }
}

/// Starts `exe args` in Windows session `session` on `winsta0\default` with the chosen token.
pub fn launch(
    session: u32,
    as_: LaunchAs,
    exe: &Path,
    args: &[&str],
) -> Result<WinProcess, HostError> {
    let token = match as_ {
        LaunchAs::System => system_token_in(session)?,
        LaunchAs::User => user_token(session)?,
    };
    let mut env: *mut core::ffi::c_void = std::ptr::null_mut();
    // SAFETY: `env` is a valid out pointer; the block is freed with DestroyEnvironmentBlock
    // below. A failure leaves `env` null and we pass no environment (inherit).
    let env_ok = unsafe { CreateEnvironmentBlock(&mut env, Some(raw(&token)), false) }.is_ok();
    let mut desktop = wide(r"winsta0\default");
    let mut cmd = wide(&command_line(exe, args));
    let si = STARTUPINFOW {
        cb: u32::try_from(std::mem::size_of::<STARTUPINFOW>()).unwrap_or(0),
        lpDesktop: PWSTR(desktop.as_mut_ptr()),
        ..Default::default()
    };
    let mut pi = PROCESS_INFORMATION::default();
    // SAFETY: every pointer is valid for the call: `cmd` and `desktop` are mutable,
    // NUL-terminated UTF-16 buffers that outlive it; `env` is a block from
    // CreateEnvironmentBlock (Unicode, hence CREATE_UNICODE_ENVIRONMENT) or null; `si`/`pi`
    // are initialised structs. On success we own the returned process and thread handles.
    let res = unsafe {
        CreateProcessAsUserW(
            Some(raw(&token)),
            PCWSTR::null(),
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
            env_ok.then_some(env.cast_const()),
            PCWSTR::null(),
            &si,
            &mut pi,
        )
    };
    if env_ok {
        // SAFETY: `env` came from CreateEnvironmentBlock and is freed once.
        let _ = unsafe { DestroyEnvironmentBlock(env) };
    }
    res.map_err(|e| os_err("CreateProcessAsUserW", &e))?;
    drop(owned(pi.hThread));
    Ok(WinProcess {
        handle: owned(pi.hProcess),
        pid: pi.dwProcessId,
    })
}

/// Blocks until process `pid` exits (the agent watches its service this way). Returns at
/// once if the process cannot be opened (already gone).
pub fn wait_for_exit(pid: u32) {
    // SAFETY: OpenProcess with SYNCHRONIZE only; on success we own the handle.
    let Ok(h) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }) else {
        return;
    };
    let h = owned(h);
    // SAFETY: valid process handle with SYNCHRONIZE.
    unsafe {
        WaitForSingleObject(raw(&h), INFINITE);
    }
}

/// Starts session agents: `TLP-Desk.exe --agent --pipe <name> --parent <service pid>` as
/// SYSTEM in the target session.
#[derive(Debug, Clone)]
pub struct WinAgentLauncher {
    /// Path of `TLP-Desk.exe` (the service's own executable).
    pub exe: PathBuf,
    pub service_pid: u32,
}

impl AgentLauncher for WinAgentLauncher {
    type Process = WinProcess;

    fn launch(&mut self, session: u32, seq: u32) -> Result<WinProcess, HostError> {
        let pipe = agent_pipe_name(session, self.service_pid, seq);
        let parent = self.service_pid.to_string();
        let p = launch(
            session,
            LaunchAs::System,
            &self.exe,
            &[AGENT_ARG, "--pipe", &pipe, "--parent", &parent],
        )?;
        tracing::info!(session, pid = p.pid, %pipe, "session agent started");
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lines_quote_spaces_quotes_and_arabic() {
        let exe = Path::new(r"C:\Program Files\TLP-Desk\TLP-Desk.exe");
        assert_eq!(
            command_line(
                exe,
                &["--agent", "--pipe", r"\\.\pipe\TLP-Desk-agent-1-2-3"]
            ),
            r#""C:\Program Files\TLP-Desk\TLP-Desk.exe" --agent --pipe \\.\pipe\TLP-Desk-agent-1-2-3"#
        );
        assert_eq!(quote(r"C:\مجلد تامر\a.exe"), r#""C:\مجلد تامر\a.exe""#);
        assert_eq!(quote(r#"a"b"#), r#""a\"b""#);
        assert_eq!(quote(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(quote(""), r#""""#);
    }

    /// Needs to run as LocalSystem in a service; never run on a developer PC.
    #[test]
    #[ignore = "needs LocalSystem (run inside the installed service)"]
    fn launch_agent_in_console_session() {
        let session = active_console_session().expect("console session");
        let mut p = launch(
            session,
            LaunchAs::System,
            Path::new(r"C:\Windows\System32\cmd.exe"),
            &["/c", "exit", "7"],
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        assert_eq!(p.try_wait(), Some(7));
    }
}
