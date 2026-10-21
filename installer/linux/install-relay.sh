#!/usr/bin/env bash
# Installs tlp-desk-relay (the TLP-Desk relay server) on a Linux server as a systemd service.
#
#   sudo ./install-relay.sh --token 'a long secret'            # from the release tarball folder
#   curl -fsSL .../install-relay.sh | sudo bash -s -- --token 'a long secret'   # downloads it
#
# Options:
#   --token SECRET     the shared secret every device must be given (required on first install)
#   --listen ADDR      control socket (default 0.0.0.0:47402)
#   --ports LOW-HIGH   session port range (default 47410-47509)
#   --version TAG      release tag to download when no binary is next to the script (default: latest)
#   --uninstall        stop and remove the service and the binary (the token file is removed too)
#
# After installing, open UDP 47402 and UDP 47410-47509 in the server's firewall (and the
# provider's firewall panel). Logs: journalctl -u tlp-desk-relay -f
set -euo pipefail

bin_dst=/usr/local/bin/tlp-desk-relay
env_file=/etc/tlp-desk-relay.env
unit=/etc/systemd/system/tlp-desk-relay.service
repo=Tamer-Mansour/TLP-Desk

token=""
listen="0.0.0.0:47402"
ports="47410-47509"
version="latest"
uninstall=0
while [ $# -gt 0 ]; do
  case "$1" in
    --token) token="$2"; shift 2 ;;
    --listen) listen="$2"; shift 2 ;;
    --ports) ports="$2"; shift 2 ;;
    --version) version="$2"; shift 2 ;;
    --uninstall) uninstall=1; shift ;;
    -h | --help) sed -n '2,16p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

if [ "$(id -u)" -ne 0 ]; then
  echo "run as root (sudo)" >&2
  exit 1
fi

if [ "$uninstall" -eq 1 ]; then
  systemctl disable --now tlp-desk-relay 2>/dev/null || true
  rm -f "$unit" "$bin_dst" "$env_file"
  systemctl daemon-reload
  echo "tlp-desk-relay removed"
  exit 0
fi

if [ -f "$env_file" ] && [ -z "$token" ]; then
  echo "keeping the existing token in $env_file"
elif [ -z "$token" ]; then
  echo "--token is required on the first install (a long secret; the same on every device)" >&2
  exit 2
elif [ "${#token}" -lt 12 ]; then
  echo "the token must be at least 12 characters" >&2
  exit 2
fi

here="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" 2>/dev/null && pwd || echo .)"
src=""
for candidate in "$here/tlp-desk-relay" "$here/bin/tlp-desk-relay"; do
  if [ -x "$candidate" ]; then src="$candidate"; break; fi
done
if [ -z "$src" ]; then
  arch="$(uname -m)"
  case "$arch" in
    x86_64) asset="tlp-desk-relay-linux-x86_64" ;;
    aarch64 | arm64) asset="tlp-desk-relay-linux-aarch64" ;;
    *) echo "no prebuilt relay for $arch; build it with: cargo build --release -p tlp-desk-relay" >&2; exit 1 ;;
  esac
  if [ "$version" = "latest" ]; then
    url="https://github.com/$repo/releases/latest/download/$asset"
  else
    url="https://github.com/$repo/releases/download/$version/$asset"
  fi
  tmp="$(mktemp)"
  echo "downloading $url"
  curl -fsSL -o "$tmp" "$url"
  src="$tmp"
fi

install -m 0755 "$src" "$bin_dst"
if [ -n "$token" ]; then
  umask 077
  printf 'TLP_RELAY_TOKEN=%s\n' "$token" > "$env_file"
  chmod 0600 "$env_file"
fi

cat > "$unit" <<EOF
[Unit]
Description=TLP-Desk relay server
After=network-online.target
Wants=network-online.target

[Service]
EnvironmentFile=$env_file
ExecStart=$bin_dst --listen $listen --ports $ports
Restart=always
RestartSec=3
DynamicUser=yes
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable --now tlp-desk-relay
sleep 1
systemctl --no-pager --lines=5 status tlp-desk-relay || true
ctl_port="${listen##*:}"
cat <<EOF

tlp-desk-relay is running.
Open these UDP ports in the firewall (server and provider panel):
  UDP $ctl_port         control
  UDP $ports    sessions

For example with ufw:
  ufw allow $ctl_port/udp && ufw allow ${ports/-/:}/udp

On every TLP-Desk device: Settings -> Security -> Relay server:
  address = this server's public IP or name, token = the one you chose.
EOF
