# First run and the firewall

## What happens the first time

When TLP-Desk starts for the first time it:

1. **Creates this PC's identity** — a private key stored in `%ProgramData%\TLP-Desk`. Your 9-digit
   device ID is calculated from it, so the ID stays the same every time you start TLP-Desk.
2. **Picks the language** from your Windows display language: Arabic if Windows is in Arabic,
   English otherwise. You can change it at any time (see [Settings](settings.md)).
3. **Starts listening** on your local network (UDP port 47400), so other PCs can connect to you,
   and **announces itself** so it appears in other PCs' device lists.
4. Shows the **home screen**.

TLP-Desk has to be **running** on a PC for others to reach it. Closing the window stops it.

## The home screen

```text
┌───────────────────────────────────────────────────────────────────────┐
│ ● TLP-Desk                                   [English ▾]  [Settings] │
├──────────────────────────────┬────────────────────────────────────────┤
│ This device                  │ Connect to a device                    │
│                              │ [ ID, name or IP address        ]      │
│ Device ID                    │ [ One-time code (optional)      ]      │
│ 482 913 057          [Copy]  │ Leave it empty to ask the other side   │
│                              │ to accept.                [Connect >]  │
│ One-time code                │                                        │
│ 042 917     [Copy] [New code]│ Devices on this network                │
│ Share it only with someone   │ ● Office-PC                 [Connect]  │
│ you trust.                   │   123 456 789 · 192.168.1.40 · Available│
│                              │                                        │
│ Name        DESKTOP-TLP01    │                                        │
│ Local address 192.168.1.23 [Copy]                                     │
└──────────────────────────────┴────────────────────────────────────────┘
```

In Arabic the whole layout is mirrored: "This device" is on the right.

- **This device** (left): what other people need to reach *you* — your device ID, your one-time
  code, your PC's name and its local IP address(es). See
  [Your device ID and one-time code](device-id-and-code.md).
- **Connect to a device** (right): where you type what you want to reach.
- **Devices on this network**: TLP-Desk PCs found automatically. See
  [Finding devices](finding-devices.md).

## The firewall

TLP-Desk uses three UDP ports, only on your local network:

| Port | Used for |
|---|---|
| 47400 | The encrypted connection itself (QUIC / TLS 1.3) |
| 47401 | Finding devices by broadcast (fallback) |
| 5353 | Finding devices by mDNS |

**Installed version:** the installer already added rules for private and domain networks. Nothing
to do — as long as your network is set to **Private** (see [Installing](installing.md#set-your-network-to-private)).

**Portable version:** the first time, Windows asks whether TLP-Desk may communicate. Tick
**Private networks**, untick **Public networks**, click **Allow**.

### If you clicked Cancel by mistake

1. Open **Windows Security → Firewall & network protection → Allow an app through firewall**.
2. Click **Change settings**, find **TLP-Desk** in the list (or **Allow another app…** and browse to
   `TLP-Desk.exe`).
3. Tick the **Private** column for it and click **OK**.

### Other security software

If you use a third-party firewall or antivirus suite, allow `TLP-Desk.exe` on UDP 47400, 47401 and
5353 for your local network.

## "Port 47400 is already in use"

If a red banner says the port is already in use, another copy of TLP-Desk is probably running (for
example a second window, or the command-line tool). This copy can still **connect to** other PCs,
but cannot **be reached**. Close the other copy and restart TLP-Desk.
