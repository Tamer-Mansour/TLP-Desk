//! TLP-Desk: single binary acting as both host and viewer.
//!
//! Command line (all optional):
//! * `--lang en|ar` — override the UI language for this run;
//! * `--connect <id|name|ip[:port]>` — start connecting immediately;
//! * `--code <6 digits>` — the host's one-time code for `--connect`;
//! * `--settings` — open on the settings screen;
//! * `--port <udp port>` — listen on another port than 47400 (testing two instances);
//! * `--fake` — run on the in-process fake backend (no network; development/screenshots).
#![forbid(unsafe_code)]
// Hide the console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::{IpAddr, Ipv6Addr, SocketAddr};

use anyhow::Context as _;

use tlp_desk_app::app::{App, StartOptions};
use tlp_desk_app::backend;
use tlp_desk_app::i18n::{tr_raw, Lang, S};
use tlp_desk_app::settings::Settings;
use tlp_desk_app::ui;

/// Window/taskbar icon: the same artwork as the exe icon (scripts/make_icon.py).
fn window_icon() -> eframe::egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon/TLP-Desk-256.png"))
        .unwrap_or_else(|_| ui::theme::app_icon())
}

struct Args {
    start: StartOptions,
    fake: bool,
    port: Option<u16>,
}

fn parse_args() -> Args {
    let mut a = Args {
        start: StartOptions::default(),
        fake: false,
        port: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--lang" => a.start.lang = args.next().as_deref().and_then(Lang::parse),
            "--connect" => a.start.connect = args.next(),
            "--code" => a.start.code = args.next(),
            "--settings" => a.start.screen_settings = true,
            "--fake" => a.fake = true,
            "--port" => a.port = args.next().and_then(|p| p.parse().ok()),
            other => eprintln!("ignoring unknown argument {other:?}"),
        }
    }
    a
}

fn main() -> anyhow::Result<()> {
    let args = parse_args();
    // Before any window exists: per-monitor DPI awareness keeps capture, input mapping and
    // the UI in physical pixels.
    if let Err(e) = tlp_desk_input::ensure_dpi_aware() {
        eprintln!("could not enable per-monitor DPI awareness: {e}");
    }
    let settings = Settings::load_or_first_run();
    let lang = args.start.lang.unwrap_or(settings.lang);

    // Backend work runs on this runtime; the UI thread only drains channels.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("tlp-desk-backend")
        .enable_all()
        .build()
        .context("starting tokio runtime")?;
    let rt = runtime.handle().clone();

    let viewport = eframe::egui::ViewportBuilder::default()
        .with_title(tr_raw(lang, S::AppTitle))
        .with_inner_size([1000.0, 660.0])
        .with_min_inner_size([760.0, 520.0])
        .with_icon(window_icon());
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    let Args { start, fake, port } = args;
    eframe::run_native(
        "TLP-Desk",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            let repaint: backend::RepaintFn = std::sync::Arc::new(move || ctx.request_repaint());
            let (backend, events) = if fake {
                backend::fake::FakeBackend::start(&rt, repaint)
            } else {
                let cfg = backend::real::RealConfig {
                    device_name: settings.device_name.clone(),
                    bind: SocketAddr::new(
                        IpAddr::V6(Ipv6Addr::UNSPECIFIED),
                        port.unwrap_or(tlp_desk_net::DEFAULT_PORT),
                    ),
                    clipboard: Some(std::sync::Arc::new(tlp_desk_files::SystemClipboard::new())),
                    // Clipboard sync is part of a normal session (revocable live by the host).
                    host: tlp_desk_host::HostConfig {
                        default_permissions: tlp_desk_host::DEFAULT_PERMISSIONS
                            .union(tlp_desk_proto::Permissions::CLIPBOARD)
                            .union(tlp_desk_proto::Permissions::WHITEBOARD),
                        ..Default::default()
                    },
                    ..Default::default()
                };
                backend::real::RealBackend::start_with(&rt, repaint, cfg)
            };
            Ok(Box::new(App::new(cc, backend, events, settings, start)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("GUI failed: {e}"))?;
    drop(runtime);
    Ok(())
}
