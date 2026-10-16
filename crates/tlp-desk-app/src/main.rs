//! TLP-Desk: single binary acting as both host and viewer.
//!
//! Command line (all optional):
//! * `--lang en|ar` — override the UI language for this run;
//! * `--connect <id|name|ip[:port]>` — start connecting immediately;
//! * `--code <6 digits>` — the host's one-time code for `--connect`;
//! * `--settings` — open on the settings screen;
//! * `--hidden` — start in the notification area this time (the service uses it so that an
//!   Accept prompt shows only its own window);
//! * `--port <udp port>` — listen on another port than 47400 (testing two instances);
//! * `--fake` — run on the in-process fake backend (no network; development/screenshots);
//! * `--accept-prompt` — internal: the stand-alone Accept window (see `prompt`).
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
            "--hidden" => a.start.start_hidden = true,
            "--fake" => a.fake = true,
            "--port" => a.port = args.next().and_then(|p| p.parse().ok()),
            other => eprintln!("ignoring unknown argument {other:?}"),
        }
    }
    a
}

fn main() -> anyhow::Result<()> {
    // The service, its session agent and (un)installation run headless: never open a window.
    #[cfg(windows)]
    if let Some(code) = tlp_desk_app::service_entry::dispatch() {
        std::process::exit(code);
    }
    // `--accept-prompt`: the stand-alone Accept window (one request on stdin, answer on stdout).
    if let Some(code) = tlp_desk_app::prompt::dispatch() {
        std::process::exit(code);
    }
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

    let Args {
        mut start,
        fake,
        port,
    } = args;
    // Fake devices must not end up in the user's real address book.
    start.address_book_in_memory |= fake;
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
                    // Default permissions, timeouts and the allow-list come from settings.json
                    // (Settings screen), not from constants here.
                    host_settings: Some(settings.host.clone()),
                    connection_log: tlp_desk_app::settings::app_data_dir(),
                    history: tlp_desk_app::settings::app_data_dir(),
                    transfer: tlp_desk_files::TransferConfig {
                        conflict: if settings.host.ask_on_upload_conflict {
                            tlp_desk_files::ConflictPolicy::Ask
                        } else {
                            tlp_desk_files::ConflictPolicy::Rename
                        },
                        download_conflict: tlp_desk_files::ConflictPolicy::Ask,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                backend::real::RealBackend::start_with(&rt, repaint, cfg)
            };
            let mut app = App::new(cc, backend, events, settings, start);
            // Notification-area icon: only the real GUI creates one (never tests or media).
            #[cfg(windows)]
            app.enable_tray(&cc.egui_ctx);
            // Incoming requests also open the stand-alone Accept window.
            app.enable_accept_popup(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("GUI failed: {e}"))?;
    drop(runtime);
    Ok(())
}
