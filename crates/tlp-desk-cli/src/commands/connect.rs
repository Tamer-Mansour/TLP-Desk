//! `connect`: connect to a host as a viewer and report fps / bitrate / RTT every second.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Args;
use tlp_desk_net::FeatureSet;
use tlp_desk_proto::InputEvent;
use tlp_desk_viewer::{DecodedFrame, ViewerEvent};

use super::common::{dial, Ctx, DialArgs};
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
    let mut dialed = dial(ctx, &args.dial, FeatureSet::new()).await?;
    let session = &dialed.session;
    let events = &mut dialed.events;
    println!(
        "{}",
        text::connected_host(session.peer(), session.permissions())
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
                Some(ViewerEvent::HostInputPriority(active)) => {
                    println!("{}", text::host_priority(active));
                }
                Some(ViewerEvent::MonitorsChanged { monitors, active }) => {
                    println!("{}", text::monitors(&monitors, active));
                    if let Some(i) = want_monitor.take() {
                        if i != active {
                            println!("{}", text::selecting_monitor(i));
                            session.select_monitor(i);
                        }
                    }
                }
                Some(ViewerEvent::Stats(s)) => {
                    if let Some(f) = session.take_frame() {
                        last = Some(f);
                    }
                    println!("{}", text::stats_line(start.elapsed(), &s));
                }
                Some(ViewerEvent::PermissionsChanged(p)) => {
                    println!("{}", text::permissions(p));
                }
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
    println!("{}", text::session_ended(&reason));
    println!("{}", text::summary(&stats, elapsed));
    if let Some(path) = &args.snapshot {
        match last {
            Some(f) => {
                write_png(path, &f)?;
                println!(
                    "{}",
                    text::snapshot_saved(&path.display().to_string(), f.width, f.height)
                );
            }
            None => println!("{}", text::no_frame()),
        }
    }
    dialed.close().await;
    Ok(())
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
