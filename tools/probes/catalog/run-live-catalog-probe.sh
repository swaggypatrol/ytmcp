#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"

if [[ "${YTM_GLASS_ALLOW_LIVE:-0}" != "1" ]]; then
  echo "NOT_RUN: live catalog probe skipped (set YTM_GLASS_ALLOW_LIVE=1 to enable)"
  exit 0
fi

cd "${REPO_ROOT}"

if [[ -z "${YTM_GLASS_LIVE_COOKIE_FILE:-}" ]] && [[ -t 0 ]]; then
  echo "BLOCKED: set YTM_GLASS_LIVE_COOKIE_FILE or pipe cookie header via stdin (never pass secrets in argv)"
  exit 2
fi

echo "Running explicit live catalog probe (YTM_GLASS_ALLOW_LIVE=1)..."
cargo test -p ytm-data --test '*' -- --ignored --nocapture 2>/dev/null || true
cargo test -p ytm-data probe::tests::live_catalog_probe -- --ignored --nocapture
