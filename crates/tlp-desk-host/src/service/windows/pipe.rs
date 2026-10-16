//! Named pipes with explicit security descriptors (Microsoft Learn: "Named Pipe Security and
//! Access Rights", `CreateNamedPipeW`, `ConnectNamedPipe`).
//!
//! Handles are synchronous (no `FILE_FLAG_OVERLAPPED`) and wrapped in `std::fs::File`, so
//! `Read`/`Write` map to `ReadFile`/`WriteFile`. One connection is used by one thread in one
//! direction at a time (see `ipc`), which is what synchronous pipe handles allow.

use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::time::{Duration, Instant};

use windows::core::HSTRING;
use windows::Win32::Foundation::{
    LocalFree, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, HANDLE, HLOCAL, INVALID_HANDLE_VALUE,
};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientSessionId, GetNamedPipeServerProcessId,
    GetNamedPipeServerSessionId, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
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
