//! Process entry points of the unattended-access service (TLP-service), Windows only.
//!
//! `TLP-Desk.exe` doubles as the service and its session agent, selected by the first
//! argument ([`dispatch`]):
//!
//! * `--service` (only the Service Control Manager passes it) runs the service;
//! * `--agent --pipe <name> --parent <pid>` (only the service passes it) runs the session agent;
//! * `install-service` / `uninstall-service` (elevated) register or remove the service.
//!
//! These processes have no window and no console (`windows_subsystem = "windows"`), so they log
//! to `%ProgramData%\TLP-Desk\logs\service.log` (local file only, rotated at 4 MB, five older
//! files kept) and report success or failure through the exit code; `install-service` and
//! `uninstall-service` typed in a terminal also print the outcome there.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tlp_desk_crypto::{PasswordStore, TicketStore, DEFAULT_TICKET_TTL, TICKETS_FILE};
use tlp_desk_host::service::windows as win;
use tlp_desk_host::service::{AGENT_ARG, INSTALL_SUBCOMMAND, SERVICE_ARG, UNINSTALL_SUBCOMMAND};
use tlp_desk_host::{HostConfig, UnattendedFeature, WindowsPower};

use crate::service_host::agent::AppAgentFeatures;
use crate::service_host::{spawn_files_pump, AppServiceExtension, ProtectFn};

use crate::i18n::{tr_raw, Lang, S};

/// The log file is rotated when it grows past this.
const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

/// Rotated log files kept (`service.1.log` newest ... `service.5.log` oldest).
const KEEP_LOGS: u32 = 5;

/// What `main` should do with the command line.
#[derive(Debug, PartialEq, Eq)]
pub enum Entry {
    /// Not a service invocation: start the normal GUI.
    Gui,
    /// Run as the Windows service.
    Service,
    /// Run as the session agent.
    Agent {
        /// The agent pipe to serve.
        pipe: String,
        /// The service process to follow.
        parent: u32,
    },
    /// `install-service`.
    Install,
    /// `uninstall-service`.
    Uninstall,
    /// A service switch with bad arguments.
    Invalid(String),
}

/// Classifies the arguments after the program name. Pure, so it is unit-tested.
pub fn classify(args: &[String]) -> Entry {
    let Some(first) = args.first() else {
        return Entry::Gui;
    };
    match first.as_str() {
        SERVICE_ARG => Entry::Service,
        INSTALL_SUBCOMMAND => Entry::Install,
        UNINSTALL_SUBCOMMAND => Entry::Uninstall,
        AGENT_ARG => {
            let mut pipe = None;
            let mut parent = None;
            let mut it = args.iter().skip(1);
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--pipe" => pipe = it.next().cloned(),
                    "--parent" => parent = it.next().and_then(|p| p.parse::<u32>().ok()),
                    _ => {}
                }
            }
            match (pipe, parent) {
                (Some(pipe), Some(parent)) => Entry::Agent { pipe, parent },
                _ => Entry::Invalid("--agent needs --pipe <name> and --parent <pid>".into()),
            }
        }
        _ => Entry::Gui,
    }
}

/// Runs a service invocation and returns the process exit code, or `None` for the GUI.
pub fn dispatch() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let entry = classify(&args);
    if entry == Entry::Gui {
        return None;
    }
    init_logging();
    Some(match entry {
        Entry::Gui => return None,
        Entry::Service => finish("service", win::run_dispatcher(service_options)),
        Entry::Agent { pipe, parent } => finish(
            "agent",
            win::run_agent(&pipe, parent, Arc::new(AppAgentFeatures::new())),
        ),
        Entry::Install => {
            let code = install();
            report(code, S::ServiceInstalledOk);
            code
        }
        Entry::Uninstall => {
            let code = finish("uninstall-service", win::uninstall_service());
            report(code, S::ServiceUninstalledOk);
            code
        }
        Entry::Invalid(why) => {
            tracing::error!(%why, "bad service command line");
            2
        }
    })
}

fn finish(what: &str, result: Result<(), tlp_desk_host::HostError>) -> i32 {
    match result {
        Ok(()) => {
            tracing::info!(what, "finished");
            0
        }
        Err(e) => {
            tracing::error!(what, %e, "failed");
            1
        }
    }
}

/// Tells a person who typed `install-service` / `uninstall-service` in a terminal how it went
/// (the exit code stays the contract for scripts and the installer).
fn report(code: i32, ok: S) {
    let lang = Lang::detect();
    let text = if code == 0 {
        tr_raw(lang, ok).to_owned()
    } else {
        format!(
            "{} {}",
            tr_raw(lang, S::ServiceCommandFailed),
            log_path().display()
        )
    };
    win::print_to_parent_console(&text);
}

fn install() -> i32 {
    let lang = Lang::detect();
    let texts = win::ServiceTexts {
        display_name: tr_raw(lang, S::ServiceDisplayName).to_owned(),
        description: tr_raw(lang, S::ServiceDescription).to_owned(),
    };
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(%e, "cannot find TLP-Desk.exe");
            return 1;
        }
    };
    finish("install-service", win::install_service(&exe, &texts))
}

