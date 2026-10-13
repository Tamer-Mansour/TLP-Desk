# tlp-desk-app

**The GUI binary `TLP-Desk.exe`** — host and viewer in one window, built with egui/eframe on the
wgpu backend, in Arabic and English.

| | |
|---|---|
| Path | `crates/tlp-desk-app` (binary `TLP-Desk` from `src/main.rs`, library `src/lib.rs`) |
| Depends on | `proto`, `crypto`, `net`, `host`, `viewer`, `input`, `eframe` 0.36 (`wgpu`, `accesskit`, `default_fonts`), `tokio`, `unicode-bidi`, `sys-locale`, `if-addrs`, `serde_json`, `anyhow` |
| Unsafe | `#![forbid(unsafe_code)]` in both `lib.rs` and `main.rs` |
| Threads | UI thread (egui); a 2-worker tokio runtime (`tlp-desk-backend`); the keyboard-grab hook thread plus a forwarding thread `tlp-desk-grab-forward` |

## Modules

| Module | Responsibility |
|---|---|
| `main.rs` | Argument parsing, DPI awareness, runtime, window, backend choice |
| `app` | Top-level state machine: home / settings / session, modals (connecting, accept prompt, host-key warning, errors), banners |
| `backend` | `Backend` trait between UI and networking; `RealBackend` and `FakeBackend` |
| `ui::home`, `ui::session`, `ui::settings_view` | The three screens |
| `ui::theme`, `ui::widgets` | Fonts, colours, RTL-aware rows/columns, text field, chevron button, copy and danger buttons |
| `ui::icons` | Vendored SVG icon set (`assets/icons/`, allow-listed, licences in `assets/icons/licenses` and `THIRD_PARTY_NOTICES.md`), tinted to the theme, HiDPI-aware, directional icons mirrored in RTL. Icon-only buttons always carry a tooltip that is also their accessible name |
| `i18n` | String table, shaping/bidi, digit and number formatting — see [i18n and Arabic](../i18n.md) |
| `input_map` | egui keys → set-1 scancodes; pointer normalisation; wheel conversion |
| `keygrab` | Turns the `tlp-desk-input` keyboard grab on and off with focus |
| `settings` | `settings.json` load/save |

## Command line

`main.rs` parses arguments by hand (no clap):

| Argument | Effect |
|---|---|
| `--lang en\|ar` | UI language for this run |
| `--connect <id\|name\|ip[:port]>` | Start connecting immediately |
| `--code <6 digits>` | One-time code for `--connect` |
| `--settings` | Open the settings screen first |
| `--port <udp port>` | Bind the endpoint to another port instead of 47400 |
| `--fake` | Use `FakeBackend` (no network; for UI work and screenshots) |

Unknown arguments are reported on stderr and ignored. Release builds use the Windows GUI
subsystem, so there is no console.

Start-up order: `ensure_dpi_aware()` before any window exists → tokio runtime → eframe window
(1000×660, minimum 760×520, wgpu) → backend.

## Backend

The UI never blocks. Every `Backend` method returns immediately; results come back as
`BackendEvent`s that the UI drains with `try_recv()` once per frame. Decoded video bypasses the
event channel through a `FrameSlot` (newest frame wins, previous buffer recycled), so frames are
never queued behind events.

`Backend` methods: `local_info`, `set_device_name`, `refresh_one_time_code`,
`connect(target, code) -> AttemptId`, `cancel_connect`, `forget_host_key`,
`respond_to_request`, `send_input`, `set_view_only`, `disconnect`, `select_monitor`.

`BackendEvent`s: `LocalInfo`, `Devices`, `AwaitingAccept`, `ConnectFailed(ConnectError)`,
`SessionStarted`, `PermissionsChanged`, `Stats`, `HostInputPriority`, `MonitorsChanged`,
`SessionEnded`, `IncomingRequest`, `IncomingRequestCancelled`, `HostSessionStarted`,
`HostUnavailable(HostProblem)`.

### `RealBackend`

- One QUIC endpoint on UDP 47400 serves both the `Host` and the `Viewer`.
- If the port is taken (typically a second TLP-Desk instance), it falls back to a **dial-only**
  endpoint on an ephemeral port and reports `HostProblem::PortInUse` — this PC can still connect
  out but cannot be reached.
- `start` never fails; problems become `HostUnavailable` banners.
- Every 2 s it reconciles the "who is connected" banner with `Host::sessions()` and re-reads the
  LAN addresses; every 5 s it publishes the discovered-device list (the first one after 1.5 s).
