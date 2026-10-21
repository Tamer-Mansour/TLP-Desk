# Design system

All colors, sizes and reusable widgets live in three files of `tlp-desk-app`:
`src/ui/theme.rs` (tokens and the egui style built from them), `src/ui/widgets.rs`
(components) and `src/ui/shell.rs` (the workspace shell every screen sits in). Screens never
hard-code a color or a size: they ask for a token.

## Shape of the app

The window is a **sidebar workspace**: a 64 pt icon rail on the reading-start side (brand
mark, Home, Devices, Sessions, Settings, theme toggle at the bottom; the active item carries a
teal glow and a bookmark bar on the canvas edge, a live-session count sits as a badge on
Sessions) and a canvas whose content column is capped at 1180 pt. Pages open with a large
semibold title and a one-line lede (`shell::page_title`). A session takes the whole window.

The mood is **deep and glassy**, dark first: a graphite canvas (`0x0C0F13`), the rail one
step deeper, and translucent **glass panels** (`theme::glass`: a lighter fill at ~92 %
opacity, a soft shadow, a rim highlight brightest along the top edge) for every card. The
identity card on Home has a teal **light** behind its top-start corner (`theme::glow_shape`, a
mesh radial gradient so it never bands). The light theme is the same structure lifted: a cool
off-white canvas and paper-white glass. The accent is a mint teal (`0x52D6C3` on dark,
`0x0A7266` as text on light, `0x128072` / `0x0D8275` as the filled primary).

Type is **Inter** (vendored, SIL OFL, with the Arabic fallback after it): Regular for body,
Medium for buttons and labels, SemiBold for titles and the device ID. Eyebrow labels
(`theme::eyebrow`: 11 pt, tracked, muted, uppercase in English) head every card instead of
icon headings. People and devices are shown with an **avatar** (`widgets::avatar`,
`theme::paint_avatar`: the first letter, Arabic included, on a tint derived from the name) and
a presence dot.

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
- Radii `theme::radius`: `SM` 6 (chips), `MD` 8 (+4 on buttons and inputs), `LG` 12 (rows,
  toasts), `XL` 16 (panels, windows), `PILL`.
- Type `theme::font`: `TITLE` 22, `HEADING` 17, `BODY` 14, `CAPTION` 12, `EYEBROW` 11,
  `DISPLAY` 44 (device ID), `CODE` 26 (one-time code), `MONO` 13.5 (IDs and IPs). Helpers:
  `theme::title`, `theme::caption`, `theme::eyebrow`, `theme::mono`, `theme::medium(size)`,
  `theme::semibold(size)`.
- Elevation: `card_shadow` (light-theme cards only; dark panels rely on the grey step and the
  glass rim), `glass_shadow_dark`, `float_shadow` (toolbars, toasts, popups), `window_shadow`
  (dialogs).

## Components (`widgets.rs`)

- `button(ui, tx, ButtonKind::{Primary, Secondary, Ghost, Danger}, icon, text)`;
  `primary_icon_button` / `danger_icon_button` return an `egui::Button` that changes color on
  hover and press. `ghost_scope` makes the buttons inside it frameless until hovered.
- `text_field` (Arabic-aware, accent focus ring), `switch` (reports itself as a checkbox),
  `chip`, `segmented`, `list_row` (hover tint and hairline), `avatar`, `section_header`
  (an eyebrow) / `icon_heading`, `icon_badge`, `divider`, `empty_state`, `toast_frame` +
  `toast_row`; `home::status_pill` (dot + caption, sized to its text).
- Shell: `shell::rail`, `shell::canvas`, `shell::page_title`. Settings is two-pane
  (`SettingsSection` navigation on the start side, the section's cards on the end side; one
  column under 760 pt).
- `fit` / `elide`: shorten text in logical order so it fits a width (Arabic loses its reading
  end, not its start). Device and address-book rows lay out their actions first and fit the name
  and address into what is left, so nothing runs under the buttons.

## RTL

Every component flows from the reading start (`theme::row`, `theme::column`); the switch knob
moves towards the reading end when on. Check both languages and both themes with
`scripts/update-readme-media.ps1 -SkipVideo`.
