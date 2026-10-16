# System test reports

## v1.0.0-rc.2

Date: 2026-10-16. Tree: `TLP-Desk` at `b7f6dd3` (`1.0.0-rc.2`) plus the fixes of PR #52. Same PC
as rc.1 (Windows 11 Pro 10.0.26300, i7-12800HX, Intel UHD Graphics with Quick Sync, one
3840x2400 display), release binaries built from the PR branch, host and viewer over loopback
on UDP 47490-47495 with scratch `--data-dir` folders. **The CPU was at 60-100 % the whole time**
(other builds were running), so frame rates and decode times below are far from what an idle
PC gives; they show the adaptive behaviour under load, not peak performance.

The moving content was a test window of our own: 190-column random text scrolling 3 px per
tick for 6 s, then still for 4 s, repeated (timestamps of every stop logged). Statistics come
from `connect --json` (now with `qp` and `t_ms`), refresh pictures from the host's debug log
(`refresh picture sent`).

### Quality gate

`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace --no-fail-fast`, `cargo deny check` and `typos`: all clean on the PR
branch (1232 tests passed, 0 failed, 42 ignored) and again after merging the current `TLP-Desk`
(1256 passed, 0 failed, 42 ignored in 62 test binaries).

### Results

| # | Check | Result |
|---|---|---|
| 1 | Quality floor, OpenH264 (`TLP_DESK_ENCODER=software`) | pass. Average QP during motion 29-32, never above 32; refresh pictures QP 12-15. 1920x1200, 15-30 fps (decoder-limited: decode 17-200 ms on the loaded CPU). |
| 2 | Quality floor, `auto` (hardware) | pass for the floor (QP 30-32 in motion, 15 for refreshes); **but** the Intel Quick Sync encoder failed after 0.5-19 s in all four runs and the session continued on OpenH264 (see "Open problems"). |
| 3 | Refresh picture after motion stops | **bug found and fixed.** Before: 0.33 and 0.37 s for two stops, but 2.69 and 3.01 s for the other two (the mouse was moving: every pointer update left an encode pending and held the refresh back). After the fix: 0.40, 0.44 and 0.50 s (software) and 0.37-0.50 s in the hardware runs after the fallback (seven stops). |
| 4 | Quality ladder under a simulated 5 Mbit/s link (`connect --sim-bandwidth-kbps 5000 --sim-seconds 35`, software, scrolling the whole time) | pass. Sequence, seconds from the start: t=0 1920x1200 @ 60 fps, 16.6 Mbit/s target; t=3.0-5.6 loss, target 16.6 -> 8.5 -> 5.1 -> 3.0 -> 1.8 -> 1.04 Mbit/s (the floor); t=6.4 one step straight to the lowest rung **1024x640 @ 15 fps** (the target had hit the floor, so no intermediate rung restored 0.07 bits per pixel); t=12 clear, target back at the ceiling by t=21 (the small picture fits in 1.8 Mbit/s); t=35 link restored; t=43.5 up to **1280x800**; t=76.9 up to **1600x1000**; t=86 the loaded CPU made the viewer lose frames again, so it stayed at 1600x1000 until the end (t=100). One rung at a time on the way up, 10 s or more apart, as designed. QP 29-32 throughout. |
| 5 | Ladder without simulation | pass. In a plain software run the CPU-starved viewer lost frames and the host stepped 1920x1200 -> 1600x1000 at t=12 by itself. |
| 6 | Explorer paste: `shellclip::windows::tests::real_clipboard_round_trip --exact --ignored` | pass. The clipboard held text + HTML (no files or images), both saved before and restored identical afterwards. |
| 7 | Explorer paste end to end | not run with two processes on one PC: host and viewer would share the one Windows clipboard, so a paste cannot show which side served it. Covered by the loopback e2e tests (`clipfiles_e2e`, `shellclip_e2e`) and the COM round trip (6); the two-PC check stays in the manual checklist. |
| 8 | `send-file` twice with the same name, host `--on-conflict keep-both / overwrite / skip` | pass: `same (2).txt` next to the old file / replaced / old file kept. |
| 9 | `get-file --on-conflict keep-both / overwrite / skip` over an existing file | keep-both and overwrite pass; **skip kept the file but the command never ended** - bug found and fixed (a download with every file skipped now completes). |
| 10 | `get-file` of a 50 MB file + an Arabic folder (`مجلد_عربي\ملف نص.txt`) | pass. 251.9 MiB/s, SHA-256 equal, Arabic names exact. The destination is now printed as `C:\...` (**fixed**: it was `\\?\C:\...`). |
| 11 | `get-file` of 1 GiB interrupted (process killed after 2.5 s, 402 MiB part file) and run again | pass, safely: the second run completed in 6.8 s, SHA-256 equal. It started from 0: a killed process leaves no resume record. Resume after a pause or Ctrl+C is covered by `transfer::pause_and_resume_sends_only_the_rest`. |
| 12 | Transfer history file (`transfers.json`) | not checked (needs the GUI to finish a transfer); covered by the `transfer_history` unit tests. |
| 13 | Installed service (rc.1, user install) | `Get-Service TLP-Desk`: Running, automatic, LocalSystem (session 0); its session agent runs in console session 1, started with the service. `%ProgramData%\TLP-Desk\logs\service.log` could not be read without elevation (SYSTEM + Administrators only, by design), so it was not read. System event log: only the install event (7045), no Service Control Manager errors; Application log: no TLP-Desk errors or crashes. |
| 14 | GUI with the default data folders while the service runs | pass. The home screen shows "Unattended access is on: the TLP-Desk service receives connections for this device." with the service's device ID and one-time code (captured with `PrintWindow`, window only, then deleted). Cosmetic: in "Devices on this network" a long IPv6 link-local address pushes the row's "Available" under the star and Connect buttons. |
| 15 | GUI smoke of every screen in English and Arabic (`--fake`) | **not done** (time). |

