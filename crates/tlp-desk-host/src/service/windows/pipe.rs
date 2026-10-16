//! Named pipes with explicit security descriptors (Microsoft Learn: "Named Pipe Security and
//! Access Rights", `CreateNamedPipeW`, `ConnectNamedPipe`).
//!
//! Handles are synchronous (no `FILE_FLAG_OVERLAPPED`) and wrapped in `std::fs::File`, so
//! `Read`/`Write` map to `ReadFile`/`WriteFile`. One connection is used by one thread in one
//! direction at a time (see `ipc`), which is what synchronous pipe handles allow.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use windows::core::{HSTRING, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, HANDLE, HLOCAL,
    INVALID_HANDLE_VALUE,
};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, IsValidSid, RevertToSelf,
    TokenIntegrityLevel, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_MANDATORY_LABEL,
    TOKEN_QUERY,
};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeClientSessionId,
    GetNamedPipeServerProcessId, GetNamedPipeServerSessionId, ImpersonateNamedPipeClient,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
    PIPE_WAIT,
};
use windows::Win32::System::Threading::{
    GetCurrentThread, OpenProcess, OpenThreadToken, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

use super::os_err;
use crate::service::PipeSpec;
use crate::HostError;

/// Output buffer of a pipe instance: large enough for control traffic; video frames stream
/// through it (the writer blocks until the reader drains, which is the backpressure we want).
const OUT_BUFFER: u32 = 1024 * 1024;
const IN_BUFFER: u32 = 64 * 1024;

/// How long a client keeps retrying while every pipe instance is busy or not created yet.
const CONNECT_PATIENCE: Duration = Duration::from_secs(2);

/// A security descriptor from `ConvertStringSecurityDescriptorToSecurityDescriptorW`, freed
/// with `LocalFree`.
struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl SecurityDescriptor {
    fn from_sddl(sddl: &str) -> Result<Self, HostError> {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let wide = HSTRING::from(sddl);
        // SAFETY: `wide` is a valid NUL-terminated UTF-16 string that outlives the call; `sd`
        // is a valid out pointer. On success the system allocated the descriptor with
        // LocalAlloc and we free it in Drop.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &wide,
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
        }
        .map_err(|e| os_err("ConvertStringSecurityDescriptorToSecurityDescriptorW", &e))?;
        Ok(Self(sd))
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        if !self.0 .0.is_null() {
            // SAFETY: the pointer came from ConvertStringSecurityDescriptor... (LocalAlloc'd)
            // and is freed exactly once here.
            unsafe {
                LocalFree(Some(HLOCAL(self.0 .0)));
            }
        }
    }
}

fn handle(f: &File) -> HANDLE {
    HANDLE(f.as_raw_handle())
}

/// Creates one server instance of `spec`'s pipe. `first` adds `FILE_FLAG_FIRST_PIPE_INSTANCE`
/// (if `spec.first_instance`): creation then fails when another process already owns the
/// name.
pub(crate) fn create_instance(spec: &PipeSpec, first: bool) -> Result<File, HostError> {
    let sd = SecurityDescriptor::from_sddl(spec.sddl)?;
    let sa = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0),
        lpSecurityDescriptor: sd.0 .0,
        bInheritHandle: false.into(),
    };
    let mut open_mode = PIPE_ACCESS_DUPLEX;
    if first && spec.first_instance {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let mut pipe_mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT;
    if spec.reject_remote {
        pipe_mode |= PIPE_REJECT_REMOTE_CLIENTS;
    }
    let name = HSTRING::from(spec.name.as_str());
    let instances = spec.max_instances.clamp(1, PIPE_UNLIMITED_INSTANCES - 1);
    // SAFETY: `name` is NUL-terminated UTF-16; `sa` and the descriptor it points to are valid
    // for the duration of the call (`sd` lives until the end of this function).
    let h = unsafe {
        CreateNamedPipeW(
            &name,
            open_mode,
            pipe_mode,
            instances,
            OUT_BUFFER,
            IN_BUFFER,
            0,
            Some(&sa),
        )
    };
    if h == INVALID_HANDLE_VALUE || h.is_invalid() {
        return Err(os_err(
            "CreateNamedPipeW",
            &windows::core::Error::from_thread(),
        ));
    }
    // SAFETY: `h` is a fresh, valid handle we own; File takes ownership and closes it.
    Ok(unsafe { File::from_raw_handle(h.0 as RawHandle) })
}

