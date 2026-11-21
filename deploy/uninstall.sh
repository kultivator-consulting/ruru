#!/usr/bin/env bash
# Ruru deploy — remove the services from this host. Run as root:
#   sudo bash deploy/uninstall.sh            # keeps /var/lib/ruru (secrets + calibration)
#   PURGE=1 sudo bash deploy/uninstall.sh     # also removes state, config, and the users
set -euo pipefail
say() { printf '\033[1;36m>>\033[0m %s\n' "$*"; }
[ "$(id -u)" -eq 0 ] || { echo "must run as root"; exit 1; }

say "stop + disable units"
systemctl disable --now ruru-mcp.service ruru-source.service 2>/dev/null || true
rm -f /etc/systemd/system/ruru-mcp.service /etc/systemd/system/ruru-source.service
systemctl daemon-reload

say "remove binaries"
rm -f /usr/local/bin/ruru-source /usr/local/bin/ruru-mcp

if [ "${PURGE:-0}" = "1" ]; then
  say "PURGE: config, state (secrets + calibration), and service users"
  rm -rf /etc/ruru /var/lib/ruru
  userdel ruru-mcp 2>/dev/null || true
  userdel ruru-source 2>/dev/null || true
  groupdel ruru 2>/dev/null || true
else
  say "kept /etc/ruru and /var/lib/ruru (secrets + calibration). PURGE=1 to remove them."
fi
say "done."
