# Manual test checklist (two PCs)

Everything the automated tests and the [system test report](../../dev/system-test-report.md)
cannot prove on one PC: real networks, a second machine, the sign-in screen, sound, the
clipboard and power. Run it before every release. Tick each box only after you saw the
**expected** result, and write down the TLP-Desk version (`TLP-Desk-cli.exe --version`), both
Windows versions and how the PCs are connected.

**Setup.** Two Windows 10/11 x64 PCs on the same switch or Wi-Fi (internet optional).
**PC A** is the *host* (is controlled), **PC B** is the *viewer*. Install the same
`TLP-Desk-<version>-setup.exe` on both (or unzip the portable zip). If you can, give PC A a
second monitor for the monitor tests. Use a test Windows account on PC A and keep no unsaved
work open there.

If an item fails, write the date and your initials next to it and open an issue.

## 1. Install and first run

- [ ] Setup shows in your Windows language (Arabic or English), installs without errors and adds a Start-menu entry named **TLP-Desk**. Expected: the app starts.
- [ ] The Windows Firewall rules added by TLP-Desk exist for **private and domain** networks only (Windows Security > Firewall > Advanced settings > Inbound rules). Expected: present, none for the public profile.
- [ ] The window title reads **TLP-Desk** (no garbled characters) and the home screen shows a 9-digit ID, a 6-digit one-time code and the PC's IP address.
- [ ] Close and reopen the app. Expected: the **same** device ID.
- [ ] Right-click `TLP-Desk-<version>-setup.exe` > Properties > Details. Expected: product name TLP-Desk and the right version.

## 2. Discovery and connecting

- [ ] On PC B open **Devices on this network**. Expected: PC A appears within a few seconds with its name (also when the name contains Arabic letters).
- [ ] Connect to PC A by clicking its entry. Expected: PC A shows the **Accept** prompt with the viewer's name and address; click **Accept**; PC B shows PC A's desktop.
- [ ] PC A shows the always-visible banner "PC B is connected", also while its main window is minimized.
- [ ] Disconnect, then connect again by typing PC A's **9-digit ID**. Expected: connects.
- [ ] Connect by typing PC A's **IP address**. Expected: connects; the first time it tells you the key is new (trust on first use).
- [ ] Connect with the **one-time code** (no Accept needed). Expected: connects; the code on PC A has changed afterwards.
- [ ] Enter a **wrong code** six times within a minute. Expected: refused each time, then a lock-out message with the minutes left; PC A's connection log shows every attempt.
- [ ] Keep the router's internet cable unplugged. Expected: everything above still works.

## 3. Screen, mouse and keyboard

- [ ] The remote picture is sharp and follows PC A. Open the statistics overlay. Expected: at least 30 fps on Wi-Fi, up to 60 fps on wired LAN while something moves; latency under 100 ms (Wi-Fi) or under 60 ms (wired).
- [ ] Mouse: move, left and right click, double-click, drag a window, scroll. Expected: exact positions on PC A.
- [ ] Keyboard: type a sentence in Notepad on PC A in **English**. Expected: exact text.
- [ ] Switch PC A's layout to **Arabic** (Alt+Shift) and type Arabic. Expected: Arabic letters appear; Ctrl+C, Ctrl+V and Alt+Tab work.
- [ ] **View only** on: mouse and keyboard no longer reach PC A; off again: they do.
- [ ] Touch PC A's own mouse while PC B moves the pointer. Expected: the local user wins for about a second.
- [ ] Fit, actual size and full screen work; full screen ends with the same button.
- [ ] The Windows key and Alt+Tab pressed on PC B while the session window is focused act on PC A.
- [ ] **Elevated window notice.** Open `regedit` as administrator on PC A while TLP-Desk on PC A runs without elevation, then type and click in it from PC B. Expected: Windows ignores the input (UIPI) and TLP-Desk shows its notice about elevated windows; with the service installed (section 12) it works.

## 4. Monitors, quality and presets

- [ ] With two monitors on PC A, switch the monitor in the toolbar. Expected: the other screen appears and the mouse maps correctly on it.
- [ ] The **All monitors** view shows both screens; clicks land on the right one.
- [ ] Presets Speed, Balanced and Quality visibly change picture quality and bitrate in the statistics overlay.
- [ ] Move PC B to a poor Wi-Fi spot (or throttle it). Expected: the picture stays current (frames are dropped), latency does not keep growing.
- [ ] Leave PC A's screen static for 30 seconds. Expected: bandwidth close to zero; moving the mouse resumes at once.

## 5. Files

- [ ] Open the file browser. Expected: two panes, drives and known folders; Arabic file names display correctly.
- [ ] Copy a 1 GB file from PC B to PC A and back. Expected: progress and speed (above 50 MB/s on gigabit); `Get-FileHash` of both copies is identical.
- [ ] Copy a folder with Arabic names. Expected: names unchanged on disk.
- [ ] Cancel a transfer halfway, then retry. Expected: it resumes and the final hash matches.
- [ ] New folder, rename and delete (with confirmation) in the remote pane. Expected: visible in Explorer on PC A.
- [ ] **Explorer paste:** copy files in Explorer on PC B and use the paste button in the session; then the other way round. Expected: the files appear in the folder open on the destination PC.
- [ ] Drag files from Explorer on PC B onto the session window. Expected: they are sent.
- [ ] Revoke **Files** on PC A's banner during a transfer. Expected: the transfer stops and the panes show "no permission".

## 6. Clipboard

