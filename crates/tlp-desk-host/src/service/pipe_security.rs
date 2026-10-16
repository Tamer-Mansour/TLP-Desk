//! Names and access control of the service's named pipes.
//!
//! Two pipes exist:
//!
//! * **Agent pipe** (created by the session agent, opened by the service): carries the screen
//!   and injects input as SYSTEM, so only `SYSTEM` may open it. Its name contains the
//!   session, the service PID and a counter, and the service additionally checks that the
//!   pipe's server process is the agent it launched (`GetNamedPipeServerProcessId`), which
//!   defeats a process that creates the name first ("pipe squatting").
//! * **GUI pipe** (created by the service, opened by the logged-in user's GUI): interactive
//!   users may read and write, but **not** create pipe instances (`FILE_CREATE_PIPE_INSTANCE`
//!   is the same bit as `FILE_APPEND_DATA`, so `GENERIC_WRITE` must not be granted). The
//!   service also requires the client to be in the active console session, to have at
//!   least Medium integrity and to be the service's own `TLP-Desk.exe`
//!   ([`check_gui_client`]); the GUI checks that the server runs in session 0 (only
//!   services do).
//!
//! Both pipes are created with `PIPE_REJECT_REMOTE_CLIENTS` and an explicit deny for network
//! logons: nothing here is reachable from another machine.
//!
//! Security descriptors are SDDL strings (Microsoft Learn: "Security Descriptor String
//! Format"); they are converted with `ConvertStringSecurityDescriptorToSecurityDescriptorW`
//! in the platform module.

/// SDDL of the agent pipe: protected DACL, network logons denied, full access for
/// LocalSystem only.
pub const AGENT_PIPE_SDDL: &str = "D:P(D;;GA;;;NU)(A;;GA;;;SY)";

/// Access mask granted to interactive users on the GUI pipe:
/// `FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE`.
/// Deliberately without `FILE_CREATE_PIPE_INSTANCE` (0x4) and `FILE_WRITE_ATTRIBUTES`.
pub const GUI_CLIENT_ACCESS: u32 = 0x0001 | 0x0002 | 0x0080 | 0x0002_0000 | 0x0010_0000;

/// SDDL of the GUI pipe: protected DACL, network logons denied, SYSTEM full access,
/// interactive users read/write data only ([`GUI_CLIENT_ACCESS`]).
pub const GUI_PIPE_SDDL: &str = "D:P(D;;GA;;;NU)(A;;GA;;;SY)(A;;0x120083;;;IU)";

/// Name of the GUI pipe (one per machine; created by the service).
pub const GUI_PIPE_NAME: &str = r"\\.\pipe\TLP-Desk-gui";

/// How a pipe is created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipeSpec {
    /// Full pipe path (`\\.\pipe\...`).
    pub name: String,
    /// Security descriptor of the pipe (SDDL).
    pub sddl: &'static str,
    /// `FILE_FLAG_FIRST_PIPE_INSTANCE` on the first instance: fail if someone else already
    /// owns the name instead of silently joining their pipe.
    pub first_instance: bool,
    /// `PIPE_REJECT_REMOTE_CLIENTS`. Always true; a field so tests can assert it.
    pub reject_remote: bool,
    /// Concurrent instances (connections) allowed.
    pub max_instances: u32,
}

impl PipeSpec {
    /// The agent pipe for `session`, created by the agent of service process `service_pid`;
    /// `seq` makes names unique across agent restarts.
    pub fn agent(session: u32, service_pid: u32, seq: u32) -> Self {
        Self {
            name: agent_pipe_name(session, service_pid, seq),
            sddl: AGENT_PIPE_SDDL,
            first_instance: true,
            reject_remote: true,
            // Capture + input per host session, plus command and events.
            max_instances: 64,
        }
    }

    /// The GUI pipe (created by the service).
    pub fn gui() -> Self {
        Self {
            name: GUI_PIPE_NAME.to_owned(),
            sddl: GUI_PIPE_SDDL,
            first_instance: true,
            reject_remote: true,
            max_instances: 16,
        }
    }
}

/// `\\.\pipe\TLP-Desk-agent-<session>-<service pid>-<seq>`.
pub fn agent_pipe_name(session: u32, service_pid: u32, seq: u32) -> String {
    format!(r"\\.\pipe\TLP-Desk-agent-{session}-{service_pid}-{seq}")
}

/// Whether `name` is a well-formed agent pipe name (the agent refuses anything else on its
/// command line, so the service cannot be tricked into serving an arbitrary path).
pub fn is_agent_pipe_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(r"\\.\pipe\TLP-Desk-agent-") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('-').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 10 && p.bytes().all(|b| b.is_ascii_digit()))
}

/// `SECURITY_MANDATORY_MEDIUM_RID`: the integrity level of a normal user process. Low
/// (`0x1000`, sandboxes, browser renderers) and Untrusted (`0`) clients are refused.
pub const MEDIUM_INTEGRITY_RID: u32 = 0x2000;

