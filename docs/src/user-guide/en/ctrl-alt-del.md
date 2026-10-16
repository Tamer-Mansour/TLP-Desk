# Ctrl+Alt+Del and lock

> **Available with the service.** Lock works everywhere; **Send Ctrl+Alt+Del** needs the TLP-Desk Windows service on the remote PC (part of **M3 — Unattended access** in the [roadmap](https://github.com/Tamer-Mansour/TLP-Desk/blob/TLP-Desk/ROADMAP.md)).

## What it does

The toolbar's **System** menu sends **Ctrl+Alt+Del** to the remote PC (pressing the keys yourself always acts on your own PC) and can **lock** it.

Needs the **System** permission.

## Status

**Lock remote PC** works from the System menu (see [Unattended access](unattended-access.md)).

**Send Ctrl+Alt+Del** is sent by the TLP-Desk service with the Windows `SendSAS` call, which is how it also works on the sign-in and lock screens. Once the host has told your viewer that the service runs, the entry loses its "Needs the TLP-Desk service on the remote PC" note. The service is limited to one request per second. Install it as described in [Unattended access](unattended-access.md#the-windows-service).

## Without the service

The entry stays in the menu, but the host ignores it. Ask the person at the remote PC, or use the remote PC's Start menu to lock or sign out.
