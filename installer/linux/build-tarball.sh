#!/usr/bin/env bash
# Builds TLP-Desk-<version>-linux-x86_64.tar.gz from a release build.
#
#   installer/linux/build-tarball.sh            # builds with cargo, writes to dist/
#   CARGO_TARGET_DIR=/tmp/tlp-build installer/linux/build-tarball.sh
#
# Build dependencies (Debian/Ubuntu): build-essential pkg-config cmake nasm libasound2-dev.
# Run-time: an X11 session (Wayland capture is not implemented yet), a working GPU driver for
# Vulkan or OpenGL, and optionally PulseAudio/PipeWire for sound.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
target="${CARGO_TARGET_DIR:-$root/target}"
out="${DIST_DIR:-$root/dist}"

version="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)"
arch="$(uname -m)"
name="TLP-Desk-${version}-linux-${arch}"

cargo build --release --manifest-path "$root/Cargo.toml" -p tlp-desk-app -p tlp-desk-cli

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
pkg="$stage/$name"
mkdir -p "$pkg/bin" "$pkg/share/applications" "$pkg/share/icons/hicolor/256x256/apps"

install -m 0755 "$target/release/TLP-Desk" "$pkg/bin/TLP-Desk"
install -m 0755 "$target/release/TLP-Desk-cli" "$pkg/bin/TLP-Desk-cli"
install -m 0644 "$root/installer/linux/TLP-Desk.desktop" "$pkg/share/applications/TLP-Desk.desktop"
install -m 0644 "$root/crates/tlp-desk-app/assets/icon/TLP-Desk-256.png" \
  "$pkg/share/icons/hicolor/256x256/apps/TLP-Desk.png"
install -m 0755 "$root/installer/linux/install.sh" "$pkg/install.sh"
install -m 0644 "$root/installer/linux/README-linux.txt" "$pkg/README-linux.txt"
install -m 0644 "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$root/THIRD_PARTY_NOTICES.md" "$pkg/"

mkdir -p "$out"
tar -C "$stage" -czf "$out/$name.tar.gz" "$name"
(cd "$out" && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
echo "wrote $out/$name.tar.gz"
