//! `TLP-Desk-cli forward <target> <LOCAL_PORT:REMOTE_HOST:REMOTE_PORT>...`: connect as a
//! viewer and forward local TCP ports to services on (or next to) the host until Ctrl+C.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use tlp_desk_net::{Endpoint, EndpointConfig, FeatureSet};
use tlp_desk_proto::Permissions;
use tlp_desk_viewer::{
    AuthMethod, ConnectTarget, ForwardSpec, KnownHostsStore, PixelFormat, TunnelCloseReason,
    TunnelEvent, TunnelViewerConfig, Viewer, ViewerConfig, ViewerError, ViewerEvent, ViewerTunnels,
    TUNNEL_FEATURE,
};

use crate::{any_addr, load_identity, text};

/// Counter lines are printed at most this often.
const STATS_EVERY: Duration = Duration::from_secs(5);

/// Options of the `forward` command.
pub struct ForwardOpts {
    pub target: String,
    pub rules: Vec<String>,
    pub auth: AuthMethod,
    /// Local UDP port of the QUIC endpoint (0 = any).
    pub port: u16,
    pub forget_key: bool,
    /// The user confirmed listening on non-loopback addresses.
    pub bind_lan: bool,
}

pub async fn forward(dir: &Path, opts: ForwardOpts) -> Result<()> {
    // Parse every rule before connecting, so a typo fails at once.
    let mut pending = Vec::new();
    for r in &opts.rules {
        let mut spec: ForwardSpec = r.parse()?;
        if spec.binds_beyond_loopback() {
            if !opts.bind_lan {
                bail!(text::lan_bind_needs_flag(spec.bind));
            }
            spec.allow_lan_bind = true;
            println!("{}", text::lan_bind_warning(spec.bind));
        }
        pending.push(spec);
    }

    let identity = load_identity(dir)?;
    let endpoint = Arc::new(Endpoint::bind(
        identity,
        EndpointConfig {
            bind: any_addr(opts.port),
            listen: false,
            ..EndpointConfig::default()
        },
    )?);
    let target: ConnectTarget = opts.target.parse()?;
    let discovery = match target {
        ConnectTarget::Id(_) | ConnectTarget::Name(_) => Some(Arc::new(
            tlp_desk_net::Discovery::start(tlp_desk_net::DiscoveryConfig::default()).await?,
        )),
        _ => None,
    };
    let tunnels = ViewerTunnels::new(TunnelViewerConfig::default());
    let viewer = Viewer::new(
        endpoint.clone(),
        ViewerConfig {
            known_hosts: KnownHostsStore::File(dir.join(tlp_desk_crypto::KNOWN_HOSTS_FILE)),
            pixel_format: PixelFormat::Rgba,
            discovery,
            features: FeatureSet::new().with(tunnels.clone()),
            ..ViewerConfig::default()
        },
    )?;
    println!("{}", text::connecting(&target.to_string()));
    if opts.auth == AuthMethod::AcceptPrompt {
        println!("{}", text::WAITING_FOR_ACCEPT);
    }
    let attempt = async {
        match viewer.connect(target.clone(), opts.auth.clone()).await {
            Err(ViewerError::HostKeyChanged { key, .. }) if opts.forget_key => {
                viewer.forget(&key)?;
                Ok(viewer.connect(target.clone(), opts.auth.clone()).await?)
            }
            Err(e @ ViewerError::HostKeyChanged { .. }) => {
                eprintln!("{}", text::key_changed_help());
                Err(anyhow::Error::from(e))
            }
            other => Ok(other?),
        }
    };
    let (session, mut events) = tokio::select! {
        r = attempt => r?,
        _ = tokio::signal::ctrl_c() => bail!("cancelled"),
    };
    println!(
        "{}",
        text::connected_host(session.peer(), session.permissions())
    );
    let key = session.feature_key();
    // Rules that run, by id, so a rule revoked by the host can be restarted when it is
    // granted again.
    let mut active = Vec::new();
    let mut last_stats = tokio::time::Instant::now();
    let mut waiting_shown = false;
    let mut failed_hard = false;

    if let Some(batch) = take_ready(&mut pending, session.permissions(), &mut waiting_shown) {
        failed_hard |= start_rules(&tunnels, key, batch, &mut pending, &mut active).await;
    }
    if failed_hard && active.is_empty() && pending.is_empty() {
        bail!("no forwarding rule could be started");
    }
    println!("{}", text::FORWARD_RUNNING);

    let reason = loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break None,
            ev = events.recv() => match ev {
                Some(ViewerEvent::PermissionsChanged(p)) => {
                    println!("{}", text::permissions(p));
                    if let Some(batch) = take_ready(&mut pending, p, &mut waiting_shown) {
                        start_rules(&tunnels, key, batch, &mut pending, &mut active).await;
                    }
                }
                Some(ViewerEvent::Feature(e)) if e.feature == TUNNEL_FEATURE => {
                    match e.payload::<TunnelEvent>() {
                        Some(TunnelEvent::Closed { id, reason }) => {
                            println!("{}", text::forward_closed(*id, reason));
                            if let Some(i) = active.iter().position(|(a, _)| a == id) {
                                let (_, spec) = active.swap_remove(i);
                                if *reason == TunnelCloseReason::PermissionRevoked {
                                    pending.push(spec);
                                    waiting_shown = false;
                                }
                            }
                        }
                        Some(TunnelEvent::Changed(list))
                            if !list.is_empty() && last_stats.elapsed() >= STATS_EVERY =>
                        {
                            println!("{}", text::forward_stats(list));
                            last_stats = tokio::time::Instant::now();
                        }
                        Some(TunnelEvent::Changed(_)) | None => {}
                    }
                }
                Some(ViewerEvent::Ended(r)) => break Some(r),
                Some(_) => {}
                None => break None,
            }
        }
    };
    let list = tunnels.list(key);
    if !list.is_empty() {
        println!("{}", text::forward_stats(&list));
    }
    let reason = match reason {
        Some(r) => r,
        None => {
            session.disconnect();
            tokio::time::timeout(Duration::from_secs(3), session.ended())
                .await
                .unwrap_or(tlp_desk_viewer::EndReason::Local)
        }
    };
    println!("{}", text::session_ended(&reason));
    drop(session);
    endpoint.close();
    let _ = tokio::time::timeout(Duration::from_secs(1), endpoint.wait_idle()).await;
    Ok(())
}

