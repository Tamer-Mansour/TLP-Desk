# Relay server (another network)

TLP-Desk works inside one network. To connect to a device at **another** home or office,
without a VPN and without opening a port on the other side's router, run your own small
**relay server** and give both devices its address. The relay only passes encrypted data: it
cannot see a session, join it or pretend to be a device, and who may connect is still decided
by the device you connect to (Accept, one-time code or password), exactly as on the LAN.

You need a machine both devices can reach on the internet: the cheapest VPS plan of any
provider (one CPU, 1 GB of memory and a few terabytes of traffic are plenty) or a PC at your
home with the ports below forwarded on your router.

## 1. Set up the relay (once, on the server)

On a Linux server (Ubuntu or Debian, as root):

```bash
curl -fsSL https://github.com/Tamer-Mansour/TLP-Desk/releases/latest/download/install-relay.sh | sudo bash -s -- --token 'choose-a-long-secret'
```

The script downloads `tlp-desk-relay`, installs it as a service that starts with the server,
and prints the firewall rules to open:

| Port | Protocol | Purpose |
|---|---|---|
| 47402 | UDP | Control (devices register and ask for each other) |
| 47410–47509 | UDP | Sessions, one port per live session (100 at a time) |

Open them in the provider's firewall too (most VPS panels have one). `journalctl -u
tlp-desk-relay -f` shows registrations and sessions.

To run it by hand instead: `tlp-desk-relay --token 'choose-a-long-secret'` (the token can also
come from the `TLP_RELAY_TOKEN` environment variable; `--listen` and `--ports` change the
ports).

**The token** is the only thing that stops strangers from using your relay: make it long
(a sentence), and give it only to the people whose devices should use the relay.

## 2. On the device you want to reach (the other home)

Settings → Security → **Relay server**:

1. **Relay address**: the server's address, e.g. `203.0.113.5` or `relay.example.com`
   (add `:port` only if you changed the control port).
2. **Relay token**: the same secret. Click **Apply**.
3. Turn on **Let devices reach this PC through the relay**.

The device now registers with the relay every 15 seconds (a few bytes). Nothing else changes:
it still asks for Accept or a code, the allow-list still applies to everyone except the relay,
and it still works on its own LAN as before.

## 3. On your device

The same Settings → Security → **Relay server**: address and token, **Apply**. The switch is
not needed unless others should reach *this* PC too.

Then connect exactly as on the LAN: type the other device's **ID** (not its address) and the
code, or leave the code empty and let them click Accept. TLP-Desk looks on your network
first; when the ID is not there it asks the relay, and the session goes through it. Save the
device in the address book like any other.

If you see *"Not found on this network or through the relay"*, the other device is off, has
no internet, or has not turned on the switch in step 2. *"The relay could not be used"*
names the reason (wrong address, wrong token, firewall).

## What the relay sees

Your public addresses, the device IDs that register, and the size and timing of encrypted
packets. Not the picture, the keystrokes, the files, the clipboard, the codes or the
passwords: those are inside the TLS 1.3 session between the two devices, and the relay cannot
complete a session itself because the connecting side pins the device's key.

Through a relay, both devices see the relay's address as the peer address (connection log,
rate limiting), and Wake-on-LAN does not work (it needs the same LAN).

## From the command line

```text
TLP-Desk-cli serve --relay relay.example.com --relay-token 'secret'      (remote PC)
TLP-Desk-cli connect 715204386 --relay relay.example.com --relay-token 'secret' --code 123456
```

`TLP_RELAY_TOKEN` can replace `--relay-token`.
