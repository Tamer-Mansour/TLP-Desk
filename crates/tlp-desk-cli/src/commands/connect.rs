//! `connect`: connect to a host as a viewer and report fps / bitrate / RTT every second.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Args;
use serde_json::{json, Value};
use tlp_desk_net::FeatureSet;
use tlp_desk_proto::InputEvent;
use tlp_desk_viewer::{
    privacy_request, DecodedFrame, MonitorDesc, NetSim, PrivacyState, PrivacyViewerFeature,
    ViewerEvent, ViewerStats,
};

use super::common::{dial, peer_json, Ctx, DialArgs};
use crate::text;

/// Arguments of `connect`.
#[derive(Debug, Clone, Args)]
pub struct ConnectArgs {
    #[command(flatten)]
    pub dial: DialArgs,
    /// Disconnect after this many seconds.
    #[arg(long, default_value_t = 10)]
    pub seconds: u64,
    /// Save the last decoded frame as PNG.
    #[arg(long)]
    pub snapshot: Option<PathBuf>,
    /// Ask the host to stream this monitor index (see the printed monitor list).
    #[arg(long)]
    pub monitor: Option<u32>,
    /// Testing: move the host pointer in a small circle around the screen center for the
    /// whole session (needs mouse permission).
    #[arg(long)]
    pub move_pointer: bool,
    /// Turn on privacy mode on the host for the session (needs the host's permission, e.g.
    /// `serve --allow-privacy`).
    #[arg(long)]
    pub privacy: bool,
    /// Testing: after a short delay type this text into the host's focused window (US key
    /// positions; a backslash-n escape is not interpreted, pass a real newline for Enter;
    /// needs keyboard permission).
    #[arg(long = "type", value_name = "TEXT")]
    pub type_text: Option<String>,
    /// Testing: after `--type`, press Ctrl+<LETTER> (e.g. `s` to save).
    #[arg(long, value_name = "LETTER")]
    pub then_ctrl: Option<char>,
    /// DEV ONLY (hidden): lose this percentage of the received video frames, to exercise the
    /// host's adaptive quality on one PC.
    #[arg(long, value_name = "PCT", hide = true, value_parser = clap::value_parser!(u8).range(0..=100))]
    pub sim_loss: Option<u8>,
    /// DEV ONLY (hidden): simulate a link of this many kbit/s for the received video (frames
    /// that do not fit are lost).
    #[arg(long, value_name = "KBPS", hide = true)]
    pub sim_bandwidth_kbps: Option<u32>,
    /// DEV ONLY (hidden): end the simulated impairment after this many seconds (default: it
    /// lasts the whole session), to watch the host recover.
    #[arg(long, value_name = "SECONDS", hide = true)]
    pub sim_seconds: Option<u64>,
}

impl ConnectArgs {
    /// The dev-only link simulation these flags ask for, if any.
    pub fn net_sim(&self) -> Option<NetSim> {
        let sim = NetSim {
            loss_pct: self.sim_loss.unwrap_or(0),
            bandwidth_kbps: self.sim_bandwidth_kbps.unwrap_or(0),
        };
        sim.is_active().then_some(sim)
    }
}

