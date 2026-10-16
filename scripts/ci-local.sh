#!/usr/bin/env bash
# Local equivalent of the CI workflow (Linux job set): the verification block of
# AGENTS.md section 5. Usage: scripts/ci-local.sh [--no-deny] [--size]
#   --no-deny  skip cargo deny
#   --size     also build the release binary and enforce the size budget (slow;
#              budget from ALKAHEST_SIZE_BUDGET_MIB, default 60)
set -euo pipefail
cd "$(dirname "$0")/.."

run() { echo "==> $*"; "$@"; }

deny=1
size=0
for arg in "$@"; do
  case "$arg" in
    --no-deny) deny=0 ;;
    --size) size=1 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

run cargo fmt --all --check
run cargo clippy --workspace --all-targets --locked -- -D warnings
if command -v cargo-nextest >/dev/null 2>&1; then
  run cargo nextest run --workspace --locked --profile ci
else
  run cargo test --workspace --locked
fi
if [[ "$deny" == 1 ]]; then
  run cargo deny check
fi
run cargo xtask check-deps
run cargo build -p alkahest --no-default-features --locked
if [[ "$size" == 1 ]]; then
  run scripts/binary-size.sh
fi
if command -v typos >/dev/null 2>&1; then run typos; fi
echo "ci-local: all checks passed"
