# Linux

Linux is a tier-2 platform: the host and viewer are built for it and cross-compiled in the
Windows development setup, but have **not yet been run on a real Linux machine**. This page
says what exists, how each part works, how to check it compiles from Windows, and what is still
missing.

## What exists

| Concern | Linux implementation | Where |
|---|---|---|
| Screen capture | X11: `GetImage` of the root window per RandR monitor, XFixes cursor image and position, CPU area-average downscale for `max_size` | `crates/tlp-desk-capture/src/linux_capture/` |
| Input injection | X11: XTest `FakeInput`, evdev keycodes, wheel as buttons 4-7 | `crates/tlp-desk-input/src/linux/`, `src/evdev.rs` |
| Viewer keyboard grab | X11: `XGrabKeyboard` on a worker thread, detectable auto-repeat through XKB | `crates/tlp-desk-input/src/linux/grab.rs` |
| System sound | cpal (ALSA, or PulseAudio/PipeWire through their ALSA plugins) on the *monitor* capture device | `crates/tlp-desk-audio/src/device/` |
| Wake-on-LAN MAC lookup | `/proc/net/arp` | `crates/tlp-desk-net/src/neighbor/linux.rs` |
| Identity folder | `~/.local/share/tlp-desk`, mode 0700 (existing `harden_dir`) | `crates/tlp-desk-crypto/src/storage.rs` |
| Settings, address book, log | `$XDG_CONFIG_HOME/tlp-desk` or `~/.config/tlp-desk` | `crates/tlp-desk-app/src/settings/mod.rs` |
| Start with the session | `~/.config/autostart/TLP-Desk.desktop` | `crates/tlp-desk-app/src/settings/autostart.rs` |
| File picker | `rfd` with the xdg-desktop-portal backend (no GTK) | `crates/tlp-desk-app/Cargo.toml` |
| Packaging | tarball + `install.sh` + `.desktop` | `installer/linux/` |

The X11 code uses `x11rb`'s pure-Rust `RustConnection`: there is no libxcb to build or link, and
no `unsafe` anywhere in the Linux backends.

### Capture details

* **Pixel format.** Only 32-bit little-endian `0xXXRRGGBB` (the layout of every desktop X
  server) is accepted. The bytes are already BGRA; the unused byte is forced to `0xFF`. Other
  formats are refused with a clear error instead of being captured wrongly.
* **Bands.** The monitor is read as horizontal bands of at most 4 MiB. All `GetImage` requests
  are sent before the first reply is read, so the server never idles. A failed request means
  the geometry changed and is reported as `CaptureError::SurfaceLost`.
* **Pacing.** X11 has no cheap "the screen changed" signal here, so `next_frame(timeout)` acts
  as a frame timer (one capture per `timeout`). The host's dirty tracker already drops frames
  identical to the previous one.
* **Monitors.** RandR 1.5 `GetMonitors` (active monitors). Primary first, then left-to-right,
  top-to-bottom. Without RandR 1.5 the whole screen is one monitor.
* **Cursor.** XFixes `GetCursorImage`; premultiplied ARGB is converted to straight RGBA. The
  shape is sent when the cursor serial changes.
* **MIT-SHM.** Not used: it needs `shmget`/`shmat` (`unsafe`). The protocol-level result is the
  same; SHM would save one socket copy per frame and is a possible follow-up.

### Input details

Wire keys are Windows set-1 scancodes plus the `E0` flag. For the main block the evdev code is
the same number; the `E0` cluster (arrows, Home/End, Insert/Delete, right Ctrl/Alt, Win keys,
Menu, keypad Enter and divide, PrintScreen) is renumbered by an explicit table in
`src/evdev.rs`. The X keycode is `evdev + 8`. Because keys are injected by keycode, the
host's active XKB layout decides the character (Arabic, English, ...), exactly as on Windows.
Pointer events are absolute XTest motion in root-window pixels (the monitor origin from RandR
is added). Wheel deltas (1/120 notch units) become button 4/5/6/7 clicks with the sub-notch
remainder carried over. The tracking injector releases everything held on `ReleaseAll` and on
drop, like the Windows one.

### Sound

Linux has no WASAPI-style loopback. PulseAudio and PipeWire expose what the speakers play as a
*monitor source*, which appears as a capture device. The host only uses a capture device whose
name contains `monitor`, or the one named by `TLP_DESK_AUDIO_SOURCE` (substring, case
insensitive). If none is found, capture fails with a message. It never falls back to the
default input, because that is the microphone.

## Not implemented on Linux (degrades gracefully)

These return `Unsupported` (or are absent) and the host keeps working without them:

* local-input priority (`LocalActivityMonitor`), remote pointer marker, privacy mode and the
  host whiteboard overlay (all need a compositor-level overlay or input hook);
