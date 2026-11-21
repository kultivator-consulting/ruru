# Contributing to Ruru

Thanks for your interest! Bug reports, camera compatibility notes and PRs are all welcome.

## Before you start

- **Bugs and small fixes:** open a PR directly, or an issue if you're not sure it's a bug.
- **Bigger changes** (new tools, protocol changes, new consumers): please open an issue
  first so we can agree on the shape before you sink time into it.
- **Security issues:** don't open a public issue. See [SECURITY.md](SECURITY.md).
- **"It works with my camera":** an issue with the model, `v4l2-ctl -L` output and the
  `[camera]` config you used is really helpful.

## Dev setup

You need Rust ≥ 1.88 (`rustup` recommended), Node ≥ 22.12, and Linux (the V4L2 backend and
Unix-socket permissions are Linux-specific). No camera is needed, because everything runs
against the mock.

```sh
make web-install    # once
make check          # fmt-check + clippy -D warnings + Rust tests + web typecheck/tests
```

`make check` is what CI runs, so run it before pushing. `make help` lists every target.

To run the stack locally, see the Quickstart in the [README](README.md). `make web-dev` runs
the Vite dev server with hot reload, proxying `/api` and `/mcp` to `ruru-mcp` on `:8080`.

### The socket-group test

`socket_group_is_applied_when_configured` needs a real `ruru` group and skips itself without
one. To run it for real:

```sh
sudo groupadd --system ruru && sudo usermod -aG ruru "$USER"   # then log out and back in
RURU_REQUIRE_GROUP=1 cargo test -p ruru-source
```

With `RURU_REQUIRE_GROUP=1` set, a missing group fails the test instead of skipping it. CI
runs it this way.

### Trying it on a real camera

The automated tests all use the mock. To try a change against real hardware, copy
`config/dev.toml` to `config/local.toml` (gitignored), set `backend = "v4l2"` and your
`/dev/v4l/by-id/…` device path, then:

```sh
cargo run -p ruru-source -- --config config/local.toml
make tap    # or make mcp, and use the admin panel's live preview
```

Your user needs to be in the `video` group to open the device.

## Guidelines

- **Keep the boundaries.** `ruru-source` stays domain-agnostic: no whiteboard logic, and no
  secrets. Image processing belongs in `ruru-mcp` or in a new consumer.
- **Tests with behaviour changes.** Integration tests in `crates/*/tests` drive the real HTTP
  and socket surfaces against the mock camera. Prefer those for anything user-visible.
- **Comments explain *why*.** The codebase leans on comments for the non-obvious decisions
  (ordering of camera controls, security trade-offs). Keep that up.
- `cargo fmt` and clippy-clean with `-D warnings`.
- Commit messages: a short imperative summary line, then the reasoning if it isn't obvious.

## License

By contributing, you agree that your contributions are dual licensed under MIT OR
Apache-2.0, the same as the project, without any additional terms or conditions.
