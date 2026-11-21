# Ruru dev loop.
#
# Logic tests run locally in seconds against a mock camera; no hardware needed.
# `make help` lists everything.

CARGO       ?= cargo
NPM         ?= npm
MUSL        := x86_64-unknown-linux-musl
DEV_CONFIG  ?= config/dev.toml
MCP_CONFIG  ?= config/mcp-dev.toml
DEV_SOCKET  ?= /tmp/ruru-source.sock
WEB         := web

.PHONY: help build test fmt fmt-check lint check run tap mcp \
        web-install web-dev web-build web-test \
        release install clean

help:
	@echo "Local — backend"
	@echo "  make build         debug build, whole workspace"
	@echo "  make test          rust unit + acceptance tests"
	@echo "  make check         fmt-check + clippy + rust tests + web test (pre-commit)"
	@echo "  make run           run ruru-source (mock) against $(DEV_CONFIG)"
	@echo "  make tap           subscribe to a running source and print frame stats"
	@echo "  make mcp           run ruru-mcp against $(MCP_CONFIG)"
	@echo ""
	@echo "Local — frontend (Vite + Vue SPA in $(WEB)/)"
	@echo "  make web-install   npm install"
	@echo "  make web-dev       Vite dev server (proxies /api,/mcp to :8080)"
	@echo "  make web-build     vite build -> web/dist (embedded by ruru-mcp)"
	@echo "  make web-test      vitest + typecheck"
	@echo ""
	@echo "Deploy — this host (systemd, non-root); see docs/deploy-runbook.md"
	@echo "  make release       static musl binaries (builds the SPA first)"
	@echo "  make install       release, then sudo deploy/install.sh (both daemons)"

build:
	$(CARGO) build --workspace

test:
	$(CARGO) test --workspace

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all -- --check

lint:
	$(CARGO) clippy --workspace --all-targets -- -D warnings

check: fmt-check lint test web-test

run:
	$(CARGO) run -p ruru-source -- --config $(DEV_CONFIG)

tap:
	$(CARGO) run -p ruru-tap -- --socket $(DEV_SOCKET)

mcp:
	$(CARGO) run -p ruru-mcp -- --config $(MCP_CONFIG)

# --- frontend -------------------------------------------------------------

web-install:
	cd $(WEB) && $(NPM) install

web-dev:
	cd $(WEB) && $(NPM) run dev

web-build:
	cd $(WEB) && $(NPM) run build

web-test:
	cd $(WEB) && $(NPM) run typecheck && $(NPM) run test

# --- release / deploy -----------------------------------------------------

# Static musl: one self-contained file per daemon, immune to glibc skew, with no runtime
# dependencies on the target. The SPA is built first so rust-embed bakes it
# into ruru-mcp.
release: web-build
	rustup target add $(MUSL)
	$(CARGO) build --release --target $(MUSL) --workspace

# This host: build, then run the privileged installer (creates users + systemd units).
install: release
	sudo bash deploy/install.sh

clean:
	$(CARGO) clean
	rm -rf $(WEB)/dist $(WEB)/node_modules
