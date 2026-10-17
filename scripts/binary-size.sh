#!/usr/bin/env bash
# Builds the headless release binary, prints its size and startup times, and fails above the
# budgets (ALK-US-0002, ALK-US-0078).
#
# Budgets (env, defaults in brackets):
#   ALKAHEST_SIZE_BUDGET_MIB       release binary size                     [30]
#   ALKAHEST_CLI_BUDGET_MS         median `alkahest --version` wall time    [50]
#   ALKAHEST_SERVE_BUDGET_MS       `alkahest serve` cold start until /healthz answers [500]
# Other env: ALKAHEST_SKIP_BUILD=1 reuses target/release/alkahest; ALKAHEST_BENCH_RUNS (21)
# is the number of `--version` runs.
#
# Startup timing uses a throw-away ALKAHEST_HOME, so no user data is read or written.
set -euo pipefail
cd "$(dirname "$0")/.."

size_budget_mib="${ALKAHEST_SIZE_BUDGET_MIB:-30}"
cli_budget_ms="${ALKAHEST_CLI_BUDGET_MS:-50}"
serve_budget_ms="${ALKAHEST_SERVE_BUDGET_MS:-500}"
runs="${ALKAHEST_BENCH_RUNS:-21}"

if [[ "${ALKAHEST_SKIP_BUILD:-0}" != 1 ]]; then
  cargo build -p alkahest --release --no-default-features --locked
fi
bin="target/release/alkahest"
[[ -f "$bin" ]] || bin="target/release/alkahest.exe"

fail=0

# --- size ------------------------------------------------------------------------------
bytes="$(wc -c <"$bin" | tr -d ' ')"
mib="$(awk -v b="$bytes" 'BEGIN { printf "%.2f", b / 1048576 }')"
echo "binary size: ${bytes} bytes (${mib} MiB), budget ${size_budget_mib} MiB"
if ((bytes > size_budget_mib * 1048576)); then
  echo "error: binary exceeds the ${size_budget_mib} MiB budget" >&2
  fail=1
fi

# --- startup ---------------------------------------------------------------------------
home="$(mktemp -d)"
server_pid=""
cleanup() {
  if [[ -n "$server_pid" ]]; then kill "$server_pid" 2>/dev/null || true; fi
  rm -rf "$home"
}
trap cleanup EXIT
export ALKAHEST_HOME="$home"

now_ns() { date +%s%N; }

# Median wall time of `alkahest --version` over $runs runs, in milliseconds.
samples=()
"$bin" --version >/dev/null # warm the page cache
for _ in $(seq "$runs"); do
  start="$(now_ns)"
  "$bin" --version >/dev/null
  samples+=($(((($(now_ns) - start)) / 1000)))
done
median_us="$(printf '%s\n' "${samples[@]}" | sort -n | sed -n "$(((runs + 1) / 2))p")"
median_ms="$(awk -v u="$median_us" 'BEGIN { printf "%.1f", u / 1000 }')"
echo "cli start (--version, median of ${runs}): ${median_ms} ms, budget ${cli_budget_ms} ms"
if ((median_us > cli_budget_ms * 1000)); then
  echo "error: CLI start exceeds the ${cli_budget_ms} ms budget" >&2
  fail=1
fi

# Cold start of `serve` until /healthz answers, on a free port.
port="$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')"
start="$(now_ns)"
"$bin" serve --host "127.0.0.1:${port}" >/dev/null 2>&1 &
server_pid=$!
ready=0
for _ in $(seq 1000); do
  if curl -fsS -o /dev/null "http://127.0.0.1:${port}/healthz" 2>/dev/null; then
    ready=1
    break
  fi
  kill -0 "$server_pid" 2>/dev/null || break
  sleep 0.005
done
if [[ "$ready" != 1 ]]; then
  echo "error: serve did not answer /healthz" >&2
  exit 1
fi
serve_ms="$((($(now_ns) - start) / 1000000))"
echo "serve cold start to ready: ${serve_ms} ms, budget ${serve_budget_ms} ms (includes curl poll interval)"
if ((serve_ms > serve_budget_ms)); then
  echo "error: serve start exceeds the ${serve_budget_ms} ms budget" >&2
  fail=1
fi
exit "$fail"
