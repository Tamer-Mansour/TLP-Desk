//! `connect`: connect to a host as a viewer and report fps / bitrate / RTT every second.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Args;
use serde_json::{json, Value};
use tlp_desk_net::FeatureSet;
use tlp_desk_proto::InputEvent;
use tlp_desk_viewer::{DecodedFrame, MonitorDesc, ViewerEvent, ViewerStats};

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
}

/// Runs `connect`.
pub async fn run(ctx: &Ctx, args: &ConnectArgs) -> Result<()> {
    let out = ctx.out;
    let mut dialed = dial(ctx, &args.dial, FeatureSet::new()).await?;
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

    let start = Instant::now();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(args.seconds);
    let mut last: Option<DecodedFrame> = None;
    let mut ended = None;
    let mut want_monitor = args.monitor;
    let mut pointer_tick = tokio::time::interval(Duration::from_millis(50));
    let mut step = 0u32;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            _ = tokio::signal::ctrl_c() => break,
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
                Some(ViewerEvent::Feature(_)) => {}
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

/// `{"type":"stats", ...}`, one per second.
pub fn stats_json(t: Duration, s: &ViewerStats) -> Value {
    json!({
        "type": "stats",
        "t_s": t.as_secs(),
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
                "rtt_ms",
                "t_s",
                "type",
                "width"
            ]
        );
        assert_eq!(v["t_s"], 3);
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
