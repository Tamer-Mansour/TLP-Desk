# tlp-desk — Project Rules for Claude Code

tlp-desk is a free, open-source, **LAN-only** remote desktop app (AnyDesk-style) written in pure Rust.
It works 100% offline inside a local network: no cloud, no relay server, no account, no telemetry.
One single binary acts as both **host** (being controlled) and **viewer** (controlling), like AnyDesk.

Read `ROADMAP.md` before starting any task. Work only on the lane/milestone you were assigned.

## Hard rules

1. **Clean-room.** Never copy code, protocol, assets, icons, names or UI text from AnyDesk, RustDesk,
   TeamViewer or any other remote desktop product. RustDesk is AGPL — do not read its source while
   implementing. Use only public OS documentation (Microsoft Learn, Apple docs, freedesktop) and crate docs.
2. **No network beyond the LAN.** No outbound internet calls, no update checks, no analytics, no crash
   uploads. The app must work with the Ethernet cable unplugged from the router's WAN port.
3. **Security is not optional.** Every connection is encrypted (QUIC/TLS 1.3) and authenticated
   (see Security model). No "debug mode" that disables auth may ship in release builds.
4. **Windows first.** Windows 10/11 x64 is the tier-1 target. Keep platform code behind traits so
   macOS/Linux can be added later, but do not block Windows work on them.
5. **Done = tested.** A feature is done only when: it builds with `cargo build --release`,
   `cargo clippy --workspace -- -D warnings` is clean, `cargo test --workspace` passes, and its
   acceptance criteria in ROADMAP.md are checked off with evidence (test name or manual test note).
6. **Small PRs per lane.** Touch only your crate unless the task says otherwise. Changes to
   `tlp-desk-proto` (wire protocol) require bumping `PROTOCOL_VERSION` and updating `docs/protocol.md`.

## Workspace layout

```
crates/
  tlp-desk-proto     # wire messages (serde + postcard), PROTOCOL_VERSION, framing
  tlp-desk-crypto    # identity keys, device ID, cert pinning, SPAKE2 password auth
  tlp-desk-net       # QUIC (quinn) transport, mDNS discovery, broadcast fallback, Wake-on-LAN
  tlp-desk-capture   # screen capture trait + Windows impl (Windows.Graphics.Capture / DXGI)
  tlp-desk-codec     # video encode/decode trait + H.264 impl (openh264), later HW encoders
  tlp-desk-input     # input injection trait + Windows impl (SendInput), key mapping
  tlp-desk-audio     # system audio loopback (cpal/WASAPI) + Opus
  tlp-desk-files     # file transfer + clipboard sync (arboard)
  tlp-desk-host      # host session logic, permissions, Windows service, desktop switching
  tlp-desk-viewer    # viewer session logic, frame pipeline, input capture
  tlp-desk-app       # the single GUI binary (egui/eframe + wgpu), tray icon, settings
  tlp-desk-cli       # headless CLI (list devices, connect, send file) for scripts/agents
installer/           # Inno Setup setup.exe + portable zip build scripts
docs/                # protocol.md, security.md, user-guide (Arabic + English)
```

## Tech choices (do not change without updating this file)

| Concern | Choice |
|---|---|
| Async runtime | `tokio` |
| Transport | `quinn` (QUIC), UDP port **47400**; control on a bidi stream, video on its own uni stream |
| Discovery | `mdns-sd`, service type `_tlpdesk._udp.local.`; UDP broadcast fallback on **47401**; manual IP always allowed |
| Serialization | `serde` + `postcard`, length-prefixed frames |
| Identity / TLS | `rcgen` self-signed cert per install, `rustls` custom verifier |
| Password auth | `spake2` (PAKE) — the password never crosses the wire |
| Capture (Win) | `windows` crate: Windows.Graphics.Capture, DXGI Desktop Duplication fallback |
| Video codec | `openh264` (software) first; Media Foundation / NVENC / QSV / AMF later behind a trait |
| Input (Win) | `windows` crate `SendInput`; Ctrl+Alt+Del via `SendSAS` from the service |
| Clipboard | `arboard` |
| Audio | `cpal` (WASAPI loopback) + `opus` |
| GUI | `egui` / `eframe` with `wgpu` backend; video frames uploaded as textures |
| Service | `windows-service` crate |
| Logging | `tracing`, local rotating log files only |
| Installer | Inno Setup 6 single setup.exe (Arabic+English), cargo-wix MSI later if needed + portable zip |

## Security model (must hold at all times)

- Each install generates a long-term key pair on first run, stored in `%ProgramData%\tlp-desk\` (ACL: SYSTEM + Administrators).
- **Device ID** = 9 digits derived from SHA-256 of the public key. Connecting by ID therefore pins the
  key: a viewer must reject any host whose cert does not hash to the ID it dialed (MITM-proof on LAN).
- When connecting by IP/hostname only, use trust-on-first-use and warn loudly if the key changes.
- Host authorizes a session by one of: (a) user clicks **Accept** on the host, (b) one-time 6-digit code
  that rotates after each use, (c) permanent unattended-access password (Argon2 hashed at rest).
- Password attempts: max 5 per minute per peer IP, then 10-minute lockout. Log every attempt.
- Every session has a permission set: view, control, keyboard, clipboard, files, audio, restart,
  privacy mode. The host can revoke any permission live. A visible on-screen banner on the host shows
  who is connected, always.
- Optional LAN allow-list (CIDR ranges). Default: only RFC1918 + link-local peers may connect.

## Performance targets (LAN, 1 Gbps or good Wi-Fi)

- 1080p @ 60 fps, glass-to-glass latency < 60 ms (measure with the built-in latency overlay).
- Adaptive: drop frames instead of queuing when the viewer falls behind; never let latency grow.
- Idle host CPU < 1%; active session < 15% on a mid-range CPU with software encoding.

## Conventions

- Rust stable, edition 2021, `#![forbid(unsafe_code)]` everywhere except platform modules that wrap
  OS APIs — those must document every `unsafe` block with a `// SAFETY:` comment.
- Errors: `thiserror` in libraries, `anyhow` only in binaries.
- All user-facing strings go through the i18n table; ship Arabic (RTL) and English.
  Arabic is first-class: any feature that displays, stores or transfers text must work in Arabic
  (shaping, bidi, RTL layout, UTF-8/UTF-16 paths) before it is done — see "Arabic" in ROADMAP.md.
- Commit messages: `lane(crate): what changed`.