/// What the service found out about a GUI pipe client. `None` = the query failed, which
/// always refuses the client (fail closed).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuiClientFacts {
    /// Windows session of the client (`GetNamedPipeClientSessionId`).
    pub session: Option<u32>,
    /// The active console session (`WTSGetActiveConsoleSessionId`).
    pub console_session: Option<u32>,
    /// Integrity RID of the client's token (`ImpersonateNamedPipeClient` ->
    /// `OpenThreadToken` -> `GetTokenInformation(TokenIntegrityLevel)` -> `RevertToSelf`).
    pub integrity_rid: Option<u32>,
    /// Image of the client process (`GetNamedPipeClientProcessId` ->
    /// `QueryFullProcessImageNameW`).
    pub image: Option<std::path::PathBuf>,
}

/// Why a GUI pipe client was refused (for the service log).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuiClientRefusal {
    /// Not in the active console session (or a session could not be read).
    NotConsoleSession,
    /// Integrity below Medium, or unreadable.
    IntegrityTooLow,
    /// Not the `TLP-Desk.exe` the service runs from, or unreadable.
    ForeignImage,
}

/// The GUI pipe's admission rule (audit F2). The pipe ACL already limits it to interactive
/// users; on top of that the client must
///
/// * run in the active console session (the person at the machine decides);
/// * have at least Medium integrity, so a sandboxed Low-integrity process of the same user
///   cannot answer Accept prompts, change permissions or read the one-time code;
/// * be the very `TLP-Desk.exe` the service runs from (`expected_exe`), so another program
///   in the session cannot either. The Accept popup (`TLP-Desk.exe --accept-prompt`) is the
///   same executable and passes.
///
/// # Errors
/// The first rule that does not hold, unreadable facts included.
pub fn check_gui_client(
    facts: &GuiClientFacts,
    expected_exe: &std::path::Path,
) -> Result<(), GuiClientRefusal> {
    match (facts.session, facts.console_session) {
        (Some(s), Some(c)) if s == c => {}
        _ => return Err(GuiClientRefusal::NotConsoleSession),
    }
    if !facts
        .integrity_rid
        .is_some_and(|rid| rid >= MEDIUM_INTEGRITY_RID)
    {
        return Err(GuiClientRefusal::IntegrityTooLow);
    }
    if !facts
        .image
        .as_deref()
        .is_some_and(|image| same_image(image, expected_exe))
    {
        return Err(GuiClientRefusal::ForeignImage);
    }
    Ok(())
}

