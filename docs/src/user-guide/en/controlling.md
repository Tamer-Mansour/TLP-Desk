# Controlling the remote PC

Once connected, the other PC's screen fills your TLP-Desk window, with a toolbar at the top. Most
toolbar buttons are **icons**; hover over one to see its name.

| Icon | Name | What it does |
|---|---|---|
| green dot | **Connected to …** | Shows which PC you are connected to |
| arrows pointing inwards (shrink) | **Fit** | Scale the remote screen to your window — see [View options](view-options.md) |
| magnifying glass with + | **Actual size** | One remote pixel per screen pixel |
| four corners outwards / inwards | **Full screen** / **Exit full screen** | |
| eye | **View only** | Watch without controlling |
| pulse line | **Stats** | Show or hide the [statistics overlay](stats.md) |
| monitor + numbers | **Screen** 1, 2, … | [Switch monitors](monitors.md) (only with two or more) |
| keyboard (highlighted) | **Keyboard goes to the remote PC** | Appears while system keys are captured (see below) |
| red button with exit arrow | **Disconnect** | End the session |

Selected toggles (for example **Fit** or **View only**) are shown highlighted. In Arabic the
toolbar is mirrored. Until the first picture arrives you see **Waiting for video…**.

## Mouse

Move, click, double-click, right-click, drag and scroll as if the remote PC were in front of you.
Clicks land on exactly the point you see, whatever the two screens' resolutions or scaling.
If the picture is smaller than your window, the empty bands around it are not part of the remote
screen; you can still finish a drag there while holding the button.

The remote PC draws its own mouse pointer into the picture, so the pointer you see is the remote
one.

## Keyboard

Type normally. TLP-Desk sends **which keys you press**, not which letters, so the **remote PC's
keyboard layout** decides what appears. To type Arabic on the remote PC, switch the remote PC's
input language (for example with Alt+Shift or Win+Space while connected) — your own layout does not
matter.

Tab, the arrow keys and Escape go to the remote PC. Ctrl+C, Ctrl+V and Ctrl+X are pressed on the
remote PC too (they copy and paste *there*; clipboard sharing between the two PCs is
[coming soon](coming-soon/clipboard.md)).

### System keys: Windows key, Alt+Tab

While the TLP-Desk window is active and you are controlling the remote PC, system shortcuts such as
the **Windows key**, **Alt+Tab**, **Alt+Esc**, **Ctrl+Esc** and **Alt+F4** are sent to the **remote
PC** instead of your own. The toolbar then shows a highlighted keyboard icon and, when there is
room, the words **Keyboard goes to the remote PC**.

- Click outside the TLP-Desk window (or switch away with the mouse) and the keys work on your own
  PC again.
- **Ctrl+Alt+Del** and **Win+L** always act on your own PC — Windows does not let any program
  intercept them. A Ctrl+Alt+Del button for the remote PC is [coming soon](coming-soon/ctrl-alt-del.md).
- When you leave the window, TLP-Desk releases every key and mouse button on the remote PC, so
  nothing stays stuck.

## When the other person uses their PC

The person at the remote PC always has priority. If they move their mouse or type, your input is
paused and a yellow notice appears:

> The person at the other device is using the mouse or keyboard. Your control resumes when they
> stop.

Your control comes back automatically about one second after they stop. Meanwhile they see a
**green pointer with your name** on their screen whenever you move the mouse, so they always know
the movement is yours (you do not see this marker).

## What does not work yet

- Windows that run **as administrator** on the remote PC, **UAC prompts**, the **lock screen** and
  the **sign-in screen** cannot be controlled (and UAC prompts or the lock screen may not be visible)
  — this needs the [unattended access service](coming-soon/unattended-access.md).
- Clipboard, chat and sound are [coming soon](coming-soon/index.md).

## Sending files

Drag files or folders onto the session window, or click the **File transfer** button in the toolbar
and choose **Send files…** or **Send folder…**. The list shows speed and time left; you can cancel a
transfer and **retry** it later, and it continues where it stopped. Files are checked on arrival,
never overwrite existing ones (a copy is named `name (2).ext`), and land in the host's Desktop
folder. The host must allow files for the session. From a script:
`TLP-Desk-cli send-file <target> <paths…> --to <host folder>` and
`TLP-Desk-cli get-file <target> <host paths…> --to <folder>`.

## Ending the session

Click **Disconnect** (red, at the end of the toolbar). If the other side ends the session, you see
**The session has ended.** Either way you return to the home screen.
