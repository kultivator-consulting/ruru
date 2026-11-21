# Security policy

Ruru puts a camera on a network, so we take reports seriously.

## Reporting a vulnerability

**Please don't open a public issue for security problems.** Report them privately through
GitHub's [private vulnerability reporting](https://github.com/kultivator-consulting/ruru/security/advisories/new)
("Report a vulnerability" on the repo's **Security** tab).

Please include what you found, how to reproduce it, and the impact you think it has. We aim
to acknowledge reports within a few working days and will keep you updated as we fix it.
We're happy to credit you in the release notes unless you'd rather stay anonymous.

## Supported versions

Ruru is pre-1.0. Security fixes land on `main` and in the next release. There are no
long-term support branches.

## Threat model

Ruru is designed for a **trusted LAN behind a firewall**. Reports are most useful when they
fit that model. The things it does and doesn't try to protect against:

**In scope**

- Bypassing the bearer key on `/mcp`, or the password and session on the admin panel.
- Reading or guessing retained capture links (`/captures/<id>.jpg`) without being given them.
- Path traversal, or reading files other than retained captures through any route.
- A process that isn't in the `ruru` group reaching the source socket.
- `ruru-mcp` gaining access to the camera device directly. It should only ever see frames
  through the socket.
- Crashes or resource exhaustion that an *unauthenticated* LAN client can trigger.
- Secrets (bearer key, password, cookie key) leaking into logs, responses or world-readable
  files.

**Out of scope, by design**

- **Plain HTTP.** The bearer key and admin password cross the LAN in cleartext unless you put
  a TLS proxy in front (see the [deploy runbook](docs/deploy-runbook.md)). Anyone who can
  sniff your LAN can capture them.
- **Capture links are unauthenticated.** Anyone holding a link can open that image until it
  is pruned. Retention is off unless you configure it.
- **One shared bearer key.** All MCP clients share it, and rotating it revokes all of them.
- Attackers with root, or with membership in the `ruru` or `video` groups, on the host.
- Exposing port 8080 to the internet. Don't.

## Hardening already in place

- Bearer keys are 256-bit random, stored as SHA-256, and compared in constant time.
- Admin passwords use argon2id. Logins are throttled per IP address, and concurrent
  verifications are bounded.
- Session cookies are encrypted and authenticated, expire after 12 hours (configurable), and
  are revoked by any password change.
- Secrets live in a `0600` file. The configs in `/etc` hold no secrets.
- Two non-root service users. Only `ruru-source` is in `video`. Both units use systemd
  sandboxing.
- Anti-framing and `nosniff` headers on every response, and a request timeout.
- Socket messages are size-capped. If `ruru-source` can't apply its socket group, it refuses
  to start.
