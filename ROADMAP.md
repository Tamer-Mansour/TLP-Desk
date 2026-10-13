# tlp-desk Roadmap

Goal: a usable LAN-only AnyDesk replacement as fast as possible, then grow to full feature parity.
Milestones are ordered; inside each milestone, **lanes** can run in parallel (one Claude Code agent per lane).
Check a box only with evidence (test name, or a short manual-test note with machines/OS used).

---

## M0 — Skeleton (1 agent, do first, everything else depends on it)

- [x] Cargo workspace with all crates from CLAUDE.md, empty but compiling
      _(evidence: `scripts/check.ps1` green, rustc 1.99.0, Win11 x64, 2026-10-13)_
- [x] CI script `scripts/check.ps1`: build release, clippy -D warnings, tests
- [x] `tlp-desk-proto`: `PROTOCOL_VERSION`, `Hello`, `AuthRequest/AuthResult`, `VideoFrame`,
      `InputEvent`, `ClipboardData`, `FileChunk`, `Permissions`, `Bye`; round-trip tests for every message
      _(evidence: `tlp-desk-proto/tests/roundtrip.rs::every_message_roundtrips`, `frames_decode_from_byte_by_byte_stream`)_
- [x] Platform traits defined: `ScreenCapturer`, `VideoEncoder`, `VideoDecoder`, `InputInjector`
- [x] `docs/protocol.md` first draft

---

## Arabic — first-class, cross-cutting (applies to every milestone)

Arabic is a primary language, not a translation afterthought. A feature that shows text is not done
until it works in Arabic. Check these per milestone as features land.

**Text rendering (M1)**
- [x] Bundled Arabic-capable font (OFL, embedded), used as a fallback for every UI font
      _(evidence: Noto Sans Arabic in `tlp-desk-app/assets/fonts`, `theme::font_definitions`)_
- [x] Correct shaping: letters join (initial/medial/final/isolated forms), lam-alef ligatures, harakat (diacritics) render
- [x] Bidi: mixed Arabic + English/numbers/IDs/IPs display in correct visual order (Unicode Bidi Algorithm)
      _(evidence: `egui_shapes_with_joining_ligatures_and_harakat`, `egui_mixed_arabic_latin_and_ip`)_
- [ ] Text input fields accept and display Arabic correctly (device names, addresses, chat, search)
      _(partial: correct when not focused + live "shown as" preview; while typing, egui orders words LTR — egui limitation)_

**Layout & language (M1)**
- [x] Full RTL mirroring in Arabic: layouts, toolbars, lists, dialogs, alignment, icons with direction (back/forward arrows)
- [x] Every user-facing string in the i18n table with Arabic + English; test fails on a missing key
      _(evidence: missing translation is a compile error; `every_key_has_english_and_arabic`)_
- [x] Language auto-detected from Windows UI language on first run; switchable live in Settings, persisted
      _(evidence: `lang_parse_and_locale_detection`, `settings_roundtrip_and_defaults`)_
- [x] Digits setting: Western (0-9) or Arabic-Indic (٠-٩) for display; IDs/codes/IPs always copy as Western digits
      _(evidence: `digits_roundtrip`, `digit_style_applies_to_numbers_only`)_
- [ ] Arabic date/time and number formatting in logs, connection history, file sizes

**Data paths (M1–M2)**
- [ ] Arabic device names round-trip through mDNS TXT, broadcast fallback and the address book (UTF-8)
- [ ] Remote typing works with the host on an Arabic keyboard layout (scancodes), including Alt+Shift layout switching
- [ ] Clipboard sync preserves Arabic text exactly (incl. RTL marks/ZWJ/ZWNJ)
- [ ] File transfer handles Arabic file and folder names (UTF-16 paths on Windows), in the UI and on disk
- [ ] Chat panel: Arabic input, shaping, RTL message bubbles

**OS surfaces (M2–M5)**
- [ ] Tray menu, notifications, host "who is connected" banner and Accept prompt in Arabic
- [ ] Windows service / event-log messages localized where user-visible
- [ ] MSI installer UI in Arabic + English (WiX localization), Arabic Start-menu entry name
- [ ] CLI: `--lang ar`, UTF-8 console output that displays Arabic correctly
- [ ] Arabic user guide with Arabic screenshots (M5)

