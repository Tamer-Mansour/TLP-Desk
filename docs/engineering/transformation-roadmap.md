# Transformation roadmap

Order of work, from the audit (`architecture-audit.md`). Each milestone ends with
`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings` and
`cargo test --workspace` green, docs and changelog updated, and one commit per concern.
Status lives in `PROGRESS.md`; what is left and blocked in `REMAINING-WORK.md`.

## P0: connection correctness (done in this branch)

1. Device addresses are a per-source set: the latest mDNS resolution replaces the previous
   one; broadcast source addresses carry a last-heard time and expire one by one.
2. Addresses are ranked for dialing: just-heard broadcast sources (newest first), then our own
   subnets, then other IPv4, then IPv6. This machine's own addresses and scopeless IPv6
   link-local are never dialed.
3. The viewer dials all addresses of a device staggered by 250 ms (happy-eyeballs style),
   each bounded by the handshake timeout; the first pinned QUIC handshake wins and only that
   connection is authorized, so the host never sees two prompts.
4. Self-connection is decided by key, not by address: an address answering with our own key is
   skipped (and refused on the host side, so no self-prompt); dialing our own ID (or the
   service's ID shown on the home screen) is a clear "That is this device" error.
5. A different device at a remembered host's old address reports "wrong device", not "host key
   changed".
6. A network watcher (interface list every 2 s) re-ranks devices, drops broadcast routes
   learned on the old network and re-queries/re-announces at once after Wi-Fi reconnects, DHCP
   changes, VPN changes and sleep/wake.
7. Another PC announcing our own ID (copied identity) is detected and shown on the home screen.

## P1: streaming quality for mismatched resolutions

1. Viewer → host display hint: the viewer reports its viewport size in physical pixels and the
   view mode (additive message, `PROTOCOL_VERSION` bump with fallback); the host picks the
   capture cap from it (no upscaling, never above native, preset ceiling still applies).
   A 4K host in a 1080p window streams ~1080p; full-screen 4K viewer gets native 4K when
   bandwidth allows.
2. Zoom & pan (25–400 %) and "fit / 100 % / fill" with crisp rendering, re-using
   `view_scale.rs` (Lanczos pre-scale when still, linear while moving, nearest at integer
   scale).
3. Text-priority mode: when the screen is static, refine to near-lossless (QP floor) at native
   capture size; "Lossless / maximum clarity" preset that raises the bpp floor and keeps the
   static refinement at the lowest QP, with its bandwidth cost shown.
4. Map the brief's five modes onto the existing presets (Auto = Balanced+adaptive, Best
   quality = Quality, Best performance = Speed, Lossless = new) without breaking stored
   settings.
5. Benchmarks: extend `tlp-desk-host/tests/perf.rs` with 4K synthetic text/IDE/scroll scenes;
   record bitrate, encode time and PSNR/SSIM-like fidelity before/after.

## P2: desktop experience

1. Integrated title bar (`with_decorations(false)` + egui drag region + native resize via
   `ViewportCommand::BeginResize`), menu bar inside it, window buttons, double-click maximize,
   Windows snap preserved; fallback to the native frame on platforms where it misbehaves.
2. Home screen: My device / Remote connection / Devices / Sessions / Settings structure using
   the existing design tokens (`ui/theme.rs`).
3. Session window: integrated toolbar with display, scale, quality, latency/bandwidth,
   security info, clipboard, files, disconnect; works when hidden; keyboard shortcuts.
4. Regenerate README media (`scripts/update-readme-media.ps1`, Windows).

## P3: release infrastructure

The secure updater and signed release pipeline already exist (rc.4). Remaining: Stable/Beta
channel naming in the UI, rollback guidance, update-available toast, CI artifact validation
of the signed manifest.

## P4: polish

Profiling (startup, idle CPU), accessibility pass (AccessKit labels, focus order), file
splitting of the >800-line modules, security review of new code, Arabic review.

## Not planned (by design)

Relay servers, rendezvous/signalling, NAT traversal and any internet service other than the
opt-in update check: they violate the LAN-only rule in CLAUDE.md.
