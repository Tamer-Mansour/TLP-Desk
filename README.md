# TLP-Desk

**Free, open-source remote desktop for your local network: offline, encrypted, and written in pure Rust.**

TLP-Desk lets one Windows PC see and control another PC on the same LAN. It has no cloud, no vendor
relay, no account and no telemetry, and it keeps working with the internet cable unplugged. To reach
a PC at another home you run your own tiny relay server; nothing else ever leaves your network.
A single app is both the **host** (the PC being controlled) and the **viewer** (the PC in control).
The interface is available in **Arabic (right-to-left)** and **English**.

> Status: **release candidate** (`1.0.0-rc.6`). Everything in the feature list below ships today;
> what is left before 1.0 is in [ROADMAP.md](ROADMAP.md).

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/media/home-en-dark@2x.png">
    <img src="docs/media/home-en-light@2x.png" width="840"
         alt="TLP-Desk home screen: this device's ID and one-time code, the connect box and devices found on the network">
  </picture>
</p>

### See it in action

<p align="center">
  <a href="docs/media/demo-en.mp4">
    <img src="docs/media/demo-en.gif" width="800"
         alt="Demo: connect with a device ID and one-time code, control the remote screen, switch monitors, disconnect">
  </a>
  <br><sub>Connect by ID and one-time code, control the remote PC, switch screens, disconnect
  (video from an earlier build; the screenshots show the current design).
  <a href="docs/media/demo-en.mp4">Full-quality MP4</a> ·
  <a href="docs/media/demo-ar.mp4">Arabic version</a></sub>
</p>

**English and Arabic (right-to-left), side by side:**

<table>
  <tr>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/media/session-en-dark.png">
        <img src="docs/media/session-en-light.png" alt="Viewer session in English with toolbar and stats overlay">
      </picture>
    </td>
    <td width="50%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/media/session-ar-dark.png">
        <img src="docs/media/session-ar-light.png" alt="The same session in Arabic: mirrored toolbar and overlay">
      </picture>
    </td>
  </tr>
</table>

<details>
<summary><b>More screenshots</b></summary>

