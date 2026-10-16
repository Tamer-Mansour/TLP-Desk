# View options

The session toolbar lets you choose how the remote screen is shown and whether you control it.
The buttons are icons — hover over one to see its name (see the table in
[Controlling the remote PC](controlling.md)).

## Fit and Actual size

| Option | What you see | When to use |
|---|---|---|
| **Fit** (default) | The whole remote screen, scaled to fit your window, with bands on the sides if the shapes differ | Most of the time |
| **Actual size** | One remote pixel per screen pixel; scroll bars appear when the remote screen is larger than your window | Reading small text, pixel-exact work |

Clicks land correctly in both modes.

Note that very large remote screens (larger than 1920 × 1200, such as 4K monitors) are sent at a
reduced size to keep the session smooth, so **Actual size** shows that reduced picture.

## Full screen

**Full screen** hides your window's frame and taskbar so the remote screen uses your whole
monitor. Click **Exit full screen** in the toolbar to return. Full screen also ends automatically
when the session ends.

There is no keyboard shortcut for leaving full screen (all keys go to the remote PC), so use the
toolbar button.

## View only

Turn on **View only** to watch without controlling: TLP-Desk then sends no mouse or keyboard input
at all, and any keys or buttons you were holding are released. Turn it off to take control again.

If the person at the remote PC allowed **viewing only**, the button is switched on and cannot be
changed; hovering over it shows **The host allows viewing only.**

While view only is on, system keys (Windows key, Alt+Tab) stay on your own PC.

## Quality

The **Quality** button (sliders icon) in the session toolbar picks a preset for the picture:

| Preset | Picture | Use it for |
|---|---|---|
| **Speed** | up to 1280 × 720, 60 fps, lower bitrate | slow Wi-Fi, lowest delay |
| **Balanced** | up to 1920 × 1200, 60 fps | everyday use (default) |
| **Quality** | the remote monitor's full size, up to 3840 × 2160, 60 fps, high bitrate | reading small text or detail on a fast network |

The change applies immediately. New sessions start with the preset chosen in **Settings**.
Within the preset the remote PC still adapts to the network by itself, and it does so by making the
picture **smaller** (for example 1920 × 1200, then 1600 × 1000, then 1280 × 800) instead of letting
it turn blurry or blocky. The size in use is shown in the [statistics overlay](stats.md); it goes
back up when the network is calm again.
