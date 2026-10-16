# Unattended access

Connect to your own PCs when nobody is sitting at them: a **permanent password** instead of the
one-time code, and **restart the remote PC** with an automatic reconnect once it is back.

## The permanent password

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

## Coming with the Windows service

These parts are built separately and are not available yet:

- TLP-Desk running as a **Windows service**, so it is reachable from the moment Windows starts,
  before anyone signs in, and the reconnect after a restart works at the sign-in screen;
- control of the **sign-in screen, lock screen and UAC prompts**;
- the **Send Ctrl+Alt+Del** button in the System menu (it shows a note that the service is needed).

## Command line

`TLP-Desk-cli set-password` sets or removes the permanent password, and `TLP-Desk-cli serve`
accepts it. See [Command line](command-line.md).
