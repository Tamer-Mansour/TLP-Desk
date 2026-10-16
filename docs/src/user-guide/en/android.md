# TLP-Desk on Android (viewer)

The Android app lets you **see and control a Windows computer** that runs TLP-Desk, from a phone or
tablet on the same Wi-Fi. It is a viewer only: an Android device cannot be controlled by others.

> **Status: experimental.** The app was built and its logic tested without an Android device.
> It has not been run on real hardware yet. Please report what you find.

## Installing

The build is debug-signed and not on any store. Copy `TLP-Desk.apk` to the phone and open it
(Android asks to allow installs from that source), or from a computer:
`adb install TLP-Desk.apk`.

## Connecting

1. On the computer you want to control, open TLP-Desk and note its **device ID** (and the
   one-time code if you want to use it).
2. On the phone, open TLP-Desk. Type the computer's **IP address** (for example `192.168.1.20`)
   or `<device ID>@<IP address>`.
3. Leave the code empty to make the computer ask its user to press **Accept**, or type the
   6-digit one-time code (or the unattended-access password).
4. Press **Connect**.

Connecting by IP address always works. Connecting by device ID alone needs the phone to receive
multicast packets, which Android blocks for apps that do not hold a Wi-Fi multicast lock. This
version does not take that lock yet, so use the IP address.

## Touch controls

| You do | The remote computer gets |
|---|---|
| Tap | left click |
| Touch and hold | right click |
| Drag one finger | left button held down while you drag (select text, move windows) |
| Two fingers moving together | mouse-wheel scroll, in the natural direction |
| Pinch / spread | zoom the picture on your phone (the remote screen is unchanged); the centre of the pinch also pans |
| **Fit screen** | back to the whole remote screen |
| **Keyboard** | opens the phone keyboard |

## Typing and Arabic

Letters, digits and punctuation are sent as key presses of a **US keyboard layout**: the
computer's own layout decides the result, so a computer set to another layout may show different
symbols. Arabic and any other character a key press cannot express are sent through the
clipboard and pasted with Ctrl+V on the computer; this needs the computer to allow clipboard
sharing for the session. The app itself is available in Arabic and English (tap the language
button) with right-to-left layout.

## Not available yet

File transfer, chat, sound, switching between monitors, quality presets, recording and discovery
by device ID. The session itself supports them; the touch screens do not yet.
