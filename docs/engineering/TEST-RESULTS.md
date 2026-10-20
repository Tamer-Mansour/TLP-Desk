# Test results

Real runs only. Environment unless stated: Linux x86_64 cloud container (4 vCPU, 15 GB RAM),
Rust 1.97.0, `CARGO_PROFILE_{DEV,TEST}_DEBUG=line-tables-only`. Windows-only code (capture,
Media Foundation, service, SendInput, title bar) is compiled and tested by CI on Windows, not
here.

## Baseline (`13ae43e`, before any change), 2026-10-20

`cargo test --workspace`: everything that ran passed, but the run stopped at the first failing
crate: `tlp-desk-crypto` `storage::tests::macos_dir_is_under_application_support` failed on
Linux (expected `TLP-Desk`, non-Windows folder is `tlp-desk`). A later run with
`--no-fail-fast` also showed `tlp-desk-files` `traversal_attempts_are_rejected` failing on
Linux (`dest\..\` rejected as `NotFound`, the test expects `InvalidPath`).

## After P0 + P1.1, 2026-10-20

| Gate | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace --no-fail-fast` | 1,370 passed, 1 failed (`traversal_attempts_are_rejected`, pre-existing, Linux only), 17 ignored (hardware/multicast) |
| After the `checked_abs` fix: `cargo test -p tlp-desk-files` | all passed (134 + new unit test), 2 ignored |

### New regression tests

| Test | Covers (brief's QA list) | Base code | New code |
|---|---|---|---|
| `tlp-desk-host/tests/connect_paths.rs::stale_and_own_addresses_ahead_of_the_real_one_still_connect` | 1, 2, 4, 5, 6 (dead + own address first) | **fails**: never connects (15 s helper timeout) | passes, < 1.8 s |
| `…::two_devices_on_one_lan_are_never_confused` | 1, 3 (two devices, stale address now used by the other) | **fails**: never reaches the right PC | passes; wrong-device case reports `IdentityMismatch`, no prompt on the other PC |
| `…::an_unreachable_device_fails_in_bounded_time` | 4, 5 (all addresses dead) | **fails**: 60.0 s (4 × 15 s) | passes, < 3.5 s (2 s handshake timeout in the test) |
| `…::dialing_your_own_id_is_a_self_connection` | 2 | n/a (new error) | passes |
| `…::dialing_your_own_address_is_a_self_connection` | 2 (no self-prompt) | n/a (new error) | passes |
| `…::the_test_pair_still_connects` | 10 | passes | passes |
| `tlp-desk-net/tests/discovery.rs::moved_device_is_dialed_at_its_new_address_and_the_old_one_expires` (Linux only) | 6 (changed DHCP address) | **fails**: new address never first | passes |
| `tlp-desk-net` unit: `addresses_that_are_ours_are_never_dialed`, `same_subnet_addresses_come_first`, `scopeless_ipv6_link_local_is_dropped`, `freshly_heard_addresses_come_first_newest_first`, `loopback_is_kept`, `own_id_from_our_own_addresses_is_not_a_conflict` | 1, 2, 3 | n/a (new function) | pass |
| `tlp-desk-host` unit: `viewport_hint_limits_the_capture_for_every_preset`, `viewport_is_not_turned_with_the_monitor`, `hostile_viewport_hints_are_clamped` | 11, 12, 16 | n/a | pass |
| `tlp-desk-host/tests/perf.rs::viewport_hint_scales_the_stream_to_the_viewer_window` | 11, 16 (frame size and bitrate follow the viewer window; withdrawn hint restores native) | n/a | passes |
| `tlp-desk-app` unit: `viewport_hint` module (5 tests), `a_frame_slightly_smaller_than_the_window_is_shown_one_to_one` | 11, 12, 17 | n/a | pass |
| `tlp-desk-files` unit: `backslash_parent_references_are_refused_on_unix_hosts` | security | **fails** (same root cause as the integration test) | passes |

"Base code" results come from running the same test files in a worktree of `13ae43e`; tests
that use new API (`SelfConnection`) cannot compile there.

## Not covered by automation here (needs real hardware)

| Brief QA item | Why | Manual procedure |
|---|---|---|
| 4 network adapter reset, 5 Wi-Fi off/on, 7 sleep/wake | Real NICs and power states | Two Windows PCs on one Wi-Fi. Connect, disconnect. Turn Wi-Fi off and on (or `ipconfig /release` + `/renew`) on the host; within ~5 s connect again from the device list. Expected: connects without restarting Wi-Fi on either PC; log line "network interfaces changed; refreshing discovery" |
| 1 with virtual adapters | Needs VirtualBox / Hyper-V / WSL on both PCs | Install VirtualBox on both PCs (host-only adapter `192.168.56.1` on each). Connect by ID from the list. Expected: connects to the other PC, never "That is this device" / no prompt on your own PC |
| 11 4K host at 1080p (visual) | 4K monitor + MF encoder | Fit to window: stats overlay shows the frame size equal to the picture area (± 2 px); small IDE text is sharper than with "Actual size" scaled down by the OS; bitrate lower than before |
| 13 multiple remote monitors with the hint | Real multi-monitor host | Switch monitors and "all monitors" while fitted: the frame keeps matching the picture area |
| 23 keyboard/mouse, 24 clipboard | Covered by existing e2e tests with fakes; real injection needs Windows | Existing manual test sheet `docs/src/user-guide/en/manual-test.md` |

## After P2.1 (integrated title bar), 2026-10-20

| Gate | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace --no-fail-fast` | 1,379 passed, 1 failed, 17 ignored. The failure, `perf.rs::static_screen_is_not_reencoded`, passed 5/5 alone and failed 1 in 3 runs of the host suite under load: a test race (a frame encoded before the snapshot arrived inside the watch window). Fixed by draining until quiet. Stressing the suite then exposed `e2e.rs::live_options_apply_to_the_next_session` (indexes the host's session list before the host registered it); fixed by waiting for `SessionStarted`. Afterwards the host suite passed 5/5 in a row |

New UI tests (egui_kittest, no GPU): `ui::title_bar::tests::*` (8): all menus and caption buttons
in English and Arabic, no caption buttons with the system frame, File → Exit and the close button
report their actions, narrow windows fold the menus, session context (connected device, Disconnect,
no quick buttons), quick connect with Arabic-Indic digits, guide links per language;
`chrome::tests::edges_and_corners_resize_the_right_way`. Writing them found that the caption
buttons and the quick-connect field had no accessible name (fixed).

Visual check: home (en/ar, light/dark), settings and session scenes rendered with lavapipe and
inspected; RTL mirrors the bar (File next to the logo on the right, caption buttons on the left).
