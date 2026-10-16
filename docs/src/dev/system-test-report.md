# System test report: v1.0.0-rc.1

Date: 2026-10-16. Tree: `TLP-Desk` at `5034f38` plus the version bump (`1.0.0-rc.1`).
Machine: one Windows 11 Pro (10.0.26300) PC, Intel Core i7-12800HX (24 threads), 31.6 GiB RAM,
one 3840x2400 display (Intel UHD Graphics with Quick Sync, no discrete GPU in use), release
binaries `TLP-Desk.exe` and `TLP-Desk-cli.exe` built with `cargo build --release --workspace`.

All tests ran host and viewer **on the same PC over loopback** (`127.0.0.1`, host on UDP 47480,
GUI on 47482/47483, scratch `--data-dir` folders, scratch `%APPDATA%`/`%ProgramData%` for the
GUI). Loopback proves the protocol, codecs, file engine and UI; it does **not** measure real
network latency, Wi-Fi behaviour or two-PC discovery (see "Still manual").

## Quality gate

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| `cargo test --workspace` (run twice) | pass, 0 failed, no flaky test |
| `cargo deny check` | advisories, bans, licenses, sources ok |
| `typos` | clean |
| `cargo build --release --workspace` | pass (`TLP-Desk.exe` 29.4 MiB, `TLP-Desk-cli.exe` 11.4 MiB) |

## End-to-end results

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

## Bugs found

None blocking. Cosmetic observation: `get-file` prints its destination with the Windows
extended-length prefix (`\?\C:\...`). The GUI launched from a non-interactive shell started
minimized (the window was restored by the test script); a normal double-click start is
covered by the manual checklist.

## Still manual (two real PCs)

Privacy mode overlay, real service install and lock-screen / UAC control, Ctrl+Alt+Del
(`SendSAS`), lock and restart with auto-reconnect, real audio devices, clipboard overwrite and
Explorer paste, Wake-on-LAN against a sleeping PC, mDNS discovery across two machines,
Wi-Fi throughput and latency, typing into an elevated window, monitor 1 and the all-monitors
view on a multi-monitor host. Steps are in `docs/src/user-guide/en/manual-test.md` (Arabic:
`docs/src/user-guide/ar/manual-test.md`).
