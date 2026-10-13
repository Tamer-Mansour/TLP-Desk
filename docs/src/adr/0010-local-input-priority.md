# 0010: The person at the host always has input priority

- **Status:** Accepted (protocol v3)
- **Date:** 2026-10-13
- **Area:** `tlp-desk-host` (`priority.rs`, `input.rs`), `tlp-desk-input` (activity hooks)

## Context

During an attended session both people have a mouse and keyboard on the same desktop. If both move
the pointer, it jumps between them; if the viewer holds a key while the host user types, keys get
stuck. More importantly, the person sitting at the PC must always be able to take control back —
for example to stop something they did not expect.

## Decision

**The local user wins.**

- The host installs low-level mouse and keyboard hooks that record the time of the last
  *physical* local event. Events injected by software — including the host's own `SendInput` and
  lower-integrity injectors — are ignored via the `*_INJECTED` flags, so the viewer's input never
  counts as local activity.
- While the last local event is younger than the **grace window (default 1000 ms)**, every
  `MouseMove`, `MouseButton`, `MouseWheel` and `Key` from the viewer is dropped. On entering this
  state the host releases every key and button the viewer still held.
- Viewer input is accepted again once the grace window passes with no local input.
- The host sends `HostInputPriority { local_active }` on each change; the viewer shows "The person
  at the other device is using the mouse or keyboard. Your control resumes when they stop."
- The grace window is configurable (`HostConfig::local_input_grace`, CLI `--local-grace-ms`); zero
  disables the feature.

## Alternatives considered

- **Last writer wins** — pointer fights and stuck keys.
- **Explicit "take control" button on the host** — slower to react and one more thing to find
  under stress; moving the mouse is instinctive.
- **Lock out the local user (like privacy mode)** — the opposite of attended support; privacy mode
  is a separate, explicit permission (M3).

## Consequences

- No pointer fights; nothing stays pressed when the host user takes over.
- A host user who keeps touching the mouse effectively blocks remote control — intended.
- Hooks start lazily on the first remote event, so a session that is only viewed costs nothing.
- Multiple simultaneous viewers each run their own hooks today; sharing one set is an open roadmap
  item.

Guarded by `remote_events_blocked_during_grace`, `takeover_releases_remote_held_state_once`,
`remote_resumes_after_grace`, `injected_events_do_not_count_as_local_input`,
`zero_grace_disables_priority` (host), `injected_events_do_not_count` (input) and the e2e test
`local_input_priority_reaches_viewer`.

## References

[Input path](../dev/architecture/input.md); [wire protocol](../protocol.md#local-input-priority-v3).
