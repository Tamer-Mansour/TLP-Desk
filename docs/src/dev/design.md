# Design system

All colors, sizes and reusable widgets live in two files of `tlp-desk-app`:
`src/ui/theme.rs` (tokens and the egui style built from them) and `src/ui/widgets.rs`
(components). Screens never hard-code a color or a size: they ask for a token.

## Color tokens

`theme::palette(dark)` (or `theme::tokens(ui)`) returns a `Palette` with the same fields for the
light and the dark theme:

| Token | Use |
|---|---|
| `bg` | Window background behind cards |
| `surface` | Cards, dialogs, the top bar |
| `surface_alt`, `surface_hover`, `surface_pressed` | Rows, segmented track, secondary buttons and their states |
| `input` | Text fields |
| `border`, `border_strong` | Hairlines, dividers, input borders; hover borders |
| `text`, `text_muted` | Primary text; captions, hints, meta lines |
| `accent` | Brand teal for text, icons and focus rings |
| `accent_fill`, `accent_hover`, `accent_pressed` | Filled primary surfaces (white `on_fill` text) |
| `accent_soft` | Tinted accent background (selected chips) |
| `success`, `warning`, `danger`, `info` (+ `_soft`) | Semantic text/icons and their tints |
| `danger_fill`, `danger_hover` | Destructive buttons |
| `shadow` | Elevation color |

`theme::Tone` (`Neutral`, `Accent`, `Success`, `Warning`, `Danger`, `Info`) maps to a
`(foreground, tint)` pair for chips, toasts and banners.

Unit tests in `theme.rs` keep every text token at a WCAG contrast of at least 4.5:1 on every
surface, every tone at 4.5:1 on its own tint, and `on_fill` at 4.5:1 on every fill, in both themes.

## Scales

- Spacing `theme::space`: `XS` 4, `S` 8, `M` 12, `L` 16, `XL` 24.
- Radii `theme::radius`: `SM` 6 (chips), `MD` 8 (buttons, inputs, rows), `LG` 12 (cards, toasts),
  `XL` 16 (windows), `PILL`.
- Type `theme::font`: `TITLE` 20, `HEADING` 17, `BODY` 14.5, `CAPTION` 12.5, `DISPLAY` 32 (device
  ID), `CODE` 24 (one-time code), `MONO` 14 (IDs and IPs). Helpers: `theme::title`,
  `theme::caption`, `theme::mono`.
- Elevation: `card_shadow` (cards), `float_shadow` (toolbars, toasts, popups), `window_shadow`
  (dialogs).

## Components (`widgets.rs`)

- `button(ui, tx, ButtonKind::{Primary, Secondary, Ghost, Danger}, icon, text)`;
  `primary_icon_button` / `danger_icon_button` return an `egui::Button` that changes color on
  hover and press. `ghost_scope` makes the buttons inside it frameless until hovered.
- `text_field` (Arabic-aware, accent focus ring), `switch` (reports itself as a checkbox),
  `chip`, `segmented`, `list_row` (hover tint), `section_header` / `icon_heading`, `icon_badge`,
  `divider`, `empty_state`, `toast_frame` + `toast_row`.
- `fit` / `elide`: shorten text in logical order so it fits a width (Arabic loses its reading
  end, not its start). Device and address-book rows lay out their actions first and fit the name
  and address into what is left, so nothing runs under the buttons.

## RTL

Every component flows from the reading start (`theme::row`, `theme::column`); the switch knob
moves towards the reading end when on. Check both languages and both themes with
`scripts/update-readme-media.ps1 -SkipVideo`.
