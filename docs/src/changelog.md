# Changelog

All notable changes to TLP-Desk are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/) for releases. The wire protocol has its own version
number (`PROTOCOL_VERSION`); peers with different protocol versions refuse to connect, so every
protocol bump is called out below.

## [Unreleased]

The workspace version is `0.1.0`; nothing has been tagged yet.

### Added

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

- Product renamed to **TLP-Desk** everywhere users see it (window title, executables, installer,
  firewall rules). Crate names, the mDNS service type, the ALPN and other wire identifiers are
  unchanged on purpose, so existing device IDs and peers keep working.

[Unreleased]: https://github.com/Tamer-Mansour/TLP-Desk/commits/TLP-Desk
