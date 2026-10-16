# TLP-Desk on a Mac

TLP-Desk runs on macOS 11 or newer, as a host (others see and control this Mac) and as a viewer.
Everything stays on your local network, exactly like on Windows.

> **Status: experimental.** The macOS port was written and cross-checked without a Mac at hand.
> It has not been run on real hardware yet. Please report what you find.

## Installing the unsigned app

The macOS build is not notarized by Apple, so the first launch needs one extra step.

1. Unzip `TLP-Desk-<version>-macos.zip` and drag **TLP-Desk.app** to **Applications**.
2. Control-click (right-click) the app, choose **Open**, then **Open** again. If macOS still says the
   app is blocked, open **System Settings > Privacy & Security** and press **Open Anyway**.
3. Or, in Terminal: `xattr -dr com.apple.quarantine /Applications/TLP-Desk.app`

## Allow the two permissions

If either permission is missing, the home screen shows a card with a button that opens the right
pane of System Settings. The card disappears by itself once you allow it.

| Permission | Why | Where |
|---|---|---|
| **Screen Recording** | so the connected device can see the screen of this Mac | Privacy & Security > Screen Recording |
| **Accessibility** | so the connected device can move the mouse and type | Privacy & Security > Accessibility |

macOS may ask you to quit and reopen TLP-Desk after you allow Screen Recording.

## Keyboard differences

A Windows viewer controlling a Mac: the **Windows key acts as Command** and **Alt as Option**, so
Win+C copies and Win+V pastes on the Mac. Keys that a Mac does not have (Pause, Scroll Lock,
Print Screen) are ignored. Typing in Arabic works: the keyboard layout of the Mac decides the
letters, as on Windows.

## What is not available on a Mac host yet

Unattended access as a system service, privacy mode, the remote-pointer marker on the host screen,
the whiteboard overlay on the host screen and the keyboard grab (system shortcuts such as Cmd+Tab
stay on the viewer). These are Windows-only for now.

Your settings and device identity are stored in `~/Library/Application Support/TLP-Desk`.