**Quality**
- [ ] Screenshot tests of key screens in Arabic (home, session toolbar, Accept prompt, settings)
- [ ] Review by a native Arabic speaker for wording (no machine-translation tone)

---

## M1 — MVP: see and control another PC on the LAN  ← target: usable

Lanes run in parallel after M0.

**Lane A — net + crypto**
- [x] Identity key generation and storage; 9-digit device ID from key hash
      _(evidence: crypto `device_id_always_nine_digits`, `persisted_identity_is_stable`, `cert_fingerprint_matches_identity`)_
- [x] QUIC server/client over `quinn` with the pinned-cert verifier
      _(evidence: net `two_peers_connect_with_accept_prompt`, `host_with_different_key_than_dialed_id_is_rejected`, `host_drops_video_instead_of_queueing`)_
- [x] mDNS advertise + browse; broadcast fallback; manual IP connect
      _(evidence: `mdns_advertise_and_browse` (ignored in CI, passed manually on Win11), `broadcast_fallback_discovers_peer_and_expires_it`)_
- [x] Accept/Reject prompt flow and one-time 6-digit code via SPAKE2
      _(evidence: `accept_prompt_reject_reaches_viewer`, `correct_one_time_code_succeeds_and_rotates`, `pake_*`)_
- [x] Tests: two in-process peers connect, wrong code fails, wrong key is rejected
      _(evidence: `wrong_code_fails_on_both_sides`, `repeated_wrong_codes_trigger_lockout`)_

**Lane B — capture + codec (Windows)**
- [x] Windows.Graphics.Capture of the primary monitor, BGRA frames, cursor position/shape separately
      _(evidence: capture `wgc_captures_primary_monitor`, `factory_captures_primary_monitor` (ignored in CI, passed on Win11 3840x2400))_
- [x] DXGI Desktop Duplication fallback
      _(evidence: `dxgi_captures_primary_monitor` (ignored in CI, passed on Win11); bench `--backend dxgi` 55 fps)_
- [x] H.264 encode/decode via openh264, configurable bitrate/fps, keyframe on request
      _(evidence: codec `encode_decode_1080p_matches_dimensions_and_psnr`, `keyframe_request_yields_idr`, `bitrate_change_takes_effect_without_keyframe`)_
- [x] Benchmark binary: capture→encode→decode at 1080p, prints fps and per-stage ms
      _(evidence: `cargo run --release -p tlp-desk-codec --example bench -- --crop 1920x1080`: 51.7 fps serial, encode 5.95 ms avg, decode 2.88 ms avg, no NASM)_

**Lane C — input**
- [x] Mouse move/click/wheel, keyboard via scancodes (layout-independent), Arabic/English layouts work
      _(evidence: input `tracker::tests::*`, `real_injection` (ignored, 14/14 exact pixels on 2-monitor mixed-DPI Win11); Arabic layout by design, typing not yet hand-tested)_
- [x] Coordinate mapping for different resolutions and DPI scaling
      _(evidence: `mapping::tests::*` incl. `host_path_multi_monitor_layouts`, `viewer_to_host_is_exact_at_native_scale_for_any_dpi`)_
- [ ] Viewer-side capture of keys including Win key and Alt+Tab when the viewer window is focused
      _(implemented: `KeyboardGrab`; `grab_logic::tests::*`, `grab_starts_toggles_and_stops` pass — needs manual key-press check once wired into the GUI)_

**Lane D — app (egui)**
- [x] Home screen: this device's ID, name, IP, one-time code; list of discovered LAN devices
      _(evidence: app screenshots ar_home/en_home ; real backend wired (`real_backend.rs`); cross-PC discovery not yet hand-tested)_
- [x] Connect by ID, name or IP; session window renders decoded frames as a wgpu texture
      _(evidence: app `tests/real_backend.rs` (3 tests); live run GUI viewer ↔ CLI host on Win11 loopback, 30 fps 1920x1200, 27 ms est.)_
- [x] Toolbar: fit/original size, fullscreen, disconnect, view-only toggle, latency/fps overlay
      _(evidence: ar_session/en_session screenshots; `ui::session::tests::view_only_sends_nothing`)_
- [x] Arabic + English UI, RTL layout
      _(evidence: `i18n::*` tests incl. `every_key_has_english_and_arabic`, `egui_shows_arabic_sentence_in_rtl_order`; ar_* screenshots)_