/// The waiting rules, if the session may forward now; otherwise prints (once) that we wait
/// for the host to grant `TUNNEL`.
fn take_ready(
    pending: &mut Vec<ForwardSpec>,
    perms: Permissions,
    waiting_shown: &mut bool,
) -> Option<Vec<ForwardSpec>> {
    if pending.is_empty() {
        return None;
    }
    if perms.contains(Permissions::TUNNEL) {
        return Some(std::mem::take(pending));
    }
    if !*waiting_shown {
        println!("{}", text::TUNNEL_WAITING_PERMISSION);
        *waiting_shown = true;
    }
    None
}

/// Starts `batch`; rules refused for lack of permission go back to `pending`. Returns `true`
/// if any rule failed for another reason (port busy, target refused...).
async fn start_rules(
    tunnels: &ViewerTunnels,
    key: u64,
    batch: Vec<ForwardSpec>,
    pending: &mut Vec<ForwardSpec>,
    active: &mut Vec<(tlp_desk_viewer::TunnelId, ForwardSpec)>,
) -> bool {
    let mut failed = false;
    for spec in batch {
        match tunnels.add(key, spec.clone()).await {
            Ok(status) => {
                println!("{}", text::forward_started(&status));
                active.push((status.id, spec));
            }
            Err(ViewerError::TunnelNotAllowed) => pending.push(spec),
            Err(e) => {
                eprintln!("{}", text::forward_failed(&spec, &e));
                failed = true;
            }
        }
    }
    failed
}
