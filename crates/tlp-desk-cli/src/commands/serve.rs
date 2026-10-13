//! `serve`: host a session (real capture, encode and input) for LAN viewers.

use std::io::{BufRead, Write};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::Args;
use serde_json::json;
use tlp_desk_host::{
    Host, HostConfig, HostEvent, HostTunnelEvent, HostTunnels, RequestId, TunnelHostConfig,
    DEFAULT_PERMISSIONS,
};
use tlp_desk_net::{Endpoint, EndpointConfig, FeatureSet};
use tlp_desk_proto::Permissions;

use super::common::{any_addr, id_str, load_identity, peer_json, Ctx};
use crate::output::Output;
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
    /// Let viewers forward TCP ports to services on this PC and its LAN (TUNNEL permission;
    /// off by default).
    #[arg(long)]
    pub allow_tunnels: bool,
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
            default_permissions: {
                let base = if self.view_only {
                    Permissions::VIEW
                } else {
                    DEFAULT_PERMISSIONS
                };
                if self.allow_tunnels {
                    base | Permissions::TUNNEL
                } else {
                    base
                }
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
    FeatureSet::new().with(HostTunnels::new(TunnelHostConfig::default()))
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
    let out = ctx.out;
    out.event(
        text::listening(host.device_id(), local, &name),
        &json!({
            "type": "listening",
            "device_id": id_str(host.device_id()),
            "fingerprint": identity.fingerprint().to_hex(),
            "name": name,
            "addr": local.to_string(),
        }),
    );
    out.progress(text::identity(
        identity.device_id(),
        &identity.fingerprint(),
        &ctx.data_dir.display().to_string(),
    ));
    if code_enabled {
        let code = host.one_time_code().display();
        out.event(text::code(&code), &json!({ "type": "code", "code": code }));
    } else {
        out.progress(text::code_disabled());
    }
    if auto_accept {
        eprintln!("{}", text::auto_accept());
    }
    if args.allow_tunnels {
        eprintln!("{}", text::tunnels_allowed());
    }

    // Open tunnels per session as (id, active connections): counters alone change every
    // second, only changes of this shape are printed.
    let mut tunnel_shape: Vec<(u64, usize)> = Vec::new();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            ev = events.recv() => {
                let Some(ev) = ev else { break };
                match ev {
                    HostEvent::IncomingRequest(r) => {
                        out.event(text::incoming(&r), &json!({
                            "type": "incoming",
                            "peer": peer_json(&r.peer),
                            "auth": format!("{:?}", r.auth),
                            "auto_accept": auto_accept,
                        }));
                        if auto_accept {
                            out.progress(text::auto_accepted(&r));
                            host.accept(r.id, None);
                        } else {
                            ask_accept(out, host.clone(), r.id);
                        }
                    }
                    HostEvent::RequestCancelled { reason, .. } => out.event(
                        text::cancelled(reason),
                        &json!({ "type": "request_cancelled", "reason": format!("{reason:?}") }),
                    ),
                    HostEvent::AuthFailed { peer, auth, reason } => out.event(
                        text::auth_failed(&peer, auth, reason),
                        &json!({
                            "type": "auth_failed",
                            "peer": peer_json(&peer),
                            "auth": format!("{auth:?}"),
                            "reason": format!("{reason:?}"),
                        }),
                    ),
                    HostEvent::CodeChanged => {
                        if code_enabled {
                            let code = host.one_time_code().display();
                            out.event(text::code_changed(&code), &json!({ "type": "code", "code": code }));
                        }
                    }
                    HostEvent::SessionStarted(v) => out.event(
                        text::connected_viewer(&v),
                        &json!({
                            "type": "session_started",
                            "session": v.id.0,
                            "peer": peer_json(&v.peer),
                            "auth": format!("{:?}", v.auth),
                            "permissions": text::permission_names(v.permissions),
                        }),
                    ),
                    HostEvent::PermissionsChanged { .. } => {}
                    HostEvent::SessionEnded { id, peer, reason } => out.event(
                        text::viewer_left(&peer, &reason),
                        &json!({
                            "type": "session_ended",
                            "session": id.0,
                            "peer": peer_json(&peer),
                            "reason": format!("{reason:?}"),
                        }),
                    ),
                    // FEATURE HOOK: CLI commands (send-file, ...) print their feature events here.
                    HostEvent::Feature { id, event } => match event.payload::<HostTunnelEvent>() {
                        Some(HostTunnelEvent::Changed(list)) => {
                            let shape: Vec<_> = list.iter().map(|t| (t.id, t.active)).collect();
                            if shape != tunnel_shape {
                                let tunnels: Vec<_> = list.iter().map(|t| json!({
                                    "tunnel": t.id,
                                    "target": t.target.to_string(),
                                    "active": t.active,
                                    "connections": t.connections,
                                    "failed": t.failed,
                                })).collect();
                                out.event(
                                    text::host_tunnels(list),
                                    &json!({ "type": "tunnels", "session": id.0, "tunnels": tunnels }),
                                );
                                tunnel_shape = shape;
                            }
                        }
                        Some(HostTunnelEvent::Refused { id: tunnel, target_host, target_port, reason }) => out.event(
                            text::host_tunnel_refused(target_host.as_deref(), *target_port, reason),
                            &json!({
                                "type": "tunnel_refused",
                                "session": id.0,
                                "tunnel": tunnel,
                                "target_host": target_host,
                                "target_port": target_port,
                                "reason": reason,
                            }),
                        ),
                        None => {}
                    },
                }
            }
        }
    }
    out.progress(text::stopping());
    host.shutdown();
    // Let the Bye reach connected viewers.
    tokio::time::sleep(Duration::from_millis(300)).await;
    endpoint.close();
    let _ = tokio::time::timeout(Duration::from_secs(1), endpoint.wait_idle()).await;
    Ok(())
}

/// Asks the person at the console whether to accept. The question goes to stderr when stdout
/// carries JSON (or with `--quiet`), so it is always visible and never corrupts the JSON stream.
fn ask_accept(out: Output, host: Arc<Host>, id: RequestId) {
    let ask = format!("{}\n{}", text::prompt_hint(), text::accept_question());
    if out.json || out.quiet {
        eprint!("{ask}");
        let _ = std::io::stderr().flush();
    } else {
        print!("{ask}");
        let _ = std::io::stdout().flush();
    }
    tokio::spawn(async move {
        let yes = tokio::task::spawn_blocking(|| {
            let mut line = String::new();
            let _ = std::io::stdin().lock().read_line(&mut line);
            matches!(line.trim(), "y" | "Y" | "yes" | "نعم")
        })
        .await
        .unwrap_or(false);
        if yes {
            host.accept(id, None);
        } else {
            host.reject(id);
        }
    });
}