/// Runs `connect`.
pub async fn run(ctx: &Ctx, args: &ConnectArgs) -> Result<()> {
    let out = ctx.out;
    // FEATURE HOOK: viewer-side session features of `connect`.
    let features = FeatureSet::new().with(Arc::new(PrivacyViewerFeature));
    let mut dialed = dial(ctx, &args.dial, features).await?;
    let session = &dialed.session;
    let events = &mut dialed.events;
    out.event(
        text::connected_host(session.peer(), session.permissions()),
        &json!({
            "type": "connected",
            "peer": peer_json(session.peer()),
            "permissions": text::permission_names(session.permissions()),
        }),
    );

    if args.privacy {
        out.progress(text::requesting_privacy());
        session.send_message(privacy_request(true));
    }
    let start = Instant::now();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(args.seconds);
    // Dev only: impair the received video, optionally for a limited time.
    let sim = args.net_sim();
    if let Some(sim) = sim {
        session.set_net_sim(Some(sim));
        out.event(
            text::net_sim(Some(sim)),
            &net_sim_json(start.elapsed(), Some(sim)),
        );
    }
    let sim_end = sim
        .and(args.sim_seconds)
        .map(|s| tokio::time::Instant::now() + Duration::from_secs(s));
    let mut last: Option<DecodedFrame> = None;
    let mut ended = None;
    let mut want_monitor = args.monitor;
    let mut pointer_tick = tokio::time::interval(Duration::from_millis(50));
    let mut step = 0u32;
    // Testing: typed after a delay so the caller can focus the target window.
    let mut typing: std::collections::VecDeque<InputEvent> = args
        .type_text
        .as_deref()
        .map(super::typing::typing_events)
        .unwrap_or_default()
        .into_iter()
        .chain(
            args.then_ctrl
                .map(super::typing::ctrl_chord)
                .unwrap_or_default(),
        )
        .collect();
    let typing_from = Instant::now() + Duration::from_secs(2);
    let mut typing_tick = tokio::time::interval(Duration::from_millis(15));
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            _ = tokio::signal::ctrl_c() => break,
            _ = sleep_until_opt(sim_end), if sim_end.is_some() && session.net_sim().is_some() => {
                session.set_net_sim(None);
                out.event(text::net_sim(None), &net_sim_json(start.elapsed(), None));
            }
            _ = pointer_tick.tick(), if args.move_pointer => {
                // Normalized coordinates: a circle of ~3% of the screen around the center.
                let a = f64::from(step) * 0.25;
                step += 1;
                let c = f64::from(u16::MAX) / 2.0;
                let r = f64::from(u16::MAX) * 0.03;
                session.send_input(InputEvent::MouseMove {
                    x: (c + r * a.cos()) as u16,
                    y: (c + r * a.sin()) as u16,
                });
            }
            _ = typing_tick.tick(), if !typing.is_empty() && Instant::now() >= typing_from => {
                if let Some(ev) = typing.pop_front() {
                    session.send_input(ev);
                }
            }
            ev = events.recv() => match ev {
                Some(ViewerEvent::HostInputPriority(active)) => out.event(
                    text::host_priority(active),
                    &json!({ "type": "host_input_priority", "local_active": active }),
                ),
                Some(ViewerEvent::MonitorsChanged { monitors, active }) => {
                    out.event(text::monitors(&monitors, active), &monitors_json(&monitors, active));
                    if let Some(i) = want_monitor.take() {
                        if i != active {
                            out.progress(text::selecting_monitor(i));
                            session.select_monitor(i);
                        }
                    }
                }
                Some(ViewerEvent::Stats(s)) => {
                    if let Some(f) = session.take_frame() {
                        last = Some(f);
                    }
                    out.event(text::stats_line(start.elapsed(), &s), &stats_json(start.elapsed(), &s));
                }
                Some(ViewerEvent::PermissionsChanged(p)) => out.event(
                    text::permissions(p),
                    &json!({ "type": "permissions", "permissions": text::permission_names(p) }),
                ),
                // FEATURE HOOK: CLI commands print their feature events here.
                Some(ViewerEvent::Feature(event)) => {
                    if let Some(state) = event.payload::<PrivacyState>() {
                        out.event(
                            text::viewer_privacy(state.enabled),
                            &json!({ "type": "privacy", "enabled": state.enabled }),
                        );
                    }
                }
                Some(ViewerEvent::Ended(r)) => {
                    ended = Some(r);
                    break;
                }
                None => break,
            }
        }
    }
    if let Some(f) = session.take_frame() {
        last = Some(f);
    }
    let elapsed = start.elapsed().as_secs_f32();
    let stats = session.stats();
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
    out.result(
        text::summary(&stats, elapsed),
        &json!({
            "type": "summary",
            "frames_decoded": stats.frames_decoded,
            "seconds": elapsed,
            "avg_fps": stats.frames_decoded as f32 / elapsed.max(0.001),
        }),
    );
    if let Some(path) = &args.snapshot {
        match last {
            Some(f) => {
                write_png(path, &f)?;
                out.result(
                    text::snapshot_saved(&path.display().to_string(), f.width, f.height),
                    &json!({
                        "type": "snapshot",
                        "written": true,
                        "path": path.display().to_string(),
                        "width": f.width,
                        "height": f.height,
                    }),
                );
            }
            None => out.result(
                text::no_frame(),
                &json!({ "type": "snapshot", "written": false }),
            ),
        }
    }
    dialed.close().await;
    Ok(())
}