**M1 acceptance (manual, two real Windows PCs on the same switch/Wi-Fi):**
- [ ] Discover, connect with Accept prompt, control the remote PC with mouse and keyboard
- [ ] 1080p ≥ 30 fps, latency < 100 ms on Wi-Fi
- [ ] Works with internet disconnected

---

## M2 — Daily-driver features

**Lane A — files + clipboard**
- [ ] Text and image clipboard sync both directions (permission-gated)
- [ ] File transfer window: browse remote, upload/download, resume, progress; drag-and-drop onto session
- [ ] Copy file in Explorer on one side, paste on the other

**Lane B — performance**
- [x] 60 fps + frame dropping under backpressure _(evidence: host e2e `slow_encoder_drops_frames_instead_of_queueing`; real loopback session 57.9 fps monitor 0 (3840x2400→1920x1200), 55.9 fps monitor 1 (1080p), latency est. 6–15 ms)_
- [ ] Adaptive bitrate; latency < 60 ms measured glass-to-glass on wired LAN; configurable capture size cap (default 1920x1200)
- [ ] Dirty-region / static-screen detection: near-zero bandwidth when nothing changes
- [ ] Hardware encoding behind the trait (Media Foundation first; NVENC/QSV/AMF if available)

**Lane C — multi-monitor + audio**
- [x] Monitor picker, switch monitor live _(evidence: host e2e `viewer_switches_monitor_live`, app `monitor_picker_selects_and_follows_host`; real 2-monitor switch on Win11 (`viewer_m1.png`), proto v3)_
- [ ] "All monitors" view
- [ ] System audio streaming (WASAPI loopback → Opus), mute toggle

**Lane D — app UX**
- [ ] Address book (local file): saved devices, aliases, groups, last seen, online status via mDNS
- [ ] Chat panel in session
- [x] Host user has input priority: local physical mouse/keyboard pauses remote input (1 s grace), remote held keys released _(evidence: host `priority::*`, `local_input_priority_reaches_viewer`)_
- [x] Green remote-pointer marker on the host screen, excluded from capture _(evidence: input `marker_*` tests, `marker_window_starts_toggles_and_stops`; manual `viewer_m0b.png` shows no marker in video)_
- [ ] One marker/hook set shared across multiple simultaneous viewers
- [ ] Settings: permissions defaults, allow-list CIDRs, quality presets (Speed / Balanced / Quality)
- [ ] System tray icon, start minimized, "who is connected" banner on host

---

## M3 — Unattended access (the hard Windows part)

- [ ] Install as Windows service running as SYSTEM
- [ ] Service launches the capture/input agent in the active console session; follows user switching
- [ ] Works on the lock screen, login screen and UAC prompts (secure desktop switching)
- [ ] Ctrl+Alt+Del button via `SendSAS`
- [ ] Permanent password for unattended access (Argon2 at rest), rate limiting and lockout
- [ ] Restart remote PC and auto-reconnect after boot
- [ ] Privacy mode: blank the host's physical screen and block local input during the session

---

## M4 — Power features

- [ ] Wake-on-LAN for devices in the address book
- [ ] Session recording to a local file (viewer side), playback in the app
- [ ] TCP port forwarding / tunnel through a session (e.g. reach the remote PC's local web server)
- [ ] Whiteboard / draw-on-screen overlay
- [ ] Connection log (who, when, from which IP, duration, permissions used)
- [ ] Remote device info panel (OS, CPU, RAM, disks, uptime)
- [ ] `tlp-desk-cli`: `list`, `connect`, `send-file`, `exec-wol` for scripts and AI agents

---

## M5 — Ship it

- [x] Single `setup.exe` (Inno Setup, Arabic + English, original icon, firewall rules private/domain, clean uninstall) _(evidence: elevated silent install/uninstall test 2026-10-13; interactive wizard not yet reviewed)_
- [ ] (optional) MSI via cargo-wix: installs service, adds Windows Firewall rules for UDP 47400/47401
      and mDNS, Start-menu shortcut, clean uninstall (removes service and firewall rules)
- [ ] Portable zip: runs without install (attended mode only)
- [ ] SHA256SUMS for every release artifact
- [ ] Arabic + English user guide with screenshots
- [ ] Landing page on the TLP site

## Later — other platforms

- [ ] Linux host/viewer (X11 first, then PipeWire/Wayland portal)
- [ ] macOS host/viewer (ScreenCaptureKit, Accessibility permission flow)
- [ ] Android viewer
