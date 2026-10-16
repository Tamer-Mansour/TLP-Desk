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
//! to `%ProgramData%\TLP-Desk\logs\service.log` (local file only, restarted at 4 MB) and report
//! success or failure through the exit code.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tlp_desk_crypto::{PasswordStore, TicketStore, DEFAULT_TICKET_TTL, TICKETS_FILE};
use tlp_desk_host::service::windows as win;
use tlp_desk_host::service::{AGENT_ARG, INSTALL_SUBCOMMAND, SERVICE_ARG, UNINSTALL_SUBCOMMAND};
use tlp_desk_host::{HostConfig, UnattendedFeature, WindowsPower};
use tlp_desk_proto::Permissions;

use crate::i18n::{tr_raw, Lang, S};

/// The log file is restarted when it grows past this.
const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

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
            win::run_agent(&pipe, parent, Arc::new(tlp_desk_host::service::NoFeatures)),
        ),
        Entry::Install => install(),
        Entry::Uninstall => finish("uninstall-service", win::uninstall_service()),
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
    // Same defaults as the GUI host: files, chat, whiteboard, restart and system actions.
    host.default_permissions = host
        .default_permissions
        .union(Permissions::FILES)
        .union(Permissions::CHAT)
        .union(Permissions::WHITEBOARD)
        .union(Permissions::RESTART)
        .union(Permissions::SYSTEM);
    host.features.push(Arc::new(UnattendedFeature::new(
        Some(tickets),
        Arc::new(WindowsPower),
    )));
    win::ServiceOptions {
        host,
        data_dir,
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
    if fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let _ = fs::rename(&path, path.with_extension("old"));
    }
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
