# ruru-source

The daemon that owns the camera. One capture loop holds the V4L2 device; every consumer —
`ruru-mcp`, or anything you write with `ruru-client` — gets frames over a Unix socket. It is
**domain-agnostic**: it knows cameras and frames, nothing about whiteboards, and it holds
**no secrets**. See the [workspace README](../../README.md) for the socket protocol.

## Why a separate daemon

A UVC camera has exactly one streaming owner — the kernel won't let two processes open the
device at once — but we want several consumers over time. So one daemon owns the device and
fans frames out, latest-frame-wins, with ref-counted activation: the camera is only open
while something is actually attached (an always-on office camera is a surveillance
capability, so idle-close is the default, and every enable/disable logs loudly).

## Build

```sh
make release          # static musl binary at target/x86_64-unknown-linux-musl/release/ruru-source
```

Static-PIE musl: one self-contained file, no glibc skew, no runtime deps. `v4l` runs in
pure-ioctl mode (no libv4l C dependency), so the musl build is clean.

## Config

Installed to `/etc/ruru/source.toml`. [`deploy/source.toml`](../../deploy/source.toml) is the
fully-annotated production config. The load-bearing bits:

| Key | Notes |
|---|---|
| `camera.device` | **`/dev/v4l/by-id/…`, never `/dev/video0`.** UVC cameras like the Brio expose several nodes and renumber them on replug; only the by-id path is stable, and the config layer rejects anything else. `ls -l /dev/v4l/by-id/` to get the exact string — the serial is part of it. |
| `camera.controls` | Locked optics, reapplied on every open. **Order matters**: each auto gate (`white_balance_automatic`, `auto_exposure`, `focus_automatic_continuous`) must be listed *before* the control it unlocks. |
| `source.socket_path` | `/run/ruru/source.sock` — created by systemd's `RuntimeDirectory=ruru`. |
| `source.socket_group` | `ruru`. The socket is `0660` group `ruru`; if the chown fails the daemon **exits** rather than downgrade the boundary. |
| `source.idle_grace_secs` | How long the device stays open after the last consumer detaches. |
| `source.controls_state_path` | Where admin control tuning is persisted (a writable state dir — `/etc` is read-only to the hardened unit). Layered over `[camera.controls]` on every open, so operator tuning survives a restart. Unset (dev/mock) ⇒ tuning stays runtime-only. |

## Run

Deployed, it runs under systemd as the non-root `ruru-source` user (groups `video` + `ruru`);
`make release && sudo bash deploy/install.sh` sets that up — see
[`docs/deploy-runbook.md`](../../docs/deploy-runbook.md). Locally against the mock:

```sh
make run              # ruru-source with the mock camera (no hardware, no groups)
make tap              # a subscriber, to prove fan-out
```

## Operate

```sh
systemctl status ruru-source
journalctl -u ruru-source -f          # structured logs; every device open/close is logged
```

A capture failure closes the handle, backs off `reopen_backoff_secs`, and reopens from
scratch by re-resolving the by-id symlink — that is the replug path, and it is automatic.
`ruru-mcp` restarting or dying does not affect this daemon at all.
