//! Headless TLP-Desk CLI for scripts and agents: print identity, list LAN devices, host a
//! session (real capture/encode/input), or connect as a viewer and report statistics.
#![forbid(unsafe_code)]

mod text;

use std::io::{BufRead, Write};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use tlp_desk_crypto::Identity;
use tlp_desk_host::{Host, HostConfig, HostEvent, DEFAULT_PERMISSIONS};
use tlp_desk_net::{Discovery, DiscoveryConfig, Endpoint, EndpointConfig};
use tlp_desk_proto::{InputEvent, Permissions};
use tlp_desk_viewer::{
    AuthMethod, ConnectTarget, DecodedFrame, KnownHostsStore, PixelFormat, Viewer, ViewerConfig,
    ViewerError, ViewerEvent,
};

#[derive(Parser)]
#[command(name = "TLP-Desk-cli", version, about = text::ABOUT)]
struct Cli {
    /// Directory holding the identity key and known hosts (default: the app's data dir).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    /// More logging (repeat for even more). RUST_LOG overrides.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print this device's ID and key fingerprint.
    Id,
    /// Discover TLP-Desk devices on the LAN.
    List {
        #[arg(long, default_value_t = 5)]
        seconds: u64,
    },
    /// Host: let LAN viewers see and control this PC.
    Serve {
        /// Accept every viewer without asking (testing only).
        #[arg(long)]
        auto_accept: bool,
        /// Allow authentication with the one-time code (printed at start).
        #[arg(long)]
        code: bool,
        /// UDP port to listen on.
        #[arg(long, default_value_t = tlp_desk_net::DEFAULT_PORT)]
        port: u16,
        /// Target frame rate.
        #[arg(long, default_value_t = 60)]
        fps: u32,
        /// Monitor index (0 = primary).
        #[arg(long, default_value_t = 0)]
        monitor: u32,
        /// Viewers may only watch (no mouse/keyboard).
        #[arg(long)]
        view_only: bool,
        /// Do not announce this host on the LAN.
        #[arg(long)]
        no_discovery: bool,
        /// Name shown to viewers (default: computer name).
        #[arg(long)]
        name: Option<String>,
        /// Local input priority window in milliseconds: viewer input is ignored this long
        /// after the host user touches their own mouse/keyboard (0 = off).
        #[arg(long, default_value_t = 1000)]
        local_grace_ms: u64,
        /// Do not show the green marker at the pointer while a viewer moves it.
        #[arg(long)]
        no_pointer_marker: bool,
    },
    /// Viewer: connect to a host and report fps / bitrate / RTT every second.
    Connect {
        /// 9-digit device ID, IP[:port], ID@IP[:port], or a device name on the LAN.
        target: String,
        /// The host's one-time code (otherwise the host user is asked to accept).
        #[arg(long)]
        code: Option<String>,
        /// Use the unattended password from the TLP_DESK_PASSWORD environment variable.
        #[arg(long)]
        password: bool,
        /// Disconnect after this many seconds.
        #[arg(long, default_value_t = 10)]
        seconds: u64,
        /// Save the last decoded frame as PNG.
        #[arg(long)]
        snapshot: Option<PathBuf>,
        /// Local UDP port (0 = any; lets the viewer run next to a host on the same PC).
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Forget a changed host key and trust the new one (only after verifying it!).
        #[arg(long)]
        forget_key: bool,
        /// Ask the host to stream this monitor index (see the printed monitor list).
        #[arg(long)]
        monitor: Option<u32>,
        /// Testing: move the host pointer in a small circle around the screen center for the
        /// whole session (needs mouse permission).
        #[arg(long)]
        move_pointer: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let level = match cli.verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| level.into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let data_dir = cli
        .data_dir
        .clone()
        .unwrap_or_else(tlp_desk_crypto::default_dir);
    rt.block_on(run(cli.cmd, data_dir))
}

fn load_identity(dir: &Path) -> Result<Arc<Identity>> {
    Ok(Arc::new(Identity::load_or_generate(dir).with_context(
        || format!("cannot load identity from {}", dir.display()),
    )?))
}

fn any_addr(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port)
}

