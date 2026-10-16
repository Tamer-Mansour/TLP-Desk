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
2. **If Windows shows "Windows protected your PC"** (Microsoft Defender SmartScreen): this appears
   because the installer is not yet signed with a paid code-signing certificate, not because
   anything is wrong with it. Click **More info**, check that the file name is the TLP-Desk
   installer, then click **Run anyway**.
3. Approve the administrator prompt (User Account Control).
4. Choose the installer language — **English** or **العربية**. It starts in your Windows language.
5. Follow the wizard. You can tick **Create a desktop shortcut** if you want one (it is off by
   default).
6. The installer copies TLP-Desk to `C:\Program Files\TLP-Desk` and adds Windows Firewall rules so
   other PCs on your network can reach it — on **private and domain networks only**, never on
   public networks such as café Wi-Fi.
7. Leave **Launch TLP-Desk** ticked and click **Finish**.

Start TLP-Desk later from the **Start menu**.

Installing a newer version over an older one keeps your device ID and settings.

## Using the portable version

1. Right-click the zip file → **Extract All…**, and extract it anywhere (Desktop, a USB stick).
2. Open the folder and double-click **TLP-Desk.exe**.
3. If SmartScreen appears, click **More info → Run anyway** as above.
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
