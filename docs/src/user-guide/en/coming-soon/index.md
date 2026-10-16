# Coming soon

Everything else in this guide is in the current version (1.0.0-rc.2). The items below are not
there yet; they are being worked on or are planned in the
[roadmap](https://github.com/Tamer-Mansour/TLP-Desk/blob/TLP-Desk/ROADMAP.md). Until they arrive,
the right-hand column says what to use instead.

| Not available yet | Until then |
|---|---|
| Dragging files from the remote side of the file browser straight into Explorer or onto the desktop, and dropping files onto a folder in the remote pane | Copy the files in the file browser, or copy them in Explorer and paste them in Explorer on the other PC; dropping files on the session window sends them to the remote PC (see [Files](../file-transfer.md)) |
| Sound in session recordings | Recordings contain the picture only (see [Session recording](../recording.md)) |
| A code-signed installer | Windows SmartScreen may warn the first time; see [Installing](../installing.md) |
| Linux on Wayland | Sign in to an X11 session (for example "GNOME on Xorg"); see [Linux](../linux.md) |
| Finding devices automatically on Android | Type the PC's IP address; see [Android](../android.md) |

None of these needs a new permission model: each one will use the permissions the host already
grants today (for example **Files** for file transfer), and the person at the host can still
withdraw them during the session.