/// Sleeps until `at`, or forever without one.
async fn sleep_until_opt(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// `{"type":"net_sim", ...}`: the dev-only link simulation started (`active: true`) or ended.
pub fn net_sim_json(t: Duration, sim: Option<NetSim>) -> Value {
    let sim = sim.unwrap_or_default();
    json!({
        "type": "net_sim",
        "t_ms": u64::try_from(t.as_millis()).unwrap_or(u64::MAX),
        "active": sim.is_active(),
        "loss_pct": sim.loss_pct,
        "bandwidth_kbps": sim.bandwidth_kbps,
    })
}

/// `{"type":"stats", ...}`, one per second. `qp` is the average H.264 quantiser of the frames
/// received in the last second (lower is sharper; 0 = unknown).
pub fn stats_json(t: Duration, s: &ViewerStats) -> Value {
    json!({
        "type": "stats",
        "t_s": t.as_secs(),
        "t_ms": u64::try_from(t.as_millis()).unwrap_or(u64::MAX),
        "qp": s.qp,
        "fps": s.fps,
        "bitrate_kbps": s.bitrate_kbps,
        "rtt_ms": s.rtt_ms,
        "latency_ms": s.latency_ms,
        "decode_ms": s.decode_ms,
        "width": s.width,
        "height": s.height,
        "frames_decoded": s.frames_decoded,
        "frames_received": s.frames_received,
    })
}

/// `{"type":"monitors","active":N,"monitors":[...]}`.
pub fn monitors_json(list: &[MonitorDesc], active: u32) -> Value {
    let monitors: Vec<Value> = list
        .iter()
        .map(|m| {
            json!({
                "index": m.index,
                "name": m.name,
                "width": m.width,
                "height": m.height,
                "x": m.x,
                "y": m.y,
                "primary": m.primary,
            })
        })
        .collect();
    json!({ "type": "monitors", "active": active, "monitors": monitors })
}

fn write_png(path: &Path, f: &DecodedFrame) -> Result<()> {
    let file =
        std::fs::File::create(path).with_context(|| format!("cannot create {}", path.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), f.width, f.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header()?;
    w.write_image_data(&f.pixels)?;
    w.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_schema() {
        let v = stats_json(Duration::from_secs(3), &ViewerStats::default());
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "bitrate_kbps",
                "decode_ms",
                "fps",
                "frames_decoded",
                "frames_received",
                "height",
                "latency_ms",
                "qp",
                "rtt_ms",
                "t_ms",
                "t_s",
                "type",
                "width"
            ]
        );
        assert_eq!(v["t_s"], 3);
        assert_eq!(v["t_ms"], 3000);
        let v = stats_json(
            Duration::from_millis(1500),
            &ViewerStats {
                qp: 23.5,
                ..ViewerStats::default()
            },
        );
        assert_eq!(
            (v["t_s"].as_u64(), v["t_ms"].as_u64()),
            (Some(1), Some(1500))
        );
        assert_eq!(v["qp"], 23.5);
    }

    #[test]
    fn sim_flags_are_hidden_dev_options() {
        use crate::commands::{Cli, Cmd};
        use clap::{CommandFactory, Parser};

        let cli = Cli::try_parse_from([
            "TLP-Desk-cli",
            "connect",
            "127.0.0.1:47490",
            "--sim-loss",
            "5",
            "--sim-bandwidth-kbps",
            "4000",
            "--sim-seconds",
            "30",
        ])
        .unwrap();
        let Cmd::Connect(c) = cli.cmd else {
            panic!("not connect")
        };
        assert_eq!(
            c.net_sim(),
            Some(NetSim {
                loss_pct: 5,
                bandwidth_kbps: 4000
            })
        );
        assert_eq!(c.sim_seconds, Some(30));
        // Without the flags (or with zeros) nothing is simulated.
        let plain =
            Cli::try_parse_from(["TLP-Desk-cli", "connect", "127.0.0.1", "--sim-loss", "0"])
                .unwrap();
        let Cmd::Connect(p) = plain.cmd else {
            panic!("not connect")
        };
        assert_eq!(p.net_sim(), None);
        assert!(
            Cli::try_parse_from(["TLP-Desk-cli", "connect", "x", "--sim-loss", "101"]).is_err()
        );
        // Hidden from --help: users never see them.
        let help = Cli::command()
            .find_subcommand_mut("connect")
            .unwrap()
            .render_long_help()
            .to_string();
        assert!(!help.contains("sim-"), "{help}");

        let j = net_sim_json(Duration::from_millis(2500), c.net_sim());
        assert_eq!(
            (j["active"].as_bool(), j["t_ms"].as_u64()),
            (Some(true), Some(2500))
        );
        assert_eq!(net_sim_json(Duration::ZERO, None)["active"], false);
    }

    #[test]
    fn monitors_schema_keeps_arabic_names() {
        let m = MonitorDesc {
            index: 1,
            name: "شاشة جانبية".into(),
            width: 1920,
            height: 1080,
            primary: false,
            x: -1920,
            y: 0,
        };
        let v = monitors_json(&[m], 0);
        assert_eq!(v["type"], "monitors");
        assert_eq!(v["monitors"][0]["name"], "شاشة جانبية");
        assert_eq!(v["monitors"][0]["x"], -1920);
    }
}
