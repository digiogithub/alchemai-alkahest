#!/usr/bin/env bash
# Test harness for install.sh: runs it in a throwaway HOME with a fake `alkahest` binary,
# so nothing is downloaded and the real home is never touched.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
script="$here/install.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

export HOME="$work/home"
mkdir -p "$HOME"
log="$work/calls.log"

# Fake binary: records its arguments and exits 0.
fake="$work/fake-alkahest"
printf '#!/bin/sh\necho "$@" >> "%s"\n' "$log" > "$fake"
chmod +x "$fake"

fail() { echo "FAIL: $*" >&2; exit 1; }
expect_contains() { grep -q -- "$2" <<<"$1" || fail "expected '$2' in output: $1"; }

# 1. --dry-run changes nothing and prints the plan.
out="$("$script" --dry-run --from-local "$fake" --variant cpu --service)"
expect_contains "$out" "\[dry-run\].*engines install llama-cpp --variant cpu"
expect_contains "$out" "\[dry-run\].*service install --user"
[ ! -e "$HOME/.local/bin/alkahest" ] || fail "dry-run installed the binary"

# 2. Dry-run of a download prints the checksum file and the placeholder URL.
out="$("$script" --dry-run --version 1.2.3)"
expect_contains "$out" "SHA256SUMS"
expect_contains "$out" "alkahest-1.2.3-"

# 3. Real install into the temp HOME, engine skipped.
"$script" --from-local "$fake" --no-engine >/dev/null
[ -x "$HOME/.local/bin/alkahest" ] || fail "binary not installed under ~/.local/bin"
[ ! -e "$log" ] || fail "--no-engine still ran the engine installer"

# 4. Idempotent: a second run reports up to date and succeeds.
out="$("$script" --from-local "$fake" --no-engine)"
expect_contains "$out" "already up to date"

# 5. Custom prefix with the engine step and service.
"$script" --from-local "$fake" --prefix "$work/prefix" --variant vulkan --service >/dev/null
[ -x "$work/prefix/bin/alkahest" ] || fail "--prefix ignored"
grep -q "engines install llama-cpp --variant vulkan" "$log" || fail "engine install not invoked"
grep -q "service install --user" "$log" || fail "service install not invoked"

# 6. Bad input fails.
if "$script" --bogus >/dev/null 2>&1; then fail "unknown option accepted"; fi
if "$script" --from-local "$work/missing" --no-engine >/dev/null 2>&1; then
  fail "missing --from-local file accepted"
fi

echo "install.sh tests passed"
