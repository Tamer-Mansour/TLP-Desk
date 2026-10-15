//! `forward <target> <LOCAL_PORT:REMOTE_HOST:REMOTE_PORT>...`: connect as a viewer and forward
//! local TCP ports to services on (or next to) the host until Ctrl+C (TLP-tunnel).

use std::time::Duration;

use anyhow::Result;
use clap::Args;
use serde_json::{json, Value};
use tlp_desk_net::FeatureSet;
use tlp_desk_proto::Permissions;
use tlp_desk_viewer::{
    ForwardSpec, TunnelCloseReason, TunnelEvent, TunnelId, TunnelStatus, TunnelViewerConfig,
    ViewerError, ViewerEvent, ViewerTunnels, TUNNEL_FEATURE,
};

use super::common::{dial, peer_json, Ctx, DialArgs};
use crate::output::Output;
use crate::text;

/// Counter lines are printed at most this often.
const STATS_EVERY: Duration = Duration::from_secs(5);

/// Arguments of `forward`.
#[derive(Debug, Clone, Args)]
pub struct ForwardArgs {
    #[command(flatten)]
    pub dial: DialArgs,
    /// LOCAL_PORT:REMOTE_HOST:REMOTE_PORT (or LOCAL_PORT:REMOTE_PORT for the host itself, or
    /// BIND_IP:LOCAL_PORT:REMOTE_HOST:REMOTE_PORT). REMOTE_HOST is an IP address as seen from
    /// the host (loopback or LAN). Repeatable.
    #[arg(required = true)]
    pub rules: Vec<String>,
    /// Allow listening on a non-loopback BIND_IP (other devices can use the tunnel).
    #[arg(long)]
    pub bind_lan: bool,
}

impl ForwardArgs {
    /// Parses every rule (before connecting, so a typo fails at once) and applies `--bind-lan`.
    pub fn specs(&self) -> Result<Vec<ForwardSpec>> {
        let mut specs = Vec::new();
        for r in &self.rules {
            let mut spec: ForwardSpec = r.parse()?;
            if spec.binds_beyond_loopback() {
                if !self.bind_lan {
                    return Err(ViewerError::TunnelLanBind(spec.bind).into());
                }
                spec.allow_lan_bind = true;
            }
            specs.push(spec);
        }
        Ok(specs)
    }
}

