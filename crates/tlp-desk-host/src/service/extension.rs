//! What the app adds to the service: the session features (files, clipboard, chat,
//! whiteboard, privacy, audio, ...), the machine-wide host settings, the connection log and
//! the app messages its GUI exchanges with the service.
//!
//! The service loop (`service::windows::run_service`) owns the endpoint, the [`Host`], the
//! session agent and the GUI pipe; it calls a [`ServiceExtension`] at fixed points:
//!
//! 1. [`ServiceExtension::configure`] before the host starts, with the [`AgentFeatureHub`] whose
//!    factories give the session features their agent-side objects (clipboard, overlay,
//!    shield, ...);
//! 2. [`ServiceExtension::started`] once the host runs;
//! 3. [`ServiceExtension::on_host_event`] for every host event and
//!    [`ServiceExtension::console_user`] whenever the console user changes;
//! 4. [`ServiceExtension::app_command`] / [`ServiceExtension::app_snapshot`] for the GUI's
//!    app messages ([`GuiCommand::App`](super::ipc::GuiCommand::App)).
//!
//! Platform-independent, so the app tests its extension without a service.

use std::path::PathBuf;
use std::sync::Arc;

use tlp_desk_crypto::{PasswordStore, PasswordVerifier, TicketStore};
use tlp_desk_net::{Endpoint, EndpointConfig};
use tlp_desk_proto::Permissions;

use super::feature_proxy::AgentFeatureHub;
use super::gui::{GuiCommands, GuiHub};
use crate::{Host, HostConfig, HostEvent};

/// The folders of the user logged on at the console (resolved with that user's token).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleUser {
    /// Windows session id.
    pub session: u32,
    /// `C:\Users\<name>`.
    pub profile: PathBuf,
    /// The user's Desktop folder (may be redirected, e.g. to OneDrive).
    pub desktop: PathBuf,
    /// The user's Downloads folder.
    pub downloads: PathBuf,
}

/// What the service hands the extension once the host runs.
#[derive(Clone)]
pub struct ServiceContext {
    pub host: Arc<Host>,
    pub endpoint: Arc<Endpoint>,
    pub hub: Arc<GuiHub>,
    pub features: Arc<AgentFeatureHub>,
}

impl std::fmt::Debug for ServiceContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceContext").finish_non_exhaustive()
    }
}

/// The app's part of the service. Every method has a do-nothing default.
pub trait ServiceExtension: Send + Sync + 'static {
    /// Adjusts the endpoint before it binds (e.g. the allow-list from the machine-wide host
    /// settings).
    fn endpoint_config(&self, cfg: EndpointConfig) -> EndpointConfig {
        cfg
    }

    /// Adds the session features to `cfg` before the host starts; `features` provides their
    /// agent-side objects.
    fn configure(&self, features: &Arc<AgentFeatureHub>, cfg: HostConfig) -> HostConfig {
        let _ = features;
        cfg
    }

    /// The host runs.
    fn started(&self, ctx: ServiceContext) {
        let _ = ctx;
    }

    /// Every host event, after the GUI hub mapped it. Must not block.
    fn on_host_event(&self, ev: &HostEvent) {
        let _ = ev;
    }

    /// The user logged on at the console changed (`None`: nobody is logged on there).
    fn console_user(&self, user: Option<ConsoleUser>) {
        let _ = user;
    }

    /// An app message from the GUI; `None` if the extension does not know it.
    fn app_command(&self, request: &[u8]) -> Option<Vec<u8>> {
        let _ = request;
        None
    }

    /// App messages a newly connected GUI needs to catch up.
    fn app_snapshot(&self) -> Vec<Vec<u8>> {
        Vec::new()
    }
}

/// Where the service's credentials live on disk, for [`GuiCommand::ReloadCredentials`]
/// (audit F5).
///
/// [`GuiCommand::ReloadCredentials`]: super::ipc::GuiCommand::ReloadCredentials
#[derive(Debug, Clone)]
pub struct CredentialFiles {
    /// Data directory holding the permanent password file ([`PasswordStore`]).
    pub data_dir: PathBuf,
    /// The host's reconnect ticket store (shared with the host), if tickets are enabled.
    pub tickets: Option<TicketStore>,
}

/// Something whose permanent password can be replaced at run time (the [`Host`]; faked in
/// tests).
pub trait PasswordTarget {
    /// Replaces the permanent password (`None` = disabled).
    fn set_password(&self, verifier: Option<PasswordVerifier>);
}

impl PasswordTarget for Host {
    fn set_password(&self, verifier: Option<PasswordVerifier>) {
        Host::set_password(self, verifier);
    }
}

