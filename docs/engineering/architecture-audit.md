# Architecture audit (2026-10-20)

Baseline: `TLP-Desk` at `13ae43e` (v1.0.0-rc.4). This audit was made before the transformation
work started, to find the real causes of the reported problems instead of guessing. Paths are
relative to the repository root; line numbers refer to the baseline commit.

## What the product actually is

| Concern | Implementation (baseline) |
|---|---|
| Language / build | Rust stable, one Cargo workspace, 14 crates (`crates/`), edition 2021 |
| GUI | `egui`/`eframe` 0.36 on `wgpu`, single binary `TLP-Desk.exe` (`tlp-desk-app`) |
| Transport | `quinn` QUIC, one endpoint per process on UDP 47400 (`[::]`, dual-stack), TLS 1.3 with mutual auth and key pinning (`tlp-desk-net/src/{endpoint,tls}.rs`) |
| Identity | ECDSA P-256 key per install (`rcgen`), device ID = 9 digits from SHA-256 of the SPKI (`tlp-desk-crypto/src/device_id.rs`) |
| Discovery | `mdns-sd` `_tlpdesk._udp.local.` + UDP broadcast fallback on 47401, merged into one table keyed by device ID (`tlp-desk-net/src/discovery.rs`) |
| Auth | Accept prompt, rotating one-time code (SPAKE2), unattended password (Argon2 + SPAKE2), reconnect tickets; per-IP rate limiting |
| Signalling / relay | **None by design.** LAN-only product: no rendezvous server, no relay, no NAT traversal. "Relay fallback" from the brief does not apply (CLAUDE.md hard rule 2) |
| Capture | Windows.Graphics.Capture with DXGI fallback, GPU downscale before readback; X11 on Linux; dirty-tile hashing (`tlp-desk-capture`) |
| Encode | Media Foundation hardware H.264 first, OpenH264 fallback (also mid-stream) (`tlp-desk-codec`) |
| Adaptation | 250 ms AIMD bitrate controller + resolution/fps ladder driven by RTT, queueing delay and viewer decode time; static-screen low-QP refinement (`tlp-desk-host/src/adapt`, `src/refine`) |
| Render | egui texture, Fit / Original modes, physical-pixel snapping, Lanczos-2 pre-scale when shrinking a still image (`tlp-desk-app/src/ui/view_scale.rs`) |
| Input | `SendInput`; Ctrl+Alt+Del via `SendSAS` from the service |
| Service | `windows-service`, the GUI mirrors it over local IPC when it hosts the PC |
| Updates | Opt-in GitHub Releases check, minisign-verified `SHA256SUMS.txt`, SHA-256 of the setup, elevated silent Inno Setup run (`tlp-desk-app/src/update`) |
| CI | `ci.yml` (Windows, macOS, Linux: fmt, clippy `-D warnings`, tests, release build; typos, cargo-deny), `release.yml` (tag → setup + portable + signed manifest), `docs.yml`, `readme-media.yml` |
| Tests | ~1,000 tests; real host+viewer pair over loopback QUIC with fakes (`tlp-desk-testkit`) |

The architecture is sound and already covers much of the brief: adaptive bitrate, dirty
detection, hardware encode, presets, secure updates, signed releases and CI exist. The work
should repair and extend it, not replace it.

## Root causes of the reported connection bugs

### BUG 1: "it says I am connecting to my own device"

There was no explicit self-connection check anywhere. The only "self" logic filtered our own
device ID out of the device list. What users experienced as "connecting to myself" came from
the viewer actually **reaching its own PC**:

1. **Virtual-adapter addresses shared by every PC.** A host announces the addresses of all its
   adapters over mDNS (`enable_addr_auto`). VirtualBox host-only (`192.168.56.1`), Docker
   (`172.17.0.1`) and Hyper-V/WSL switches use the same address on every machine. The viewer
   sorted a device's addresses numerically (`discovery.rs:280`), so `172.17.0.1` came before
   `192.168.1.x`. Dialing it reached the viewer's own PC: our own key was presented, the
   connection failed with "identity check failed", or with an accept prompt on our own screen
   (TOFU/IP dial).
