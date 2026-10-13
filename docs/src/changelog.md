# Changelog

All notable changes to TLP-Desk are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/) for releases. The wire protocol has its own version
number (`PROTOCOL_VERSION`); peers with different protocol versions refuse to connect, so every
protocol bump is called out below.

## [Unreleased]

The workspace version is `0.1.0`; nothing has been tagged yet.

### Added

- **Wire protocol v4** — remote file-system messages: `FsRequest { req_id, op }` and
  `FsResponse { req_id, result }` with the operations `ListDir`, `Stat`, `Roots`, `MakeDir`,
  `Rename` and `Delete`. Every message now declares the permission the host enforces for it
  (`Message::required_permissions()`). Groundwork for the remote file browser; there is no UI yet.
- Permission bits `CHAT` (8), `TUNNEL` (9), `WHITEBOARD` (10) and `SYSTEM` (11, for
  Ctrl+Alt+Del and lock). Existing bits keep their positions.
- Typed stream header for side streams: every non-control QUIC stream will start with a 9-byte
  header (stream kind + 64-bit id). Kinds: video, file data, tunnel, audio, clipboard.
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

### Changed

- Product renamed to **TLP-Desk** everywhere users see it (window title, executables, installer,
  firewall rules). Crate names, the mDNS service type, the ALPN and other wire identifiers are
  unchanged on purpose, so existing device IDs and peers keep working.

[Unreleased]: https://github.com/Tamer-Mansour/TLP-Desk/commits/TLP-Desk