/// Whether two Windows executable paths name the same file: case-insensitive, `/` and `\`
/// alike, with or without the `\\?\` prefix (`std::env::current_exe` and
/// `QueryFullProcessImageNameW` may differ in exactly these ways).
pub fn same_image(a: &std::path::Path, b: &std::path::Path) -> bool {
    fn norm(p: &std::path::Path) -> String {
        let s = p.to_string_lossy().replace('/', "\\");
        let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
        s.to_lowercase()
    }
    let (a, b) = (norm(a), norm(b));
    !a.is_empty() && a == b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trusted() -> GuiClientFacts {
        GuiClientFacts {
            session: Some(1),
            console_session: Some(1),
            integrity_rid: Some(MEDIUM_INTEGRITY_RID),
            image: Some(r"C:\Program Files\TLP-Desk\TLP-Desk.exe".into()),
        }
    }

    const EXE: &str = r"C:\Program Files\TLP-Desk\TLP-Desk.exe";

    /// Regression test for audit F2: only TLP-Desk.exe itself, at Medium integrity or above,
    /// in the console session, may use the GUI pipe; every unreadable fact refuses.
    #[test]
    fn gui_client_admission_fails_closed() {
        let exe = std::path::Path::new(EXE);
        assert_eq!(check_gui_client(&trusted(), exe), Ok(()));
        // Elevated GUI (High) and the accept popup (same exe) pass.
        let high = GuiClientFacts {
            integrity_rid: Some(0x3000),
            ..trusted()
        };
        assert_eq!(check_gui_client(&high, exe), Ok(()));
        let popup = GuiClientFacts {
            image: Some(r"\\?\c:\program files\tlp-desk\TLP-DESK.EXE".into()),
            ..trusted()
        };
        assert_eq!(check_gui_client(&popup, exe), Ok(()));

        for (facts, why) in [
            (
                GuiClientFacts {
                    session: Some(2),
                    ..trusted()
                },
                GuiClientRefusal::NotConsoleSession,
            ),
            (
                GuiClientFacts {
                    console_session: None,
                    ..trusted()
                },
                GuiClientRefusal::NotConsoleSession,
            ),
            (
                GuiClientFacts {
                    integrity_rid: Some(0x1000),
                    ..trusted()
                },
                GuiClientRefusal::IntegrityTooLow,
            ),
            (
                GuiClientFacts {
                    integrity_rid: None,
                    ..trusted()
                },
                GuiClientRefusal::IntegrityTooLow,
            ),
            (
                GuiClientFacts {
                    image: Some(r"C:\Users\سارة\Downloads\TLP-Desk.exe".into()),
                    ..trusted()
                },
                GuiClientRefusal::ForeignImage,
            ),
            (
                GuiClientFacts {
                    image: Some(r"C:\Windows\System32\cmd.exe".into()),
                    ..trusted()
                },
                GuiClientRefusal::ForeignImage,
            ),
            (
                GuiClientFacts {
                    image: None,
                    ..trusted()
                },
                GuiClientRefusal::ForeignImage,
            ),
            (
                GuiClientFacts::default(),
                GuiClientRefusal::NotConsoleSession,
            ),
        ] {
            assert_eq!(check_gui_client(&facts, exe), Err(why), "{facts:?}");
        }
    }

    #[test]
    fn image_paths_compare_like_windows_does() {
        use std::path::Path;
        assert!(same_image(Path::new(EXE), Path::new(&EXE.to_uppercase())));
        assert!(same_image(
            Path::new(r"\\?\D:\برامج\TLP-Desk.exe"),
            Path::new("D:/برامج/tlp-desk.exe")
        ));
        assert!(!same_image(Path::new(EXE), Path::new(r"C:\TLP-Desk.exe")));
        assert!(!same_image(Path::new(""), Path::new("")));
    }

    /// One `(type;flags;rights;;;sid)` ACE of an SDDL DACL.
    #[derive(Debug)]
    struct Ace<'a> {
        kind: &'a str,
        rights: &'a str,
        sid: &'a str,
    }

    fn aces(sddl: &str) -> Vec<Ace<'_>> {
        let dacl = sddl.strip_prefix("D:P").expect("protected DACL");
        dacl.split(')')
            .filter(|s| !s.is_empty())
            .map(|s| {
                let f: Vec<&str> = s.trim_start_matches('(').split(';').collect();
                assert_eq!(f.len(), 6, "ACE {s}");
                Ace {
                    kind: f[0],
                    rights: f[2],
                    sid: f[5],
                }
            })
            .collect()
    }

    fn mask(rights: &str) -> u32 {
        u32::from_str_radix(rights.trim_start_matches("0x"), 16).unwrap()
    }

    #[test]
    fn agent_pipe_allows_system_only() {
        let spec = PipeSpec::agent(2, 1234, 7);
        assert_eq!(spec.name, r"\\.\pipe\TLP-Desk-agent-2-1234-7");
        assert!(spec.reject_remote && spec.first_instance);
        let a = aces(spec.sddl);
        let allows: Vec<_> = a.iter().filter(|a| a.kind == "A").collect();
        assert_eq!(allows.len(), 1);
        assert_eq!(allows[0].sid, "SY");
        // Network logons are denied, and the deny comes first.
        assert_eq!(a[0].kind, "D");
        assert_eq!(a[0].sid, "NU");
    }

    #[test]
    fn gui_pipe_lets_interactive_users_talk_but_not_create_instances() {
        let spec = PipeSpec::gui();
        assert!(spec.reject_remote && spec.first_instance);
        let a = aces(spec.sddl);
        assert_eq!(a[0].kind, "D");
        assert_eq!(a[0].sid, "NU");
        // Nobody but SYSTEM and interactive users is granted anything: no Everyone (WD),
        // anonymous (AN), authenticated users (AU), built-in users (BU) or network (NU).
        for ace in a.iter().filter(|a| a.kind == "A") {
            assert!(["SY", "IU"].contains(&ace.sid), "unexpected grant {ace:?}");
        }
        let iu = a.iter().find(|a| a.sid == "IU").unwrap();
        assert!(iu.rights.starts_with("0x"), "explicit mask, never GA/GW");
        let m = mask(iu.rights);
        assert_eq!(m, GUI_CLIENT_ACCESS);
        const FILE_CREATE_PIPE_INSTANCE: u32 = 0x4;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
        const WRITE_DAC: u32 = 0x4_0000;
        const WRITE_OWNER: u32 = 0x8_0000;
        assert_eq!(m & FILE_CREATE_PIPE_INSTANCE, 0);
        assert_eq!(m & FILE_WRITE_ATTRIBUTES, 0);
        assert_eq!(m & (WRITE_DAC | WRITE_OWNER), 0);
        assert_ne!(m & 0x1, 0, "read data");
        assert_ne!(m & 0x2, 0, "write data");
    }

    #[test]
    fn agent_pipe_names_are_validated() {
        assert!(is_agent_pipe_name(&agent_pipe_name(1, 4321, 0)));
        assert!(!is_agent_pipe_name(r"\\.\pipe\TLP-Desk-gui"));
        assert!(!is_agent_pipe_name(r"\\.\pipe\TLP-Desk-agent-1-2"));
        assert!(!is_agent_pipe_name(r"\\.\pipe\TLP-Desk-agent-1-2-x"));
        assert!(!is_agent_pipe_name(r"\\server\pipe\TLP-Desk-agent-1-2-3"));
        assert!(!is_agent_pipe_name(r"\\.\pipe\TLP-Desk-agent-1-2-3-4"));
        assert!(!is_agent_pipe_name(r"\\.\pipe\TLP-Desk-agent-Ù¡-2-3"));
        assert!(!is_agent_pipe_name(
            r"\\.\pipe\TLP-Desk-agent-1-2-99999999999"
        ));
    }
}
