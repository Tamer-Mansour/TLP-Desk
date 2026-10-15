# The connection log

![The connection log](../../media/connection-log-en-light.png)

TLP-Desk keeps a local record of who connected to this PC and which devices this PC viewed. Open
it with **Connection log** on the home screen or in the tray menu. The log never leaves your
computer.

Each entry shows the device name and ID, direction (incoming or outgoing), date and time, the
address, how the session was authorised (Accept prompt, one-time code or password), how long it
lasted, the permissions it held and how it ended: closed by either side, permission withdrawn,
connection lost, or a refused attempt (declined, not answered, wrong code or password, too many
attempts, not allowed by your settings).

- **Search** matches names (Arabic too), IDs, addresses and key fingerprints. IDs can be typed
  with or without spaces, in 0-9 or Arabic-Indic digits.
- The filters show **All**, **Incoming**, **Outgoing** or **Refused** attempts only.
- **Export CSV** saves everything to a spreadsheet-friendly file (UTF-8, Arabic opens correctly in
  Excel).

The file is `%APPDATA%\TLP-Desk\connections.log`, one JSON record per line. When it grows past a
size limit it is rotated to `connections.log.1`, `.2` and so on and the oldest is deleted, so it
never fills the disk.
