# 0007: egui for the GUI, with our own bidi pass

- **Status:** Accepted
- **Date:** 2026-10-13
- **Area:** `tlp-desk-app`

## Context

The GUI must be a single native binary, draw decoded video frames as GPU textures every frame,
and support **Arabic as a first-class language**: correct letter shaping, right-to-left order,
mixed Arabic/Latin/digits, and mirrored layouts.

## Decision

Use **egui / eframe with the wgpu backend** and fix bidirectional text ourselves:

- egui 0.36 shapes each font run correctly (contextual forms, lam-alef ligatures, harakat) but has
  no Unicode Bidirectional Algorithm. Before text reaches egui, `i18n/shaping.rs` resolves bidi
  levels with `unicode-bidi`, reorders runs into visual order, mirrors brackets, compensates for
  egui reversing Arabic-font runs, and protects LTR isolates (IDs, codes, IPs).
- All UI strings come from a table with English and Arabic side by side; layout helpers mirror rows
  and alignment in RTL.
- Noto Sans Arabic (OFL) is embedded as a fallback font.

## Alternatives considered

- **Native Win32 / WinUI** — native bidi and shaping, but no cross-platform path (macOS/Linux are
  on the roadmap) and a much larger amount of platform code.
- **A web view (Tauri/WebView2)** — excellent text support, but video frames would have to cross
  into the web view every frame, and it adds a large runtime dependency.
- **Other Rust GUI toolkits** — at the time, none combined immediate-mode simplicity, wgpu texture
  upload and mature Arabic shaping better than egui plus a bidi pass.

## Consequences

- One small binary; video textures are uploaded directly with wgpu.
- Static and display-only text is correct in Arabic, verified by tests that run the real epaint
  layout and check glyph positions.
- egui's `TextEdit` has no bidi caret and `TextEdit::layouter` cannot reorder (the galley must have
  the edited characters in edited order), so fields whose text contains Arabic are edited by our
  own `ui::bidi_edit::BidiEdit`: logical `String`, screen-order layout (`i18n::bidi_line`), a caret
  and arrow keys that follow the screen, selection by clusters, wrapping for multi-line fields.
  Plain Latin text still uses egui's `TextEdit` (IME, undo, accessibility). Known gaps while a
  field contains Arabic: no IME pre-edit string and no accessibility text tree.
- Labels in RTL do not soft-wrap (`wrap_mode = Extend`), because reordering works per line.
- Every string must be shaped exactly once — the `Tx` helpers enforce the pattern.
- If egui gains a bidi algorithm upstream, `shaping.rs` should shrink to the parts egui still lacks;
  the `egui_without_our_bidi_gets_word_order_wrong` test will start failing and signal it.

## References

[i18n and Arabic](../dev/i18n.md); [tlp-desk-app](../dev/crates/app.md).