/// Host settings of the service: it runs as SYSTEM, so it does not read a user's settings
/// file. It uses the machine-wide data directory (identity, permanent password, reconnect
/// tickets) shared with the GUI and the CLI.
fn service_options() -> win::ServiceOptions {
    let data_dir = tlp_desk_crypto::default_dir();
    let tickets = TicketStore::load(&data_dir.join(TICKETS_FILE), DEFAULT_TICKET_TTL)
        .unwrap_or_else(|e| {
            tracing::warn!(%e, "cannot read reconnect tickets; starting empty");
            TicketStore::in_memory(DEFAULT_TICKET_TTL)
        });
    let password = PasswordStore::new(&data_dir)
        .load()
        .map_err(|e| tracing::warn!(%e, "cannot read the permanent password; disabled"))
        .ok()
        .flatten();
    let mut host = HostConfig {
        device_name: tlp_desk_net::default_device_name(),
        password,
        reconnect_tickets: Some(tickets.clone()),
        ..HostConfig::default()
    };
    host.features.push(Arc::new(UnattendedFeature::new(
        Some(tickets),
        Arc::new(WindowsPower),
    )));
    // Files, Explorer paste, clipboard, chat, whiteboard, privacy, audio; the machine-wide
    // host settings (permissions, allow-list, timeouts...) and the connection log.
    let protect: ProtectFn = Arc::new(|path: &std::path::Path| {
        if let Err(e) = win::set_file_dacl(path, win::SETTINGS_FILE_SDDL) {
            tracing::warn!(%e, path = %path.display(), "cannot set the file's access rights");
        }
    });
    let extension = AppServiceExtension::new(data_dir.clone(), protect);
    spawn_files_pump(&extension);
    win::ServiceOptions {
        host,
        data_dir,
        extension: Some(extension),
        ..win::ServiceOptions::default()
    }
}

fn log_path() -> PathBuf {
    tlp_desk_crypto::default_dir()
        .join("logs")
        .join("service.log")
}

/// Appends tracing output to the log file; silent when the file cannot be opened.
fn init_logging() {
    let path = log_path();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    rotate_logs(&path, MAX_LOG_BYTES, KEEP_LOGS);
    let Ok(file) = fs::OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let file = Arc::new(Mutex::new(file));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_ansi(false)
        .with_writer(move || LogWriter(file.clone()))
        .try_init();
}

/// `service.N.log` next to `path`.
fn rotated(path: &std::path::Path, n: u32) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!("{stem}.{n}.log"))
}

/// Size-based rotation at start: when `path` is larger than `max_bytes`, `path` becomes
/// `.1`, `.1` becomes `.2`, ... and the oldest beyond `keep` is deleted. Errors are ignored
/// (a log must never stop the service).
fn rotate_logs(path: &std::path::Path, max_bytes: u64, keep: u32) {
    if !fs::metadata(path).is_ok_and(|m| m.len() > max_bytes) || keep == 0 {
        return;
    }
    let _ = fs::remove_file(rotated(path, keep));
    for n in (1..keep).rev() {
        let _ = fs::rename(rotated(path, n), rotated(path, n + 1));
    }
    let _ = fs::rename(path, rotated(path, 1));
}

struct LogWriter(Arc<Mutex<fs::File>>);

impl Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self.0.lock() {
            Ok(mut f) => f.write(buf),
            Err(_) => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self.0.lock() {
            Ok(mut f) => f.flush(),
            Err(_) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn logs_rotate_by_size_and_keep_n_files() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("service.log");
        let write = |p: &std::path::Path, n: usize| fs::write(p, vec![b'x'; n]).unwrap();
        // Small: untouched.
        write(&log, 10);
        rotate_logs(&log, 100, 3);
        assert!(log.exists() && !rotated(&log, 1).exists());
        // Four rotations with keep = 3: the oldest falls off.
        for i in 0..4 {
            write(&log, 200 + i);
            rotate_logs(&log, 100, 3);
        }
        assert!(!log.exists());
        assert_eq!(fs::metadata(rotated(&log, 1)).unwrap().len(), 203);
        assert_eq!(fs::metadata(rotated(&log, 2)).unwrap().len(), 202);
        assert_eq!(fs::metadata(rotated(&log, 3)).unwrap().len(), 201);
        assert!(!rotated(&log, 4).exists());
        assert_eq!(rotated(&log, 2).file_name().unwrap(), "service.2.log");
    }

    #[test]
    fn plain_and_gui_arguments_start_the_gui() {
        assert_eq!(classify(&[]), Entry::Gui);
        assert_eq!(classify(&a(&["--lang", "ar"])), Entry::Gui);
        assert_eq!(classify(&a(&["--connect", "1.2.3.4"])), Entry::Gui);
    }

    #[test]
    fn service_switches() {
        assert_eq!(classify(&a(&["--service"])), Entry::Service);
        assert_eq!(classify(&a(&["install-service"])), Entry::Install);
        assert_eq!(classify(&a(&["uninstall-service"])), Entry::Uninstall);
    }

    #[test]
    fn agent_needs_pipe_and_parent() {
        assert_eq!(
            classify(&a(&["--agent", "--pipe", r"\\.\pipe\x", "--parent", "42"])),
            Entry::Agent {
                pipe: r"\\.\pipe\x".into(),
                parent: 42
            }
        );
        assert!(matches!(classify(&a(&["--agent"])), Entry::Invalid(_)));
        assert!(matches!(
            classify(&a(&["--agent", "--pipe", "p", "--parent", "nope"])),
            Entry::Invalid(_)
        ));
    }
}
