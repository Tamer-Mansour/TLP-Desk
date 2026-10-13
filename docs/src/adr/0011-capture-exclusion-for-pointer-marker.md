# 0011: Remote-pointer marker excluded from capture

- **Status:** Accepted
- **Date:** 2026-10-13
- **Area:** `tlp-desk-input` (`win/marker.rs`, `marker_shape.rs`), `tlp-desk-host`

## Context

When someone controls a PC remotely, the person sitting there sees the pointer move "by itself".
They should be able to tell at a glance that the movement is remote, and by whom. Drawing an
indicator on the host's screen has a catch: the host is also capturing that screen, so the
indicator would appear in the viewer's video, on top of their own pointer, and would be encoded
into every frame.

## Decision

Show a **green marker** (an arrow larger than the system pointer, `#22C55E` with a white border and
a dark rim, plus a label with the viewer's name) in a layered, click-through, top-most tool window
that follows the cursor, and call
`SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)` on it so screen capture **does not
include it**.

- Shown while the viewer moved the pointer within the last 1.5 s and the local user is idle;
  hidden as soon as the local user takes over (see [ADR 0010](0010-local-input-priority.md)).
- Created lazily on the first remote pointer move.
- Re-rendered for the monitor's DPI; the label supports Arabic (right-to-left reading order).
- If exclusion is not supported, the host logs a warning (`excluded_from_capture()` reports it).

## Alternatives considered

- **Draw into the captured frame only** — the viewer would see it, the host user would not.
- **Change the system cursor shape** — affects every application, cannot carry a name, and would
  be captured.
- **No indicator** — confusing and less transparent for the host user.

## Consequences

- The host user always sees *who* is moving the pointer; the viewer's video stays clean.
- Requires Windows 10 version 2004 or later for `WDA_EXCLUDEFROMCAPTURE`; on older builds the
  marker would be captured.
- The marker is a real top-most window; it must never take focus or clicks (`WS_EX_TRANSPARENT`,
  `WS_EX_NOACTIVATE`, `HTTRANSPARENT`).
- It complements, but does not replace, the "who is connected" banner required by the security
  model.

Guarded by `marker_*` shape tests, `marker_window_starts_toggles_and_stops` (ignored, needs a
desktop), `marker_follows_remote_pointer_and_hides_for_local`,
`marker_hides_after_remote_pointer_goes_idle`; manually verified that the marker does not appear in
the captured video.

## References

[tlp-desk-input](../dev/crates/input.md#remote-pointer-marker-host);
[Microsoft Learn: SetWindowDisplayAffinity](https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-setwindowdisplayaffinity).
