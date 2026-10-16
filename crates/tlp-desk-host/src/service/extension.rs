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

/// The GUI commands the service serves: the host's own, plus the extension's app messages.
pub struct ServiceCommands {
    host: Arc<Host>,
    ext: Option<Arc<dyn ServiceExtension>>,
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
        Self { host, ext }
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
}