/// Blocks until a client connects to this instance.
pub(crate) fn accept(pipe: &File) -> Result<(), HostError> {
    // SAFETY: the handle is a valid pipe server handle owned by `pipe`; synchronous call
    // without OVERLAPPED.
    match unsafe { ConnectNamedPipe(handle(pipe), None) } {
        Ok(()) => Ok(()),
        // The client connected between CreateNamedPipe and ConnectNamedPipe: fine.
        Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => Ok(()),
        Err(e) => Err(os_err("ConnectNamedPipe", &e)),
    }
}

/// Windows session of the connected client.
pub(crate) fn client_session_id(pipe: &File) -> Result<u32, HostError> {
    let mut id = 0u32;
    // SAFETY: valid pipe handle; `id` is a valid out pointer.
    unsafe { GetNamedPipeClientSessionId(handle(pipe), &mut id) }
        .map_err(|e| os_err("GetNamedPipeClientSessionId", &e))?;
    Ok(id)
}

/// Process id of the connected client (`GetNamedPipeClientProcessId`).
pub(crate) fn client_process_id(pipe: &File) -> Result<u32, HostError> {
    let mut id = 0u32;
    // SAFETY: valid pipe server handle; `id` is a valid out pointer.
    unsafe { GetNamedPipeClientProcessId(handle(pipe), &mut id) }
        .map_err(|e| os_err("GetNamedPipeClientProcessId", &e))?;
    Ok(id)
}

/// Full Win32 path of process `pid`'s executable (`QueryFullProcessImageNameW`).
pub(crate) fn process_image(pid: u32) -> Result<PathBuf, HostError> {
    // SAFETY: plain call; on success we own the process handle and close it below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map_err(|e| os_err("OpenProcess", &e))?;
    // Long-path capable buffer (32 767 UTF-16 units + NUL).
    let mut buf = vec![0u16; 32_768];
    let mut len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    // SAFETY: `process` is a valid handle with PROCESS_QUERY_LIMITED_INFORMATION; `buf` is
    // writable for `len` UTF-16 units and `len` is a valid in/out pointer.
    let queried = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    };
    // SAFETY: we opened `process` above and close it exactly once.
    let _ = unsafe { CloseHandle(process) };
    queried.map_err(|e| os_err("QueryFullProcessImageNameW", &e))?;
    let used = usize::try_from(len).unwrap_or(0);
    let wide = buf.get(..used).unwrap_or_default();
    Ok(PathBuf::from(OsString::from_wide(wide)))
}

/// Integrity RID (`SECURITY_MANDATORY_*_RID`) of the connected client's token: impersonate
/// the client, open the thread token, revert, then read `TokenIntegrityLevel`. Must be called
/// after a message was read from the client.
pub(crate) fn client_integrity_rid(pipe: &File) -> Result<u32, HostError> {
    // SAFETY: valid pipe server handle that a client is connected to.
    unsafe { ImpersonateNamedPipeClient(handle(pipe)) }
        .map_err(|e| os_err("ImpersonateNamedPipeClient", &e))?;
    let mut token = HANDLE::default();
    // SAFETY: the current thread pseudo handle; `token` is a valid out pointer. `true` =
    // check access as the service (the token belongs to the client).
    let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, true, &mut token) };
    // SAFETY: plain call; ends the impersonation started above. It must run whatever
    // OpenThreadToken did.
    if let Err(e) = unsafe { RevertToSelf() } {
        if opened.is_ok() {
            // SAFETY: the token was opened above and is closed once.
            let _ = unsafe { CloseHandle(token) };
        }
        // The thread may still run as the client: the caller ends this connection's thread.
        return Err(os_err("RevertToSelf", &e));
    }
    opened.map_err(|e| os_err("OpenThreadToken", &e))?;
    let rid = token_integrity_rid(token);
    // SAFETY: the token was opened above and is closed once.
    let _ = unsafe { CloseHandle(token) };
    rid
}

