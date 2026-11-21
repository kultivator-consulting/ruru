# ruru-mcp

The HTTP MCP endpoint an AI agent talks to, plus the admin panel and the whiteboard
pipeline (dewarp / crop / downscale). A client of `ruru-source`; see the [workspace
README](../../README.md) for the wider picture.

- **`POST /mcp`** — JSON-RPC 2.0, bearer-gated. What the agent calls.
- **`/`** — admin SPA (password login): live preview, camera controls (persisted), key +
  password management, and board **calibration** (`/calibration`). Embedded in the binary.
- **`/captures/<id>.jpg`** — retained snapshots, served as unguessable public links.

## Tools

| Tool | Arguments | Returns |
|---|---|---|
| `capture_whiteboard` | `mode?` — `dewarped` (default) or `high_contrast` | The board, dewarped head-on and downscaled to ≤1568 px. **The normal read.** |
| `capture_region` | `x,y,w,h` (0..1, in **dewarped** space), `mode?` | A sub-rectangle at **full source resolution** — the escape hatch for dense detail the downscaled board softens. |
| `capture_raw` | — | The raw camera frame: no dewarp, crop or downscale. Debugging / re-calibration. |
| `list_capabilities` | — | Geometry, calibration state, downscale budget, available modes. |

`capture_whiteboard` and `capture_region` return an image content block (`image/jpeg`); when
capture retention is configured they also return a text block with a **view link**
(`/captures/<id>.jpg`) the agent can hand to the user. On failure the model gets
`isError: true` and a text reason, so it can react rather than crash.

---

## Point a fresh agent at Ruru

Everything an agent needs is a **URL** and a **bearer key**. Ruru is LAN-only; there is no
public endpoint and no account.

### 1. Get a key

Bearer keys are minted on the host and shown **once** (stored only as a SHA-256 hash):

```sh
ruru-mcp --config /etc/ruru/mcp.toml mint-key
#  -> ruru_XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX   (copy it now)
```

### 2. Configure the client

The endpoint is `http://<ruru-host>:<port>/mcp` with `Authorization: Bearer <key>`.

**Claude Code** — add an HTTP MCP server:

```sh
claude mcp add --transport http ruru http://<ruru-host>:8080/mcp \
  --header "Authorization: Bearer ruru_XXXX"
```

or drop it into a project's `.mcp.json`:

```json
{
  "mcpServers": {
    "ruru": {
      "type": "http",
      "url": "http://<ruru-host>:8080/mcp",
      "headers": { "Authorization": "Bearer ruru_XXXX" }
    }
  }
}
```

**OpenCode** (and any other HTTP-MCP client) is the same three facts — a remote/HTTP
transport, the `/mcp` URL, and the bearer header, e.g. in `opencode.json`:

```json
{
  "mcp": {
    "ruru": {
      "type": "remote",
      "url": "http://<ruru-host>:8080/mcp",
      "headers": { "Authorization": "Bearer ruru_XXXX" },
      "enabled": true
    }
  }
}
```

### 3. Use it

Once connected, the agent just asks — "what's on the whiteboard?" — and calls
`capture_whiteboard`. For a dense corner it can't quite read, `capture_region` with the
normalized rectangle (in **dewarped** coordinates) pulls that patch at full resolution.

To drive `/mcp` directly without an MCP client, it is plain JSON-RPC over HTTP:

```sh
KEY=ruru_XXXX; URL=http://<ruru-host>:8080/mcp
H=(-H "Authorization: Bearer $KEY" -H 'Content-Type: application/json')

# handshake
curl -s "${H[@]}" -d '{"jsonrpc":"2.0","id":1,"method":"initialize",
  "params":{"protocolVersion":"2024-11-05","capabilities":{},
            "clientInfo":{"name":"my-agent","version":"1"}}}' "$URL"

# list tools, then read the board (image comes back base64 in content[0].data)
curl -s "${H[@]}" -d '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' "$URL"
curl -s "${H[@]}" -d '{"jsonrpc":"2.0","id":3,"method":"tools/call",
  "params":{"name":"capture_whiteboard"}}' "$URL"
```

A missing or wrong key is rejected with **HTTP 401** before any tool runs.

---

## Calibrate first

`capture_whiteboard` only returns a *dewarped* board once someone has marked the four
corners. Until then it falls back to the raw frame (and `list_capabilities` reports
`calibrated: false`). Calibrate in the admin panel at **`/calibration`**: drag the four
handles onto the board edges on the live view, set the measured width : height so text
isn't stretched, and it saves as you drag. Corners persist in `calibration.json` and
survive restarts.

## Deploy (systemd)

`ruru-mcp` runs as the non-root `ruru-mcp` user (group `ruru`, **not** `video` — it reaches
frames only through the source socket, never the device). The SPA is baked into the binary
by rust-embed, so there is no Node runtime in production — the binary serves `/` itself.

```sh
make release                    # static musl binary, SPA embedded
sudo bash deploy/install.sh     # install both daemons; see docs/deploy-runbook.md
# first-run, as the ruru-mcp user:
sudo -u ruru-mcp ruru-mcp --config /etc/ruru/mcp.toml set-password
sudo -u ruru-mcp ruru-mcp --config /etc/ruru/mcp.toml mint-key
sudo systemctl start ruru-mcp.service
```

Config is `/etc/ruru/mcp.toml`; secrets (0600) and calibration live in `/var/lib/ruru`. The
full build → install → verify → operate sequence is in
[`docs/deploy-runbook.md`](../../docs/deploy-runbook.md).

## Notes

- **Downscale budget ~1568 px** on the longest edge of `capture_whiteboard` — enough for a
  model to read a whole board, small enough to stay cheap. Reach for `capture_region` when
  a specific area needs more.
- **Region coordinates are in dewarped space**, not raw-camera space — `(0,0)` is the
  top-left of the head-on board, `(1,1)` the bottom-right.
- A higher-resolution source makes `capture_region` sharper. The Brio does 3840×2160 MJPG;
  `capture_whiteboard` still downscales to the budget, so 4K costs nothing on the normal
  read but gives the escape hatch far more detail to crop from.
- **Retained links (optional).** With `[captures]` set (`dir` + `public_base_url`), each
  whiteboard/region capture is saved and the response carries an unguessable
  `/captures/<id>.jpg` link the user can open to see the exact frame. Served unauthenticated —
  the id is the access control — and pruned by age + count. Off unless configured.
- **Admin hardening.** Sessions expire after `[server].session_max_age_secs` (default 12 h)
  and every password change signs out all other sessions. Five failed logins from one
  address lock it out for 15 minutes (HTTP 429). At most `[server].max_concurrent_captures`
  (default 2) image-processing calls run at once; extra calls get a "busy" tool error instead
  of queueing up 4K dewarps. See [SECURITY.md](../../SECURITY.md) for the full threat model.
- **Self-describing.** `initialize` returns a server `instructions` string, so any MCP harness
  learns the when/how (normal read vs region, the board is pre-dewarped, re-capture over time)
  with no bespoke skill file.
