# Finding devices on your network

The **Devices on this network** card lists every PC on your local network that is running
TLP-Desk. Nothing needs to be configured: devices announce themselves and find each other
automatically.

While the first search is running you see **Searching for devices…**. Each device found then
appears as a row:

```text
● Office-PC                                         [Connect]
  123 456 789 · 192.168.1.40 · Available
```

- **Name** — the name that PC announces (usually its Windows computer name).
- **Device ID** · **IP address** · status.
- **Connect** — connects to that device by its ID (the safest way, see
  [Connecting](connecting.md)).

The list refreshes every few seconds. A PC disappears from the list a short time after TLP-Desk is
closed on it.

## How discovery works

TLP-Desk uses two methods at the same time, both limited to your local network:

1. **mDNS** (multicast DNS, the same technique printers and smart speakers use).
2. **UDP broadcast** on port 47401, as a fallback when mDNS is blocked.

Only devices with local addresses are listed; anything else is ignored.

## A PC is missing from the list

You can always connect without the list by typing the other PC's **IP address** (shown under
**Local address** on its home screen). If a device does not appear:

- Make sure TLP-Desk is **open** on that PC.
- Make sure both PCs are on the **same network** (same router or Wi-Fi). Guest Wi-Fi networks
  often isolate devices from each other.
- Make sure the network is set to **Private** on both PCs.
- Check the firewall (see [First run and the firewall](first-run.md#the-firewall)).
- Wait 15–20 seconds and look again.

More causes and fixes: [Troubleshooting](troubleshooting.md#the-other-pc-does-not-appear).
