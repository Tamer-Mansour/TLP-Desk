# TLP-Desk

TLP-Desk is a free, open-source remote desktop application for Windows. It lets you see and
control another PC on the **same local network** — the same office switch, the same home router,
the same Wi-Fi — without any cloud service, relay server, account or telemetry. It keeps working
with the internet cable unplugged.

One program does both jobs: the PC you sit at is the **viewer**, the PC you control is the
**host**. Every install of TLP-Desk can be either, at any time.

> **العربية:** هذا الدليل متوفّر بالعربية أيضًا — انظر قسم «دليل المستخدم (العربية)» في القائمة.

## What it does today

- Shows each PC a 9-digit **device ID** and a 6-digit **one-time code**.
- Finds other TLP-Desk PCs on the LAN automatically, or connects to an IP address you type.
- Lets the person at the host either click **Accept**, or lets the viewer type the one-time code.
- Streams the host screen as H.264 video at up to 60 frames per second and sends your mouse and
  keyboard back.
- Encrypts every connection (QUIC with TLS 1.3) and checks the host's identity, so a different
  machine cannot pretend to be the PC you meant to reach.
- Speaks Arabic and English, with a full right-to-left layout in Arabic.

Many more features (file transfer, clipboard, audio, unattended access, and others) are on the
[roadmap](https://github.com/Tamer-Mansour/TLP-Desk/blob/TLP-Desk/ROADMAP.md). Pages for those
features are marked **Coming soon** in this book.

## How this book is organised

| Part | For whom | What you will find |
|---|---|---|
| User guide (English / العربية) | Everyone who installs and uses TLP-Desk | Installing, connecting, controlling, settings, troubleshooting |
| Security | Users and reviewers | What TLP-Desk protects against, and what it does not |
| Contributor guide | Developers | Architecture, one page per crate, building, testing, releasing |
| Architecture decisions | Developers and reviewers | Why things are the way they are |
| Reference | Everyone | Wire protocol, glossary, changelog |

## Status

TLP-Desk is young software. The first milestone (see and control another PC on the LAN) is
implemented and is being tested on real hardware; the rest of the roadmap is in progress. If
something in this book does not match what you see in the app, the app wins — please
[open an issue](https://github.com/Tamer-Mansour/TLP-Desk/issues) so the book can be fixed.
