# Settings

![The settings screen](../../media/settings-en-light.png)

Open **Settings** with the gear button in the title bar, **File → Settings** or **Ctrl+,**.
Click **Back** to return. Changes apply immediately and are remembered.

## Language and display

### Language

Choose **English** or **العربية** (Arabic). The whole interface switches at once, including the
layout: in Arabic everything is mirrored right-to-left.

On first start TLP-Desk uses Arabic if your Windows display language is Arabic, and English
otherwise. You can also switch the language with **View → Language**, or start TLP-Desk once in a
specific language with `TLP-Desk.exe --lang ar` (or `--lang en`).

### Digits

Choose how numbers are **displayed**:

- **Western (0-9)** — `482 913 057`
- **Arabic-Indic (٠-٩)** — `٤٨٢ ٩١٣ ٠٥٧`

This affects only what you see. **IDs, codes and addresses are always copied with 0-9 digits**, so
they work when pasted anywhere. You can type IDs and codes with either kind of digits.

### Theme

**Same as the system** (default), **Light** or **Dark**. Also under **View → Theme**.

### Menus in the title bar

On (the default on Windows), TLP-Desk draws its own title bar, like Visual Studio Code: the logo,
the menus **File, Edit, View, Connections, Devices, Tools, Window, Help**, quick buttons for the
connection log, recordings and settings, and the minimize / maximize / close buttons. Drag the
empty part of the bar to move the window (dragging it to a screen edge snaps it as usual),
double-click it to maximize or restore, and drag any window edge to resize. During a session the
bar shows which device you are connected to; in full screen it is hidden.

Off, Windows draws its normal title bar and the same TLP-Desk bar (menus and quick buttons) sits
under it. The switch applies at once.

Shortcuts outside a session: **Ctrl+N** new connection, **Ctrl+,** settings, **F1** user guide,
**F11** full screen. During a session every key goes to the remote computer.

## This device

### Device name

The name other TLP-Desk users see in their **Devices on this network** list and in the Accept
prompt. By default it is your Windows computer name.

Type a new name and click **Save** (or press Enter). Arabic names are fine. As the note under the
field says, **restart TLP-Desk** so other PCs see the new name.

Keep in mind that the device name is visible to everyone on your network — do not put anything
private in it.

## Incoming connections

![Incoming connections settings](../../media/settings-incoming-en-light.png)

- **Accept incoming connections** — turn off to pause hosting: every viewer is refused before the
  handshake. The tray menu has the same switch (**Pause incoming**).
- **What connected devices may do** — one checkbox per permission (see the screen, mouse,
  keyboard, clipboard, files, sound, chat, drawing, port forwarding, privacy mode, restart,
  Ctrl+Alt+Del). They are the starting permissions of the **next** session; you can still take
  any of them back during a session. By default sessions get view, mouse, keyboard, clipboard,
  files, chat and drawing; **port forwarding is off** and is switched on per session from the
  banner.
- **Decline unanswered requests after** — how long the Accept prompt waits (10 s to 10 min).
- **Allow connecting with the one-time code** — off means only the Accept prompt (or a password)
  lets people in.
- **Show connection requests in their own window, even when TLP-Desk is hidden** — on by default:
  requests appear in the [Accept window](accept-prompt.md). Off: inside the TLP-Desk window only.
- **Play a sound when a request arrives** — the short Windows notification sound when the Accept
  window appears.
- **Pause remote control after I use my own mouse or keyboard for** — local priority, 0 to 5
  seconds (0 turns it off).
- **Show where the remote pointer is on this screen** — the green marker with the viewer's name.

## Allowed networks

![Allowed networks, quality and start-up settings](../../media/settings-network-en-light.png)

Only devices whose address is inside one of these ranges can connect. The default is the private
ranges (10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16) and link-local addresses. Type an address or a
range such as `192.168.1.0/24` and click **Add**. A range that includes public addresses is
refused with a warning unless you click **Add anyway**. At least one range must remain; to refuse
everyone, pause incoming connections instead. **Restore defaults** brings back the default list.
Changes apply to the next connection attempt.

## Picture quality

**Speed**, **Balanced** or **Quality**: the preset used when you connect to another device. It is
remembered and sent to the host as a quality request.

When the remote screen is shown **fitted to the window**, TLP-Desk also tells the other computer
how big the picture is on your screen, and the other computer sends exactly that size. A 4K
screen viewed in a smaller window is therefore scaled once, on the other computer's graphics
card, and shown pixel for pixel here: text stays sharper and less data is sent. In **actual
size** the preset's own size is used.

## Start-up and notification area

- **Start TLP-Desk when I sign in to Windows** — adds or removes a per-user entry
  (`HKCU\...\Run`); no administrator rights are needed.
- **Start hidden in the notification area** — the window starts minimised; hosting still works.
- **Keep running in the notification area when I close the window** — closing the window
  minimises it instead of quitting, so this PC stays reachable. Use **Quit** in the tray menu to
  exit for real.

The tray icon's tooltip shows your ID, and its menu has **Open**, **Copy ID**, **New one-time
code**, **Pause incoming**, **Settings**, **Connection log** and **Quit**. When someone asks to
connect while the window is minimised, it comes back to the front with the Accept prompt. While a
viewer is connected and the window is minimised, a small always-on-top banner with their name and
an **End session** button stays visible (see [The Accept prompt and the banner](accept-prompt.md)).

## Where settings are stored

`%APPDATA%\TLP-Desk\settings.json` — a small text file. Older files without the new sections keep
working (missing values get their defaults), and out-of-range values are corrected on load. Your
device's identity (and therefore its ID) is stored separately and is not affected by settings.
Deleting `settings.json` resets everything to the defaults.
