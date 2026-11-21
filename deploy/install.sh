#!/usr/bin/env bash
# Ruru deploy — install both daemons as non-root systemd services on THIS host.
#
# Idempotent: safe to re-run to pick up new binaries or config. Run as root:
#   sudo bash deploy/install.sh
#
# It does NOT set the admin password or mint a bearer key — those are yours to run once,
# as the ruru-mcp user (the script prints the exact commands at the end).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_DIR="${BIN_DIR:-$REPO_ROOT/target/x86_64-unknown-linux-musl/release}"

say() { printf '\033[1;36m>>\033[0m %s\n' "$*"; }

[ "$(id -u)" -eq 0 ] || { echo "must run as root (sudo bash deploy/install.sh)"; exit 1; }
for b in ruru-source ruru-mcp; do
  [ -x "$BIN_DIR/$b" ] || { echo "missing binary $BIN_DIR/$b — run 'make release' first"; exit 1; }
done

say "service group + users (non-root)"
getent group ruru >/dev/null || groupadd --system ruru
if ! id ruru-source >/dev/null 2>&1; then
  useradd --system --no-create-home --shell /usr/sbin/nologin --groups video,ruru ruru-source
fi
if ! id ruru-mcp >/dev/null 2>&1; then
  useradd --system --no-create-home --shell /usr/sbin/nologin --groups ruru ruru-mcp
fi
id ruru-source | sed 's/^/   /'
id ruru-mcp    | sed 's/^/   /'

say "binaries -> /usr/local/bin"
install -m0755 "$BIN_DIR/ruru-source" /usr/local/bin/ruru-source
install -m0755 "$BIN_DIR/ruru-mcp"    /usr/local/bin/ruru-mcp

say "config -> /etc/ruru (world-readable, no secrets)"
install -d -m0755 /etc/ruru
# Seed only: never clobber an operator's edited config on a re-install.
for f in source.toml mcp.toml; do
  if [ -f "/etc/ruru/$f" ]; then
    say "  /etc/ruru/$f already present — left as is (compare with deploy/$f)"
  else
    install -m0644 "$SCRIPT_DIR/$f" "/etc/ruru/$f"
    say "  seeded /etc/ruru/$f — EDIT IT: set [camera].device to your camera's by-id path"
  fi
done

say "state dir -> /var/lib/ruru (0700 ruru-mcp; holds the 0600 secrets + calibration)"
install -d -o ruru-mcp -g ruru -m0700 /var/lib/ruru
say "  calibrate the board in the admin panel at /calibration once ruru-mcp is up"

say "systemd units -> /etc/systemd/system"
install -m0644 "$SCRIPT_DIR/ruru-source.service" /etc/systemd/system/ruru-source.service
install -m0644 "$SCRIPT_DIR/ruru-mcp.service"    /etc/systemd/system/ruru-mcp.service
systemctl daemon-reload
systemctl enable ruru-source.service ruru-mcp.service >/dev/null

say "start ruru-source (needs no secrets)"
systemctl restart ruru-source.service
sleep 1
systemctl --no-pager --lines=0 status ruru-source.service | sed 's/^/   /' || true

if [ -f /var/lib/ruru/mcp-secrets.toml ]; then
  say "secrets present — (re)starting ruru-mcp"
  systemctl restart ruru-mcp.service
  sleep 1
  systemctl --no-pager --lines=0 status ruru-mcp.service | sed 's/^/   /' || true
else
  cat <<EOF

  ─────────────────────────────────────────────────────────────────────────────
  ruru-mcp is installed + enabled but NOT started: it has no secrets yet.
  Run these two once (the password is yours — I don't type it), then start it:

    sudo -u ruru-mcp ruru-mcp --config /etc/ruru/mcp.toml set-password   # prompts / stdin
    sudo -u ruru-mcp ruru-mcp --config /etc/ruru/mcp.toml mint-key        # prints the bearer ONCE
    sudo systemctl start ruru-mcp.service

  ─────────────────────────────────────────────────────────────────────────────
EOF
fi

say "done. journalctl -u ruru-source -u ruru-mcp -f  to watch."
