# Frequently asked questions

### Is TLP-Desk free?

Yes. It is free and open source (MIT or Apache-2.0 licence), with no paid edition, no account and
no advertising.

### Can I connect over the internet, from home to the office?

No — on purpose. TLP-Desk works only inside one local network and never uses a server on the
internet. That is what makes it private and independent of any company's servers. If you are
connected to your office network by a VPN that makes the office PCs reachable as local
addresses, it may work, but that is not a supported setup.

### Does it work without internet?

Yes. Unplug the internet line from your router: TLP-Desk keeps working between the PCs on your
network.

### Does the other person need to be at their PC?

Not necessarily. With their **one-time code** you can connect without anyone clicking Accept —
once. With a **permanent password** set on that PC you can connect any time TLP-Desk is running
(see [Unattended access](unattended-access.md)).

### Does my device ID ever change?

Only if the PC's TLP-Desk identity is deleted — for example when Windows is reinstalled. Updating,
uninstalling and reinstalling TLP-Desk keep it.

### Can someone watch my screen without me knowing?

Not without your permission (Accept or your one-time code), and while anyone is connected TLP-Desk
shows a banner with their name and an **End session** button.

### Can several people connect to my PC at the same time?

Yes. Each one needs permission, and each appears on its own line in the connection banner.

### Can I control a PC that is at the sign-in screen or locked?

Not yet; this needs the [unattended access service](unattended-access.md).

### Does it support Mac, Linux, Android?

Windows 10 and 11 (64-bit) only for now. Other systems are on the long-term roadmap.

### Why does Windows show a SmartScreen warning?

The installer is not yet signed with a commercial code-signing certificate. Click **More info → Run
anyway**, and compare the file's SHA-256 with `SHA256SUMS.txt` if you want to be sure.

### Does TLP-Desk support Arabic?

Fully: the whole interface is available in Arabic with a right-to-left layout, you can choose
Arabic-Indic digits, device names can be Arabic, and typing Arabic on the remote PC works through
the remote PC's keyboard layout.

### How much network bandwidth does it use?

Nothing while the screen is still; typically a few megabits per second while you work; up to about
15 Mbps for fast full-screen motion at 60 frames per second.

### Where can I report a problem or suggest a feature?

On [GitHub](https://github.com/Tamer-Mansour/TLP-Desk/issues).
