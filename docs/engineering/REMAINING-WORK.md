# Remaining work

What is left of the transformation brief, in priority order, with blockers. Update this file
whenever an item is finished or a new one is found.

## Needs a real Windows PC (cannot be done or verified from the Linux cloud session)

| Item | Why it is blocked | What to do |
|---|---|---|
| Manual check of the P0 fixes on two PCs: one with VirtualBox/Hyper-V/WSL adapters, Wi-Fi off/on, DHCP renew (`ipconfig /release` + `/renew`), sleep/wake | Needs two physical Windows PCs on one LAN | Follow the "Network recovery" section of `TEST-RESULTS.md`, note the results there |
| Visual check of viewport-matched streaming: 4K host, 1080p viewer, fit to window, small text in an IDE | Needs a 4K Windows host and the Media Foundation encoder | Compare text sharpness with "Actual size" and with the previous release; record bitrate from the stats overlay |
| README media regeneration after visible UI changes | `scripts/update-readme-media.ps1` runs on Windows (`readme-media.yml` on CI) | Run the script or the workflow; commit `docs/media/` changes |
| Integrated title bar (P2.1, implemented) | Window chrome behaviour needs Windows 10 and 11 | Check: drag to move, drag to screen edges (snap), double-click maximize/restore, resize from all edges and corners, maximize does not cover the taskbar, moving between monitors with different DPI, Alt+Space, the Settings switch back to the system frame. Snap-layout flyout on the maximize button is not available (DECISIONS.md D9) |
| README media for the new title bar | Rendered by CI on Windows (WARP) | Take the `readme-media` artifact of the PR run, or merge and let the workflow open `TLP-media-refresh` |

## P1: streaming (next)

1. **Zoom & pan** in the session view (25–400 %, Ctrl+wheel, keyboard shortcuts), reusing
   `view_scale.rs`; send the viewport hint for the zoomed size up to native.
2. **Maximum clarity / lossless mode.** Today's best is the static refinement (very low QP
   keyframe after 300 ms still). Options: a `QualityPreset` value is a wire change (needs a
   `PROTOCOL_VERSION` bump), so prefer `Custom` with a high fixed bitrate floor first and
   measure. True lossless H.264 (High 4:4:4 Predictive, `qp=0`) is not supported by OpenH264 or
   most MF encoders.
3. **Benchmarks** with 4K synthetic text/IDE/scroll scenes in `crates/tlp-desk-host/tests/perf.rs`
   (bitrate, encode time, PSNR) before and after each change.
4. Map the brief's mode names (Auto / Best quality / Balanced / Best performance) onto the
   existing presets in the UI text (no wire change).

## P2: desktop experience

Title bar done (P2.1). Left: home screen restructure (My device / Remote connection / Devices /
Sessions / Settings), session toolbar (latency, bandwidth, security info). Uses the existing
design tokens in `crates/tlp-desk-app/src/ui/theme.rs` and `docs/src/dev/design.md`.

## P3: release

The signed updater and release pipeline exist (rc.4). Left: channel naming (Stable/Beta) in
Settings → Updates, an update-available toast, CI check that `SHA256SUMS.txt.minisig` verifies
with the built-in key.

## P4: polish

Split files over 800 lines (`ui/session.rs`, `backend/real.rs`, `ui/address_book.rs`,
`ui/chat.rs`, `ui/file_browser.rs`), accessibility pass, startup profiling, Arabic native review
of the new strings (`ErrSelfConnection`, `IdentityConflict`).

## Decisions waiting for the maintainer

* Firewall rules for the **Public** network profile (see DECISIONS.md D6).
* A UI action "Reset this device's identity" for cloned installs (today: reinstall and delete
  the data folders, documented in Troubleshooting). It changes the device ID that other PCs
  have pinned, so it should be a deliberate, confirmed action.
