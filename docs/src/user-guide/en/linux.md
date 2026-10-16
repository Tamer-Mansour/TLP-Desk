# Linux

TLP-Desk also runs on Linux, as a host (the PC being controlled) and as a viewer. This support is
new: it has been compiled and unit-tested, but not yet run on many real Linux PCs, so please
report anything odd.

## What you need

* A 64-bit Linux desktop with an **X11 session** (Xorg). On Wayland, pick the
  "... on Xorg" session at the login screen to share the screen. Sharing a native Wayland screen
  is not available yet; **viewing** other PCs from a Wayland session works.
* A graphics driver with Vulkan or OpenGL (any normal desktop driver).
* For sound from the Linux PC: PulseAudio or PipeWire (with `pipewire-pulse`).
* The same network rules as on Windows: UDP 47400 and 47401, and mDNS (UDP 5353). With `ufw`:
  `sudo ufw allow from 192.168.0.0/16 to any port 47400,47401 proto udp`.

## Install

Download `TLP-Desk-<version>-linux-x86_64.tar.gz`, unpack it and run the installer script:

```bash
tar xzf TLP-Desk-*-linux-x86_64.tar.gz
cd TLP-Desk-*-linux-x86_64
./install.sh                 # for you only, into ~/.local (no root needed)
sudo ./install.sh --system   # for everyone, into /usr/local
```

TLP-Desk then appears in the application menu. To remove it run `./install.sh --uninstall`
(add `--system` if you installed system-wide). Your device ID and settings are kept.

To build it yourself, see [Building on Windows](../../dev/building.md) for the idea and
[Linux](../../dev/linux.md) for the Linux packages (`build-essential pkg-config cmake nasm
libasound2-dev`), then run `installer/linux/build-tarball.sh`.

## Where things are stored

| What | Where |
|---|---|
| Device ID and keys | `~/.local/share/tlp-desk` (only you can read it) |
| Settings, address book, connection log | `~/.config/tlp-desk` |
| Start with the desktop session | `~/.config/autostart/TLP-Desk.desktop` |

## Using it

Connecting, the Accept prompt, files, clipboard, chat, recording and the command line work as
described in the rest of this guide.

* **Keyboard.** While the session window is focused the keyboard is grabbed on X11, so keys like
  Super and Alt+Tab go to the remote PC. If another program already holds the grab (an open
  menu), TLP-Desk falls back to normal key events.
* **Typing in Arabic.** Keys are sent by position, so the layout of the PC being controlled
  decides the letters. Switch that PC's layout, not yours.
* **Sound.** The Linux PC shares what it plays through the "Monitor of ..." device of your sound
  card. If the wrong one is chosen, set `TLP_DESK_AUDIO_SOURCE` to part of its name before
  starting TLP-Desk. It never records the microphone.

## What is not available on Linux yet

* Sharing a native Wayland screen.
* Privacy mode, the green remote-pointer marker, the whiteboard drawing on the Linux screen and
  the local-input priority (your own mouse taking over). Everything else still works.
* The tray icon: closing the window quits TLP-Desk.
* Unattended access as a system service and Ctrl+Alt+Del.

## Troubleshooting

* **"Wayland session"** error when hosting: log in with an Xorg session, or see
  [Linux](../../dev/linux.md).
* **No sound:** run `pavucontrol` and check a "Monitor of ..." source exists, then set
  `TLP_DESK_AUDIO_SOURCE`.
* **Others cannot find this PC:** open the UDP ports above in your firewall.