### Bugs found and fixed (PR #52)

- **Refresh picture held back while the mouse moves** (host). A pointer-only change no longer
  blocks the refresh, which draws the pointer itself. Test:
  `e2e::moving_pointer_does_not_hold_back_the_refine_picture` (failed before the fix).
- **`get-file --on-conflict skip` never finished** when every file was skipped. Test:
  `conflicts::download_with_every_file_skipped_still_finishes` (timed out before the fix).
- **`get-file` printed `\\?\C:\...`** (seen in rc.1 too). Test:
  `send_file::tests::destination_is_shown_without_the_long_path_prefix`.
- **The hardware fallback hid its cause** ("not usable" only). The real error is now kept and
  logged.
- The CLI `connect` stats lacked the quantiser: added (`qp`, `Q~`), plus `t_ms`.
- New dev-only tool: `ViewerSession::set_net_sim` and the hidden `connect --sim-loss`,
  `--sim-bandwidth-kbps` and `--sim-seconds` flags, which drop received video like a lossy or
  slow link so the ladder can be tested on one PC.

### Open problems

- **Intel Quick Sync falls back to OpenH264 within seconds on this loaded PC** (4 of 4 runs).
  The first cause seen was `ProcessInput` failing with `MF_E_NOTACCEPTING`; the encoder now
  waits for the MFT's next input request and retries, but the next run failed with an
  `MEError` event carrying `S_OK`. The synthetic hardware test
  `real_hw_encoder::intel_survives_live_bitrate_and_refresh_churn` (bitrate retunes, keyframe
  requests, boosted refreshes, idle gaps) passes, so the trigger is the live timing under load.
  The fallback works as designed (no gap, the session continues), but hardware encoding should
  be re-checked on an idle PC before the final release.
- The device row overlap in "Devices on this network" (cosmetic).
- Not done this round: the GUI smoke of every screen in both languages, the transfer history
  check and README media regeneration (no visible UI change in this PR).

## v1.0.0-rc.1

Date: 2026-10-16. Tree: `TLP-Desk` at `5034f38` plus the version bump (`1.0.0-rc.1`).
Machine: one Windows 11 Pro (10.0.26300) PC, Intel Core i7-12800HX (24 threads), 31.6 GiB RAM,
one 3840x2400 display (Intel UHD Graphics with Quick Sync, no discrete GPU in use), release
binaries `TLP-Desk.exe` and `TLP-Desk-cli.exe` built with `cargo build --release --workspace`.

All tests ran host and viewer **on the same PC over loopback** (`127.0.0.1`, host on UDP 47480,
GUI on 47482/47483, scratch `--data-dir` folders, scratch `%APPDATA%`/`%ProgramData%` for the
GUI). Loopback proves the protocol, codecs, file engine and UI; it does **not** measure real
network latency, Wi-Fi behaviour or two-PC discovery (see "Still manual").

### Quality gate

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| `cargo test --workspace` (run twice) | pass, 0 failed, no flaky test |
| `cargo deny check` | advisories, bans, licenses, sources ok |
| `typos` | clean |
| `cargo build --release --workspace` | pass (`TLP-Desk.exe` 29.4 MiB, `TLP-Desk-cli.exe` 11.4 MiB) |

### End-to-end results

