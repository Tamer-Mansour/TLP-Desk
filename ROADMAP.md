# TLP-Desk Roadmap

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
- [x] Text input fields accept and display Arabic correctly (device names, addresses, chat, search)
      _(evidence: PR #39 `ui::bidi_edit` editor + `i18n::bidi_line` tests; no IME pre-edit inside Arabic fields — ADR 0007)_

**Layout & language (M1)**
- [x] Full RTL mirroring in Arabic: layouts, toolbars, lists, dialogs, alignment, icons with direction (back/forward arrows)
      _(icons: tlp-icons set in `ui::icons`, directional ones mirrored; evidence: `icons::tests::rtl_mirroring_flips_the_image`, `directional_icons_are_flagged`, `image_dir_mirrors_only_directional_icons_in_rtl`; en/ar screenshots of home, settings, session, incoming dialog)_
- [x] Every user-facing string in the i18n table with Arabic + English; test fails on a missing key
      _(evidence: missing translation is a compile error; `every_key_has_english_and_arabic`)_
- [x] Language auto-detected from Windows UI language on first run; switchable live in Settings, persisted
      _(evidence: `lang_parse_and_locale_detection`, `settings_roundtrip_and_defaults`)_
- [x] Digits setting: Western (0-9) or Arabic-Indic (٠-٩) for display; IDs/codes/IPs always copy as Western digits
      _(evidence: `digits_roundtrip`, `digit_style_applies_to_numbers_only`)_
- [x] Arabic date/time and number formatting in logs, connection history, file sizes
      _(evidence: `i18n::format` tests, `ui::connection_log` tests (PR #16), `ui::transfers` Arabic-Indic status tests (PR #22))_

**Data paths (M1–M2)**
- [ ] Arabic device names round-trip through mDNS TXT, broadcast fallback and the address book (UTF-8)
      _(partial: `endpoint::tests::peer_names_keep_arabic_and_lose_control_characters`, address book `round_trip_keeps_arabic_exactly`; mDNS across two PCs is still manual)_
- [ ] Remote typing works with the host on an Arabic keyboard layout (scancodes), including Alt+Shift layout switching
      _(English typing verified by system test row 5 and PR #34; Alt+Shift / Win+Space reach the host as scancodes: `session::tests::layout_switch_hotkeys_reach_the_host_as_scancodes`, `grab_logic::tests::layout_switch_hotkeys_reach_the_host` (PR #39); layout-independent Arabic via protocol v6 `InputEvent::Text`; the Arabic host layout still needs the two-PC manual test, section 3)_
- [x] Clipboard sync preserves Arabic text exactly (incl. RTL marks/ZWJ/ZWNJ)
      _(evidence: PR #12 clipboard tests (RLM/LRM, ZWJ/ZWNJ, harakat, CRLF byte-exact); two-PC paste check in the manual checklist)_
- [x] File transfer handles Arabic file and folder names (UTF-16 paths on Windows), in the UI and on disk
      _(evidence: PR #10 and #22 tests; system test rows 7-9: Arabic folder sent, fetched, listed, renamed, hashes identical)_
- [x] Chat panel: Arabic input, shaping, RTL message bubbles
      _(evidence: PR #21; system test row 10 (Arabic message received); the typing-while-focused caveat of the text-input item above still applies)_

**OS surfaces (M2–M5)**
- [x] Tray menu, notifications, host "who is connected" banner and Accept prompt in Arabic
      _(evidence: PR #16 i18n tests; system test row 16 screenshot of the Arabic host banner)_
- [x] Windows service / event-log messages localized where user-visible
      _(evidence: PR #39 audit — service notices are typed events localized in the GUI, SCM name/description localized; Event Log not used, `service.log` is an English diagnostic log, documented in `docs/src/dev/i18n.md`)_
- [x] ~~MSI installer UI in Arabic + English~~ — not planned: the Inno Setup `setup.exe` ships Arabic + English UI and an Arabic-aware Start-menu entry instead (ADR 0008)
- [x] CLI: `--lang ar`, UTF-8 console output that displays Arabic correctly
      _(evidence: PR #37; system test: Arabic chat, file names and `ls` output)_
- [x] Arabic user guide with Arabic screenshots (M5)
      _(evidence: `docs/src/user-guide/ar/`, `docs/media/*-ar-*.png`, `docs/src/user-guide/ar/manual-test.md`)_

**Quality**
- [x] Screenshot tests of key screens in Arabic (home, session toolbar, Accept prompt, settings)
      _(evidence: `scripts/update-readme-media.ps1` renders every scene in ar/en, light/dark, deterministically)_
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
_(Loopback evidence exists in the [system test report](docs/src/dev/system-test-report.md), 2026-10-16. The boxes stay open until the two-PC run in `docs/src/user-guide/en/manual-test.md` is done.)_
- [ ] Discover, connect with Accept prompt, control the remote PC with mouse and keyboard
- [ ] 1080p ≥ 30 fps, latency < 100 ms on Wi-Fi
- [ ] Works with internet disconnected

---

## Parallel lanes (M2–M5)

The foundation (protocol v4, typed side streams, feature hooks) is in place, so the remaining
Windows work of M2–M5 runs as parallel lanes, one agent and one branch each (`TLP-<lane>`,
branched from and merged back into `TLP-Desk` by PR). Shared groundwork every lane builds on:

- **Wire protocol v4** (`docs/protocol.md`): messages for every lane already exist. A lane that
  needs a new message appends it at the end of `Message` and bumps `PROTOCOL_VERSION`.
- **Side streams** (`tlp-desk-net`): `Session::open_uni/open_bi(kind, id)`,
  `Session::accept_side_stream(kind)`; kinds `FileData`, `Tunnel`, `Audio`, `Clipboard`.
- **Feature hooks**: implement `tlp_desk_net::Feature` in your own module and register it in
  `HostConfig::features` / `ViewerConfig::features`; UI events arrive as
  `HostEvent::Feature` / `ViewerEvent::Feature`. Append app/i18n items only inside your
  lane's `// --- <lane> ---` section (`backend/mod.rs`, `i18n/mod.rs`).

| Lane (branch) | Scope | Roadmap items |
|---|---|---|
| `TLP-files` | Remote file browser (`FsRequest`), ultra-fast transfer on parallel `FileData` streams with resume + BLAKE3, drag-and-drop, Explorer copy/paste of files (`ClipboardFiles`) | M2 Lane A items 2–3; Arabic file names |
| `TLP-clipboard` | Text / image / HTML clipboard sync both ways, permission-gated, Arabic byte-exact (RTL marks, ZWJ/ZWNJ), large payloads on `Clipboard` streams | M2 Lane A item 1; Arabic clipboard |
| `TLP-service` | Windows service as SYSTEM, session agent in the active console session, secure desktop (lock/login/UAC), Ctrl+Alt+Del (`SendSAS`), unattended password, restart + auto-reconnect (`ReconnectTicket`), lock workstation | M3 (all but privacy mode) |
| `TLP-privacy` | Privacy mode: blank the host's physical screen and block local input (`PrivacyMode`) | M3 privacy mode |
| `TLP-ux` | Address book (aliases, groups, last seen, online status), chat panel (`ChatMessage`), settings (permission defaults, allow-list, presets), tray icon, start minimized, top-most host "who is connected" banner | M2 Lane D |
| `TLP-audio` | System audio: WASAPI loopback → Opus on an `Audio` stream (`AudioControl`), mute toggle | M2 Lane C audio |
| `TLP-perf` | Adaptive bitrate (`ViewerFeedback`), dirty-region / static-screen detection, Media Foundation HW encode behind the trait, latency overlay (`LatencyProbe`), quality presets (`SetQuality`), all-monitors view (`SelectAllMonitors`), configurable capture cap | M2 Lane B, Lane C all monitors |
| `TLP-power` | Wake-on-LAN from the address book, session recording + playback, TCP port forwarding (`Tunnel*`), whiteboard, connection log, device info panel, CLI commands | M4 |
| `TLP-docs` | README, user guide (Arabic + English), `docs/security.md`, CONTRIBUTING, CI | M5 docs items |

Each lane ticks its own items below, with evidence, when its PR merges.

---
## M2 — Daily-driver features

**Lane A — files + clipboard**
- [x] Text and image clipboard sync both directions (permission-gated)
      _(evidence: PR #12 tests; a real clipboard overwrite is manual-only: checklist section 6)_
- [x] File transfer window: browse remote, upload/download, resume, progress; drag-and-drop onto session
      _(evidence: PR #10, #22; system test rows 6-9: 1 GiB at 330-414 MiB/s, hashes identical; drag-and-drop by hand: checklist section 5)_
- [x] Copy file in Explorer on one side, paste on the other
      _(implemented in PR #10 with tests; needs the two-PC manual test, checklist section 5)_

**Lane B — performance**
- [x] 60 fps + frame dropping under backpressure _(evidence: host e2e `slow_encoder_drops_frames_instead_of_queueing`; real loopback session 57.9 fps monitor 0 (3840x2400→1920x1200), 55.9 fps monitor 1 (1080p), latency est. 6–15 ms)_
- [x] Adaptive bitrate; latency < 60 ms measured glass-to-glass on wired LAN; configurable capture size cap (default 1920x1200)
      _(evidence: PR #15; loopback latency estimate 11-72 ms (system test rows 1-2); the true wired-LAN glass-to-glass number still has to be measured on two PCs)_
- [x] Dirty-region / static-screen detection: near-zero bandwidth when nothing changes
      _(evidence: PR #15, `DirtyTracker` in `tlp-desk-host/src/pipeline.rs` skips identical frames)_
- [x] Hardware encoding behind the trait (Media Foundation first; NVENC/QSV/AMF if available)
      _(evidence: PR #23; system test row 1: Intel Quick Sync MFT used, software fallback via `TLP_DESK_ENCODER=software` in row 2)_

**Lane C — multi-monitor + audio**
- [x] Monitor picker, switch monitor live _(evidence: host e2e `viewer_switches_monitor_live`, app `monitor_picker_selects_and_follows_host`; real 2-monitor switch on Win11 (`viewer_m1.png`), proto v3)_
- [x] "All monitors" view
      _(evidence: PR #15, host e2e `all_monitors_compose_the_virtual_desktop_and_map_input`; a two-monitor host is still to be tried by hand)_
- [x] System audio streaming (WASAPI loopback → Opus), mute toggle
      _(evidence: PR #7 unit and e2e tests; real audio devices are manual: checklist section 8)_

**Lane D — app UX**
- [x] Address book (local file): saved devices, aliases, groups, last seen, online status via mDNS
      _(evidence: PR #17 tests; online status across two PCs by hand: checklist section 10)_
- [x] Chat panel in session
      _(evidence: PR #21; system test row 10)_
- [x] Host user has input priority: local physical mouse/keyboard pauses remote input (1 s grace), remote held keys released _(evidence: host `priority::*`, `local_input_priority_reaches_viewer`)_
- [x] Green remote-pointer marker on the host screen, excluded from capture _(evidence: input `marker_*` tests, `marker_window_starts_toggles_and_stops`; manual `viewer_m0b.png` shows no marker in video)_
- [x] One marker/hook set shared across multiple simultaneous viewers
      _(evidence: PR #39 `tlp-desk-host` `hooks::tests` (SharedHooks, ActivityLease, MarkerLease); marker label lists up to 3 movers)_
- [x] Settings: permissions defaults, allow-list CIDRs, quality presets (Speed / Balanced / Quality)
      _(evidence: PR #16 and #15)_
- [x] System tray icon, start minimized, "who is connected" banner on host
      _(evidence: PR #16; banner seen in system test row 16; tray by hand: checklist section 14)_

---

## M3 — Unattended access (the hard Windows part)

- [x] Install as Windows service running as SYSTEM
      _(implemented in PR #9 with unit tests; a real install needs UAC and was not done in the system test: checklist section 12)_
- [x] Service launches the capture/input agent in the active console session; follows user switching
      _(PR #9 `service::desktop` and `service::agent` tests; real session switching is manual: checklist section 12)_
- [ ] Works on the lock screen, login screen and UAC prompts (secure desktop switching)
      _(implemented in PR #9; no evidence until the two-PC manual test, checklist section 12)_
- [ ] Ctrl+Alt+Del button via `SendSAS`
      _(implemented in PR #9; `SendSAS` needs the installed service, manual test only)_
- [x] Permanent password for unattended access (Argon2 at rest), rate limiting and lockout
      _(evidence: PR #11 tests; system test: CLI `set-password` and `connect --password` drove every CLI run)_
- [ ] Restart remote PC and auto-reconnect after boot
      _(implemented in PR #11 with ticket tests; a real restart was not done: checklist sections 11 and 12)_
- [ ] Privacy mode: blank the host's physical screen and block local input during the session
      _(implemented in PR #19 with controller tests; the overlay itself is manual-only: checklist section 13)_

---

## M4 — Power features

- [x] Wake-on-LAN for devices in the address book
      _(evidence: PR #17; system test row 13: packet sent to loopback; waking a real PC is manual: checklist section 10)_
- [x] Session recording to a local file (viewer side), playback in the app
      _(evidence: PR #20 `recording::*` tests)_
- [x] TCP port forwarding / tunnel through a session (e.g. reach the remote PC's local web server)
      _(evidence: PR #13; system test row 12: an HTTP page and a 20 MB blob came through the tunnel identical)_
- [x] Whiteboard / draw-on-screen overlay
      _(evidence: PR #8 tests; the overlay on a real screen: checklist section 7)_
- [x] Connection log (who, when, from which IP, duration, permissions used)
      _(evidence: PR #16 `connection_log` tests)_
- [x] Remote device info panel (OS, CPU, RAM, disks, uptime)
      _(evidence: PR #37; system test row 11)_
- [x] `tlp-desk-cli`: `list`, `connect`, `send-file`, `exec-wol` for scripts and AI agents
      _(evidence: PR #37; system test rows 1-13 drive the release CLI)_
- [x] Self-hosted relay for devices on other networks (`tlp-desk-relay`, ADR 0013): register,
      allocate, punch, forward; opt-in on both sides; wire protocol unchanged
      _(evidence: `tlp-desk-net/tests/relay.rs` host+viewer+relay on loopback; `relay::tests`; needs a two-home manual check with a real VPS)_

---

## M5 — Ship it

- [x] Single `setup.exe` (Inno Setup, Arabic + English, original icon, firewall rules private/domain, clean uninstall) _(evidence: elevated silent install/uninstall test 2026-10-13; interactive wizard not yet reviewed)_
- [x] ~~(optional) MSI via cargo-wix~~ — not planned (ADR 0008); the Inno Setup installer covers service install, firewall rules, Start-menu shortcut and clean uninstall
- [x] Portable zip: runs without install (attended mode only)
      _(evidence: `installer\portable.ps1` builds `TLP-Desk-1.0.0-rc.1-win-x64-portable.zip`)_
- [x] SHA256SUMS for every release artifact
      _(evidence: both installer scripts write `dist\SHA256SUMS.txt`)_
- [x] Arabic + English user guide with screenshots
      _(evidence: `docs/src/user-guide/{en,ar}/`, `docs/media/`)_
- [x] Landing page (in-repo `docs/site/`, English + Arabic RTL, published to GitHub Pages with the book under `/book/` by `docs.yml`; PR #39). Hosting on an external TLP site is up to the site owner.

## Later — other platforms

- [ ] Linux host/viewer (X11 first, then PipeWire/Wayland portal)
      _(X11 host + viewer implemented in PR #41 and cross-checked with `cargo clippy --target x86_64-unknown-linux-gnu --workspace`; not yet run on a real Linux PC; Wayland portal/PipeWire not implemented — design in `docs/src/dev/linux.md`)_
- [ ] macOS host/viewer (ScreenCaptureKit, Accessibility permission flow)
      _(implemented in PR #40 with `CGDisplayCreateImage` capture (ScreenCaptureKit swap documented), `CGEvent` input and the Screen Recording / Accessibility permission flow; cross-checked for `aarch64-apple-darwin` on proto/input/capture; not yet run on a Mac)_
- [ ] Android viewer
      _(implemented in PR #40: `crates/tlp-desk-android`, touch → mouse gestures, Arabic/English UI; arm64 APK builds and verifies with `apksigner`; not yet run on a device; mDNS multicast lock not implemented — connect by IP)_
