# Uninstalling

## Installed version

1. Close TLP-Desk.
2. Open **Settings → Apps → Installed apps** (Windows 11) or **Settings → Apps → Apps & features**
   (Windows 10).
3. Find **TLP-Desk**, click **⋯** (or the entry) and choose **Uninstall**.

The uninstaller removes the program, its Start-menu and desktop shortcuts, and the three Windows
Firewall rules (`TLP-Desk (QUIC)`, `TLP-Desk (discovery)`, `TLP-Desk (mDNS)`).

## Portable version

Close TLP-Desk and delete the folder you extracted. If Windows Firewall created a rule when you
allowed it, remove it in **Windows Security → Firewall & network protection → Allow an app through
firewall**.

## What is kept, and how to remove it

On purpose, uninstalling **keeps** two folders, so that if you install TLP-Desk again your PC keeps
the same device ID and other PCs keep trusting it:

| Folder | Contains |
|---|---|
| `%ProgramData%\TLP-Desk` | This PC's identity key (its device ID) and the keys of PCs it connected to |
| `%APPDATA%\TLP-Desk` | Your settings |

To remove TLP-Desk **completely**, delete both folders after uninstalling (paste each path into
the File Explorer address bar). Deleting `%ProgramData%\TLP-Desk` needs administrator rights. If
the portable version could not use ProgramData, also check `%LOCALAPPDATA%\TLP-Desk`.

After a complete removal, a new installation gets a **new device ID**, and PCs that connected to
this one before will show the [identity changed warning](host-key-changed.md) once.
