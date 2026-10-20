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

In **Fit**, TLP-Desk tells the other computer how big the picture is in your window and gets
exactly that size, scaled once on the other computer's graphics card, so a 4K screen in a smaller
window stays sharp. A remote screen exactly half the size of your picture area (1080p on a 4K
monitor) is shown with each remote pixel doubled exactly, not blurred.

**Zoom** (the percentage next to the two buttons): 25 %, 50 %, 75 %, 100 %, 125 %, 150 %, 200 % or
300 % of actual size, with scroll bars. 200 % and 300 % show every remote pixel as a crisp square:
the easiest way to read very small text. Choosing a zoom leaves Fit; **Fit** in the same menu goes
back. On a narrow window the zoom menu moves into **More** (⋯).

In **Actual size** and the zoom levels, very large remote screens are sent at the quality
preset's size (1920 × 1200 for Balanced; up to 3840 × 2160 for Quality), so pick **Quality** to
see a 4K screen at its full resolution.

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

### Sharp text

When the picture stops moving (you stop scrolling or typing) the remote PC sends one extra
high-quality picture about a third of a second later, so text turns crisp on its own. In **Actual
size** every remote pixel is shown on exactly one screen pixel, and when the window is smaller than
the remote screen, TLP-Desk re-scales the still picture with a sharp filter. Nothing to configure.
