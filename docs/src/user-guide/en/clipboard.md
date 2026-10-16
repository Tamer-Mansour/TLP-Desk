# Clipboard sharing

Copy on one PC and paste on the other, in both directions, while a session is open.

## What is shared

- **Text**, exactly as copied. Arabic text keeps its direction marks (RLM and LRM), joiners
  (ZWJ and ZWNJ), diacritics (harakat) and line breaks.
- **Formatted text** copied from a web page or a document (HTML), so bold, links and tables
  survive the trip. Pasting into a plain-text field still gives plain text.
- **Images**, such as a screenshot or a picture copied from a browser.

To copy **files** between the PCs, use the [file browser and Explorer copy/paste](file-transfer.md).

## How to use it

Nothing to set up. Copy as usual (Ctrl+C), then paste (Ctrl+V) on the other PC. A short message
in the session says "Clipboard sent" or "Clipboard received".

- The **Sync clipboard** switch in the session toolbar turns sharing on or off for that session
  on your side. It is on by default.
- **Settings > Clipboard** sets the default for new sessions.

## Permission

Sharing needs the **Clipboard** permission, which the person at the host grants. They can take it
back at any time during the session: the switch then shows that the other PC does not allow it,
and nothing more is sent in either direction. A one-time code or an accepted request gives the
default permissions of the host; check the host's permission list if sharing does not work.

## Limits and privacy

- Text and formatted text up to 16 MB, images up to 64 MB once decoded. Larger items are skipped
  with a message; nothing else is affected.
- What you copy is sent only to the other PC of the session, encrypted. It is never stored, and
  the log files record only that something was copied (its kind and size), never its content.
- Something that arrived from the other PC is not sent back, and the same content is not sent
  twice in a row.
- Content you paste into a host is not passed on to other viewers of the same host.

## Command line

`TLP-Desk-cli clip-send <host> "text"` (or `--file notes.txt`) puts text on the host's clipboard,
and `TLP-Desk-cli serve --clipboard` allows clipboard sharing on a headless host. See the
[command line](command-line.md) page.
