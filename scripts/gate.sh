#!/usr/bin/env bash
# Local gate — the same job as `.github/workflows/ci.yml`, without GitHub Actions.
#
# GitHub Actions is disabled for this user (`HTTP 422: Actions has been disabled
# for this user` on workflow_dispatch; `gh run list` is empty). This script is
# the enterprise-grade check for that product form: clippy, workspace tests,
# wasm artifacts, JS/Python hosts, CLI smoke. A GitHub check-run is not claimed.
#
#   bash scripts/gate.sh
#
# What is deliberately NOT here matches ci.yml: rustfmt, Chromium OPFS/browser
# checks, consumer benches that skip a missing sibling corpus.

set -euo pipefail
cd "$(dirname "$0")/.."

step() {
  echo
  echo "== $*"
}

step "clippy (0 warnings is the gate)"
cargo clippy --workspace --all-targets -- -D warnings

step "tests (default-members; index-accel cannot build for the host)"
cargo test --workspace

step "analytics kernel builds for its only target"
cargo build --release -p index-accel --target wasm32-unknown-unknown

step "build every wasm artifact"
bash scripts/build-wasm.sh

step "js host smoke (text engine, shipped artifact)"
node js/smoke.mjs

step "js image smoke (image tier, shipped artifact)"
node js/image-smoke.mjs

step "native shared library + python host (stdlib ctypes only)"
cargo build --release -p index-wasm
python3 host/python/index_ffi.py

step "cli smoke (pipe of rows in, search out)"
bash scripts/cli-smoke.sh

echo
echo "OVERALL: PASS"
