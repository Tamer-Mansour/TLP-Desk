# Third-party notices

TLP-Desk is licensed under MIT OR Apache-2.0. The `TLP-Desk.exe` binary embeds the following
third-party assets. Their license texts ship in the `licenses\` folder of the installer and the
portable zip.

| Asset | Used for | License | License file |
| --- | --- | --- | --- |
| [tlp-icons](https://github.com/Tamer-Mansour/tlp-icons) (static SVG icons, see `crates/tlp-desk-app/assets/icons/README.md` for the list and the source commit) | GUI icons | MIT | `licenses\tlp-icons-LICENSE.txt` |
| [Lucide](https://lucide.dev) icon geometry, included in tlp-icons | GUI icons | ISC | `licenses\lucide-LICENSE.txt` |
| [Noto Sans Arabic](https://github.com/notofonts) | Arabic text | SIL Open Font License 1.1 | `licenses\NotoSansArabic-OFL.txt` |
| [Inter](https://rsms.me/inter/) 4.1 | Latin UI text | SIL Open Font License 1.1 | `licenses\Inter-OFL.txt` |
| egui default fonts (`epaint_default_fonts`) | Latin text, emoji | see each file | `licenses\egui-*.txt` |

Copyright notices:

- tlp-icons: Copyright (c) 2026 Tamer and tlp-icons contributors (MIT).
- Lucide: Copyright (c) 2026 Lucide Icons and Contributors (ISC); icons derived from Feather:
  Copyright (c) 2013-present Cole Bemis (MIT), as stated in `lucide-LICENSE.txt`.

Rust crates compiled into the binaries are listed in `Cargo.lock`. Their licenses are restricted to
permissive ones (MIT, Apache-2.0, BSD, ISC, Zlib, Unicode-3.0, BSL-1.0) and checked with
`cargo deny check` (see `deny.toml`).
