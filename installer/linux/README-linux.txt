TLP-Desk for Linux
==================

Free, open-source, LAN-only remote desktop. No cloud, no account, no telemetry.

Install
-------
  ./install.sh                 per user, into ~/.local (no root)
  sudo ./install.sh --system   for everyone, into /usr/local
  ./install.sh --uninstall     remove it again

Requirements
------------
  * An X11 session (Xorg, or XWayland is NOT enough to share the screen). On Wayland, log in
    with the "... on Xorg" session. Native Wayland capture (xdg-desktop-portal ScreenCast +
    PipeWire) is planned but not implemented yet.
  * A GPU driver with Vulkan or OpenGL for the window.
  * Optional, for sound from this PC: PulseAudio or PipeWire (pipewire-pulse). Set
    TLP_DESK_AUDIO_SOURCE to part of the name of the "Monitor of ..." device if the
    automatic choice is wrong.
  * LAN: UDP 47400 (QUIC), 47401 (discovery fallback) and mDNS (UDP 5353) must be reachable.
    With ufw:   sudo ufw allow from 192.168.0.0/16 to any port 47400,47401 proto udp

Data
----
  Identity (device ID):  ~/.local/share/tlp-desk   (mode 0700)
  Settings, address book, connection log:  ~/.config/tlp-desk

Not available on Linux yet
--------------------------
  Privacy mode, the remote pointer marker, the whiteboard overlay on the host, local-input
  priority, the tray icon, the Windows-service style unattended host and Ctrl+Alt+Del.
  Everything else (view, control, files, clipboard, chat, tunnels, recording) is built and
  cross-compiled for Linux; it has not been run on real Linux machines yet, so please report
  what you find.

Documentation: docs/src/user-guide/en/linux.md in the source tree.