- Only private and link-local IPv4 and `fc00::/7` addresses are shown as "Local address".
- Accepting from the GUI grants `DEFAULT_PERMISSIONS` (view + mouse + keyboard). One-time codes
  are accepted; password access is not configured by the GUI yet.
- The viewer decodes straight to RGBA so frames can be uploaded as textures without another
  conversion.

### `FakeBackend` (`--fake`)

Deterministic data for UI work: own ID `482 913 057` and name `DESKTOP-TLP01`; three devices
(including an Arabic name) one of which goes on and off line; `192.168.1.66` fails once with a
changed host key; a 1280×720 test pattern at 30 fps on two monitors; a simulated incoming request
after about 6 s.

## Session view details

- **Pointer** positions are normalised over the displayed image (letterboxed); moves outside the
  image are sent only while a button is held. Wheel deltas are converted to Windows `WHEEL_DELTA`
  units.
- **Keys** are sent as scancodes of the *physical* key, so the viewer's layout does not matter.
  Tab, arrows and Escape go to the remote PC instead of moving egui focus. Ctrl+C/X/V (which egui
  turns into clipboard events) are forwarded as key presses.
- **ReleaseAll** is sent when the window or view loses focus or a modal opens.
- **Keyboard grab** (`keygrab.rs`): active only while the session view has focus in a focused
  window, view-only is off and the session has `KEYBOARD`. Grabbed keys are forwarded from the
  hook thread directly to `Backend::send_input`, not via the next egui frame, so they are not
  delayed. If the hook cannot be installed, system keys simply stay local.
- **Monitor picker** appears only with two or more host monitors; the highlight follows the host's
  confirmation, not the click.

## Settings

`%APPDATA%\TLP-Desk\settings.json`, e.g. `{"lang":"ar","digits":"arabic_indic"}` with optional
`device_name`. A corrupt file falls back to defaults; saving is best effort. Language defaults to
the Windows UI language on first run (`ar*` → Arabic, else English). Keys and trust data are
**not** stored here — they live in the crypto storage directory.

## Logging

The GUI does not install a `tracing` subscriber yet, so `tracing` output from the libraries is
discarded and **no log file is written**. CLAUDE.md calls for local rotating log files; that is
still to be done.

## Build script

`build.rs` includes the shared `winres.rs`, which finds `rc.exe` (from `$RC`, `PATH`, or the newest
Windows 10 SDK) and embeds the icon and VERSIONINFO without extra crates. Without `rc.exe` the
build continues with a warning (no icon) unless `TLP_DESK_REQUIRE_EXE_ICON=1`. See
[Building on Windows](../building.md).

## Tests

None are ignored; all run headless.

- i18n: `every_key_has_english_and_arabic`, `placeholders_match_between_languages`,
  `arabic_display_strings_are_prepared_once`, `digit_style_applies_to_numbers_only`,
  `lang_parse_and_locale_detection`, and the shaping tests that lay text out with the real epaint
  pipeline (`egui_shows_arabic_sentence_in_rtl_order`, `egui_mixed_arabic_latin_and_ip`,
  `egui_shapes_with_joining_ligatures_and_harakat`, `egui_without_our_bidi_gets_word_order_wrong`,
  ...); format: `digits_roundtrip`, `number_formatting`, `unit_formatting`.
- UI (headless egui): `session_forwards_pointer_keys_and_releases_on_focus_loss`,
  `view_only_sends_nothing`, `keyboard_grab_follows_focus_and_suppresses_egui_keys`,
  `keyboard_grab_stays_off_in_view_only`, `monitor_picker_selects_and_follows_host`,
  `picker_hidden_with_one_monitor_and_banner_renders`, `fit_rect_preserves_aspect_and_centers`.
- Input mapping, settings, backend helpers (`lan_address_filter`,
  `viewer_errors_map_to_ui_errors`, ...).
- `tests/real_backend.rs` — two real backends on 127.0.0.1 with real QUIC, TLS, SPAKE2 and H.264:
  `host_and_viewer_through_the_backend_trait`, `changed_host_key_is_loud_and_can_be_forgotten`,
  `busy_port_degrades_to_dial_only_with_a_clear_problem`.

```powershell
cargo test -p tlp-desk-app
cargo run -p tlp-desk-app -- --fake --lang ar    # UI without a network
```
