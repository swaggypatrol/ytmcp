set shell := ["bash", "-euo", "pipefail", "-c"]

export CARGO_TARGET_DIR := env_var_or_default("CARGO_TARGET_DIR", justfile_directory() + "/target")

# Check toolchain, Wayland/KDE/GPU/Audio/Secret-Service environment and lockfile hashes
doctor:
    @echo "=== YTMcp Doctor ==="
    @date -Iseconds
    @echo "git_head: $(git rev-parse --short HEAD 2>/dev/null || echo 'UNCOMMITTED') ($(git status --porcelain | wc -l) dirty files)"
    @uname -srmo
    @echo "XDG_SESSION_TYPE=${XDG_SESSION_TYPE:-unset} WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-unset} XDG_CURRENT_DESKTOP=${XDG_CURRENT_DESKTOP:-unset}"
    @command -v plasmashell >/dev/null && plasmashell --version || echo "plasmashell: MISSING"
    @command -v kwin_wayland >/dev/null && kwin_wayland --version || echo "kwin_wayland: MISSING"
    @command -v nvidia-smi >/dev/null && nvidia-smi --query-gpu=name,driver_version --format=csv,noheader || echo "nvidia-smi: MISSING"
    @rustc --version
    @cargo --version
    @node --version
    @pnpm --version
    @just --version
    @if [ -x apps/desktop/node_modules/.bin/electron ]; then echo "electron: $(./apps/desktop/node_modules/.bin/electron --version)"; else echo "electron: MISSING"; fi
    @command -v pactl >/dev/null && pactl info | grep -E 'Server Name|Server Version' || echo "pactl: MISSING"
    @busctl --user list 2>/dev/null | grep -E 'org\.freedesktop\.secrets|kwallet|StatusNotifierWatcher' || true
    @fc-list : family | grep -iE 'Inter|Noto Sans CJK' | sort -u | head -n 10 || true
    @sha256sum config/app-identity.json Cargo.lock pnpm-lock.yaml 2>/dev/null || true

# Generate JSON Schema and TypeScript definitions from crates/protocol
gen-protocol:
    cargo run -p ytm-protocol --bin gen_schema -- packages/protocol/schema/protocol.v1.schema.json
    pnpm --filter @ytmcp/protocol run generate:types

# Generate the external Control v1 schema and TypeScript types; leaves host wire v1 unchanged
gen-control:
    cargo run -p ytm-control-api --bin gen_control_schema -- packages/protocol/schema/control.v1.schema.json
    pnpm --filter @ytmcp/protocol run generate:types --control

# Typecheck all Rust crates and TypeScript packages
types-check:
    cargo check --workspace --all-targets
    pnpm --filter @ytmcp/protocol run typecheck
    pnpm --filter @ytmcp/bridge run typecheck
    pnpm --filter @ytmcp/desktop run typecheck
    pnpm --filter @ytmcp/ui run check

# Run offline and mock tests across Rust and TypeScript
test:
    cargo test --workspace --locked
    cargo build --locked -p ytm-daemon
    pnpm -r test
    YTMCP_DAEMON_BIN="{{CARGO_TARGET_DIR}}/debug/ytm-daemon" pnpm run test:process
    pnpm run test:desktop
    pnpm run test:e2e

# Exercise the SDK stdio adapter and private socket against a single synthetic daemon
test-control:
    cargo build -p ytm-daemon -p ytmcp
    python3 tools/mcp/offline-smoke.py "{{CARGO_TARGET_DIR}}/debug/ytm-daemon" "{{CARGO_TARGET_DIR}}/debug/ytmcp"
    python3 -m unittest tools/mcp/test_dj_harness.py

# Run linters and formatting checks
lint:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    pnpm --filter @ytmcp/protocol run typecheck
    pnpm --filter @ytmcp/bridge run typecheck
    pnpm --filter @ytmcp/desktop run typecheck
    pnpm --filter @ytmcp/ui run check

# Run offline bridge DOM/fixture checks and report live bridge status
bridge-check:
    pnpm --filter @ytmcp/bridge run test
    @echo "Live music.youtube.com bridge check is isolated from offline CI; run 'just test-live' when logged in."

# Run desktop/UI in explicit mock/fixture mode
dev-mock:
    pnpm --filter @ytmcp/ui run build
    pnpm --filter @ytmcp/desktop run build
    YTMCP_MODE=mock ./apps/desktop/node_modules/.bin/electron apps/desktop/dist/main.mjs

# Build and run desktop app against real music.youtube.com
dev:
    cargo build -p ytm-daemon
    pnpm --filter @ytmcp/bridge run build
    pnpm --filter @ytmcp/ui run build
    pnpm --filter @ytmcp/desktop run build
    YTMCP_DAEMON_BIN="{{CARGO_TARGET_DIR}}/debug/ytm-daemon" ./apps/desktop/node_modules/.bin/electron apps/desktop/dist/main.mjs

# Build release artifacts (Rust daemon, bridge, UI, desktop shell)
build-release:
    cargo build --release -p ytm-daemon -p ytmcp
    pnpm --filter @ytmcp/bridge run build
    pnpm --filter @ytmcp/ui run build
    pnpm --filter @ytmcp/desktop run build

# Run isolated live tests (requires explicit opt-in and active logged-in session)
test-live:
    @if [ "${YTMCP_ALLOW_LIVE:-0}" != "1" ]; then echo "Refusing live test without YTMCP_ALLOW_LIVE=1"; exit 2; fi
    cargo test -p ytm-data --features live-tests -- --ignored

# Build the Arch package from the current clean HEAD (no sudo; output in packaging/out)
package:
    tools/package/build-package.sh

# Optional daily-use recorder for the installed app, outside the development and release gates
test-soak:
    node tools/qa/soak.mjs
