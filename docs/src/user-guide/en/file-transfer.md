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

Copy files in Explorer on one PC as usual (Ctrl+C). When the viewer is connected, a button
**Paste files copied on the remote PC** appears in the toolbar and in the file browser; it
downloads those files into the folder shown in your **This PC** pane (or your Downloads folder).
In the other direction, copy files in Explorer on your PC and click **Paste files copied on this
PC** in the browser to send them to the folder open in the **Remote PC** pane.

Limitation: this is an explicit paste button, not Ctrl+V inside Explorer. TLP-Desk does not
register a delayed-render clipboard object, so pasting directly into an Explorer window on the
other PC is not possible.

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
