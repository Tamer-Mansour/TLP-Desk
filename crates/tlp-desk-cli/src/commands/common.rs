//! Helpers shared by every subcommand: the per-run context, identity loading, and the
//! viewer-side dial + authenticate sequence (`connect`, `info`, `send-file`, `ls`, ... all
//! reuse [`DialArgs`], [`AuthArgs`] and [`dial`]).

use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Args;
use serde_json::{json, Value};
use tlp_desk_crypto::{DeviceId, Identity};
use tlp_desk_net::{Discovery, DiscoveryConfig, Endpoint, EndpointConfig, FeatureSet, PeerInfo};
use tlp_desk_viewer::{
    AuthMethod, ConnectTarget, KnownHostsStore, PixelFormat, Viewer, ViewerConfig, ViewerError,
    ViewerEvents, ViewerSession,
};

use crate::exit::CliError;
use crate::output::Output;
use crate::text;

/// What to do when a file being received already exists (`--on-conflict`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum OnConflict {
    /// Keep both: the new file becomes `name (2).ext` (default).
    #[default]
    KeepBoth,
    /// Replace the existing file.
    Overwrite,
    /// Keep the existing file, skip the new one.
    Skip,
}

impl OnConflict {
    /// The engine policy.
    pub fn policy(self) -> tlp_desk_files::ConflictPolicy {
        match self {
            Self::KeepBoth => tlp_desk_files::ConflictPolicy::Rename,
            Self::Overwrite => tlp_desk_files::ConflictPolicy::Overwrite,
            Self::Skip => tlp_desk_files::ConflictPolicy::Skip,
        }
    }
}

/// Environment variable holding the unattended-access password. The password is never
/// accepted on the command line, where other local users could read it from the process list.
pub const PASSWORD_ENV: &str = "TLP_DESK_PASSWORD";

/// What every command gets besides its own arguments.
#[derive(Debug, Clone)]
pub struct Ctx {
    /// Directory holding the identity key and known hosts.
    pub data_dir: PathBuf,
    /// Where output goes (`--json`, `--quiet`).
    pub out: Output,
}

/// A device ID in its compact machine form (9 ASCII digits, no spaces).
pub fn id_str(id: DeviceId) -> String {
    format!("{:09}", id.as_u32())
}

/// JSON description of a peer.
pub fn peer_json(peer: &PeerInfo) -> Value {
    json!({
        "device_id": id_str(peer.device_id),
        "name": peer.name,
        "addr": peer.addr.to_string(),
    })
}

/// The user's relay server (TLP-relay), shared by `serve` and every dialing command.
#[derive(Debug, Clone, Default, Args)]
pub struct RelayArgs {
    /// Relay server (`host` or `host:port`) for devices that are not on this network.
    #[arg(long, value_name = "HOST[:PORT]")]
    pub relay: Option<String>,
    /// The relay's token (or the TLP_RELAY_TOKEN environment variable).
    #[arg(
        long,
        env = "TLP_RELAY_TOKEN",
        hide_env_values = true,
        value_name = "SECRET"
    )]
    pub relay_token: Option<String>,
}

impl RelayArgs {
    /// The relay config, if `--relay` was given (then the token is required).
    pub fn config(&self) -> Result<Option<tlp_desk_net::relay::RelayConfig>> {
        let Some(addr) = &self.relay else {
            return Ok(None);
        };
        let token = self
            .relay_token
            .as_deref()
            .context("--relay needs --relay-token (or TLP_RELAY_TOKEN)")?;
        Ok(Some(tlp_desk_net::relay::RelayConfig::new(addr, token)?))
    }
}

/// How the viewer authenticates (shared by every command that dials a host).
#[derive(Debug, Clone, Default, Args)]
pub struct AuthArgs {
    /// The host's one-time code (otherwise the host user is asked to accept).
    #[arg(long)]
    pub code: Option<String>,
    /// Use the unattended password from the TLP_DESK_PASSWORD environment variable.
    #[arg(long)]
    pub password: bool,
}

impl AuthArgs {
    /// The auth method these flags select.
    ///
    /// Fails when `--password` is given but the environment variable is missing.
    pub fn method(&self) -> Result<AuthMethod> {
        Ok(match (&self.code, self.password) {
            (Some(c), _) => AuthMethod::OneTimeCode(c.clone()),
            (None, true) => {
                AuthMethod::Password(std::env::var(PASSWORD_ENV).with_context(|| {
                    format!("--password needs the {PASSWORD_ENV} environment variable")
                })?)
            }
            (None, false) => AuthMethod::AcceptPrompt,
        })
    }
}

