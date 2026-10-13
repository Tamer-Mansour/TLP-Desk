//! `serve`: host a session (real capture, encode and input) for LAN viewers.

use std::io::{BufRead, Write};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::Args;
use tlp_desk_host::{Host, HostConfig, HostEvent, DEFAULT_PERMISSIONS};
use tlp_desk_net::{Endpoint, EndpointConfig, FeatureSet};
use tlp_desk_proto::Permissions;

use super::common::{any_addr, load_identity, Ctx};
use crate::text;

/// Arguments of `serve`.
#[derive(Debug, Clone, Args)]
pub struct ServeArgs {
    /// Accept every viewer without asking (testing only).
    #[arg(long)]
    pub auto_accept: bool,
    /// Allow authentication with the one-time code (printed at start).
    #[arg(long)]
    pub code: bool,
    /// UDP port to listen on.
    #[arg(long, default_value_t = tlp_desk_net::DEFAULT_PORT)]
    pub port: u16,
    /// Target frame rate.
    #[arg(long, default_value_t = 60)]
    pub fps: u32,
    /// Monitor index (0 = primary).
    #[arg(long, default_value_t = 0)]
    pub monitor: u32,
    /// Viewers may only watch (no mouse/keyboard).
    #[arg(long)]
    pub view_only: bool,
    /// Do not announce this host on the LAN.
    #[arg(long)]
    pub no_discovery: bool,
    /// Name shown to viewers (default: computer name).
    #[arg(long)]
    pub name: Option<String>,
    /// Local input priority window in milliseconds: viewer input is ignored this long
    /// after the host user touches their own mouse/keyboard (0 = off).
    #[arg(long, default_value_t = 1000)]
    pub local_grace_ms: u64,
    /// Do not show the green marker at the pointer while a viewer moves it.
    #[arg(long)]
    pub no_pointer_marker: bool,
}

impl ServeArgs {
    /// The host configuration these flags select.
    pub fn host_config(&self) -> HostConfig {
        HostConfig {
            local_input_grace: Duration::from_millis(self.local_grace_ms),
            remote_pointer_marker: !self.no_pointer_marker,
            monitor: self.monitor,
            fps: self.fps,
            allow_one_time_code: self.code,
            default_permissions: if self.view_only {
                Permissions::VIEW
            } else {
                DEFAULT_PERMISSIONS
            },
            advertise: !self.no_discovery,
            device_name: self
                .name
                .clone()
                .unwrap_or_else(tlp_desk_net::default_device_name),
            features: host_features(),
            ..HostConfig::default()
        }
    }
}

/// The session features the CLI host serves.
// FEATURE HOOK: each lane registers its host-side feature here, e.g. `.with(files)`.
fn host_features() -> FeatureSet {
    FeatureSet::new()
}

/// Runs `serve` until Ctrl+C.
pub async fn run(ctx: &Ctx, args: &ServeArgs) -> Result<()> {
    let cfg = args.host_config();
    let auto_accept = args.auto_accept;
    let identity = load_identity(&ctx.data_dir)?;
    let name = cfg.device_name.clone();
    let code_enabled = cfg.allow_one_time_code;
    let endpoint = Arc::new(Endpoint::bind(
        identity.clone(),
        EndpointConfig {
            bind: any_addr(args.port),
            device_name: name.clone(),
            listen: true,
            ..EndpointConfig::default()
        },
    )?);
    let local = endpoint.local_addr()?;
    let (host, mut events) = Host::start(endpoint.clone(), cfg).await?;
    let host = Arc::new(host);
    println!("{}", text::listening(host.device_id(), local, &name));
    println!(
        "{}",
        text::identity(
            identity.device_id(),
            &identity.fingerprint(),
            &ctx.data_dir.display().to_string()
        )
    );
    if code_enabled {
        println!("{}", text::code(&host.one_time_code().display()));
    } else {
        println!("{}", text::code_disabled());
    }
    if auto_accept {
        println!("{}", text::auto_accept());
    }

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            ev = events.recv() => {
                let Some(ev) = ev else { break };
                match ev {
                    HostEvent::IncomingRequest(r) => {
                        if auto_accept {
                            println!("{}", text::auto_accepted(&r));
                            host.accept(r.id, None);
                        } else {
                            println!("{}", text::incoming(&r));
                            println!("{}", text::prompt_hint());
                            print!("{}", text::accept_question());
                            let _ = std::io::stdout().flush();
                            let host = host.clone();
                            tokio::spawn(async move {
                                let yes = tokio::task::spawn_blocking(|| {
                                    let mut line = String::new();
                                    let _ = std::io::stdin().lock().read_line(&mut line);
                                    matches!(line.trim(), "y" | "Y" | "yes")
                                })
                                .await
                                .unwrap_or(false);
                                if yes {
                                    host.accept(r.id, None);
                                } else {
                                    host.reject(r.id);
                                }
                            });
                        }
                    }
                    HostEvent::RequestCancelled { reason, .. } => println!("{}", text::cancelled(reason)),
                    HostEvent::AuthFailed { peer, auth, reason } => {
                        println!("{}", text::auth_failed(&peer, auth, reason));
                    }
                    HostEvent::CodeChanged => {
                        if code_enabled {
                            println!("{}", text::code_changed(&host.one_time_code().display()));
                        }
                    }
                    HostEvent::SessionStarted(v) => println!("{}", text::connected_viewer(&v)),
                    HostEvent::PermissionsChanged { .. } => {}
                    HostEvent::SessionEnded { peer, reason, .. } => {
                        println!("{}", text::viewer_left(&peer, &reason));
                    }
                    // FEATURE HOOK: CLI commands (send-file, ...) print their feature events here.
                    HostEvent::Feature { .. } => {}
                }
            }
        }
    }
    println!("{}", text::stopping());
    host.shutdown();
    // Let the Bye reach connected viewers.
    tokio::time::sleep(Duration::from_millis(300)).await;
    endpoint.close();
    let _ = tokio::time::timeout(Duration::from_secs(1), endpoint.wait_idle()).await;
    Ok(())
}
