#!/usr/bin/env bash
# Builds the headless release binary, prints its size and fails above the budget.
# Env: ALKAHEST_SIZE_BUDGET_MIB (default 60).
set -euo pipefail
cd "$(dirname "$0")/.."

budget_mib="${ALKAHEST_SIZE_BUDGET_MIB:-60}"
cargo build -p alkahest --release --no-default-features --locked
bin="target/release/alkahest"
[[ -f "$bin" ]] || bin="target/release/alkahest.exe"
bytes="$(wc -c <"$bin" | tr -d ' ')"
mib="$(awk -v b="$bytes" 'BEGIN { printf "%.2f", b / 1048576 }')"
echo "binary size: ${bytes} bytes (${mib} MiB), budget ${budget_mib} MiB"
if ((bytes > budget_mib * 1048576)); then
  echo "error: binary exceeds the ${budget_mib} MiB budget" >&2
  exit 1
fi
