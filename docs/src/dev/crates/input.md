# tlp-desk-input

**Everything about mouse and keyboard.** On the host: injecting the viewer's input with
`SendInput`, noticing when the local user touches their own mouse or keyboard, and drawing the
green remote-pointer marker. On the viewer: mapping pointer positions to the wire format and
grabbing system keys (Win, Alt+Tab) while the session window is focused.

| | |
|---|---|
| Path | `crates/tlp-desk-input` |
| Depends on | `proto`, `thiserror`; on Windows `windows` 0.62 |
| Used by | `host`, `app` |
| Unsafe | `#![deny(unsafe_code)]`; only `mod win` is `#[allow(unsafe_code)]`, every block has a `// SAFETY:` comment. Off Windows, `mod stub` returns `Unsupported` |
| Threads | three dedicated OS threads with message loops: keyboard grab, local activity, pointer marker |

## Public API

```rust
pub trait InputInjector: Send {
    /// Pointer coordinates are normalized 0..=65535 over the captured monitor.
    fn inject(&mut self, event: &InputEvent) -> Result<(), InputError>;
    fn release_all(&mut self) -> Result<(), InputError> { /* injects ReleaseAll */ }
    /// false = the caller must recreate the injector for the new monitor.
    fn set_monitor(&mut self, monitor: MonitorRect) -> bool { false }
}

pub trait PointerMarker: Send {
    fn set_visible(&mut self, visible: bool);
    fn excluded_from_capture(&self) -> bool { true }
}

pub trait LocalActivity: Send + Sync {
    fn last_physical_input(&self) -> Option<Instant>;
    fn is_local_active(&self, within: Duration) -> bool;
}
```

Factories: `injector_for_monitor(MonitorRect)`, `start_local_activity_monitor()`,
`start_pointer_marker(label)`. Windows-only types are also exported directly:
`WindowsInjector`, `KeyboardGrab`, `LocalActivityMonitor`, `RemotePointerMarker`,
`ensure_dpi_aware`, `virtual_desktop_rect`, `vk_to_scancode`.

`InputError`: `Unsupported`, `Blocked(String)` (secure desktop or UIPI — an elevated window
refused the input; needs the M3 service), `Platform(String)`.

## Coordinate mapping (`mapping.rs`)

The wire carries pointer positions as `u16` normalised over the **streamed monitor**, so the
viewer never needs the host's resolution or DPI, and a GPU-downscaled video still maps clicks to
exact physical pixels.

- Viewer: `ViewRect::fit(container, image_w, image_h)` letterboxes the video;
  `point_to_normalized(px, py, image_w, image_h)` clamps to the edges (NaN → 0). Units only need
  to be consistent, so the viewer's own DPI does not matter.
- Host: `MonitorRect { x, y, width, height }` in physical virtual-desktop pixels (monitors left
  of or above the primary have negative origins); `normalized_to_pixel` maps 0 to the first pixel
  and 65535 to the last. `pixel_to_virtual_absolute` then produces the `MOUSEEVENTF_ABSOLUTE |
  MOUSEEVENTF_VIRTUALDESK` value that Windows maps back to **exactly** that pixel (verified for
  every pixel of many desktop layouts).
- `ensure_dpi_aware()` makes the process per-monitor-v2 DPI aware before any window exists;
  injection batches also run under a per-monitor-v2 thread DPI scope. The virtual desktop rect is
  re-read on every batch that contains a move, so hot-plugging a monitor is handled.

## Keys

- Keys travel as **hardware scancodes** (`Key { scancode, extended, down }`) and are injected with
  `KEYEVENTF_SCANCODE` and no virtual-key code, so the **host's** keyboard layout decides which
  character appears. Arabic and English layouts need no translation.
- `normalize_scancode` accepts plain or `0xE0`-prefixed forms; Pause/Break (`E1` sequences) is not
  representable.
- `vk_to_scancode` uses `MapVirtualKeyW(MAPVK_VK_TO_VSC_EX)` and forces the extended bit for
  navigation keys, Insert/Delete, Win, Apps, numpad `/`, right Ctrl and right Alt.
