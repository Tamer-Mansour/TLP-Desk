# Wake-on-LAN

![Magic packet sent](../../media/wake-on-lan-en-light.png)

TLP-Desk can switch on a sleeping or powered-off PC from the [address book](address-book.md),
using the standard Wake-on-LAN "magic packet" on your local network.

## Using it

When a saved device is offline, its row shows **Wake up** instead of **Connect**. Click it; the
message **Magic packet sent to …** confirms the packet left this PC. Give the PC a minute to
start, then connect when its dot turns green.

**Wake up** needs the PC's MAC address:

- TLP-Desk learns it automatically while the PC is on: when you connect to it or see it in
  **Devices on this network**, the address is read from Windows' own neighbour (ARP) cache.
  Nothing extra is sent on the network for this.
- Or type it yourself in the edit dialog (`00:1a:2b:3c:4d:5e`, dashes also work). A MAC you
  typed is never replaced by a learned one.

The packet is sent as a broadcast to the subnet where the PC was last seen (or to every network
of this PC if that subnet is unknown). It never leaves your local network.

## Requirements on the PC to wake

- Wake-on-LAN enabled in the BIOS/UEFI.
- In Device Manager, on the network adapter: **Power Management** → *Allow this device to wake
  the computer*, and on **Advanced** → *Wake on Magic Packet* enabled.
- Wired Ethernet is the reliable choice; most Wi-Fi adapters cannot be woken.
- The PC must be on the same local network (broadcasts do not cross routers).
