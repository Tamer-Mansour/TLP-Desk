# Engineering decisions

Short records of choices made during the transformation, newest last. Bigger architectural
decisions also get an ADR in `docs/src/adr/`.

## D1: Self-connection is decided by key, never by address (P0)

Two PCs can share addresses (virtual adapters, reused DHCP leases), public IPs and host name
patterns; only the TLS key is unique per install. So:

* an address equal to one of this machine's own non-loopback interface addresses is never
  dialed (it can only reach us);
* if any address answers with our own key, that address is skipped
  (`NetError::SelfConnection`) and the host side closes it without a prompt;
* dialing our own device ID (or the service's ID shown on the home screen) fails at once with
  "That is this device".

Loopback addresses are kept: two installs on one machine (tests, a developer box) are
legitimately different devices, and the key check still catches a real self-dial.

## D2: Dial every address of a device, staggered, authorize only the winner (P0)

Happy-eyeballs style: start the best address, then one more every 250 ms (immediately when an
attempt fails), each bounded by the endpoint's handshake timeout (10 s default). The first QUIC
handshake whose key matches the pin wins; the others are dropped before any `Hello`, so the
host user never sees two prompts and rate limits are not charged twice. Authorization
(`Endpoint::authorize`) runs on the winner only.

Alternative rejected: keep the sequential loop with a short per-address timeout. With 3–4
announced addresses (Wi-Fi, Ethernet, virtual switches) that is still seconds per attempt, and
picking the timeout is a guess on slow Wi-Fi.

When no address works the reported error is, in order: a key mismatch (possible attack, or
another device at a stale address), else the last transport error, else "That is this device"
when only ourselves answered.

## D3: Addresses are per-source state, not an accumulating list (P0)

The latest mDNS resolution replaces the mDNS address set; broadcast sources are kept with a
last-heard time and expire individually after `expiry` (16 s). Ranking happens whenever either
source or the local interface list changes. A device with no dialable address is hidden rather
than shown as connectable.

## D4: Network changes are detected by polling the interface list every 2 s (P0)

`if_addrs` on a blocking thread, compared with the previous snapshot. Portable (Windows,
Linux, macOS, Android), no `unsafe`, cheap (one syscall). `NotifyIpInterfaceChange` would be
instant on Windows but needs a platform module and buys < 2 s. The QUIC endpoint is not rebound:
it binds the wildcard address, which keeps working across interface changes.

## D5: "Wrong device" beats "key changed" when the IDs differ (P0)

With a remembered fingerprint, a different key used to always mean "host key changed". When the
attempt was by device ID and the presented key hashes to a *different* ID, it is another
device at that address, not a changed key for this one: report `IdentityMismatch`. The
connection still fails closed; only the message (and the absence of a "forget key" button)
changes. Connections by address keep the loud TOFU warning.

## D6: Firewall profile stays Private/Domain (P0, needs maintainer decision to change)

A Wi-Fi classified as Public blocks inbound UDP 47400 with the installer's rules. Opening the
Public profile would make the PC reachable on hotel/airport networks; that is a security
trade-off for the maintainer, so it is documented in Troubleshooting instead.

## D7: Engineering notes live in `docs/engineering/` (process)

Requested by the maintainer for resumable hand-offs between sessions. They are not part of the
mdBook (contributor docs stay in `docs/src/dev/`). CLAUDE.md's layout table lists the folder.

## D8: Viewport hint rides in the existing `SetQuality` fields (P1)

The presets used to ignore `max_width`/`max_height`; a non-zero pair now means "the viewer's
fit-to-window picture area in physical pixels". Old hosts keep ignoring it, old viewers send
zeros, so no `PROTOCOL_VERSION` bump (a bump makes rc.4 and newer peers refuse each other). The
hint is applied after turning the preset cap to the monitor's orientation (the viewer's window
does not rotate with a portrait monitor) and before the adaptive ladder. Sent only after the
area kept one size for 400 ms: each new size reopens the capturer and costs a keyframe.

## D9: Integrated title bar = undecorated window + OS move/resize loops (P2)

`with_decorations(false)` and egui-drawn bar. Moving uses `ViewportCommand::StartDrag` and
resizing `ViewportCommand::BeginResize`, which winit maps to the native move/size loops
(`WM_NCLBUTTONDOWN`), so Aero snap and multi-monitor DPI changes behave like a normal window.
Not available without a native hit-test hook: the Windows 11 snap-layout flyout on hovering
maximize (it needs `HTMAXBUTTON` from `WM_NCHITTEST`, which egui/winit do not expose); that
would need a platform module subclassing the window. The setting can switch back to the system
frame live (`ViewportCommand::Decorations`). Default on for Windows only.

`egui::MenuBar` fills the available width and forces left-to-right, so it is given a child area
exactly as wide as the measured menu labels, with the menu order reversed in RTL.

## D10: README media are regenerated by CI on Windows, not from the Linux session (P2)

The media script's output is byte-deterministic per renderer; the workflow uses WARP on Windows,
this session only has Mesa lavapipe. Committing lavapipe PNGs would churn on the next CI run.
On the PR the `readme-media` workflow uploads the Windows-rendered files as an artifact, and
after merge it opens a `TLP-media-refresh` PR by itself. Lavapipe renders are still used here
to look at the UI while working (`cargo run -p tlp-desk-app --example readme_media -- --out
<tmp> --only home`; needs `mesa-vulkan-drivers`).