- [ ] Copy English text on PC A and paste on PC B, then the other way. Expected: identical.
- [ ] Copy **Arabic text** with diacritics mixed with English. Expected: identical, including direction.
- [ ] Copy an image (a screenshot) on one PC and paste into Paint on the other. Expected: the same picture.
- [ ] Copy formatted text from Word or a web page. Expected: formatting arrives in WordPad or Word.
- [ ] Turn **Sync clipboard** off in the toolbar. Expected: nothing is shared any more.

## 7. Chat, whiteboard, recording

- [ ] Send chat messages from both sides in Arabic and English. Expected: correct shaping and RTL bubbles; an unread badge shows while the panel is closed.
- [ ] Whiteboard: draw with pen, highlighter, arrow and rectangle, undo, clear. Expected: strokes show on PC A in a click-through overlay, no mouse input reaches PC A; the overlay disappears when you leave the whiteboard or disconnect.
- [ ] Start a recording, work for a minute, stop. Expected: an `.mp4` in `%USERPROFILE%\Videos\TLP-Desk` that plays in **Recordings** and in a normal video player.
- [ ] End the app with Task Manager during a recording, restart it and use **Repair**. Expected: the recording plays.

## 8. Sound

- [ ] Play music or a video on PC A and turn **Sound** on at PC B. Expected: no crackling, delay under about 150 ms; the volume slider works; sound off stops it.
- [ ] Revoke **Audio** on PC A. Expected: sound stops at once.

## 9. Port forwarding

- [ ] Start a web server on PC A (for example `python -m http.server 8000`). On PC B open **Port forwarding** and add local 18000 to 127.0.0.1:8000 (PC A must have allowed port forwarding on its banner). Expected: `http://127.0.0.1:18000` in PC B's browser shows PC A's page.
- [ ] Revoke **Port forwarding** on PC A. Expected: the tunnel closes at once.
- [ ] A rule that targets a public internet address is refused.

## 10. Device info, address book, Wake-on-LAN

- [ ] **Device info** shows PC A's correct name, Windows version, CPU, RAM, disks and uptime.
- [ ] Save PC A in the **Address book** with an alias and a group. Expected: it survives an app restart and shows an online dot while PC A is on.
- [ ] **Wake-on-LAN:** enable "Wake on magic packet" in PC A's BIOS and network adapter, shut PC A down, then click **Wake up** on its saved entry on PC B. Expected: PC A starts within a minute and its dot turns green. Repeat with `TLP-Desk-cli.exe wol <MAC>`.

## 11. Permanent password, restart, lock

- [ ] On PC A set a password in Settings > Unattended access. On PC B connect with **Password**. Expected: connects without an Accept prompt; a wrong password is refused and 5 wrong tries lock that PC out for 10 minutes.
- [ ] **System > Lock remote PC**. Expected: PC A locks.
- [ ] **System > Restart remote PC** (confirm). Expected: PC A restarts, PC B shows "Restarting... waiting for the remote PC", and the session returns by itself once PC A is back (without the service: after someone signs in).
- [ ] Revoke **Restart this device** on PC A. Expected: the menu entry no longer restarts it.

## 12. Windows service, lock screen, UAC, Ctrl+Alt+Del

- [ ] On PC A, in an **administrator** prompt, run `TLP-Desk.exe install-service` (or tick it in setup). Expected: service **TLP-Desk** is running and set to start automatically.
- [ ] Sign out of PC A. From PC B connect with the permanent password. Expected: you see and control the **sign-in screen**.
- [ ] Lock PC A and connect from PC B. Expected: you see the lock screen and can type the PIN or password.
- [ ] Trigger a UAC prompt on PC A. Expected: PC B sees it (secure desktop) and can click Yes.
- [ ] **System > Send Ctrl+Alt+Del**. Expected: PC A shows the Windows security screen, also at the sign-in screen.
- [ ] Restart PC A. Expected: PC B reconnects even before anyone signs in.
- [ ] Run `TLP-Desk.exe uninstall-service`. Expected: the service is gone.

## 13. Privacy mode

- [ ] Allow privacy mode on PC A's banner. On PC B turn **Privacy mode** on. Expected: PC A's screens go black with the explanatory text, PC A's keyboard and mouse are blocked, PC B still sees and controls the real desktop.
- [ ] On PC A hold **Ctrl+Alt+Shift+P** for 3 seconds. Expected: privacy mode ends. Turn it on again and end it with **Ctrl+Alt+Del**.
- [ ] Turn it on, then disconnect PC B. Expected: PC A's screen returns at once. Also try pulling PC B's network cable, and locking PC A.

## 14. Settings, tray, connection log

- [ ] Change the language (Arabic and English) live. Expected: the layout mirrors correctly; the digits setting works.
- [ ] Change the default permissions, the Accept timeout and the allowed networks, then connect from an address outside the allowed range. Expected: refused.
- [ ] Close-to-tray and start-minimized: the tray menu (Open, Copy ID, New code, Pause incoming, Settings, Connection log, Quit) works; an incoming request brings the window forward.
- [ ] Start with Windows: reboot and check the app starts (minimized if set).
- [ ] The **connection log** lists every session and refused attempt with time, address, duration and permissions; the CSV export opens in Excel.

## 15. Command line

- [ ] On PC A run `TLP-Desk-cli.exe serve --code --allow-files`. On PC B run `list`, `connect <ip> --code <code> --seconds 10`, `send-file`, `get-file`, `ls`, `info`, `chat` and `forward`. Expected: every command works across the network; `--lang ar` prints Arabic.

## 16. Uninstall

- [ ] Uninstall from Windows Settings. Expected: the app, the firewall rules and the service are removed; `%ProgramData%\TLP-Desk` (the device ID) stays on purpose; reinstalling gives the same ID.

## Sign-off

Version: ______  PC A: ______  PC B: ______  Network: ______  Date: ______  Tester: ______