* the tray icon (the `tray-icon` crate needs libappindicator/GTK on Linux): closing the window
  quits;
* the machine-wide service, secure desktop and Ctrl+Alt+Del: use the app or `TLP-Desk-cli serve`
  inside the user session.

## Wayland: design for the next step

A Wayland compositor does not let a client read the screen or inject input. The supported route
is `xdg-desktop-portal`:

1. **Capture.** `org.freedesktop.portal.ScreenCast`: `CreateSession`, `SelectSources`
   (monitor, cursor mode *metadata*), `Start` (shows the user a dialog), `OpenPipeWireRemote`.
   The returned fd carries a PipeWire stream; frames arrive as `SPA_VIDEO_FORMAT_BGRx`
   (or DMA-BUF, which must be negotiated down to memory buffers). The portal session token can
   be stored to skip the dialog next time.
2. **Input.** `org.freedesktop.portal.RemoteDesktop` on the same session:
   `NotifyPointerMotionAbsolute`, `NotifyPointerButton`, `NotifyPointerAxisDiscrete`,
   `NotifyKeyboardKeycode` (evdev codes, the same table as above).
3. **Crates.** `ashpd` (portal client) and `pipewire` (needs `libpipewire` headers and clang at
   build time, so it would sit behind a `wayland` cargo feature, default off).

Until that exists, capture on a Wayland session fails with an explicit message
(`TLP_DESK_FORCE_X11=1` captures the XWayland root, which shows only X11 windows and is usually
black), and input injection and the keyboard grab report that the RemoteDesktop portal is
missing. Run the host in an "... on Xorg" session. The viewer runs on Wayland too: the GUI
toolkit supports it, and without the keyboard grab the app falls back to the key events the
window receives (system keys such as Super then stay local).

## Checking Linux compiles from Windows

No WSL or Linux toolchain is needed to cross-check. The pure-Rust crates check with just the
target:

```powershell
rustup target add x86_64-unknown-linux-gnu
cargo check --target x86_64-unknown-linux-gnu -p tlp-desk-input -p tlp-desk-capture
```

Crates with C build scripts (`ring`, `blake3`, `openh264`, `opus`, `alsa-sys`) need a cross C
compiler. This is how it was set up (all under a tools folder outside the repo):

* [Zig](https://ziglang.org/download/) as the C/C++ compiler: a tiny wrapper executable that
  calls `zig cc -target x86_64-linux-gnu.2.31 ...` and drops cc-rs's `--target=` flag (Zig
  rejects the Rust triple). Point `CC_x86_64_unknown_linux_gnu`, `CXX_...` and `AR_...` at the
  wrappers.
* CMake and Ninja (for libopus), `CMAKE_GENERATOR=Ninja`, and a toolchain file that sets
  `CMAKE_C_COMPILER`, `CMAKE_CXX_COMPILER`, `CMAKE_AR` and `CMAKE_RANLIB` to the wrappers
  (`CMAKE_TOOLCHAIN_FILE`).
* `alsa-sys` only probes `pkg-config`; a stub `PKG_CONFIG` that prints `-lasound` is enough for
  `cargo check` (linking is not performed).

Keep these variables out of your normal Windows shell: they would break host builds.

With that, `cargo check` and `cargo clippy --all-targets -- -D warnings` pass for the
`proto`, `crypto`, `net`, `capture`, `input`, `files`, `codec`, `audio`, `host`, `viewer`,
`cli` and `app` crates on `x86_64-unknown-linux-gnu`. That proves the code compiles and lints
clean; it does not prove it behaves. Linking and running need a real Linux machine.

## Testing on Linux

```bash
sudo apt-get install -y build-essential pkg-config cmake nasm libasound2-dev xvfb xauth
cargo test --workspace
# Real X11 backends against a virtual display (never touches a real screen):
xvfb-run -a -s "-screen 0 1280x800x24 +extension XTEST +extension RANDR" \
  cargo test -p tlp-desk-capture -p tlp-desk-input -- --ignored x11 connects_and_releases
```

The pure parts (key table, wheel clicks, band arithmetic, monitor ordering, cursor conversion,
CPU downscale, session detection, ARP parsing, monitor-device choice, XDG paths, `.desktop`
quoting) are ordinary unit tests and also run on Windows.

## Manual test (two machines)

1. On the Linux PC (X11 session) run `TLP-Desk`; note the device ID.
2. From a Windows PC connect by ID, accept on Linux. Check: picture of every monitor, mouse
   move/click/wheel, typing in English and Arabic (switch the Linux layout), Super and Alt+Tab
   with the keyboard grab on, clipboard both ways, file transfer, sound with `pavucontrol`
   showing a *Monitor of ...* source.
3. Connect the other way (Linux viewer to a Windows host) and repeat.