async fn run(cmd: Cmd, dir: PathBuf) -> Result<()> {
    match cmd {
        Cmd::Id => {
            let id = load_identity(&dir)?;
            println!(
                "{}",
                text::identity(
                    id.device_id(),
                    &id.fingerprint(),
                    &dir.display().to_string()
                )
            );
            Ok(())
        }
        Cmd::List { seconds } => list(seconds).await,
        Cmd::Serve {
            auto_accept,
            code,
            port,
            fps,
            monitor,
            view_only,
            no_discovery,
            name,
            local_grace_ms,
            no_pointer_marker,
        } => {
            let cfg = HostConfig {
                local_input_grace: Duration::from_millis(local_grace_ms),
                remote_pointer_marker: !no_pointer_marker,
                monitor,
                fps,
                allow_one_time_code: code,
                default_permissions: if view_only {
                    Permissions::VIEW
                } else {
                    DEFAULT_PERMISSIONS
                },
                advertise: !no_discovery,
                device_name: name.unwrap_or_else(tlp_desk_net::default_device_name),
                ..HostConfig::default()
            };
            serve(&dir, port, cfg, auto_accept).await
        }
        Cmd::Connect {
            target,
            code,
            password,
            seconds,
            snapshot,
            port,
            forget_key,
            monitor,
            move_pointer,
        } => {
            let auth = match (code, password) {
                (Some(c), _) => AuthMethod::OneTimeCode(c),
                (None, true) => AuthMethod::Password(
                    std::env::var("TLP_DESK_PASSWORD")
                        .context("--password needs the TLP_DESK_PASSWORD environment variable")?,
                ),
                (None, false) => AuthMethod::AcceptPrompt,
            };
            let opts = ConnectOpts {
                seconds,
                snapshot,
                port,
                forget_key,
                monitor,
                move_pointer,
            };
            connect(&dir, &target, auth, opts).await
        }
    }
}

async fn list(seconds: u64) -> Result<()> {
    println!("{}", text::searching(seconds));
    let disc = Discovery::start(DiscoveryConfig::default()).await?;
    disc.refresh().await;
    tokio::time::sleep(Duration::from_secs(seconds)).await;
    let devices = disc.devices();
    if devices.is_empty() {
        println!("{}", text::NO_DEVICES);
    }
    for d in devices {
        println!("{}", text::device_row(&d));
    }
    Ok(())
}

async fn serve(dir: &Path, port: u16, cfg: HostConfig, auto_accept: bool) -> Result<()> {
    let identity = load_identity(dir)?;
    let name = cfg.device_name.clone();
    let code_enabled = cfg.allow_one_time_code;
    let endpoint = Arc::new(Endpoint::bind(
        identity.clone(),
        EndpointConfig {
            bind: any_addr(port),
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
            &dir.display().to_string()
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
                            println!("{}", text::PROMPT_HINT);
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
    println!("{}", text::STOPPING);
    host.shutdown();
    // Let the Bye reach connected viewers.
    tokio::time::sleep(Duration::from_millis(300)).await;
    endpoint.close();
    let _ = tokio::time::timeout(Duration::from_secs(1), endpoint.wait_idle()).await;
    Ok(())
}

struct ConnectOpts {
    seconds: u64,
    snapshot: Option<PathBuf>,
    port: u16,
    forget_key: bool,
    monitor: Option<u32>,
    move_pointer: bool,
}

async fn connect(dir: &Path, target: &str, auth: AuthMethod, opts: ConnectOpts) -> Result<()> {
    let ConnectOpts {
        seconds,
        snapshot,
        port,
        forget_key,
        monitor,
        move_pointer,
    } = opts;
    let identity = load_identity(dir)?;
    let endpoint = Arc::new(Endpoint::bind(
        identity,
        EndpointConfig {
            bind: any_addr(port),
            listen: false,
            ..EndpointConfig::default()
        },
    )?);
    let target: ConnectTarget = target.parse()?;
    let discovery = match target {
        ConnectTarget::Id(_) | ConnectTarget::Name(_) => Some(Arc::new(
            Discovery::start(DiscoveryConfig::default()).await?,
        )),
        _ => None,
    };
    let viewer = Viewer::new(
        endpoint.clone(),
        ViewerConfig {
            known_hosts: KnownHostsStore::File(dir.join(tlp_desk_crypto::KNOWN_HOSTS_FILE)),
            pixel_format: PixelFormat::Rgba,
            discovery,
            ..ViewerConfig::default()
        },
    )?;
    println!("{}", text::connecting(&target.to_string()));
    if auth == AuthMethod::AcceptPrompt {
        println!("{}", text::WAITING_FOR_ACCEPT);
    }
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
    let (session, mut events) = tokio::select! {
        r = attempt => r?,
        _ = tokio::signal::ctrl_c() => bail!("cancelled"),
    };
    println!(
        "{}",
        text::connected_host(session.peer(), session.permissions())
    );

    let start = Instant::now();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    let mut last: Option<DecodedFrame> = None;
    let mut ended = None;
    let mut want_monitor = monitor;
    let mut pointer_tick = tokio::time::interval(Duration::from_millis(50));
    let mut step = 0u32;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            _ = tokio::signal::ctrl_c() => break,
            _ = pointer_tick.tick(), if move_pointer => {
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
    if let Some(path) = snapshot {
        match last {
            Some(f) => {
                write_png(&path, &f)?;
                println!(
                    "{}",
                    text::snapshot_saved(&path.display().to_string(), f.width, f.height)
                );
            }
            None => println!("{}", text::NO_FRAME),
        }
    }
    drop(session);
    endpoint.close();
    let _ = tokio::time::timeout(Duration::from_secs(1), endpoint.wait_idle()).await;
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