/// Where and how to dial (shared by every command that connects to a host).
#[derive(Debug, Clone, Args)]
pub struct DialArgs {
    /// 9-digit device ID, IP[:port], ID@IP[:port], or a device name on the LAN.
    pub target: String,
    #[command(flatten)]
    pub auth: AuthArgs,
    /// Local UDP port (0 = any; lets the viewer run next to a host on the same PC).
    #[arg(long, default_value_t = 0)]
    pub port: u16,
    /// Forget a changed host key and trust the new one (only after verifying it!).
    #[arg(long)]
    pub forget_key: bool,
    #[command(flatten)]
    pub relay: RelayArgs,
}

/// Loads (or creates on first use) this install's identity.
pub fn load_identity(dir: &Path) -> Result<Arc<Identity>> {
    Ok(Arc::new(Identity::load_or_generate(dir).with_context(
        || format!("cannot load identity from {}", dir.display()),
    )?))
}

/// The wildcard address on `port` (dual-stack).
pub fn any_addr(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port)
}

/// An open viewer session plus the endpoint it runs on.
pub struct Dialed {
    pub session: ViewerSession,
    pub events: ViewerEvents,
    pub endpoint: Arc<Endpoint>,
}

impl Dialed {
    /// Closes the endpoint after the session was dropped, giving the Bye a moment to leave.
    pub async fn close(self) {
        let Dialed {
            session, endpoint, ..
        } = self;
        drop(session);
        endpoint.close();
        let _ = tokio::time::timeout(Duration::from_secs(1), endpoint.wait_idle()).await;
    }
}

/// Binds a client endpoint, resolves `args.target` and authenticates. Prints progress lines;
/// Ctrl+C cancels. `features` are started for the session.
pub async fn dial(ctx: &Ctx, args: &DialArgs, features: FeatureSet) -> Result<Dialed> {
    let auth = args.auth.method()?;
    let identity = load_identity(&ctx.data_dir)?;
    let endpoint = Arc::new(Endpoint::bind(
        identity,
        EndpointConfig {
            bind: any_addr(args.port),
            listen: false,
            ..EndpointConfig::default()
        },
    )?);
    let target: ConnectTarget = args.target.parse()?;
    let discovery = match target {
        ConnectTarget::Id(_) | ConnectTarget::Name(_) => Some(Arc::new(
            Discovery::start(DiscoveryConfig::default()).await?,
        )),
        _ => None,
    };
    let viewer = Viewer::new(
        endpoint.clone(),
        ViewerConfig {
            known_hosts: KnownHostsStore::File(
                ctx.data_dir.join(tlp_desk_crypto::KNOWN_HOSTS_FILE),
            ),
            pixel_format: PixelFormat::Rgba,
            discovery,
            features,
            relay: args.relay.config()?,
            ..ViewerConfig::default()
        },
    )?;
    ctx.out.progress(text::connecting(&target.to_string()));
    if auth == AuthMethod::AcceptPrompt {
        ctx.out.progress(text::waiting_for_accept());
    }
    let forget_key = args.forget_key;
    let attempt = async {
        match viewer.connect(target.clone(), auth.clone()).await {
            Err(ViewerError::HostKeyChanged { key, .. }) if forget_key => {
                // The changed entry is the only one pinned for this target; a successful
                // reconnect re-remembers the device-ID entry with the new key too.
                viewer.forget(&key)?;
                Ok(viewer.connect(target.clone(), auth.clone()).await?)
            }
            Err(e @ ViewerError::HostKeyChanged { .. }) => {
                eprintln!("{}", text::key_changed_help());
                Err(anyhow::Error::from(e))
            }
            other => Ok(other?),
        }
    };
    let (session, events) = tokio::select! {
        r = attempt => r?,
        _ = tokio::signal::ctrl_c() => return Err(CliError::Interrupted.into()),
    };
    Ok(Dialed {
        session,
        events,
        endpoint,
    })
}

#[cfg(test)]
mod on_conflict_tests {
    use super::*;

    #[test]
    fn on_conflict_maps_to_engine_policies_and_defaults_to_keeping_both() {
        use tlp_desk_files::ConflictPolicy as P;
        assert_eq!(OnConflict::default().policy(), P::Rename);
        assert_eq!(OnConflict::Overwrite.policy(), P::Overwrite);
        assert_eq!(OnConflict::Skip.policy(), P::Skip);
    }
}
