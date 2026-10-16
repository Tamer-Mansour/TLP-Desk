# Updating TLP-Desk

TLP-Desk never goes on the internet by itself. There are two ways to learn about a new version,
and you start both.

## A newer version on your network

Every TLP-Desk on your network tells the others which version it runs (as part of the normal
LAN discovery, no internet involved). When another PC runs a **newer** version, the home screen
shows a small notice:

> A newer version (1.0.1) is running on Office-PC — **Download**

**Download** opens the TLP-Desk releases page in your browser; nothing is copied between the
PCs. **Hide** hides the notice until an even newer version appears.

## Checking GitHub for updates (opt-in)

Open **Settings → Updates**:

- **Check for updates now** asks GitHub (github.com) whether a newer release exists.
- **Check daily** does the same once a day while TLP-Desk runs. It is **off** by default.
- **Include test versions (release candidates)** also offers `-rc` builds. It is on while
  TLP-Desk is in its release-candidate phase.

These are the only times TLP-Desk contacts the internet. If the PC has no internet, the check
simply fails; everything else keeps working.

When a newer version exists, a window shows its number and the first lines of its release notes:

- **Download & install** downloads the setup into `%TEMP%\TLP-Desk\update\` with a progress bar.
- **Later** closes the window.

### How the download is checked

Before anything runs, TLP-Desk verifies the download:

1. `SHA256SUMS.txt` (the list of file checksums of the release) must carry a valid **signature**
   from the TLP-Desk release key, which is built into the app.
2. The setup file's **SHA-256** must match the signed list.

If either check fails, the file is deleted, nothing is installed, and you see "the download failed
verification". This protects you even if the download was tampered with on the way.

After a successful check, **Install now** asks Windows for administrator rights (the usual UAC
prompt), runs the setup silently, and closes TLP-Desk. The setup updates the program and the
TLP-Desk service and starts them again. Your device ID, settings and address book are kept.

## Updating by hand

You can always download `TLP-Desk-<version>-setup.exe` from the releases page and run it; it
upgrades the installed version in place. See [Installing](installing.md).