fn token_integrity_rid(token: HANDLE) -> Result<u32, HostError> {
    // u64 elements: TOKEN_MANDATORY_LABEL holds a pointer and needs pointer alignment.
    let mut buf = vec![0u64; 16];
    let bytes = u32::try_from(std::mem::size_of_val(buf.as_slice())).unwrap_or(0);
    let mut needed = 0u32;
    // SAFETY: `token` has TOKEN_QUERY; `buf` is writable for `bytes` bytes (a label is
    // a pointer plus a small SID, far below 128 bytes); `needed` is a valid out pointer.
    unsafe {
        GetTokenInformation(
            token,
            TokenIntegrityLevel,
            Some(buf.as_mut_ptr().cast()),
            bytes,
            &mut needed,
        )
    }
    .map_err(|e| os_err("GetTokenInformation", &e))?;
    // SAFETY: on success the buffer starts with a TOKEN_MANDATORY_LABEL whose SID points into
    // the same buffer, which outlives these reads. The SID is validated before it is parsed.
    unsafe {
        let label = &*buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
        let sid = label.Label.Sid;
        if !IsValidSid(sid).as_bool() {
            return Err(HostError::Os {
                call: "IsValidSid",
                code: 0,
            });
        }
        let count = u32::from(*GetSidSubAuthorityCount(sid));
        let last = count.checked_sub(1).ok_or(HostError::Os {
            call: "GetSidSubAuthorityCount",
            code: 0,
        })?;
        Ok(*GetSidSubAuthority(sid, last))
    }
}

/// Process id of the pipe's server (anti-squatting check on the client side).
pub(crate) fn server_process_id(pipe: &File) -> Result<u32, HostError> {
    let mut id = 0u32;
    // SAFETY: valid pipe client handle; `id` is a valid out pointer.
    unsafe { GetNamedPipeServerProcessId(handle(pipe), &mut id) }
        .map_err(|e| os_err("GetNamedPipeServerProcessId", &e))?;
    Ok(id)
}

/// Windows session of the pipe's server (0 for a service).
pub(crate) fn server_session_id(pipe: &File) -> Result<u32, HostError> {
    let mut id = 0u32;
    // SAFETY: valid pipe client handle; `id` is a valid out pointer.
    unsafe { GetNamedPipeServerSessionId(handle(pipe), &mut id) }
        .map_err(|e| os_err("GetNamedPipeServerSessionId", &e))?;
    Ok(id)
}

