# Unattended access

> **Coming soon.** This feature is planned but not available yet. It is part of **M3 — Unattended access** in the [roadmap](https://github.com/Tamer-Mansour/TLP-Desk/blob/TLP-Desk/ROADMAP.md).

## What it will do

Connect to your own PCs when nobody is sitting at them:

- TLP-Desk runs as a **Windows service**, so it is available from the moment Windows starts, even before anyone signs in;
- a **permanent password** you set on the host (stored only in a protected, scrambled form; never sent over the network);
- control of the **sign-in screen, lock screen and UAC prompts**;
- **restart the remote PC** and reconnect automatically after it boots.

Wrong passwords will be limited (5 attempts per minute, then a 10-minute block) and every attempt logged.

## Status

Password sign-in is already implemented in the connection layer; the service, the settings screen and secure-desktop support are not.

## Until then

Use the one-time code: read it from the host, or ask someone there to read it to you.
