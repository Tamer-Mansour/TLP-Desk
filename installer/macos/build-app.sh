#!/usr/bin/env bash
# Builds an unsigned TLP-Desk.app bundle and a zip from the release binary.
# Run on a Mac:  installer/macos/build-app.sh [output-dir]
# Needs: Rust stable, and (for the icon) the PNG in crates/tlp-desk-app/assets/icon/.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
out="${1:-${TMPDIR:-/tmp}/tlp-desk-macos}"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"
target="${CARGO_TARGET_DIR:-$root/target}"

cargo build --release -p tlp-desk-app --manifest-path "$root/Cargo.toml"

app="$out/TLP-Desk.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$target/release/TLP-Desk" "$app/Contents/MacOS/TLP-Desk"
sed "s/@VERSION@/${version}/g" "$root/installer/macos/Info.plist.in" > "$app/Contents/Info.plist"

# Optional icon: build an .icns from a 1024px PNG when sips/iconutil are present.
png="$(ls "$root"/crates/tlp-desk-app/assets/icon/*.png 2>/dev/null | head -1 || true)"
if [ -n "$png" ] && command -v iconutil >/dev/null; then
  set_dir="$out/TLP-Desk.iconset"
  rm -rf "$set_dir"; mkdir -p "$set_dir"
  for s in 16 32 128 256 512; do
    sips -z $s $s "$png" --out "$set_dir/icon_${s}x${s}.png" >/dev/null
    sips -z $((s*2)) $((s*2)) "$png" --out "$set_dir/icon_${s}x${s}@2x.png" >/dev/null
  done
  iconutil -c icns "$set_dir" -o "$app/Contents/Resources/TLP-Desk.icns"
  rm -rf "$set_dir"
fi

# Ad-hoc signature: gives the app a stable identity so the Screen Recording and Accessibility
# permissions stick between launches. It is NOT a Developer ID signature (see the README).
codesign --force --deep --sign - "$app"

(cd "$out" && ditto -c -k --keepParent TLP-Desk.app "TLP-Desk-${version}-macos.zip")
echo "Built $app and $out/TLP-Desk-${version}-macos.zip"
