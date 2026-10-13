# Security and privacy

TLP-Desk lets another person see and control your computer, so it is built to be careful. In plain
words:

## Your data stays on your network

- TLP-Desk **never connects to the internet**: no account, no cloud, no relay server, no update
  checks, no statistics, no crash reports. It works with the internet unplugged.
- It accepts connections only from **local network addresses** (such as `192.168.x.x` or
  `10.x.x.x`), and refuses everything else.

## Everything is encrypted

Every session — the picture, your mouse and keyboard — is encrypted with **TLS 1.3**, the same
standard that protects banking websites. There is no way to switch encryption off.

## Nobody connects without permission

Someone can connect to your PC only if:

- you click **Accept** when they ask, or
- you gave them your current **one-time code**, which works once and then changes.

The one-time code is **never sent over the network** — not even encrypted. Both PCs only prove to
each other that they know it, so someone listening on the network cannot learn it. Guessing is
pointless: after a few wrong tries the code changes, and the guessing PC is blocked for 10 minutes.

## You know who is connected

- While anyone is connected, a **banner** at the top of TLP-Desk shows their name, with an **End
  session** button.
- A **green pointer with their name** shows when they move your mouse.
- **You always have priority**: if you touch your mouse or keyboard, their control pauses.

## You know who you reached

Each PC has a secret key and its **device ID is derived from that key**. When you connect by ID,
TLP-Desk checks that the PC really owns that ID — another device cannot pretend to be it. After
the first connection TLP-Desk also remembers the PC's key and [warns you](host-key-changed.md) if
it ever changes.

## What others on your network can see

Other devices on the same network can see that TLP-Desk is running on your PC, along with its
**device ID** and **name** (that is how the device list works). They cannot see your screen or what
you do. Choose a device name that does not reveal private information.

## What is stored on your PC

| What | Where |
|---|---|
| Your secret identity key and the keys of PCs you connected to | `%ProgramData%\TLP-Desk` (only administrators and you can read it) |
| Your settings | `%APPDATA%\TLP-Desk\settings.json` |

TLP-Desk keeps no history of your sessions and records nothing of what happens in them.

## Good habits

- Share your one-time code only with people you trust, and only when needed. Click **New code**
  after a session if you read it aloud in a room.
- Accept only requests you expect. The **ID** in the request is verified; the **name** is not.
- Connect by **device ID** when you can.
- Keep both PCs updated to the same TLP-Desk version.
- On a public network (café, hotel), do not use TLP-Desk; the installer does not open the firewall
  there anyway.

Technical details: [Security model](../../security.md).