- The internal `TrackingInjector` remembers held keys and buttons so `ReleaseAll` releases
  exactly what is down, in one atomic `SendInput` batch; it also runs on drop. If `SendInput`
  inserts fewer events than requested the result is `Blocked`.

## Keyboard grab (viewer)

`KeyboardGrab::start()` installs a `WH_KEYBOARD_LL` hook on the thread
`tlp-desk-keyboard-grab` and returns a receiver of `InputEvent`s. While active
(`set_active(true)` / `set_active_for_window(hwnd)`) and the target window is in the
foreground, system combinations — Win, Alt+Tab, Alt+Esc, Ctrl+Esc, Alt+F4, Win+R, PrintScreen —
are swallowed locally and forwarded as scancodes.

- Ctrl+Alt+Del and Win+L cannot be intercepted by any user-mode program.
- Injected events (`LLKHF_INJECTED`) are never grabbed, so a host and viewer on the same machine
  cannot feed back into each other.
- A key-up for a key that was pressed before the grab started passes through locally.
- Losing focus emits a single `ReleaseAll`.

The decision logic is pure (`GrabLogic`) and unit-tested; the app decides when the grab is
active (see [tlp-desk-app](app.md)).

## Local activity (host)

`LocalActivityMonitor::start()` installs `WH_MOUSE_LL` and `WH_KEYBOARD_LL` hooks on the thread
`tlp-desk-local-activity`. The hooks never swallow anything; they only record the time of the
last **physical** event, ignoring events flagged `LLMHF_INJECTED | LLMHF_LOWER_IL_INJECTED` /
`LLKHF_INJECTED | LLKHF_LOWER_IL_INJECTED` — including the host's own `SendInput`. The timestamp
is a lock-free `AtomicU64` that only moves forward. The grace window (default 1000 ms) is
applied by the host, see [tlp-desk-host](host.md) and
[ADR 0010](../../adr/0010-local-input-priority.md).

## Remote pointer marker (host)

`RemotePointerMarker` is a layered, click-through, top-most tool window (thread
`tlp-desk-pointer-marker`) that follows the cursor every 16 ms and shows a green arrow
(`#22C55E` with a white border and dark rim) with an optional label — the viewer's name, drawn
with Segoe UI and right-to-left reading order for Arabic. It re-renders when the monitor DPI
changes and re-asserts top-most about once a second.

It calls `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)` (Windows 10 2004 and later) so
it is **not** captured into the video; `excluded_from_capture()` reports whether that worked. See
[ADR 0011](../../adr/0011-capture-exclusion-for-pointer-marker.md).

Preview it without capture exclusion:

```powershell
cargo run -p tlp-desk-input --example marker_preview -- "viewer-pc" 4
```

## Tests

Run in CI: mapping (`normalize_pixel_roundtrips_every_pixel`,
`absolute_roundtrips_every_pixel_of_many_desktops`, `absolute_clamps_outside_points_and_corners`,
`host_path_multi_monitor_layouts`, `viewer_to_host_is_exact_at_native_scale_for_any_dpi`,
`viewer_scaled_and_letterboxed`, `viewer_downscaled_and_upscaled_hit_proportional_pixel`,
`viewer_degenerate_inputs_do_not_panic`), tracker (`release_all_releases_exactly_what_is_held`,
`drop_releases_held_keys`, `failed_release_is_retried_later`, ...), grab logic
(`grabbed_system_combos_are_forwarded_and_suppressed`, `injected_events_are_ignored`,
`losing_foreground_releases_remote_keys_once`, ...), activity, keys, marker shape, and the
Windows smoke tests `grab_starts_toggles_and_stops` and
`monitor_starts_ignores_injected_input_and_stops` (these install real hooks but need no user).

**Ignored:**

- `marker_window_starts_toggles_and_stops` — shows a window on the interactive desktop.
- `real_mouse_move_lands_on_exact_pixel` (`tests/real_injection.rs`) — moves the real mouse
  cursor; needs an unlocked desktop.

```powershell
cargo test -p tlp-desk-input
cargo test -p tlp-desk-input --test real_injection -- --ignored --nocapture
```
