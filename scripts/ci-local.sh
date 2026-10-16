#!/usr/bin/env bash
# Local equivalent of .github/workflows/ci.yml (Linux job set): the verification block of
# AGENTS.md section 5. Usage: scripts/ci-local.sh [--no-deny]
set -euo pipefail
cd "$(dirname "$0")/.."

run() { echo "==> $*"; "$@"; }

run cargo fmt --all --check
run cargo clippy --workspace --all-targets --locked -- -D warnings
if command -v cargo-nextest >/dev/null 2>&1; then
  run cargo nextest run --workspace --locked --profile ci
else
  run cargo test --workspace --locked
fi
if [[ "${1:-}" != "--no-deny" ]]; then
  run cargo deny check
fi
run cargo xtask check-deps
run cargo build -p alkahest --no-default-features --locked
if command -v typos >/dev/null 2>&1; then run typos; fi
echo "ci-local: all checks passed"