| # | Test | Result |
|---|---|---|
| 1 | CLI host + CLI viewer, permanent password (`--password`), monitor 0, hardware encoder (auto: Intel Quick Sync H.264 MFT, constrained baseline, 13.8 Mbps target) | pass. 10 s: 24.5 fps average, 4-8 Mbps, RTT 0.4-4 ms, latency estimate 20-35 ms, decode 8-12 ms, 1920x1200 (3840x2400 capped), 245/245 frames decoded. The screen showed a 25 fps video, so fps is source-limited, not encoder-limited. |
| 2 | Same with `TLP_DESK_ENCODER=software` (openh264) | pass. 10 s: 24.8 fps average, 5.4-6.0 Mbps, RTT 0.5-2.5 ms, latency estimate 11-72 ms, decode 7-10 ms, 248/248 frames. |
| 3 | Snapshot (`--snapshot`) for hardware, software and GUI-host runs | pass. PNG decoded at 1920x1200 and looked right (viewed). |
| 4 | Monitor 1 | not run: this PC has one monitor. Monitor switching is covered by host e2e `viewer_switches_monitor_live` and the earlier two-monitor manual run in the ROADMAP. |
| 5 | Typing: `connect --type` into a WinForms TextBox window in the foreground | pass. `Quick brown fox: jumps, over (the) lazy dog! 0123456789 @#` arrived exactly. Two earlier attempts typed nothing into the box because the test window had lost the foreground (this PC was in use); the final run with the window verified as foreground matched the text byte for byte. Typing goes to whichever window has focus on the host, as designed. |
| 6 | `send-file` of a 1 GiB random file | pass. 2.5 s, **413.7 MiB/s**, SHA-256 of source and destination identical. |
| 7 | `send-file` of a folder with Arabic names (`مجلد_عربي\فرعي\صورة.bin`, `ملف نص.txt`) | pass. 3 files, 2.9 MiB; names and per-file SHA-256 identical. |
| 8 | `get-file` of the 1 GiB file plus the Arabic folder | pass. 4 files, 1.0 GiB, 3.1 s, **329.4 MiB/s**; hashes identical. |
| 9 | `ls`, `mkdir` (also with an Arabic name), `mv` (Arabic to Arabic), `rm` | pass. Each result confirmed with a following `ls` and on disk. |
| 10 | `chat` with Arabic + English text | pass. Host log shows `[chat] TAMERPC: مرحبا من المشاهد - hello`. |
| 11 | `info` | pass. Name, OS, CPU, RAM, disks, uptime returned. |
| 12 | `forward 47492:127.0.0.1:47491` to a local Python HTTP server | pass. Page (Arabic text) and a 20 MB random blob came back identical (`cmp`), about 226 MiB/s through the tunnel. |
| 13 | `wol` to loopback (plain and `--json`), unknown alias | pass. Packet sent; JSON `wol_sent`; unknown alias gives "not found". |
| 14 | `clip-send` | **skipped**: it needs the host clipboard permission, which would overwrite this PC's real clipboard. Covered by `tlp-desk-files` clipboard tests and the manual checklist. |
| 15 | GUI as viewer (`TLP-Desk.exe --connect 127.0.0.1:47480 --code ...`) to the CLI host | pass. Window title "TLP-Desk — systest" (no mojibake), full session toolbar (fit, zoom, fullscreen, view-only, stats, privacy, devices, transfer, whiteboard, clipboard, files, upload, chat, sound), live decoded picture, stats overlay (25 fps, 22 ms, 5.7 Mbps, 1920x1200). Host log: connected via one-time code, code rotated. |
| 16 | GUI as host (`--lang ar`) with a CLI viewer using the shown one-time code | pass. Arabic home screen renders correctly mirrored (ID, one-time code, address, buttons); the "TAMERPC connected to this device" banner appeared with end-session and port-forwarding controls; the CLI viewer decoded 293 frames in 12 s (24.4 fps). |
| 17 | Arabic mode screenshot | pass (viewed): RTL layout, shaping and mixed Latin text correct. |

All processes and windows opened for the test were closed afterwards; scratch data was
deleted, nothing was written inside the repository.

### Bugs found

None blocking. Cosmetic observation: `get-file` prints its destination with the Windows
extended-length prefix (`\?\C:\...`). The GUI launched from a non-interactive shell started
minimized (the window was restored by the test script); a normal double-click start is
covered by the manual checklist.

### Still manual (two real PCs)

Privacy mode overlay, real service install and lock-screen / UAC control, Ctrl+Alt+Del
(`SendSAS`), lock and restart with auto-reconnect, real audio devices, clipboard overwrite and
Explorer paste, Wake-on-LAN against a sleeping PC, mDNS discovery across two machines,
Wi-Fi throughput and latency, typing into an elevated window, monitor 1 and the all-monitors
view on a multi-monitor host. Steps are in `docs/src/user-guide/en/manual-test.md` (Arabic:
`docs/src/user-guide/ar/manual-test.md`).
