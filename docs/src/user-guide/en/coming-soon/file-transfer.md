# File transfer and remote file browser

> **Coming soon.** This feature is planned but not available yet. It is part of **M2 — Daily-driver features, Lane A** in the [roadmap](https://github.com/Tamer-Mansour/TLP-Desk/blob/TLP-Desk/ROADMAP.md).

## What it will do

Copy files between the two PCs during a session:

- a **file transfer window** showing the remote PC's drives and folders (Desktop, Documents, Downloads, ...), where you can browse, create folders, rename and delete;
- **upload and download** of files and whole folders, with progress, and **resume** after an interruption;
- **drag and drop** files onto the session window;
- every file checked after transfer, so a damaged copy is detected.

File and folder names in Arabic will be kept exactly.

Needs the **Files** permission, which the person at the host must grant.

## Status

The wire protocol already defines the remote file system and transfer messages (protocol v4), and file data travels on separate low-priority streams so a large copy never slows down the picture or your mouse.

## Until then

Use a shared network folder, a USB stick, or e-mail between the PCs.