/// Re-reads the permanent password and the reconnect tickets (audit F5): before this, a
/// password set or removed while the service ran took effect only after a service restart.
/// A password file that cannot be read disables the password (fail closed). Returns whether
/// everything was read.
pub fn reload_credentials(target: &dyn PasswordTarget, files: &CredentialFiles) -> bool {
    let mut ok = true;
    match PasswordStore::new(&files.data_dir).load() {
        Ok(verifier) => {
            tracing::info!(set = verifier.is_some(), "permanent password reloaded");
            target.set_password(verifier);
        }
        Err(e) => {
            tracing::warn!(%e, "cannot read the permanent password; disabled");
            target.set_password(None);
            ok = false;
        }
    }
    if let Some(t) = &files.tickets {
        if let Err(e) = t.reload() {
            tracing::warn!(%e, "cannot reload reconnect tickets; keeping the current ones");
            ok = false;
        }
    }
    ok
}

/// The GUI commands the service serves: the host's own, plus the extension's app messages.
pub struct ServiceCommands {
    host: Arc<Host>,
    ext: Option<Arc<dyn ServiceExtension>>,
    credentials: Option<CredentialFiles>,
}

impl std::fmt::Debug for ServiceCommands {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceCommands")
            .field("extension", &self.ext.is_some())
            .finish_non_exhaustive()
    }
}

impl ServiceCommands {
    /// Commands on `host`, with the app messages of `ext`.
    pub fn new(host: Arc<Host>, ext: Option<Arc<dyn ServiceExtension>>) -> Self {
        Self {
            host,
            ext,
            credentials: None,
        }
    }

    /// Lets [`GuiCommand::ReloadCredentials`](super::ipc::GuiCommand::ReloadCredentials)
    /// re-read `files`.
    #[must_use]
    pub fn with_credentials(mut self, files: CredentialFiles) -> Self {
        self.credentials = Some(files);
        self
    }
}

impl GuiCommands for ServiceCommands {
    fn respond(&self, request: u64, accept: bool) -> bool {
        GuiCommands::respond(&*self.host, request, accept)
    }
    fn rotate_code(&self) {
        GuiCommands::rotate_code(&*self.host);
    }
    fn disconnect(&self, session: u64) -> bool {
        GuiCommands::disconnect(&*self.host, session)
    }
    fn set_permissions(&self, session: u64, permissions: Permissions) -> bool {
        GuiCommands::set_permissions(&*self.host, session, permissions)
    }
    fn app_command(&self, request: &[u8]) -> Option<Vec<u8>> {
        self.ext.as_ref()?.app_command(request)
    }
    fn app_snapshot(&self) -> Vec<Vec<u8>> {
        self.ext
            .as_ref()
            .map(|e| e.app_snapshot())
            .unwrap_or_default()
    }
    fn reload_credentials(&self) -> bool {
        self.credentials
            .as_ref()
            .is_some_and(|files| reload_credentials(&*self.host, files))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tlp_desk_crypto::{now_unix_ms, Fingerprint, DEFAULT_TICKET_TTL, TICKETS_FILE};

    use super::*;

    #[derive(Default)]
    struct FakeHost(Mutex<Vec<Option<PasswordVerifier>>>);

    impl PasswordTarget for FakeHost {
        fn set_password(&self, verifier: Option<PasswordVerifier>) {
            self.0.lock().unwrap().push(verifier);
        }
    }

    /// Audit F5: a password saved (or removed) on disk while the service runs, and tickets
    /// revoked on disk, take effect on `ReloadCredentials` without a restart.
    #[test]
    fn reload_picks_up_password_and_ticket_changes() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("بيانات");
        let fp = Fingerprint([5; 32]);
        let tickets = TicketStore::load(&data.join(TICKETS_FILE), DEFAULT_TICKET_TTL).unwrap();
        tickets.issue(&fp, 1, now_unix_ms()).unwrap();
        let files = CredentialFiles {
            data_dir: data.clone(),
            tickets: Some(tickets.clone()),
        };
        let host = FakeHost::default();

        // Another process (the elevated GUI) saves a password and revokes the tickets.
        let v = PasswordVerifier::derive("كلمة-سر-طويلة-2026", &fp).unwrap();
        PasswordStore::new(&data).save(&v).unwrap();
        TicketStore::load(&data.join(TICKETS_FILE), DEFAULT_TICKET_TTL)
            .unwrap()
            .revoke_all();
        assert!(reload_credentials(&host, &files));
        assert_eq!(host.0.lock().unwrap().last(), Some(&Some(v)));
        assert!(tickets.is_empty());

        // Removing the password disables it.
        PasswordStore::new(&data).clear().unwrap();
        assert!(reload_credentials(&host, &files));
        assert_eq!(host.0.lock().unwrap().last(), Some(&None));
    }
}