/// Runs `forward` until Ctrl+C or the session ends.
pub async fn run(ctx: &Ctx, args: &ForwardArgs) -> Result<()> {
    let out = ctx.out;
    let mut pending = args.specs().inspect_err(|e| {
        if let Some(ViewerError::TunnelLanBind(b)) = e.downcast_ref::<ViewerError>() {
            eprintln!("{}", text::lan_bind_needs_flag(*b));
        }
    })?;
    for s in pending.iter().filter(|s| s.binds_beyond_loopback()) {
        eprintln!("{}", text::lan_bind_warning(s.bind));
    }
    let tunnels = ViewerTunnels::new(TunnelViewerConfig::default());
    let mut dialed = dial(ctx, &args.dial, FeatureSet::new().with(tunnels.clone())).await?;
    let session = &dialed.session;
    out.event(
        text::connected_host(session.peer(), session.permissions()),
        &json!({
            "type": "connected",
            "peer": peer_json(session.peer()),
            "permissions": text::permission_names(session.permissions()),
        }),
    );
    let key = session.feature_key();
    let mut active: Vec<(TunnelId, ForwardSpec)> = Vec::new();
    let mut waiting_shown = false;

    if let Some(batch) = take_ready(out, &mut pending, session.permissions(), &mut waiting_shown) {
        let failed = start_rules(out, &tunnels, key, batch, &mut pending, &mut active).await;
        if let Some(e) = failed.filter(|_| active.is_empty() && pending.is_empty()) {
            dialed.close().await;
            return Err(e.into());
        }
    }
    out.progress(text::forward_running());

    let mut last_stats = tokio::time::Instant::now();
    let events = &mut dialed.events;
    let ended = loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break None,
            ev = events.recv() => match ev {
                Some(ViewerEvent::PermissionsChanged(p)) => {
                    out.event(
                        text::permissions(p),
                        &json!({ "type": "permissions", "permissions": text::permission_names(p) }),
                    );
                    if let Some(batch) = take_ready(out, &mut pending, p, &mut waiting_shown) {
                        start_rules(out, &tunnels, key, batch, &mut pending, &mut active).await;
                    }
                }
                Some(ViewerEvent::Feature(e)) if e.feature == TUNNEL_FEATURE => {
                    match e.payload::<TunnelEvent>() {
                        Some(TunnelEvent::Closed { id, reason }) => {
                            out.event(
                                text::forward_closed(*id, reason),
                                &json!({ "type": "forward_closed", "rule": id.0, "reason": format!("{reason:?}") }),
                            );
                            if let Some(i) = active.iter().position(|(a, _)| a == id) {
                                let (_, spec) = active.swap_remove(i);
                                // Restart it if the host grants port forwarding again.
                                if *reason == TunnelCloseReason::PermissionRevoked {
                                    pending.push(spec);
                                    waiting_shown = false;
                                }
                            }
                        }
                        Some(TunnelEvent::Changed(list))
                            if !list.is_empty() && last_stats.elapsed() >= STATS_EVERY =>
                        {
                            out.event(text::forward_stats(list), &stats_json(list));
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
        out.result(text::forward_stats(&list), &stats_json(&list));
    }
    let reason = match ended {
        Some(r) => r,
        None => {
            session.disconnect();
            tokio::time::timeout(Duration::from_secs(3), session.ended())
                .await
                .unwrap_or(tlp_desk_viewer::EndReason::Local)
        }
    };
    out.event(
        text::session_ended(&reason),
        &json!({ "type": "ended", "reason": format!("{reason:?}") }),
    );
    dialed.close().await;
    Ok(())
}

/// `{"type":"forward_stats","rules":[...]}`.
pub fn stats_json(list: &[TunnelStatus]) -> Value {
    let rules: Vec<Value> = list.iter().map(rule_json).collect();
    json!({ "type": "forward_stats", "rules": rules })
}

fn rule_json(s: &TunnelStatus) -> Value {
    json!({
        "rule": s.id.0,
        "local": s.local.to_string(),
        "target": s.target_display(),
        "active": s.active,
        "connections": s.connections,
        "failed": s.failed,
        "bytes_sent": s.bytes_sent,
        "bytes_received": s.bytes_received,
    })
}

/// The waiting rules if the session may forward now; otherwise says (once) that we wait for
/// the host to grant `TUNNEL`.
fn take_ready(
    out: Output,
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
        out.progress(text::tunnel_waiting_permission());
        *waiting_shown = true;
    }
    None
}

/// Starts `batch`; rules refused for lack of permission go back to `pending`. Returns the last
/// other error (port busy, target refused...), if any.
async fn start_rules(
    out: Output,
    tunnels: &ViewerTunnels,
    key: u64,
    batch: Vec<ForwardSpec>,
    pending: &mut Vec<ForwardSpec>,
    active: &mut Vec<(TunnelId, ForwardSpec)>,
) -> Option<ViewerError> {
    let mut failed = None;
    for spec in batch {
        match tunnels.add(key, spec.clone()).await {
            Ok(status) => {
                let mut v = rule_json(&status);
                v["type"] = json!("forward_started");
                out.event(text::forward_started(&status), &v);
                active.push((status.id, spec));
            }
            Err(ViewerError::TunnelNotAllowed) => pending.push(spec),
            Err(e) => {
                eprintln!("{}", text::forward_failed(&spec, &e));
                failed = Some(e);
            }
        }
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Cli, Cmd};
    use clap::Parser;

    fn parse(args: &[&str]) -> ForwardArgs {
        let cli = Cli::try_parse_from(std::iter::once("TLP-Desk-cli").chain(args.iter().copied()))
            .unwrap();
        let Cmd::Forward(f) = cli.cmd else {
            panic!("not forward")
        };
        f
    }

    #[test]
    fn forward_parses_rules_and_auth() {
        let f = parse(&[
            "forward",
            "192.168.1.20",
            "8080:127.0.0.1:80",
            "٩٠٠٠:٣٣٨٩",
            "--code",
            "123456",
        ]);
        assert_eq!(f.dial.target, "192.168.1.20");
        assert_eq!(f.dial.auth.code.as_deref(), Some("123456"));
        let specs = f.specs().unwrap();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].bind, "127.0.0.1:8080".parse().unwrap());
        assert_eq!(specs[1].target_port, 3389);
    }

    #[test]
    fn lan_binding_needs_the_flag() {
        let f = parse(&["forward", "10.0.0.2", "0.0.0.0:8080:127.0.0.1:80"]);
        let e = f.specs().unwrap_err();
        assert_eq!(crate::exit::Exit::of(&e), crate::exit::Exit::Usage);
        let f = parse(&[
            "forward",
            "10.0.0.2",
            "0.0.0.0:8080:127.0.0.1:80",
            "--bind-lan",
        ]);
        assert!(f.specs().unwrap()[0].allow_lan_bind);
    }

    #[test]
    fn forward_needs_a_rule() {
        assert!(
            Cli::try_parse_from(["TLP-Desk-cli", "forward", "10.0.0.2"]).is_err(),
            "a rule is required"
        );
    }

    #[test]
    fn stats_schema() {
        let s = TunnelStatus {
            id: TunnelId(2),
            local: "127.0.0.1:8080".parse().unwrap(),
            target_host: None,
            target_port: 80,
            active: 1,
            connections: 3,
            failed: 0,
            bytes_sent: 5,
            bytes_received: 7,
        };
        let v = stats_json(&[s]);
        assert_eq!(v["type"], "forward_stats");
        assert_eq!(v["rules"][0]["target"], "localhost:80");
        assert_eq!(v["rules"][0]["bytes_received"], 7);
    }
}
