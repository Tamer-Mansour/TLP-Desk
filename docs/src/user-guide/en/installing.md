# Installing

TLP-Desk comes in two forms. Install it the same way on **every** PC you want to use.

| | Installer (`TLP-Desk-<version>-setup.exe`) | Portable (`TLP-Desk-<version>-win-x64-portable.zip`) |
|---|---|---|
| Best for | PCs you use regularly | Trying it out, a USB stick, PCs where you cannot install |
| Needs administrator rights | Yes | No |
| Windows Firewall | Configured automatically | Windows asks the first time |
| Start-menu entry | Yes | No |

Both need Windows 10 or 11, 64-bit. On Windows 10, use version 2004 or newer: older versions
install and run, but the green remote-pointer marker may then show up in the viewer's picture.

## Check the download (optional)

Each release lists a `SHA256SUMS.txt` file. In PowerShell:

```powershell
Get-FileHash .\TLP-Desk-1.0.0-rc.1-setup.exe -Algorithm SHA256
```

The long number must match the line for that file in `SHA256SUMS.txt`.

## Using the installer

1. Double-click `TLP-Desk-<version>-setup.exe`.
2. **If Windows shows "Windows protected your PC"** (Microsoft Defender SmartScreen): click
   **More info**, check that the file name is the TLP-Desk installer, then click **Run anyway**.
   See [Why Windows warns](#why-windows-warns-and-how-to-install-anyway) below.
3. Approve the administrator prompt (User Account Control). It may say **Unknown publisher**, for
   the same reason.
4. Choose the installer language — **English** or **العربية**. It starts in your Windows language.
5. Follow the wizard. You can tick **Create a desktop shortcut** if you want one (it is off by
   default).
6. The installer copies TLP-Desk to `C:\Program Files\TLP-Desk` and adds Windows Firewall rules so
   other PCs on your network can reach it — on **private and domain networks only**, never on
   public networks such as café Wi-Fi.
7. Leave **Launch TLP-Desk** ticked and click **Finish**.

Start TLP-Desk later from the **Start menu**.

Installing a newer version over an older one keeps your device ID and settings.

## Why Windows warns and how to install anyway

When you open a file downloaded from the internet, Microsoft Defender SmartScreen checks whether the
file comes from a **known publisher** with a good reputation. The TLP-Desk files are not yet signed
with a code-signing certificate, so Windows cannot name a publisher and shows:

> **Windows protected your PC** — Microsoft Defender SmartScreen prevented an unrecognized app from
> starting. *Publisher: Unknown publisher*

This is a reputation check, not a virus finding: Microsoft Defender Antivirus scans TLP-Desk like any
other file and finds nothing. To install anyway:

1. Make sure the file came from the official
   [TLP-Desk releases page](https://github.com/Tamer-Mansour/TLP-Desk/releases), and, if you like,
   [check the download](#check-the-download-optional) against `SHA256SUMS.txt`.
2. In the blue window click **More info**.
3. Check that **App** shows the TLP-Desk file name (`TLP-Desk-<version>-setup.exe` or
   `TLP-Desk.exe`).
4. Click **Run anyway**.

Windows remembers this for that file; you only do it once per download. If your PC has **Smart
App Control** turned on (Windows 11), it may block an unsigned app without offering **Run anyway**;
on such a PC, wait for a signed release. Once releases are signed, the warning fades out as the
publisher builds reputation. Maintainers: see [Code signing](../../dev/code-signing.md).

## Install without the warning using winget

The Windows Package Manager (`winget`, built into Windows 10 and 11) downloads the installer itself,
checks it against the fingerprint published in the winget catalog, and starts it without the
SmartScreen window. Open **Terminal** or **PowerShell** and run:

```powershell
winget install --id TLP.TLP-Desk --exact
```

Accept the administrator prompt; the installer then runs silently with the default choices
(no desktop shortcut, no Windows service). To install the
[unattended-access service](unattended-access.md) at the same time:

```powershell
winget install --id TLP.TLP-Desk --exact --custom "/MERGETASKS=installservice"
```

Update later with `winget upgrade TLP.TLP-Desk`, remove with `winget uninstall TLP.TLP-Desk`
(your device ID and settings are kept, as with the normal uninstaller).

TLP-Desk becomes available in winget only after its package is accepted into the Windows Package
Manager catalog. Check with `winget search TLP-Desk`; if nothing is found yet, use the installer
above.

## Using the portable version

1. Right-click the zip file → **Extract All…**, and extract it anywhere (Desktop, a USB stick).
2. Open the folder and double-click **TLP-Desk.exe**.
3. If SmartScreen appears, click **More info → Run anyway**
   ([why](#why-windows-warns-and-how-to-install-anyway)).
4. Windows Firewall asks whether TLP-Desk may communicate on networks. Tick **Private networks**,
   leave **Public networks** unticked, and click **Allow**. See
   [First run and the firewall](first-run.md) if you clicked Cancel by mistake.

The portable version uses the same data folders as the installed one, so on the same Windows
account switching between them normally keeps your device ID. (If `%ProgramData%` cannot be written,
the portable version falls back to `%LOCALAPPDATA%\TLP-Desk`, which gives that account its own ID.)

## Set your network to Private

Windows treats unknown networks as **Public** and blocks most incoming connections there. On a
home or office network you trust, set it to Private on both PCs:

**Settings → Network & internet → (Wi-Fi or Ethernet) → your network → Network profile type →
Private network.**

## What is installed

| Item | Where |
|---|---|
| `TLP-Desk.exe` (the app) and `TLP-Desk-cli.exe` (command-line tool) | `C:\Program Files\TLP-Desk` |
| `README.txt` and font licences | same folder |
| Firewall rules `TLP-Desk (QUIC)`, `TLP-Desk (discovery)`, `TLP-Desk (mDNS)` | Windows Defender Firewall |
| Your settings (created on first run) | `%APPDATA%\TLP-Desk` |
| Your device's identity key (created on first run) | `%ProgramData%\TLP-Desk` |

Next: [First run and the firewall](first-run.md).
