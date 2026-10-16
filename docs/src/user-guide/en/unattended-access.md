# Unattended access

Connect to your own PCs when nobody is sitting at them: a **permanent password** instead of the
one-time code, and **restart the remote PC** with an automatic reconnect once it is back.

## The permanent password

![The Unattended access card in Settings](../../media/settings-unattended-en-light.png)

On the PC you want to reach:

1. Open **Settings > Unattended access**.
2. Type a password twice and click **Set password**. A **strength** meter shows how hard it is to
   guess; any characters work, including Arabic letters. Use the eye button to check what you typed.
3. To change it, type the **current password** first, then the new one. To turn password sign-in
   off, use **Remove password** (the current password is needed too).

The password is stored only as a scrambled (Argon2id) check value in TLP-Desk's data folder,
protected so that only administrators and the system can read it. It is **never sent over the
network**: both sides only prove to each other that they know it. If TLP-Desk is not running as
administrator it can only protect the file for your own Windows account, and Settings says so.

On the other PC, click **Connect** and choose **Password** next to the one-time code. The field is
masked (use the eye button to show it) and the password is never written to the log.

### Wrong passwords

A peer may try **5 times per minute**; after that it is blocked for **10 minutes**. The viewer
shows how long ("Too many wrong attempts. You are locked out for N minutes.") and every attempt is
written to the host's [connection log](connection-log.md).

## Restart the remote PC

During a session open the **System** menu in the toolbar and choose **Restart remote PC**. You
are asked to confirm, because programs on that PC are closed and unsaved work is lost.

- It needs the **Restart this device** permission, which the person at the host can withdraw at any
  time. Without it the menu entry does nothing.
- Just before it restarts, the host gives your viewer a one-use **reconnect ticket**. The session
  switches to **Restarting… waiting for the remote PC** and tries again by device ID, with growing
  pauses, for up to about 5 minutes. When the PC is back you are signed in again and the picture
  returns by itself. **Stop waiting** ends the wait.
- The ticket is a 256-bit random secret that only your TLP-Desk (identified by its own key) can use,
  once, within 10 minutes. The host keeps only a hash of it, and it is cancelled if the permissions
  change, if you disconnect, or if the restart fails.

**Before anyone signs in**, TLP-Desk is not running on a normal Windows PC, so the reconnect only
works once someone has signed in, unless the TLP-Desk service is installed (see below).

## Lock the remote PC

**System > Lock remote PC** locks the remote Windows session. It needs the **Ctrl+Alt+Del and
locking** permission.

## The Windows service

Without the service, TLP-Desk only runs while someone is signed in to Windows. Install the
**TLP-Desk service** and the PC can be reached from the moment Windows starts, at the **sign-in
screen**, the **lock screen** and over **UAC prompts** (the dimmed "Do you want to allow this app
to make changes?" screen).

### Install it

- In the setup program tick **Install as a Windows service (unattended access)**; or
- run `TLP-Desk.exe install-service` from an **administrator** prompt. `TLP-Desk.exe
  uninstall-service` removes it again (uninstalling TLP-Desk does this too).

The service is called **TLP-Desk**, starts automatically with Windows and runs as the system
account. It owns the connection port (UDP 47400), so the TLP-Desk window on that PC does not
listen itself any more: it shows the service's device ID and one-time code, the **Accept**
prompts and the "who is connected" banner, and says "Unattended access is on". Closing the
window does not stop the service. If the service stops or is removed, TLP-Desk goes back to
hosting by itself on the next start.

Installing also allows services to send Ctrl+Alt+Del: it sets the
`SoftwareSASGeneration` policy (registry value `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System`)
to *services*. A domain Group Policy can override it. Nothing else about Windows' security is
changed.

### What it does

- A small helper (the **session agent**) is started in the session at the PC's screen and
  follows it when someone signs in or out, locks or unlocks, or switches user. If it ever
  stops, it is restarted automatically with growing pauses.
- On the lock screen, sign-in screen or a UAC prompt the helper switches to that screen, so you
  keep seeing and controlling it. The viewer shows a blue banner: "The remote computer is locked"
  or "shows a secure screen".
- The **Send Ctrl+Alt+Del** entry in the System menu works (see
  [Ctrl+Alt+Del and lock](coming-soon/ctrl-alt-del.md)). Without the service it is still shown, with
  a note that the service is needed.
- The **reconnect after a restart** works at the sign-in screen too: the service is up before
  anyone signs in, so your viewer gets back in on its own.
- The permanent password, one-time code and Accept rules are the same as without the service.
  The service uses the machine-wide data folder `%ProgramData%\TLP-Desk\`. Its log is
  `%ProgramData%\TLP-Desk\logs\service.log` (a technical log in English, for support; notices you
  see on screen are in your language, and nothing is written to the Windows Event Log).

A viewer that has the permanent password or a valid reconnect ticket connects without an
**Accept** click, so choose a strong password.

## Command line

`TLP-Desk-cli set-password` sets or removes the permanent password, and `TLP-Desk-cli serve`
accepts it. See [Command line](command-line.md).