/// Opens a client connection; `access` is an explicit access mask (the GUI pipe does not
/// grant `GENERIC_WRITE`). Retries for a short while if the pipe is busy or not created yet.
pub(crate) fn connect(name: &str, access: Option<u32>) -> Result<File, HostError> {
    let deadline = Instant::now() + CONNECT_PATIENCE;
    loop {
        let mut opts = OpenOptions::new();
        match access {
            Some(mask) => {
                opts.access_mode(mask);
            }
            None => {
                opts.read(true).write(true);
            }
        }
        match opts.open(name) {
            Ok(f) => return Ok(f),
            Err(e) => {
                let busy = e.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32);
                let missing = e.kind() == std::io::ErrorKind::NotFound;
                if (busy || missing) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                return Err(e.into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use super::*;
    use crate::service::{GUI_CLIENT_ACCESS, GUI_PIPE_SDDL};

    fn unique(tag: &str) -> String {
        format!(
            r"\\.\pipe\TLP-Desk-test-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        )
        .replace(['(', ')'], "")
    }

    /// The agent pipe ACL really keeps out everyone but SYSTEM: this test process (a normal
    /// or admin user, never SYSTEM) is denied. Creates a local pipe only.
    #[test]
    fn agent_pipe_denies_non_system_clients() {
        let spec = PipeSpec {
            name: unique("agent"),
            ..PipeSpec::agent(1, 2, 3)
        };
        let server = create_instance(&spec, true).unwrap();
        let err = connect(&spec.name, None).unwrap_err();
        assert!(
            matches!(&err, HostError::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied),
            "{err:?}"
        );
        // First-instance flag: a second "first" instance of the same name fails (squatting).
        assert!(create_instance(&spec, true).is_err());
        drop(server);
    }

    /// The facts the GUI pipe admission check (audit F2) reads about a client, against this
    /// very test process as the client: its pid, its image and its integrity (a normal or
    /// elevated test run is Medium or High). Local test pipe only.
    #[test]
    fn client_facts_identify_the_connected_process() {
        let spec = PipeSpec {
            name: unique("facts"),
            // Test-only: the creator (owner) may connect; remote clients are rejected.
            sddl: "D:P(A;;GA;;;OW)(A;;GA;;;SY)",
            ..PipeSpec::gui()
        };
        let server = create_instance(&spec, true).unwrap();
        let name = spec.name.clone();
        let t = std::thread::spawn(move || {
            accept(&server).unwrap();
            let mut s = server;
            let mut b = [0u8; 1];
            // Impersonation needs a message from the client first.
            s.read_exact(&mut b).unwrap();
            let pid = client_process_id(&s).unwrap();
            let image = process_image(pid).unwrap();
            let rid = client_integrity_rid(&s).unwrap();
            s.write_all(&b).unwrap();
            (pid, image, rid)
        });
        let mut c = connect(&name, None).unwrap();
        c.write_all(b"x").unwrap();
        let mut back = [0u8; 1];
        c.read_exact(&mut back).unwrap();
        let (pid, image, rid) = t.join().unwrap();
        assert_eq!(pid, std::process::id());
        assert!(crate::service::same_image(
            &image,
            &std::env::current_exe().unwrap()
        ));
        assert!(rid >= crate::service::MEDIUM_INTEGRITY_RID, "{rid:#x}");
    }

    /// Interactive users get data access to the GUI pipe, but only with the explicit mask:
    /// asking for GENERIC_WRITE (which includes FILE_CREATE_PIPE_INSTANCE) is denied.
    /// Needs an interactive logon (the INTERACTIVE SID), so it is ignored in CI.
    #[test]
    #[ignore = "needs an interactive logon session"]
    fn gui_pipe_admits_interactive_users_with_data_rights_only() {
        let spec = PipeSpec {
            name: unique("gui"),
            sddl: GUI_PIPE_SDDL,
            ..PipeSpec::gui()
        };
        let server = create_instance(&spec, true).unwrap();
        let name = spec.name.clone();
        let t = std::thread::spawn(move || {
            accept(&server).unwrap();
            let mut s = server;
            let mut buf = [0u8; 5];
            s.read_exact(&mut buf).unwrap();
            s.write_all(&buf).unwrap();
            s
        });
        assert!(
            connect(&name, None).is_err(),
            "generic write must be denied"
        );
        let mut c = connect(&name, Some(GUI_CLIENT_ACCESS)).unwrap();
        c.write_all(b"hello").unwrap();
        let mut back = [0u8; 5];
        c.read_exact(&mut back).unwrap();
        assert_eq!(&back, b"hello");
        assert_eq!(server_process_id(&c).unwrap(), std::process::id());
        let server = t.join().unwrap();
        assert_eq!(
            client_session_id(&server).unwrap(),
            super::super::current_session_id().unwrap()
        );
    }
}
