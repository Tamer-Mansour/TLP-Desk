# Privacy mode

> **In development.** Implemented on the `TLP-privacy` branch (M3 in the [roadmap](https://github.com/Tamer-Mansour/TLP-Desk/blob/TLP-Desk/ROADMAP.md)); it still needs a test on two real PCs before release. Windows 10 version 2004 or newer is required on the remote PC.

## What it does

While you work on a remote PC, privacy mode **turns its screens black and blocks its own keyboard and mouse**, so people near that PC cannot watch or interfere. You keep seeing and controlling the real desktop. The black screen says, in Arabic and English, that the PC is being used remotely, who is connected, and how to end privacy mode there.

Needs the **Privacy mode** permission; the remote PC must allow it explicitly (it is off by default).

## How to use it

1. In the session toolbar, click **Privacy mode** (the eye-and-shield icon). It is greyed out when the remote PC does not allow it.
2. Read the confirmation and click **Turn on**. A blue banner shows while privacy mode is on.
3. Click the icon again to turn it off. It also ends by itself when you disconnect, when the connection drops, when the remote PC withdraws the permission, or when TLP-Desk on the remote PC exits.

## For the person at the remote PC

You can always get your PC back:

- hold **Ctrl+Alt+Shift+P** for 3 seconds, or
- press **Ctrl+Alt+Del** (Windows never lets any program block it).

Locking the PC, the sign-in screen and administrator (UAC) prompts also end privacy mode: Windows shows them above everything, so they cannot be hidden.

## Limits

- The mouse pointer is still visible on the black screen when the viewer moves it.
- The Start menu and some Windows pop-ups can briefly appear above the black screen when the viewer opens them.
- While privacy mode is on, the "local user has priority" rule is paused (the blocked keyboard and mouse cannot take control back; use the keys above instead).
