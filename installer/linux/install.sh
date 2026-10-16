#!/usr/bin/env bash
# Installs TLP-Desk from the unpacked tarball.
#
#   ./install.sh                 # per user: ~/.local (no root needed)
#   sudo ./install.sh --system   # system wide: /usr/local
#   ./install.sh --uninstall     # remove what this script installed (add --system if used)
#
# Your identity (device ID) and settings in ~/.local/share/tlp-desk and ~/.config/tlp-desk are
# never touched by install or uninstall.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
prefix="$HOME/.local"
uninstall=0
for arg in "$@"; do
  case "$arg" in
    --system) prefix="/usr/local" ;;
    --uninstall) uninstall=1 ;;
    -h | --help) sed -n '2,10p' "$0"; exit 0 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

files=(
  "bin/TLP-Desk"
  "bin/TLP-Desk-cli"
  "share/applications/TLP-Desk.desktop"
  "share/icons/hicolor/256x256/apps/TLP-Desk.png"
)

if [ "$uninstall" -eq 1 ]; then
  for f in "${files[@]}"; do rm -f "$prefix/$f"; done
  echo "TLP-Desk removed from $prefix"
else
  for f in "${files[@]}"; do
    mkdir -p "$prefix/$(dirname "$f")"
    install -m "$([ "${f%%/*}" = bin ] && echo 0755 || echo 0644)" "$here/$f" "$prefix/$f"
  done
  echo "TLP-Desk installed to $prefix (start it from the application menu or run TLP-Desk)"
fi

command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$prefix/share/applications" >/dev/null 2>&1 || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q "$prefix/share/icons/hicolor" >/dev/null 2>&1 || true
