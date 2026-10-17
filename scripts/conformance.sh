#!/usr/bin/env bash
# SDK conformance suite (ALK-US-0038, ALK-US-0037): the official `ollama`, `openai` and
# `anthropic` Python clients
# against Alkahest. Creates a virtual environment under target/conformance/venv (never
# committed) with the client packages, then:
#   default            runs crates/alkahest-api/tests/conformance.rs (in-process server
#                      over a fake llama-server; no model or engine needed, CI-friendly)
#   --url URL          runs the scripts against an already running server instead, with
#                      the models named by --chat MODEL and --embed MODEL (a tiny real
#                      model, e.g. `alkahest serve` with tiny-llamas)
# Pins: ALKAHEST_CONFORMANCE_PACKAGES (default "ollama openai anthropic").
set -euo pipefail
cd "$(dirname "$0")/.."

url=""
chat="hf.co/ggml-org/tiny-llamas:Q4_0"
embed="qwen3-embedding:0.6b"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --url) url="$2"; shift 2 ;;
    --chat) chat="$2"; shift 2 ;;
    --embed) embed="$2"; shift 2 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

venv="target/conformance/venv"
# target/ is restored from the CI cache, which can bring back a venv whose interpreter
# symlink still resolves but whose pip is gone; rebuild it unless pip actually runs.
if [[ ! -f "$venv/pyvenv.cfg" ]] || ! "$venv/bin/python" -m pip --version >/dev/null 2>&1; then
  echo "==> creating $venv"
  python3 -m venv --clear "$venv"
fi
# shellcheck disable=SC2086
"$venv/bin/python" -m pip install --quiet --disable-pip-version-check ${ALKAHEST_CONFORMANCE_PACKAGES:-ollama openai anthropic}
"$venv/bin/python" -m pip list --disable-pip-version-check 2>/dev/null | grep -Ei '^(ollama|openai|anthropic) '

if [[ -n "$url" ]]; then
  for script in ollama_sdk.py openai_sdk.py anthropic_sdk.py; do
    echo "==> $script against $url"
    "$venv/bin/python" -I "crates/alkahest-api/tests/conformance/$script" "$url" "$chat" "$embed"
  done
else
  echo "==> cargo test -p alkahest-api --test conformance"
  ALKAHEST_TEST_CONFORMANCE=1 ALKAHEST_CONFORMANCE_PYTHON="$PWD/$venv/bin/python" \
    cargo test -p alkahest-api --test conformance --locked -- --nocapture
fi
echo "conformance: all checks passed"
