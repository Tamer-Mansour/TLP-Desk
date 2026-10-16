# Files: browse, copy and paste between the two PCs

During a session you can work with the files of the other PC without leaving TLP-Desk.

## The file browser

Click the **folder** button in the session toolbar. A panel opens under the remote screen with two
panes: **This PC** and **Remote PC** (in the Arabic interface the two panes swap sides).

- The first view of a pane lists drives (with label, size and free space) and the usual folders
  (Desktop, Documents, Downloads, Pictures, Videos, Music). Double-click one to go in.
- The path above the list is clickable (breadcrumbs). Use **Back**, **Forward**, **Up** and
  **Refresh** in the pane toolbar.
- Click a column header to sort by name, size or date. Folders always come first. Ctrl+click
  and Shift+click select several items.
- **New folder**, **Rename** and **Delete** act on the selected items of that pane. Delete asks
  for confirmation first.
- **Copy to the other PC** (the arrow button) sends the selected items to the folder open in the
  other pane. You can also drag the selection from one pane onto the other. Progress shows in
  the transfer list (the arrow-up button in the toolbar), where you can cancel or retry; a retry
  continues where the copy stopped.

Names in Arabic, English or any mix are shown and kept exactly.

Everything here needs the **Files** permission, which the person at the host grants (and can take
back at any time). Without it the panes show a message instead of the other PC's files. Only local
drives are listed; network drives and `\server\share` paths are never offered.

## Copy and paste between the PCs

Select files or folders in Explorer on one PC and press **Ctrl+C**. On the other PC, open an
Explorer window (or the desktop) and press **Ctrl+V**. The files arrive in that folder, in
both directions, with Explorer's own copy dialog showing the progress; the transfer also
appears in the transfers list. Nothing is sent when you copy: the bytes only travel when you
paste, so copying a large folder and never pasting it costs nothing.

This needs the **Files** and **Clipboard** permissions on the session (the person at the host
grants them and can take them back at any time; taking either away removes the pending paste at
once). Folder structure, Arabic file names and file dates are kept. If you copy something else
(text, an image, other files) the earlier paste is replaced; when the session ends it is removed.

Notes:

- The other PC's files appear on your clipboard as soon as it copies them, replacing what you had
  copied here. That is how Ctrl+V can work in Explorer.
- Explorer starts the copy dialog right away, but each file only starts to fill in once it has
  fully arrived (it is verified first), so a single very large file shows no progress in
  Explorer's dialog until it completes; watch the transfers list for its progress.
- File names longer than the Windows limit of 259 characters (with their folders) are not offered.
- While a paste is running, temporary copies are kept in `%TEMP%\TLP-Desk\paste` and deleted when
  the session ends or you copy something else.

If Ctrl+V in Explorer is not an option, the buttons still work: **Paste files copied on the
remote PC** (toolbar and file browser) downloads those files into the folder shown in your
**This PC** pane (or your Downloads folder), and **Paste files copied on this PC** sends the
files you copied to the folder open in the **Remote PC** pane. Ctrl+V while the session window
is focused does the same as the second button.

## Drag and drop

Drop files onto the session window to send them to the remote PC (they land in the Desktop
folder unless you pick a folder in the browser).

## From the command line

`TLP-Desk-cli` can do the same without a window; see [Command line](command-line.md):

```text
TLP-Desk-cli ls <host> D:\Shared
TLP-Desk-cli mkdir <host> D:\Shared\new-folder
TLP-Desk-cli mv <host> D:\Shared\a.txt D:\Shared\b.txt
TLP-Desk-cli rm <host> D:\Shared\old --recursive
```
