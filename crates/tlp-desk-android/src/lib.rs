//! TLP-Desk for Android: a touch **viewer** that connects to a TLP-Desk host (Windows) on the
//! same local network, shows its screen and controls it. Android cannot be a host.
//!
//! # Layout
//! * [`gesture`]: touch recognizer (tap, long press, drag, two-finger scroll, pinch zoom) and the
//!   mapper that turns gestures into wire `InputEvent`s. Pure logic, unit-tested on any OS.
//! * [`view`]: zoom/pan of the picture inside the phone screen.
//! * [`keyboard`]: soft-keyboard text to key presses (US layout) or clipboard paste (Arabic and
//!   any other character a scancode cannot express).
//! * [`session`]: dials the host on a background tokio runtime through `tlp-desk-viewer`.
//! * [`ui`]: the egui connect form and session view; [`i18n`]: Arabic/English strings with
//!   the shared RTL shaping.
//!
//! The Android entry point is [`android_main`] (NativeActivity, loaded from the APK).
//!
//! # LAN discovery on Android
//! Connecting by IP address always works. Connecting by device ID needs mDNS, and Android drops
//! multicast packets unless the app holds a `WifiManager.MulticastLock`. The manifest already
//! requests `CHANGE_WIFI_MULTICAST_STATE`; acquiring the lock needs a JNI call
//! (`WifiManager.createMulticastLock("tlp-desk").acquire()`) that is **not implemented yet**,
//! so by-ID connections fall back to the UDP broadcast probe, which many phones also block.
//! Use `<id>@<ip>` or the plain IP. See `docs/src/dev/platforms.md`.
//!
//! # Not yet on Android
//! File transfer, chat, audio, monitor switching, quality presets and recording are not wired
//! into the touch UI; the session itself supports them.
#![deny(unsafe_code)]

pub mod gesture;
pub mod i18n;
pub mod keyboard;
pub mod session;
pub mod ui;
pub mod view;

/// Entry point called by `android-activity` when the activity starts.
// SAFETY: `android_main` is the one symbol the NativeActivity glue looks up by name; nothing else
// in this library uses that name.
#[cfg(target_os = "android")]
#[allow(unsafe_code)]
#[no_mangle]
pub fn android_main(app: android_activity::AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );
    let data_dir = app.internal_data_path().unwrap_or_else(std::env::temp_dir);
    let options = eframe::NativeOptions {
        android_app: Some(app),
        ..Default::default()
    };
    if let Err(e) = eframe::run_native(
        "TLP-Desk",
        options,
        Box::new(move |cc| Ok(Box::new(ui::ViewerApp::new(cc, data_dir)?))),
    ) {
        log::error!("TLP-Desk stopped: {e}");
    }
}