| | |
|---|---|
| ![Incoming request with Accept and Decline](docs/media/accept-popup-en-dark.png) | ![Devices page: the LAN list and the address book](docs/media/devices-page-en-dark.png) |
| **Accept prompt** on the host, with per-session permissions | **Devices**: found on the network, and the address book |
| ![Sessions page: who is connected now and the connection log](docs/media/sessions-page-en-dark.png) | ![Settings in Arabic with Arabic-Indic digits](docs/media/settings-ar-dark.png) |
| **Sessions**: who is connected now, and every connection so far | **Settings** in Arabic: language, digits, theme |
| ![Connect with a device ID and one-time code](docs/media/connect-code-en-dark.png) | ![Warning that the host's key changed](docs/media/key-changed-en-dark.png) |
| **Connect** by ID, name or IP, with an optional code | **Loud warning** if a host's identity changes |

All images and videos are generated from the real UI by
[`scripts/update-readme-media.ps1`](scripts/update-readme-media.ps1); see
[docs/media](docs/media) for every screen in both languages and themes.

</details>

---

## Features

- **Find devices automatically** on the LAN (mDNS, with a UDP broadcast fallback), or connect by
  9-digit device ID, name or IP address. Saved devices live in an **address book** with aliases,
  groups, online status and **Wake-on-LAN**.
- **Secure by default.** Every connection uses QUIC with TLS 1.3. Each install has its own key, and
  the 9-digit ID is derived from that key, so dialing an ID pins the host's identity.
  Trust-on-first-use with a loud warning applies when you connect by IP.
- **Three ways to allow a session:** the host user clicks *Accept* (choosing what the viewer may
  do), the viewer enters a one-time 6-digit code (SPAKE2, so the code never crosses the network
  and it rotates after use), or a permanent **unattended-access password** (Argon2 at rest).
- **LAN only.** By default only private (RFC 1918) and link-local addresses may connect. Login
  attempts are rate-limited, with a lockout after repeated failures. The host always shows a
  banner naming who is connected, and can revoke any permission live.
- **Smooth, sharp video:** about 60 fps of H.264 (hardware encoding through Media Foundation where
  available), captured with Windows.Graphics.Capture (DXGI fallback), scaled on the host's GPU to
  exactly your window so a 4K PC looks crisp on a laptop. Adaptive quality, a quality floor so text
  never smears, a near-lossless refresh when the screen settles, **zoom** 25–300 %, and frames
  dropped rather than queued so lag doesn't build up.
- **Full control:** mouse, wheel and keyboard sent as hardware scancodes, so Arabic and English
  keyboard layouts both work; Win, Alt+Tab and Ctrl+Esc go to the remote PC; Ctrl+Alt+Del, the
  lock screen and UAC through the **Windows service**; **privacy mode** blanks the host's screens.
- **The person at the host always wins.** Touching the local mouse or keyboard pauses remote input,
  and a marker shows where the remote pointer is. Several viewers can share one PC, each with its
  own permissions.
- **Files and clipboard:** a two-pane **remote file browser**, drag and drop with Explorer, copy in
  Explorer on one PC and paste on the other, fast parallel transfers with resume and BLAKE3
  verification; clipboard sync for text, formatted text and images.
- **Chat, sound and drawing:** in-session chat, **system audio** from the remote PC (Opus),
  a **whiteboard** to draw on the remote screen, and **session recording** to MP4 with playback.
- **Multi-monitor hosts:** switch between the host's screens live, or view them all at once.
- **Automatic reconnect** after a network drop, **port forwarding** through a session, remote
  device info, and a **connection log** with search and CSV export.
- **Arabic first-class:** properly joined letters, mixed Arabic/English in the right order, mirrored
  layouts, and a choice of Arabic-Indic or Western digits.
- **A design of its own:** the TLP brand (burgundy, rose, ivory and ink), an icon rail, glass
  panels, light and dark themes, and menus drawn in the window's own title bar.
- **Another network? Your own relay.** Run the small `tlp-desk-relay` on a VPS or a PC at home,
  give both devices its address and token, and connect by ID across the internet — still end
  to end encrypted and pinned, the relay only copies packets. No port to open at the other
  home, works behind CGNAT. Those devices get their own **Remote devices** page, each card
  with a picture of its screen; the stream there is capped at 1080p.
  ([guide](docs/src/user-guide/en/relay.md))

  ![Remote devices](docs/media/remote-devices-en-dark.png)
- **Opt-in updates** checked against GitHub Releases and verified with the release signing key;
  a notice when a device on the LAN runs a newer version. Nothing else ever leaves the LAN
  unless you configure a relay.
- **Single `setup.exe` installer** (Arabic + English) that adds Windows Firewall rules for private
  networks only, a portable zip, a winget manifest, and a **CLI** (`TLP-Desk-cli.exe`) for scripts.
- **Linux (X11) and macOS** host and viewer, and an **Android viewer**, are experimental.

---

## Install

1. Download `TLP-Desk-<version>-setup.exe` from the
   [Releases](https://github.com/Tamer-Mansour/TLP-Desk/releases) page.
2. Run it. The installer isn't code-signed yet, so Windows SmartScreen may warn:
   choose **More info → Run anyway**.
3. Start **TLP-Desk** on both PCs. Make sure both are on the same network and that the network is
   set to **Private** in Windows.

**Portable:** unzip `TLP-Desk-<version>-win-x64-portable.zip` and run `TLP-Desk.exe`. Allow the
Windows Firewall prompt the first time.

**Requirements:** Windows 10 (2004 or newer) or Windows 11, x64.

**Current release:** `v1.0.0-rc.6` (release candidate). See the
[changelog](docs/src/changelog.md), the [system test report](docs/src/dev/system-test-report.md)
and the [two-PC manual test checklist](docs/src/user-guide/en/manual-test.md)
([بالعربية](docs/src/user-guide/ar/manual-test.md)).

### Network ports

| Port | Protocol | Purpose |
|---|---|---|
| 47400 | UDP | Sessions (QUIC / TLS 1.3) |
| 47401 | UDP | Discovery broadcast fallback |
| 5353 | UDP | mDNS discovery (`_tlpdesk._udp.local.`) |
| 47402, 47410–47509 | UDP | Only on a relay server you run (`tlp-desk-relay`) |

---

## Quick start

1. On the PC you want to control, open TLP-Desk and note its **device ID** and **one-time code**.
2. On your PC, pick it from **Devices on this network**, or type its ID or IP address.
3. Either enter the one-time code, or leave the code empty and click **Accept** on the other PC.
4. Control it. Use the toolbar for fit / actual size and zoom, full screen, view-only, screen
   switching, files, chat, the stats overlay, and disconnect.

---

## Documentation

The full documentation is an [mdBook](https://rust-lang.github.io/mdBook/) in [`docs/`](docs/)
(build it with `mdbook build docs`; it is published to GitHub Pages from `TLP-Desk`):

- **User guide:** [English](docs/src/user-guide/en/index.md) ·
  [العربية](docs/src/user-guide/ar/index.md) — installing, connecting, controlling, settings,
  troubleshooting, FAQ.
- **[Security model](docs/src/security.md)** — threat model, identity and pinning, known limitations.
- **Contributor guide:** [architecture](docs/src/dev/architecture/index.md),
  [one page per crate](docs/src/dev/crates/index.md), [building](docs/src/dev/building.md),
  [testing](docs/src/dev/testing.md), [i18n and Arabic](docs/src/dev/i18n.md),
  [release process](docs/src/dev/release.md).
- **[Architecture decision records](docs/src/adr/index.md)** and the
  [wire protocol](docs/protocol.md).

---

## Build from source

Prerequisites (Windows):

- [Rust](https://rustup.rs) stable (the version is pinned in `rust-toolchain.toml`)
- Visual Studio Build Tools with **Desktop development with C++** and the Windows 11 SDK
- [NASM](https://www.nasm.us) on `PATH` (optional, makes the H.264 encoder much faster)
- [Inno Setup 6](https://jrsoftware.org/isinfo.php) (only to build the installer)

```powershell
git clone https://github.com/Tamer-Mansour/TLP-Desk.git
cd TLP-Desk

# Optional: keep build output on a drive with plenty of space
$env:CARGO_TARGET_DIR = "C:\tlp-build\main"

cargo build --release            # TLP-Desk.exe and TLP-Desk-cli.exe
cargo run --release -p tlp-desk-app

# Full quality gate: format, release build, clippy -D warnings, tests
powershell -ExecutionPolicy Bypass -File scripts\check.ps1

# Installer and portable zip (written to dist\)
powershell -ExecutionPolicy Bypass -File installer\build-setup.ps1
powershell -ExecutionPolicy Bypass -File installer\portable.ps1
```

**Linux** (experimental, X11): install `build-essential pkg-config cmake nasm libasound2-dev`, then
`cargo build --release` and `installer/linux/build-tarball.sh` (see
[docs/src/dev/linux.md](docs/src/dev/linux.md)).

---

## Project layout

| Path | What lives there |
|---|---|
| `crates/tlp-desk-proto` | Wire messages, `PROTOCOL_VERSION`, framing |
| `crates/tlp-desk-crypto` | Identity keys, device ID, pinning, SPAKE2, rate limiting |
| `crates/tlp-desk-net` | QUIC transport, discovery, Wake-on-LAN |
| `crates/tlp-desk-capture` | Screen capture (Windows.Graphics.Capture / DXGI) |
| `crates/tlp-desk-codec` | Video encode/decode (H.264 via OpenH264) |
| `crates/tlp-desk-input` | Input injection, keyboard capture, local-priority hooks |
| `crates/tlp-desk-host` / `tlp-desk-viewer` | Session logic for each side |
| `crates/tlp-desk-app` | The GUI (egui / wgpu), Arabic + English |
| `crates/tlp-desk-cli` | Headless CLI for scripts and testing |
| `installer/` | Inno Setup installer and portable packaging |
| `docs/` | Protocol, code style, user and developer guides |

More detail: [docs/protocol.md](docs/protocol.md) · [docs/CODE_STYLE.md](docs/CODE_STYLE.md)

---

## Contributing

Contributions are welcome. In short:

- Branch from `TLP-Desk` as `TLP-<topic>` and open a pull request back into `TLP-Desk`.
- Commit messages follow `TLP-<area>: what changed`.
- Every PR must pass `cargo fmt`, `cargo clippy -- -D warnings` and `cargo test --workspace`.
- **Clean-room rule:** never copy code, protocol details, assets or UI text from other remote
  desktop products.
- Any feature that shows or moves text must work in Arabic.
- Changed something visible? Run `scripts\update-readme-media.ps1` and commit the updated
  `docs/media/` in the same PR. Other screenshots never go into git.

Start with [CONTRIBUTING.md](CONTRIBUTING.md). The full rules for humans and coding agents are in
[CLAUDE.md](CLAUDE.md) and
[docs/CODE_STYLE.md](docs/CODE_STYLE.md).

---

## Security

TLP-Desk is designed for trusted local networks. Please report vulnerabilities privately through
GitHub Security Advisories rather than public issues; see [SECURITY.md](SECURITY.md).

---

## نبذة بالعربية

<div dir="rtl">

**TLP-Desk** برنامج مجاني ومفتوح المصدر للتحكم بجهاز آخر عن بُعد داخل الشبكة المحلية فقط، بدون
إنترنت ولا خوادم ولا حسابات. البرنامج نفسه يعمل كجهاز مُتحكَّم به وكجهاز متحكِّم، وواجهته متوفرة
بالعربية (من اليمين لليسار) وبالإنجليزية.

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/media/home-ar-dark@2x.png">
    <img src="docs/media/home-ar-light@2x.png" width="760" alt="الشاشة الرئيسية لبرنامج TLP-Desk بالعربية">
  </picture>
</p>

<p align="center">
  <a href="docs/media/demo-ar.mp4">
    <img src="docs/media/demo-ar.gif" width="760" alt="عرض توضيحي: الاتصال برقم الجهاز والرمز، التحكم، التنقل بين الشاشات">
  </a>
  <br><sub><a href="docs/media/demo-ar.mp4">الفيديو بجودة كاملة (MP4)</a></sub>
</p>

- اكتشاف الأجهزة على الشبكة تلقائيًا، أو الاتصال برقم الجهاز المكوّن من 9 خانات أو بعنوان IP،
  ودفتر عناوين للأجهزة المحفوظة مع إيقاظها عبر الشبكة (Wake-on-LAN).
- كل اتصال مشفّر، والسماح بالجلسة يتم بالضغط على «قبول» أو برمز لمرة واحدة أو بكلمة مرور دائمة
  للوصول غير المراقَب، مع خدمة ويندوز لشاشة القفل وCtrl+Alt+Del ووضع الخصوصية.
- صورة سلسة وحادة بحدود 60 إطارًا في الثانية مع تكبير 25–300٪، وتحكم كامل بالفأرة ولوحة المفاتيح
  (العربية والإنجليزية).
- نقل ملفات سريع ومتصفح ملفات بعيد، مزامنة الحافظة، دردشة، صوت النظام، رسم على الشاشة، وتسجيل الجلسة.
- صاحب الجهاز له الأولوية دائمًا: لمس الفأرة المحلي يوقف التحكم عن بُعد مؤقتًا، وشريط ظاهر يبيّن من
  متصل.
- التنقل بين الشاشات إذا كان للجهاز الآخر أكثر من شاشة، وإعادة اتصال تلقائية عند انقطاع الشبكة.
- تصميم بهوية TLP (الخمري والوردي والعاجي)، بوضعين فاتح وداكن.
- الاتصال بجهاز في شبكة أخرى عبر خادم وسيط صغير **خاص بك** (`tlp-desk-relay`) بالمعرّف نفسه، والتشفير يبقى
  من طرف إلى طرف.
- مثبّت واحد `setup.exe` بالعربية والإنجليزية، ونسخة محمولة، وأداة سطر أوامر.

للتثبيت: نزّل ملف `TLP-Desk-<version>-setup.exe` من صفحة
[الإصدارات](https://github.com/Tamer-Mansour/TLP-Desk/releases) وشغّله على الجهازين.

الدليل الكامل بالعربية: [دليل المستخدم](docs/src/user-guide/ar/index.md).
قائمة الاختبار اليدوي على جهازين: [قائمة الاختبار](docs/src/user-guide/ar/manual-test.md).

</div>

---

## License

Dual-licensed under either of

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option. Bundled fonts and icons keep their own licenses (SIL OFL, MIT, ISC); see the
license files shipped with the installer and [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
