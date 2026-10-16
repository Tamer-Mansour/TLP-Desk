//! The app's part of the TLP-Desk Windows service (TLP-service-integration).
//!
//! When the installed service hosts this PC (it owns UDP 47400), the session features must
//! behave as in the GUI host:
//!
//! * [`AppServiceExtension`] (service, SYSTEM, session 0): registers files, Explorer paste,
//!   clipboard sync, chat, whiteboard, privacy mode, audio and device info with the service's
//!   host, backed by the session agent through the proxies in [`proxies`]; writes the
//!   machine-wide connection log; applies and saves the machine-wide host settings
//!   ([`settings`]); and sends the user's GUI what it shows for host sessions (transfers,
//!   upload questions, chat, privacy) as app messages ([`wire`]).
//! * [`agent::AppAgentFeatures`] (session agent, the user's session): the real clipboard,
//!   virtual files for Explorer, overlay, marker, hooks, shield and loopback audio.
//!
//! Platform-independent apart from what the OS crates do; tests use fakes and temp folders.

pub mod agent;
mod extension;
pub mod proxies;
pub mod settings;
pub mod wire;

pub use extension::{log_dir, spawn_files_pump, ui_event, AppServiceExtension, ProtectFn};
