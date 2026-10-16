# Changelog

All notable changes to TLP-Desk are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/) for releases. The wire protocol has its own version
number (`PROTOCOL_VERSION`); peers with different protocol versions refuse to connect, so every
protocol bump is called out below.

## [Unreleased]

### Added

- **Visual design refresh.** A token-based design system (light and dark palettes with semantic colors, spacing, radii, type scale, soft elevation) and new components: button variants with hover states, focus rings, switches, chips, segmented controls, hoverable rows. The home screen has a hero card for this device, segmented tabs and status chips; long IPv6 addresses and long names no longer run under the row buttons. Settings use switches; the session toolbar, stats overlay and Accept card follow the same look. See [Design system](dev/design.md).
- **The Accept prompt is its own window.** A connection request now appears as a separate, always-on-top window in the bottom-right corner of the screen, even while TLP-Desk is minimized, hidden in the notification area or not running (the service starts the GUI quietly in the notification area to ask). It shows the device with a countdown ring, how it asked, permission chips you can switch off for this connection, and Trust this device (remembered for a future auto-accept; it grants nothing yet), with an optional notification sound. The buttons arm a moment after the window appears and Enter never accepts. The same card replaces the old dialog inside the main window, which stays as the fallback. Two new settings under Incoming connections. See [The Accept prompt](user-guide/en/accept-prompt.md). No wire change.
- **Ready for code signing, and a winget package.** The installer scripts and the release workflow sign `TLP-Desk.exe`, `TLP-Desk-cli.exe`, the setup and its uninstaller (SHA-256, timestamped, verified) as soon as a certificate is configured: Azure Artifact Signing, a PFX file or a certificate on a hardware token. Until then builds stay unsigned and say so, which is why Windows SmartScreen warns. A validated winget manifest (`TLP.TLP-Desk`) and a script that refreshes it from each release are included. The user guide explains the SmartScreen window and the winget route; the [code-signing guide](dev/code-signing.md) lists the certificate options. File properties of the exes and the setup now show a proper copyright line and the project links.
- **Drag files from the remote PC into Explorer.** Select files or folders in the file browser's **Remote PC** pane and drag them out of the window onto Explorer, the desktop or any program that accepts virtual files; Windows copies them with its own progress dialog. The files are listed while you drag and pulled through the transfer engine only when dropped (resume, BLAKE3 verification). Needs the Files permission. No wire change.
- **Drop into the remote folder.** Files dropped from Explorer onto the **Remote PC** pane are copied into the folder open there (the pane shows where they go); dropped elsewhere on the session window they still go to the remote Desktop.
- **Everything works when the host runs as the Windows service.** Explorer copy/paste, clipboard sync, file transfer (limited to the signed-in user's folder, arriving on their Desktop), chat, whiteboard, pointer marker, local priority, privacy mode and sound now work under the TLP-Desk service: a helper in the user's session does the desktop parts. The TLP-Desk window shows incoming transfers, "replace or keep both?" questions, chat and the service's sessions in the connection log. Changed host settings are applied by the service at once and saved machine-wide in `%ProgramData%\TLP-Desk\host-settings.json`. `service.log` rotates (4 MB, five older files), and `install-service` / `uninstall-service` print their outcome in a terminal. Local IPC version 2 (service, agent and GUI must be the same release); no wire change.
- CLI `connect` statistics show the quantiser (`Q~`, `"qp"`) and a millisecond timestamp (`"t_ms"`). Hidden, development-only `connect --sim-loss`, `--sim-bandwidth-kbps` and `--sim-seconds` flags simulate a lossy or slow link for testing the adaptive quality on one PC.
- **Several viewers with full control at once.** Two (or more) colleagues can control one PC at
  the same time, each with their own picture, monitor choice, quality, permissions and pointer
  marker; one leaving does not disturb the others. Sessions on the same monitor share one screen
  capturer (each keeps its own encoder), so a second viewer does not double the capture work
  (`HostConfig::share_capture`, `serve --no-shared-capture` to compare).
- **Pause input and View only per viewer.** While several people are connected, each line of the
  host banner can mute that person's mouse and keyboard (permissions unchanged) or make them
  view-only, live (`Host::set_input_paused`).
- **Automatic reconnect after a network drop.** Every session gets a single-use resume ticket
  (bound to the viewer's key, valid 150 s, renewed every 30 s and on every permission change).
  If the connection drops, the viewer reconnects on its own for up to two minutes without a new
  Accept and with the permissions the host allows at that moment, behind a "Connection lost.
  Reconnecting…" overlay (Arabic and English). Deliberate ends revoke the ticket. No wire change:
  it reuses `ReconnectTicket`.

### Changed

- High-resolution wheel deltas (precision touchpads) keep their sub-notch remainder, so slow
  scrolling is no longer lost to rounding.

### Fixed

- **No blur flash after the screen settles.** The frame that follows a still screen (the host's sharp refresh picture, or a single keystroke) is no longer shown bilinear for about 120 ms before the sharp version; the viewer re-scales it at once (pictures up to 1920x1200).
- **`--connect` opens the window normally.** A run started with `--connect` no longer starts minimized (from the start-minimized setting or a console/script launcher).
- The sharp refresh picture after scrolling or typing no longer waits until the mouse stops moving. (Found by the rc.2 [system test](dev/system-test-report.md).)
- `get-file --on-conflict skip` no longer hangs when every file already exists, and `get-file` prints its destination as `C:\...` instead of `\\?\C:\...`.
- Hardware encoder: when Media Foundation refuses a frame the encoder waits for its next request instead of giving up, and a fallback to OpenH264 now logs the real cause.

## [1.0.0-rc.2] - 2026-10-16

Streaming quality and AnyDesk-style file transfer.

### Added

- **Quality first: lower the resolution, never smear the picture.** The encoder has a quality floor (QP 12-32 on OpenH264, best-effort QP bounds on Media Foundation hardware encoders), and when the link gets tight the host steps the resolution down (1920x1200 -> 1600x1000 -> 1280x800 -> 1024x640) and the frame rate (60 -> 45 -> 30) instead of letting the picture turn blocky; it climbs back with hysteresis. Higher default bitrate (about 15 Mbps for 1080p60). The stats overlay shows the effective resolution and an approximate quantiser. See [Performance notes](dev/performance.md).

- **Sharper text.** When the screen stops changing, the host sends one near-lossless refresh picture (QP 14) so text turns crisp after scrolling or typing; the GPU downscaler uses a Lanczos-2 filter for non-2x ratios; the viewer snaps the picture to physical pixels, shows 1:1 views with exact pixels and re-scales a still, shrunk picture with a sharp filter. See [Performance notes](dev/performance.md#sharpness).

- **Copy in Explorer on one PC, paste in Explorer on the other.** Ctrl+C on files or folders on either PC and Ctrl+V in an Explorer window (or the desktop) on the other PC now works in both directions, with Explorer's own copy dialog. The files are offered as virtual clipboard files and pulled only when you paste, using the existing transfer engine (resume, BLAKE3 verification). Needs the Files and Clipboard permissions; revoking either removes the pending paste. The paste buttons and Ctrl+V in the session window stay as a fallback. No wire change.

- **Complete file-transfer experience.** TLP-Desk asks what to do when a file already exists (keep both, replace, skip or resume, per file or for all); a transfer list with pause, resume, cancel, retry, per-file progress, sorting, total speed and a history of recent transfers with send again; a progress pill on the session toolbar; on the host, a **File transfers** window, a banner button and an arrival notice, so a viewer's transfers are never hidden; `--on-conflict` for `get-file` and `serve`. Thousands of small files copy faster.

### Fixed

- **The session toolbar fits any window width.** Items that do not fit move into a **More** menu (least important first); the connection label is shortened with an ellipsis and **Disconnect** is always visible at the reading end, in English and Arabic. A running recording shows a red dot on **More** while its button is in the menu.

## [1.0.0-rc.1] - 2026-10-16

First release candidate: every roadmap feature for Windows is implemented and covered by
automated tests (`cargo test --workspace`, clippy, fmt, cargo-deny and typos are clean) and a
one-PC end-to-end [system test](dev/system-test-report.md). Wire protocol v6. What still needs
two real PCs (privacy overlay, service and lock screen, Ctrl+Alt+Del, restart and reconnect,
real audio, clipboard and Explorer paste, Wake-on-LAN, Wi-Fi performance) is listed in the
[manual test checklist](user-guide/en/manual-test.md). Also included, as previews not yet run on real
hardware: a Linux X11 host/viewer, a macOS host/viewer and an Android viewer (arm64 APK, debug-signed).
Wayland is not supported yet; an MSI is not planned (see ADR 0008).

### Added

- **Arabic text fields behave while you type.** Fields that contain Arabic (address box, device name, search boxes, rename fields, chat, address-book notes) are edited by a new bidi-aware editor: words in the right visual order, a caret and selection that follow the arrow keys on screen, wrapping for multi-line fields. The "Shown as:" preview is gone.
- **Protocol v6, `InputEvent::Text`.** Unicode text typed on the host with `SendInput` (`KEYEVENTF_UNICODE`, surrogate pairs), gated by the keyboard permission. The viewer sends IME commits, emoji and composed text this way, and a new setting **Type text using my keyboard layout** sends every typed character as text so Arabic typed on the viewer appears as Arabic whatever the host layout is. Alt+Shift and Win+Space still switch the host layout. Linux and macOS hosts do not type Unicode yet.
- **Shared host hooks.** Several simultaneous viewers now share one local-activity hook set and one remote-pointer marker (labelled with the names of the viewers moving the pointer).
- **Landing page** in English and Arabic (`docs/site/`), published at the GitHub Pages root with the book under `/book/`.

- **Linux host and viewer (X11).** Screen capture through X11 `GetImage` with RandR monitors, the XFixes cursor and a CPU downscale for the size cap; input through XTest with an evdev key table; an `XGrabKeyboard` viewer keyboard grab; system sound from the PulseAudio/PipeWire monitor source (never the microphone); Wake-on-LAN MAC lookup from `/proc/net/arp`; XDG data and autostart folders; the xdg-desktop-portal file picker; a tarball with `install.sh` and a `.desktop` entry; a Linux CI job. Cross-compiled and linted for `x86_64-unknown-linux-gnu` from Windows, not yet run on real Linux machines. Wayland screen sharing (ScreenCast portal + PipeWire), privacy mode, the pointer marker, the host whiteboard overlay, local-input priority and the tray icon are not available on Linux yet. See [Linux](dev/linux.md). No wire change.

- **macOS host and viewer (experimental).** Screen capture with Quartz Display Services, mouse and keyboard injection with `CGEvent` (Windows scancodes mapped to macOS key codes), Screen Recording and Accessibility permission checks with an Arabic/English guidance card that opens the right System Settings pane, data in `~/Library/Application Support/TLP-Desk`, an unsigned `.app` bundle script and a macOS CI job. Written and cross-checked without a Mac; not yet run on real hardware. See [TLP-Desk on a Mac](user-guide/en/macos.md). (PR #40)
- **Android viewer (experimental).** New `tlp-desk-android` crate: connect by IP or `id@ip`, tap/hold/drag/two-finger scroll/pinch-zoom touch controls, soft keyboard (Arabic through the clipboard), Arabic and English UI with right-to-left layout. Viewer only; not yet run on a device. See [TLP-Desk on Android](user-guide/en/android.md). (PR #40)
- **Windows service, secure desktop and Ctrl+Alt+Del.** `TLP-Desk.exe install-service` runs TLP-Desk as a SYSTEM service that starts a session agent in the active console session and follows user switching, so the sign-in screen, lock screen and UAC prompts can be seen and controlled. Ctrl+Alt+Del is sent with `SendSAS` from the service. (PR #9)
- **Permanent password, restart and reconnect, lock.** An unattended-access password (Argon2id at rest, never on the wire, 5 attempts per minute then a 10-minute lockout), **Restart remote PC** with a one-use reconnect ticket and automatic reconnect, and **Lock remote PC**. (PR #11)
- **Privacy mode.** Blanks the host's screens and blocks its local input during a session; ends with Ctrl+Alt+Shift+P (3 s) or Ctrl+Alt+Del, on disconnect or when permission is withdrawn. (PR #19)
- **Hardware H.264 encoding** through Media Foundation (Intel Quick Sync, NVENC and AMF where available) with an automatic software (openh264) fallback; `TLP_DESK_ENCODER=software` forces the fallback. (PR #23)
- **Adaptive quality, presets, latency overlay and all-monitors view.** Adaptive bitrate and frame dropping, Speed / Balanced / Quality presets, a latency probe in the statistics overlay and a view of all monitors at once. (PR #15)
- **Address book and Wake-on-LAN.** Saved devices with aliases, groups, last seen and online status; magic packets from the address book and `wol` in the CLI. (PR #17)
- **Fast file transfer** over parallel side streams with resume and BLAKE3 verification. (PR #22; measured 330-414 MiB/s on loopback)
- **Typing fix** so remote keystrokes always arrive. (PR #34)
- **QA hardening.** Hostile-peer tests, a soak test, flaky-test and port-leak fixes. (PR #6)
- Documentation: full user guide in English and Arabic, README screenshots and demo video generated from the real UI (PR #1, #3), the icon set (PR #2), a two-PC manual test checklist and the system test report.

- **System audio (sound from the remote PC).** The host captures its default output (WASAPI loopback through `cpal`) only while a viewer has sound on, converts it to 48 kHz stereo, codes it with Opus in 10 ms frames and sends nothing while it is silent; packets travel on `StreamKind::Audio` side streams (`u16` length + packet). The viewer plays through a jitter buffer of about 30 ms (never above 60 ms; late packets are dropped, lost ones concealed, drift corrected). Gated by the `Audio` permission (off by default, live-revocable). The session toolbar has a speaker toggle and volume slider; Settings > Sound sets the default for new sessions. Sound is not yet recorded into session recordings. No wire change (v4 `AudioControl`).
- **Settings, tray, host banner and connection log.** Settings now has sections for incoming connections (default permission checkboxes, Accept timeout, one-time code on/off, local-input grace, remote-pointer marker), an allowed-networks (CIDR) editor with a public-range warning, quality presets (Speed / Balanced / Quality), start with Windows (HKCU Run key behind a trait), start minimised and close-to-tray, and applies host options live (`Host::set_live_options`). `settings.json` is versioned and migrates older files; the settings are the source of truth for the host's default permissions (files, chat, whiteboard and clipboard on, port forwarding off). A notification-area icon (`tray-icon`) has Open, Copy ID, New code, Pause incoming, Settings, Connection log and Quit, an ID tooltip, and brings the window forward for an incoming request. A small topmost banner window shows who is connected while the main window is minimised (not excluded from screen capture, on purpose). A JSON-lines connection log (`%APPDATA%\TLP-Desk\connections.log`, rotated) records every session and refused attempt, with a viewer screen (search, filter, CSV export). No wire change.

- **Remote device info and CLI completion.** The host answers a viewer's `DeviceInfoRequest` (needs `View`) with its computer name, OS, CPU, memory, disks and uptime; the viewer toolbar has a Device info window with refresh, Arabic digits and units. CLI: `info <target>`, `wol <MAC|alias>` (alias `exec-wol`), `list --json`, a stable exit-code table in `--help`, `--lang ar` with Arabic `--help` and UTF-8 console output, password only through `TLP_DESK_PASSWORD`. No wire change (v4 messages).
- **Port forwarding (tunnels).** The viewer listens on a local port (loopback by default; LAN binding needs an explicit choice and shows a warning) and pipes each accepted TCP connection through the session to a host:port on the remote PC or its own LAN, over a `Tunnel` side stream per connection with half-close. The host side is gated by the `Tunnel` permission, which is off by default: the host banner has an **Allow port forwarding** switch, and revoking it closes every tunnel at once. Targets are limited to loopback and the host's own local network (never public addresses), with caps on tunnels and connections and an idle timeout. App: a port forwarding window with rules, connection counts and bytes. CLI: `forward <target> <local>:<host>:<port>` and `serve --allow-tunnels`. No wire change (v5 messages).
- **Whiteboard (draw on screen).** The viewer's Draw button turns the pointer into a pen, highlighter, arrow, rectangle or eraser with colors, widths, undo/redo, clear and optional fade-out; no input reaches the remote PC while it is on. Strokes are simplified (Ramer-Douglas-Peucker), streamed in chunks while drawing and drawn again locally on the viewer. The host shows them in a topmost, click-through layered window over the streamed monitor (excluded from capture) with a clear button; revoking `Whiteboard` or ending the session removes it. Sessions get `Whiteboard` by default in the app. No wire change (v4 messages).
- **Session recording and playback.** The viewer records a session into a standard fragmented MP4 (H.264 stored as received, no re-encoding, no wire change) at `%USERPROFILE%\Videos\TLP-Desk\TLP-Desk <peer> <date time>.mp4`. Crash-safe (one-second fragments, interrupted files can be repaired), a new file starts when the remote picture size changes, a keyframe is requested at the start and every 4 seconds, and recording refuses to start on a nearly full disk. The toolbar has a record toggle with a red indicator and elapsed time; the home screen has a Recordings list (play, open folder, delete, repair) and a playback window (play/pause, keyframe seek, 0.5x-4x, fit/actual size). The host is not notified; the indicator is viewer-only.
- **Clipboard sharing.** Text, formatted text (HTML) and images are copied between the two PCs
  in both directions while the host grants `Clipboard`; the permission is re-checked on every
  send and apply, so revoking it stops sharing at once. The viewer toolbar has a "Sync clipboard"
  switch and Settings has the default for new sessions. Change detection is event-driven on
  Windows (`AddClipboardFormatListener`, no polling). Arabic text is exact (RLM/LRM, ZWJ/ZWNJ,
  harakat, CRLF). Images travel as PNG (`ClipboardData::Png`, protocol v5); payloads over 1 MiB
  use a clipboard side stream; caps are 16 MiB for text and HTML and 64 MiB decoded for images,
  checked before allocating. Echo loops are prevented, duplicates are dropped, and only kinds and
  sizes are ever logged. CLI: `clip-send`, `serve --clipboard`.
- **Remote file browser and Explorer copy/paste.** A two-pane file manager (this PC | remote PC,
  mirrored in Arabic) opens from the session toolbar: drives and known folders, breadcrumbs,
  sorting, multi-select, new folder, rename, delete with confirmation, and copy between the panes
  through the transfer engine. Files copied in Explorer on either PC can be pasted on the other
  with a button (no delayed-render clipboard object, so not Ctrl+V inside Explorer). The host
  serves the `FsRequest` messages only while the session holds `Files`, re-checked on every
  request, and refuses traversal, alternate data streams, devices and network paths. CLI:
  `ls`, `mkdir`, `rm`, `mv`.
- **Wire protocol v4** — the message set for the rest of the roadmap, defined ahead of the
  features: remote file system (`FsRequest` / `FsResponse`), file transfer (`TransferOffer` …
  `TransferError`), clipboard HTML and files, chat, audio control, session and system control
  (Ctrl+Alt+Del, lock, restart, privacy mode, reconnect tickets), quality and latency probes, an
  all-monitors view, TCP tunnels, whiteboard and device info. Every message declares the
  permission it needs (`Message::required_permissions()`). **v4 peers cannot connect to v3
  peers.** No user-visible feature uses the new messages yet.
- Permission bits `CHAT` (8), `TUNNEL` (9), `WHITEBOARD` (10) and `SYSTEM` (11, for
  Ctrl+Alt+Del and lock). Existing bits keep their positions.
- Typed streams: every non-control QUIC stream starts with a 9-byte header (stream kind + 64-bit
  id). Video uses it, and features can open **side streams** (file data, tunnel, audio,
  clipboard) with per-kind accept queues and QUIC priorities that keep bulk data behind input and
  video. Flow-control windows were enlarged for LAN-speed bulk transfers.
- Session feature plug-ins: features register once and the host and viewer route their messages
  to them per session, after checking permissions.
- GUI icon set (vendored SVG icons with RTL mirroring); the session toolbar uses icon buttons
  with tooltips.
- Continuous integration on GitHub Actions (fmt, clippy, tests, release build, spelling,
  dependency policy), issue and pull-request templates, Dependabot.
- Project documentation book (this site) in English and Arabic.
- Windows installer (`TLP-Desk-<version>-setup.exe`, Inno Setup, Arabic + English) that adds
  Windows Firewall rules for private and domain networks only, and a portable zip.
  Both builds write `SHA256SUMS.txt`.
- Graphical app (`TLP-Desk.exe`): home screen with device ID, name, IP and one-time code;
  list of devices on the network; connect by ID, name or IP; Accept prompt; session window with
  fit / actual size, fullscreen, view-only, monitor picker and a stats overlay; settings for
  language and digit style. Full Arabic (RTL) and English interface.
- Command-line tool (`TLP-Desk-cli.exe`) for headless hosting and scripted connections.
- Host: Windows.Graphics.Capture with DXGI Desktop Duplication fallback, GPU downscaling of
  large screens, H.264 (openh264) encoding at up to 60 fps, frames dropped instead of queued
  under backpressure, live monitor switching.
- Input: mouse and keyboard injection with layout-independent scancodes and exact coordinate
  mapping across resolutions and DPI scaling; the person at the host always has priority
  (remote input pauses for 1 s after local activity); a green remote-pointer marker on the host
  that is excluded from the captured video.
- Network: QUIC over UDP 47400 with mutual TLS 1.3, 9-digit device IDs derived from the identity
  key and pinned on connect, trust-on-first-use with a loud warning when connecting by IP,
  SPAKE2 one-time-code authentication with channel binding and key confirmation, rate limiting
  (5 attempts per minute, then a 10-minute lockout), LAN allow-list, mDNS discovery
  (`_tlpdesk._udp.local.`) with a UDP 47401 broadcast fallback.
- Wire protocol v1–v3: v2 added SPAKE2 key confirmation, keyframe requests and one QUIC stream
  per video frame; v3 added local input priority notices and the monitor list.

### Fixed

- **Typing on the host did not arrive.** The viewer dropped window key events whenever the OS
  keyboard grab was on, even when the grab had not captured the key (hook removed by Windows,
  mismatched window handle), and a toolbar click left keyboard focus on the button. Keys the
  grab misses are now sent from the window, the grab reinstalls its hook, and the session view
  takes keyboard focus back (unless a text field has it).
- **A local mouse blocked remote keys.** Host-side "local user wins" priority is now per device:
  local typing holds back remote keys, local mouse use holds back only the remote pointer.
- The host shows a notice (and the CLI prints a line) when Windows refuses injected input because
  an administrator window or secure screen is in front; the service lane lifts that limit.
- `connect --type TEXT --then-ctrl S` types US-layout scancodes into the host for testing.

### Changed

- **Arabic wording review.** One Arabic term per concept across the interface, the user guide and the landing page (terminology table in the contributor docs); a native-speaker review is still recommended.
- Service notices are documented as localized; the Windows Event Log is not used.

- Product renamed to **TLP-Desk** everywhere users see it (window title, executables, installer,
  firewall rules). Crate names, the mDNS service type, the ALPN and other wire identifiers are
  unchanged on purpose, so existing device IDs and peers keep working.

[Unreleased]: https://github.com/Tamer-Mansour/TLP-Desk/compare/v1.0.0-rc.1...TLP-Desk
[1.0.0-rc.1]: https://github.com/Tamer-Mansour/TLP-Desk/releases/tag/v1.0.0-rc.1
