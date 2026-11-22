# Deploy runbook

How to build, install, first-run and operate the two daemons as non-root systemd services on
the Linux machine the camera is plugged into.

> **Run on bare metal, not in a VM.** UVC video is isochronous USB. QEMU's emulated xHCI
> controller can't carry it reliably at 4K, and device-level USB passthrough stalls in ways
> that look like a broken camera. If you must virtualise, pass through a whole PCIe USB
> controller rather than the device. Plugging the camera straight into the host is simplest.

**Requirements:** a systemd-based Linux (developed on Ubuntu), a UVC camera on a USB 3 port
(check `lsusb -t` shows `5000M`; at USB 2 speeds many cameras silently drop to 1080p), and a
Rust toolchain + Node on the build machine.

Everything below is driven by two files: [`deploy/install.sh`](../deploy/install.sh) and
[`deploy/uninstall.sh`](../deploy/uninstall.sh). The units and configs they install live
alongside them in [`deploy/`](../deploy).

## What gets installed

| Path | What | Owner / mode |
|---|---|---|
| `/usr/local/bin/ruru-source`, `ruru-mcp` | static musl binaries | root, 0755 |
| `/etc/ruru/source.toml`, `mcp.toml` | config, no secrets | root, 0644 |
| `/var/lib/ruru/mcp-secrets.toml` | bearer hash + admin argon2id + cookie key | ruru-mcp, **0600** |
| `/var/lib/ruru/calibration.json` | board corners + aspect (written by the admin panel) | ruru-mcp, 0644 |
| `/var/lib/ruru/captures/` | retained capture JPEGs, served as links (if enabled) | ruru-mcp, 0700 dir |
| `/var/lib/ruru-source/controls.json` | persisted admin control tuning | ruru-source, 0700 dir |
| `/run/ruru/source.sock` | the frame socket | ruru-source:ruru, **0660** |
| `/etc/systemd/system/ruru-{source,mcp}.service` | units | root, 0644 |

Two service users, deliberately different reach: **`ruru-source`** in `video` + `ruru` (owns
the camera and the socket); **`ruru-mcp`** in `ruru` only — **not** `video`, so it can never
open the device; frames only ever reach it through the socket.

## 1. Build

```sh
make release
```

Builds the SPA (`vite build` → `web/dist`, baked into `ruru-mcp` by rust-embed — no Node in
production) and both static-musl binaries.

## 2. Install (root)

```sh
sudo bash deploy/install.sh
```

Idempotent — re-run it any time to roll out a new build. It creates the group + users,
installs binaries and units, seeds `/etc/ruru/*.toml` **only if they don't exist yet** (so
your edits survive), enables both units, and starts `ruru-source`. It stops short of
`ruru-mcp` because that needs secrets first.

**Before it can capture, edit `/etc/ruru/source.toml`:** set `[camera].device` to your
camera's `/dev/v4l/by-id/…-video-index0` path (`ls -l /dev/v4l/by-id/`), and check the
resolution, `power_line_frequency` and control names against `v4l2-ctl --list-formats-ext`
and `v4l2-ctl -L`. Then `sudo systemctl restart ruru-source`.

## 3. First-run secrets (once, as the ruru-mcp user)

The admin password is yours — set it directly; it is never stored in plaintext (argon2id).
The bearer key is shown **once** (stored only as SHA-256):

```sh
sudo -u ruru-mcp ruru-mcp --config /etc/ruru/mcp.toml set-password    # password on stdin
sudo -u ruru-mcp ruru-mcp --config /etc/ruru/mcp.toml mint-key         # copy the ruru_… key now
sudo systemctl start ruru-mcp.service
```

Re-running `install.sh` after this sees the secrets and starts `ruru-mcp` for you.

> **Rotate keys and passwords from the admin panel** — that updates the running service
> immediately (and a password change signs out every other admin session). The CLI
> `set-password` / `mint-key` rewrite the secrets file from a separate process, so the
> running `ruru-mcp` only sees them after `sudo systemctl restart ruru-mcp`.

## 4. Verify

```sh
systemctl is-active ruru-source ruru-mcp          # both: active
ps -o user= -C ruru-source -C ruru-mcp            # both: NON-root (ruru-source / ruru-mcp)
stat -c '%a %G' /run/ruru/source.sock             # 660 ruru
stat -c '%a %U' /var/lib/ruru/mcp-secrets.toml    # 600 ruru-mcp
curl -fsS -o /dev/null -w '%{http_code}\n' http://localhost:8080/         # 200 — SPA served
curl -s -o /dev/null -w '%{http_code}\n' -X POST http://localhost:8080/mcp # 401 — bearer required
```

Reconnect check: `sudo systemctl restart ruru-source`, then a `capture_whiteboard`
still succeeds and `ruru-mcp` never restarted — it reconnects on the next capture.

## 5. Operate

```sh
journalctl -u ruru-source -u ruru-mcp -f          # structured logs → journald
sudo systemctl restart ruru-mcp                   # after editing /etc/ruru/mcp.toml
```

Then open `http://<host>:8080/`, sign in, and **calibrate** the board at `/calibration`
(drag the four corners onto the board edges). Pointing an agent at it — endpoint, key, client
config — is in the [`ruru-mcp` README](../crates/ruru-mcp/README.md).

**Admin panel won't load from a phone?** On Android 17 and later, apps need the "Nearby
devices" (local network access) permission to reach LAN addresses. Without it Chrome shows
`ERR_TOO_MANY_RETRIES` and the request never leaves the phone. Allow it under
Settings → Apps → Chrome → Permissions. Also type the URL with `http://`: Ruru serves plain
HTTP unless you add the TLS proxy below.

## Network exposure (do this)

`/mcp` binds `0.0.0.0:8080` so LAN agents can reach it, but it is plain HTTP — the bearer
crosses the LAN in cleartext. Scope the port to your trusted subnet rather than leaving it
open; `ruru-source` needs no port at all (its only API is the Unix socket).

Retained capture links (`/captures/<id>.jpg`) are served **unauthenticated** on this same
port — the id is an unguessable capability, not a secret — so the subnet scoping below is
also what keeps those links LAN-only. Set `[captures].public_base_url` to a LAN address for
the same reason.

```sh
sudo ufw default deny incoming
sudo ufw allow from 192.0.2.0/24 to any port 8080 proto tcp   # replace with YOUR subnet
sudo ufw enable
```

**Never expose port 8080 to the internet.** For remote access use a VPN (WireGuard,
Tailscale) rather than port-forwarding.

### Optional: TLS with a reverse proxy

To stop the bearer key and admin password crossing the network in cleartext, put a TLS proxy
in front and bind `ruru-mcp` to loopback (`[server] bind = "127.0.0.1:8080"`). For example,
with [Caddy](https://caddyserver.com/) and an internal hostname:

```caddyfile
ruru.lan {
    tls internal
    reverse_proxy 127.0.0.1:8080
}
```

Set `[captures].public_base_url` to the `https://` origin too. Behind a proxy every request
appears to come from `127.0.0.1`, so the per-address login lockout becomes global — stricter,
never weaker.

## Uninstall

```sh
sudo bash deploy/uninstall.sh              # removes services + binaries, keeps secrets/calibration
PURGE=1 sudo bash deploy/uninstall.sh      # also removes /etc/ruru, /var/lib/ruru, and the users
```
