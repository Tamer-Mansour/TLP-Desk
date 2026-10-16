# Troubleshooting

Start with the quick checks; then find your symptom below.

## Quick checks

1. TLP-Desk is **open on both PCs** (a closed TLP-Desk cannot be reached).
2. Both PCs are on the **same local network** — same router, switch or Wi-Fi name.
3. The network is set to **Private** on both PCs (Settings → Network & internet → your network →
   Network profile type → Private network).
4. Both PCs run the **same TLP-Desk version**.

## The other PC does not appear

The device list relies on network discovery, which some networks block. You can always connect
by typing the other PC's **IP address** (shown under **Local address** on its home screen). If
connecting by IP works, only discovery is blocked — that is fine for everyday use.

Common causes:

| Cause | Fix |
|---|---|
| Network set to **Public** | Set it to Private (see Quick checks). |
| **Firewall** blocks discovery (UDP 5353 and 47401) | Installed version: reinstall to restore the rules. Portable: allow TLP-Desk for Private networks (see [First run and the firewall](first-run.md#if-you-clicked-cancel-by-mistake)). |
| **Guest Wi-Fi** or **AP / client isolation** | Many routers and mesh systems keep wireless devices from seeing each other on guest networks or when "AP isolation", "client isolation" or "wireless isolation" is on. Use the main (non-guest) network, or turn isolation off in the router's settings. Connecting by IP will not work either while isolation is on. |
| One PC on **Wi-Fi**, the other on **cable**, through different routers | They must be on the same network (same IP range, e.g. both `192.168.1.x`). |
| A **VPN** is connected | Many VPN clients route all traffic through the VPN or block the local network. Disconnect the VPN, or enable its "allow local network access / LAN access" option. |
| **Virtual network adapters** (Hyper-V, VirtualBox, VMware, WSL, Docker) | Discovery and the "Local address" list may include addresses of virtual networks that the other PC cannot reach. Use the address of your real Ethernet or Wi-Fi adapter (`ipconfig` shows which). |
| Waited only a few seconds | Devices announce themselves every few seconds; give it 15–20 seconds. |

## "The device cannot be reached"

The address answered nothing. Check:

- the address is right (copy it from the other PC's home screen);
- TLP-Desk is running there and its home screen does **not** show a red "port is already in use"
  banner;
- the firewall on the **other** PC allows TLP-Desk (UDP 47400) on Private networks;
- no VPN or isolation issue (see the table above);
- you can reach the other PC at all: `ping 192.168.1.40` in a command prompt (some PCs do not
  answer ping even when everything is fine, so this is only a hint).

## "Port 47400 is already in use"

Another program — almost always a second copy of TLP-Desk, or `TLP-Desk-cli serve` — is using the
port. This copy can still connect **out**, but nobody can connect **to** it. Close the other copy
(look for a second TLP-Desk window, or end `TLP-Desk.exe` / `TLP-Desk-cli.exe` in Task Manager) and
restart TLP-Desk.

If some other application really needs UDP 47400, start TLP-Desk with `--port <other number>` and
connect to it as `address:port`.

## Windows says "Windows protected your PC"

That is SmartScreen. The TLP-Desk installer is not signed with a paid certificate yet, so Windows
does not recognise it. Click **More info**, check the name, then **Run anyway**. If you want to be
sure the file is genuine, compare its SHA-256 hash with the release's `SHA256SUMS.txt` (see
[Installing](installing.md#check-the-download-optional)).

## The connection was declined or timed out

- The person at the other PC must click **Accept** within 60 seconds.
- Or type the other PC's **one-time code** to connect without a prompt — it changes after every
  use, so ask for the current one.
- "Too many attempts" means your PC is blocked for 10 minutes after repeated wrong codes or
  requests.

## The picture is slow or jerky

- Prefer a **cable** for at least one of the PCs; Wi-Fi far from the router is the most common
  cause.
- Close large downloads or video streaming on the same network.
- Check the [statistics overlay](stats.md): low latency with a lower frame rate means TLP-Desk is
  skipping pictures to keep the delay short — the network or one of the PCs is busy.
- A very slow or old PC on either side may not manage 60 pictures per second.

## Keys or mouse buttons seem stuck on the remote PC

Click into the TLP-Desk window and press and release the stuck key. TLP-Desk releases everything
automatically when you leave its window, switch monitors or turn on View only.

## The Windows key or Alt+Tab acts on my own PC

System keys go to the remote PC only while the TLP-Desk window is active, View only is off, and the
toolbar shows the highlighted keyboard icon (**Keyboard goes to the remote PC**). Click into the
remote picture first.
Ctrl+Alt+Del and Win+L always act on your own PC.

## I cannot click in some windows on the remote PC

Windows that run **as administrator**, **UAC** prompts, the lock screen and the sign-in screen are
protected by Windows from ordinary programs. Controlling them will need the
[unattended access service](unattended-access.md). Until then, ask the person at the
remote PC to handle those prompts.

## Typing produces the wrong characters

The **remote** PC's keyboard layout decides the characters. Switch the input language on the
remote PC (for example Alt+Shift while connected), not on your own.

## Still stuck?

Open an issue on [GitHub](https://github.com/Tamer-Mansour/TLP-Desk/issues) with: both Windows
versions, the TLP-Desk version, how the PCs are connected (cable/Wi-Fi), and the exact message.
