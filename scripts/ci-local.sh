#!/usr/bin/env bash
# Local equivalent of the CI workflow (Linux job set): the verification block of
# AGENTS.md section 5. Usage: scripts/ci-local.sh [--no-deny] [--size] [--desktop]
#   --no-deny  skip cargo deny
#   --size     also build the release binary and enforce the size budget (slow;
#              budget from ALKAHEST_SIZE_BUDGET_MIB, default 30)
#   --desktop  also run the desktop job: tokens --check --contrast, clippy and GUI tests
#              with the desktop feature, the desktop build and the first-frame smoke test
#              (under xvfb-run when installed, else on $DISPLAY / $WAYLAND_DISPLAY)
set -euo pipefail
cd "$(dirname "$0")/.."

run() { echo "==> $*"; "$@"; }

deny=1
size=0
desktop=0
for arg in "$@"; do
  case "$arg" in
    --no-deny) deny=0 ;;
    --size) size=1 ;;
    --desktop) desktop=1 ;;
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
run cargo xtask notice --check
run cargo xtask tokens --check --contrast
run cargo build -p alkahest --no-default-features --locked
run cargo xtask docs-cli --check
if command -v mdbook >/dev/null 2>&1; then run mdbook build docs/book; fi
if [[ "$size" == 1 ]]; then
  run scripts/binary-size.sh
fi
if [[ "$desktop" == 1 ]]; then
  run cargo clippy -p alkahest-desktop -p alkahest \
    --features alkahest-desktop/gui-test,alkahest/desktop --all-targets --locked -- -D warnings
  run cargo test -p alkahest-desktop --features gui-test --locked
  run cargo build -p alkahest --features desktop --locked
  smoke_home="$(mktemp -d)"
  if command -v xvfb-run >/dev/null 2>&1; then
    run env ALKAHEST_HOME="$smoke_home" xvfb-run -a -s "-screen 0 1440x900x24" \
      target/debug/alkahest desktop --smoke-test
  else
    run env ALKAHEST_HOME="$smoke_home" target/debug/alkahest desktop --smoke-test
  fi
  rm -rf "$smoke_home"
fi
if command -v typos >/dev/null 2>&1; then run typos; fi
echo "ci-local: all checks passed"