2. **No fallback after an identity error.** The dial loop (`tlp-desk-viewer/src/lib.rs:385-409`)
   only moved to the next address after a transport error; any identity error aborted the whole
   attempt, so the correct address was never tried.
3. **Service vs GUI identity.** When the Windows service hosts the PC, the home screen shows the
   service's ID, but the device-list filter used the GUI's own (per-user) ID, so the local
   service could appear as "another device" with this PC's name.
4. **Copied identities** (cloned disk/VM, copied `ProgramData\TLP-Desk`) give two PCs the same
   ID; each then hides the other as "me". Nothing detected it.

### BUG 2: "visible but cannot connect until Wi-Fi is restarted"

1. **Addresses only accumulated.** mDNS resolutions and broadcast announcements appended
   addresses to a device and nothing ever removed one while the device stayed visible
   (`discovery.rs:500-504, 592-594`). After a DHCP change or adapter switch the dead address
   stayed in the list, often first.
2. **Sequential dial with no per-address timeout.** `connecting.await` had no timeout
   (`endpoint.rs:266`), so each dead address cost the 15 s QUIC idle timeout before the next was
   tried. Two stale addresses meant ~30 s of "Connecting…" and usually a give-up.
3. **No reaction to network changes.** Nothing watched the interface list. Restarting Wi-Fi
   "fixed" it because the mDNS goodbye/TTL expiry removed the device and a fresh resolution
   re-created it with only current addresses.
4. **IPv6 link-local addresses without a scope** (`fe80::…%0`) were dialable entries that can
   never work.
5. Contributing, outside the code: the installer's firewall rules cover the Private and Domain
   profiles only. A network Windows classifies as Public blocks inbound UDP 47400 while
   outbound announcements still work, so the device is listed but unreachable. Windows
   re-classifies on reconnect. This stays a documented user setting (widening the rules to
   Public is a security decision for the maintainer).

Not causes: the endpoint binds the wildcard address (survives interface changes without a
rebind), and known-hosts pins are keyed by ID, not address.

## Other findings (prioritised)

| # | Finding | Severity | Status |
|---|---|---|---|
| F1 | BUG 1 / BUG 2 causes above | Critical | Fixed in P0 |
| F2 | `cargo test --workspace` failed on Linux/macOS: `storage::tests::macos_dir_is_under_application_support` expected `TLP-Desk` but non-Windows builds use `tlp-desk` | Medium (red CI on non-Windows) | Fixed in P0 |
| F3 | A different device answering at a stale address of a remembered host showed the scary "host key changed" warning (inviting the user to forget the key) instead of "wrong device" | Medium (security UX) | Fixed in P0 |
| F4 | The viewer never tells the host its window size; the host streams at a fixed cap (1920×1200 by default, native up to 3840×2160 only with the Quality preset). A 4K host viewed in a small window wastes bandwidth; a 4K host viewed full-screen on a 4K viewer is downscaled | High (P1) | Open |
| F5 | Only Fit and Original view modes; no zoom/pan | Medium (P1/P2) | Open |
| F6 | No true lossless or text-priority mode; the static refinement is a low-QP keyframe | Medium (P1) | Open |
| F7 | Native title bar plus an in-window toolbar; no integrated title bar/menu | Medium (P2) | Open |
| F8 | Large UI files (`ui/session.rs` 1,673 lines, `backend/real.rs` 2,060) exceed the 800-line guideline | Low | Open |
| F9 | Viewer-side automatic reconnect exists only for host restarts (ticket) and network blips during a session; there is no host-side endpoint rebind (not needed for wildcard sockets) | Low | Documented |

## Constraints that shape the roadmap

* LAN-only: no relay, no rendezvous, no internet except the opt-in update check.
* Wire changes require a `PROTOCOL_VERSION` bump; peers with different versions refuse to
  connect. Prefer additive, backward-compatible extensions (as the `ver` trailer did).
* Windows is tier 1, but this cloud environment is Linux: Windows-only code (capture, MF
  encoder, title bar integration, service) can be compiled for Windows only on CI, and
  visual/hardware checks need a real Windows PC.
