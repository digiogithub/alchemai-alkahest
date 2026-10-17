#!/usr/bin/env bash
# End-to-end suite (ALK-US-0079): one entry point for the scenarios that need a real engine,
# real model files and a running `alkahest serve`. Everything happens in a fresh temporary
# ALKAHEST_HOME that is deleted afterwards (set ALKAHEST_E2E_KEEP=1 to keep it).
#
# Usage: scripts/e2e.sh [--legs LIST] [--list]
#   LIST is comma separated; default "cpu". Legs:
#     cpu          engine install, pull tiny models, CLI (ls show cp rm run ps stop), embeddings,
#                  completions, keep-alive unload, SIGTERM shutdown without orphan engines
#     conformance  the ollama / openai / anthropic Python SDK scripts against the live server
#                  (needs python3 and network for pip; reuses scripts/conformance.sh)
#     ollama       read-only reuse of an existing Ollama store: metadata snapshot before and
#                  after serving and running a model from it must be identical (skipped when
#                  no Ollama store exists)
#     decision     /v1/decisions and /v1/systemone against the decision model `tev1:0.8b`
#                  (taken from the Ollama store; skipped when it is not there)
#     gpu          GPU engine build, a model loaded on the GPU, VRAM reported by `ps`
#                  (opt in with ALKAHEST_TEST_GPU=1, needs an NVIDIA GPU)
#     rust         the opt-in Rust tests (ALKAHEST_TEST_ENGINE=1 ...) through cargo nextest
#   `all` expands to cpu,conformance,ollama,decision, plus gpu when ALKAHEST_TEST_GPU=1.
#
# Environment (all optional):
#   ALKAHEST_E2E_BIN            alkahest binary; default target/debug/alkahest (built when missing)
#   ALKAHEST_E2E_ENGINES_CACHE  directory of a previous engines install (a copy of
#                               <home>/data/engines) reused instead of downloading the engine;
#                               filled on the first run when the directory does not exist
#   ALKAHEST_E2E_VARIANT        engine variant of the cpu, conformance and rust legs (default cpu)
#   ALKAHEST_E2E_CHAT           chat/completion model   (default hf.co/ggml-org/tiny-llamas:Q4_0)
#   ALKAHEST_E2E_EMBED          embedding model         (default all-minilm)
#   ALKAHEST_E2E_CHAT_TEMPLATE_MODEL  small model with a chat template for the conformance leg
#                               (default smollm2:135m; the completion-only CHAT model cannot chat)
#   ALKAHEST_E2E_DECISION       decision model          (default tev1:0.8b)
#   ALKAHEST_E2E_OLLAMA_MODELS  Ollama store to use for the ollama and decision legs; default
#                               $OLLAMA_MODELS, then /usr/share/ollama/.ollama/models,
#                               then ~/.ollama/models
#   ALKAHEST_E2E_OUT            report directory (default target/e2e): junit.xml and logs/
#   ALKAHEST_E2E_KEEP=1         keep the temporary home
#   ALKAHEST_TEST_GPU=1         enable the gpu leg
#   ALKAHEST_E2E_GPU_VARIANT    engine variant for the gpu leg (default: best for the machine)
#
# Output: a console line per step, target/e2e/junit.xml (one testcase per step, classname
# e2e.<leg>) for `gintrack spec ingest`, and the output of every step under target/e2e/logs/.
# Exit status is non-zero when any step failed. Skipped legs are reported as skipped.
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"

legs="cpu"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --legs) legs="$2"; shift 2 ;;
    --list) sed -n '2,/^set -uo/p' "$0" | sed 's/^# \{0,1\}//' | head -n -1; exit 0 ;;
    *) echo "unknown option: $1 (try --list)" >&2; exit 2 ;;
  esac
done
if [[ "$legs" == "all" ]]; then
  legs="cpu,conformance,ollama,decision"
  [[ "${ALKAHEST_TEST_GPU:-}" == "1" ]] && legs="$legs,gpu"
fi
IFS=',' read -r -a LEGS <<< "$legs"

CHAT="${ALKAHEST_E2E_CHAT:-hf.co/ggml-org/tiny-llamas:Q4_0}"
EMBED="${ALKAHEST_E2E_EMBED:-all-minilm}"
CHATTY="${ALKAHEST_E2E_CHAT_TEMPLATE_MODEL:-smollm2:135m}"
DECISION="${ALKAHEST_E2E_DECISION:-tev1:0.8b}"
OUT="${ALKAHEST_E2E_OUT:-$ROOT/target/e2e}"
mkdir -p "$OUT/logs"
rm -f "$OUT/logs/"*.log "$OUT/cases.tsv"
: > "$OUT/cases.tsv"

BIN="${ALKAHEST_E2E_BIN:-$ROOT/target/debug/alkahest}"
if [[ ! -x "$BIN" ]]; then
  echo "==> building $BIN"
  cargo build -p alkahest --no-default-features --locked || exit 1
fi

TMP="$(mktemp -d "${TMPDIR:-/tmp}/alkahest-e2e.XXXXXX")"
HOME_E2E="$TMP/home"
mkdir -p "$HOME_E2E"
SERVER_PID=""
FAILED=0
START_ALL=$(date +%s)

ORIG_OLLAMA_MODELS="${OLLAMA_MODELS:-}"
# Never inherit configuration from the developer's shell.
for v in $(env | grep -E '^(ALKAHEST_|OLLAMA_MODELS)' | grep -vE '^ALKAHEST_(E2E|TEST|CONFORMANCE)_' | cut -d= -f1); do unset "$v"; done
export ALKAHEST_HOME="$HOME_E2E"

cleanup() {
  stop_server >/dev/null 2>&1 || true
  if [[ "${ALKAHEST_E2E_KEEP:-}" == "1" ]]; then
    echo "kept $TMP"
  else
    rm -rf "$TMP"
  fi
}
trap cleanup EXIT

# --- reporting -------------------------------------------------------------------------------

CUR_LEG="cpu"
slug() { printf '%s' "$1" | tr -c 'A-Za-z0-9._-' '_' | cut -c1-80; }

# record NAME STATUS SECONDS LOGFILE  (STATUS: pass | fail | skip)
record() {
  printf '%s\t%s\t%s\t%s\t%s\n' "$CUR_LEG" "$1" "$2" "$3" "$4" >> "$OUT/cases.tsv"
  local mark="ok  "
  [[ "$2" == fail ]] && mark="FAIL" && FAILED=$((FAILED + 1))
  [[ "$2" == skip ]] && mark="skip"
  printf '  [%s] %s.%s (%ss)\n' "$mark" "$CUR_LEG" "$1" "$3"
  if [[ "$2" == fail && -f "$4" ]]; then sed 's/^/        | /' "$4" | tail -n 15; fi
}

# step NAME CMD...: run CMD, capture output to a log, record the outcome.
step() {
  local name="$1"; shift
  local log="$OUT/logs/$CUR_LEG.$(slug "$name").log"
  local t0=$SECONDS
  if "$@" > "$log" 2>&1; then
    record "$name" pass $((SECONDS - t0)) "$log"
  else
    record "$name" fail $((SECONDS - t0)) "$log"
  fi
}

skip() { # skip NAME REASON
  local log="$OUT/logs/$CUR_LEG.$(slug "$1").log"
  echo "$2" > "$log"
  record "$1" skip 0 "$log"
}

write_junit() {
  python3 - "$OUT/cases.tsv" "$OUT/junit.xml" <<'PY'
import sys, xml.sax.saxutils as x
rows = [l.rstrip("\n").split("\t") for l in open(sys.argv[1]) if l.strip()]
fails = sum(r[2] == "fail" for r in rows)
skips = sum(r[2] == "skip" for r in rows)
total = sum(float(r[3]) for r in rows)
out = ['<?xml version="1.0" encoding="UTF-8"?>',
       f'<testsuites name="alkahest-e2e" tests="{len(rows)}" failures="{fails}" skipped="{skips}" time="{total}">',
       f'<testsuite name="e2e" tests="{len(rows)}" failures="{fails}" skipped="{skips}" time="{total}">']
for leg, name, status, secs, log in rows:
    try:
        text = open(log, errors="replace").read()[-4000:]
    except OSError:
        text = ""
    text = "".join(c for c in text if c in "\n\t" or ord(c) >= 32)
    out.append(f'<testcase classname="e2e.{x.escape(leg)}" name="{x.escape(name)}" time="{secs}">')
    if status == "fail":
        out.append(f'<failure message="step failed">{x.escape(text)}</failure>')
    elif status == "skip":
        out.append(f'<skipped message="{x.escape(text.strip())}"/>')
    else:
        out.append(f'<system-out>{x.escape(text)}</system-out>')
    out.append('</testcase>')
out += ['</testsuite>', '</testsuites>']
open(sys.argv[2], "w").write("\n".join(out) + "\n")
PY
}

# --- helpers ----------------------------------------------------------------------------------

alk() { "$BIN" "$@" </dev/null; }

free_port() {
  python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])'
}

# start_server [extra env assignments...]: serve on an ephemeral loopback port and wait until
# /api/version answers. Sets PORT, ALKAHEST_HOST and SERVER_PID.
start_server() {
  PORT="$(free_port)"
  env "$@" "$BIN" serve --host "127.0.0.1:$PORT" >"$OUT/logs/$CUR_LEG.server.log" 2>&1 </dev/null &
  SERVER_PID=$!
  export ALKAHEST_HOST="127.0.0.1:$PORT"
  for _ in $(seq 1 100); do
    curl -sf "http://127.0.0.1:$PORT/api/version" >/dev/null 2>&1 && return 0
    kill -0 "$SERVER_PID" 2>/dev/null || { echo "server exited early"; return 1; }
    sleep 0.1
  done
  echo "server never answered on port $PORT"; return 1
}

# stop_server: SIGTERM and wait up to 20 s; prints the exit status. Fails when it had to kill.
stop_server() {
  [[ -n "$SERVER_PID" ]] || return 0
  kill -TERM "$SERVER_PID" 2>/dev/null || true
  for _ in $(seq 1 200); do
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
      wait "$SERVER_PID" 2>/dev/null; local rc=$?
      SERVER_PID=""
      echo "server exited with status $rc"
      return 0
    fi
    sleep 0.1
  done
  kill -KILL "$SERVER_PID" 2>/dev/null || true
  SERVER_PID=""
  echo "server ignored SIGTERM for 20 s"
  return 1
}

# No engine process of this run may outlive the server: their command lines contain the home.
no_orphans() {
  local left
  left="$(pgrep -af -- "$HOME_E2E" || true)"
  [[ -z "$left" ]] || { echo "orphan processes:"; echo "$left"; return 1; }
}

wait_for() { # wait_for SECONDS CMD...: poll CMD every 0.5 s
  local limit="$1"; shift
  local end=$((SECONDS + limit))
  while (( SECONDS < end )); do "$@" && return 0; sleep 0.5; done
  return 1
}

ps_has() { alk ps --json 2>/dev/null | python3 -c 'import json,sys;sys.exit(0 if any(m["name"]==sys.argv[1] for m in json.load(sys.stdin)) else 1)' "$1"; }
ps_lacks() { ! ps_has "$1"; }
ps_empty() { alk ps --json 2>/dev/null | python3 -c 'import json,sys;sys.exit(0 if not json.load(sys.stdin) else 1)'; }
ls_has() { alk ls --json 2>/dev/null | python3 -c 'import json,sys;sys.exit(0 if any(m["name"]==sys.argv[1] for m in json.load(sys.stdin)) else 1)' "$1"; }

contains() { grep -q -- "$2" <<< "$1"; }

install_engine() {
  local cache="${ALKAHEST_E2E_ENGINES_CACHE:-}"
  if [[ -n "$cache" && -d "$cache" ]]; then
    mkdir -p "$ALKAHEST_HOME/data"
    cp -a "$cache" "$ALKAHEST_HOME/data/engines"
    echo "reused engines from $cache"
    return 0
  fi
  alk engines install llama-cpp --variant "${1:-${ALKAHEST_E2E_VARIANT:-cpu}}" || return 1
  if [[ -n "$cache" ]]; then
    mkdir -p "$(dirname "$cache")"
    cp -a "$ALKAHEST_HOME/data/engines" "$cache"
    echo "filled engines cache $cache"
  fi
}

# Directory of the Ollama store to test against, or empty.
ollama_store() {
  local c
  for c in "${ALKAHEST_E2E_OLLAMA_MODELS:-}" "${ORIG_OLLAMA_MODELS:-}" /usr/share/ollama/.ollama/models "$HOME/.ollama/models"; do
    [[ -n "$c" && -d "$c/manifests" ]] && { echo "$c"; return 0; }
  done
  return 1
}

# Stable fingerprint of a store: names, sizes, mtimes and the content hash of every manifest.
# Blob contents are not hashed (tens of GB); their names are digests and the sizes must match.
store_fingerprint() {
  (cd "$1" && find . -type f -printf '%p %s %T@\n' | sort; find manifests -type f -print0 | sort -z | xargs -0 -r sha256sum)
}

# --- legs -------------------------------------------------------------------------------------

cli_checks() {
  step "ls-lists-pulled-models" bash -c '
    out="$("$0" ls)"; grep -q "$1" <<< "$out" && grep -q "$2" <<< "$out"' "$BIN" "$CHAT" "$EMBED"
  step "show" bash -c '"$0" show "$1" </dev/null | grep -qi "quant\|architecture"' "$BIN" "$CHAT"
  step "show-json" bash -c '"$0" show "$1" --json </dev/null | python3 -c "import json,sys;json.load(sys.stdin)"' "$BIN" "$CHAT"
  step "cp" bash -c '"$0" cp "$1" e2e-copy </dev/null && "$0" ls </dev/null | grep -q "e2e-copy"' "$BIN" "$CHAT"
  step "rm-copy-keeps-original" bash -c '"$0" rm e2e-copy </dev/null && ! "$0" ls </dev/null | grep -q "e2e-copy" && "$0" ls </dev/null | grep -q "$1"' "$BIN" "$CHAT"
}

leg_cpu_body() {
  step "version" alk --version
  step "engine-install" install_engine
  step "pull-chat" alk pull "$CHAT"
  step "pull-embed" alk pull "$EMBED"
  step "serve-starts" start_server ALKAHEST_KEEP_ALIVE=5s
  [[ -n "$SERVER_PID" ]] || { echo "  server did not start; remaining cpu steps skipped"; return; }
  cli_checks
  step "run-one-shot" bash -c 'out="$("$0" run "$1" "Once upon a time" </dev/null)"; [[ -n "$out" ]]' "$BIN" "$CHAT"
  step "ps-lists-loaded-model" ps_has "$CHAT"
  step "completions-api" bash -c '
    curl -sf "http://$ALKAHEST_HOST/v1/completions" -d "{\"model\":\"$0\",\"prompt\":\"Once\",\"max_tokens\":5}" |
      python3 -c "import json,sys;d=json.load(sys.stdin);assert d[\"choices\"][0][\"text\"]"' "$CHAT"
  step "ollama-generate-api" bash -c '
    curl -sf "http://$ALKAHEST_HOST/api/generate" -d "{\"model\":\"$0\",\"prompt\":\"Once\",\"stream\":false,\"options\":{\"num_predict\":5}}" |
      python3 -c "import json,sys;d=json.load(sys.stdin);assert d[\"done\"] and d[\"response\"]"' "$CHAT"
  step "embeddings-openai" bash -c '
    curl -sf "http://$ALKAHEST_HOST/v1/embeddings" -d "{\"model\":\"$0\",\"input\":[\"hi\",\"there\"]}" |
      python3 -c "import json,sys;d=json.load(sys.stdin);assert len(d[\"data\"])==2 and len(d[\"data\"][0][\"embedding\"])>8"' "$EMBED"
  step "embeddings-ollama" bash -c '
    curl -sf "http://$ALKAHEST_HOST/api/embed" -d "{\"model\":\"$0\",\"input\":\"hi\"}" |
      python3 -c "import json,sys;d=json.load(sys.stdin);assert len(d[\"embeddings\"][0])>8"' "$EMBED"
  step "unknown-model-404" bash -c '[[ "$(curl -s -o /dev/null -w "%{http_code}" "http://$ALKAHEST_HOST/api/show" -d "{\"model\":\"no-such-model\"}")" == 404 ]]'
  step "stop-unloads" bash -c '"$0" stop "$1" </dev/null' "$BIN" "$CHAT"
  step "ps-after-stop" wait_for 10 ps_lacks "$CHAT"
  # Keep-alive: the server runs with a 5 s keep-alive; both models must unload by themselves.
  step "keep-alive-unload" bash -c '
    curl -sf "http://$ALKAHEST_HOST/api/generate" -d "{\"model\":\"$0\",\"prompt\":\"Hi\",\"stream\":false,\"options\":{\"num_predict\":2}}" >/dev/null || exit 1
    for _ in $(seq 1 120); do
      [[ "$("$1" ps --json </dev/null)" == "[]" ]] && exit 0; sleep 0.5
    done
    echo "models still loaded after 60 s"; "$1" ps </dev/null; exit 1' "$CHAT" "$BIN"
  step "sigterm-shutdown" stop_server
  step "no-orphan-engines" no_orphans
}

leg_conformance() {
  if ! command -v python3 >/dev/null; then skip "sdk-scripts" "python3 not installed"; return; fi
  step "serve-starts" start_server
  [[ -n "$SERVER_PID" ]] || return
  step "pull-chat-model" alk pull "$CHATTY"
  step "sdk-scripts" env ALKAHEST_CONFORMANCE_TOOLS="${ALKAHEST_CONFORMANCE_TOOLS:-0}" "$ROOT/scripts/conformance.sh" --url "http://127.0.0.1:$PORT" --chat "$CHATTY" --embed "$EMBED"
  step "sigterm-shutdown" stop_server
  step "no-orphan-engines" no_orphans
}

leg_ollama() {
  local store; store="$(ollama_store)" || { skip "ollama-read-only" "no Ollama store found"; return; }
  local before="$TMP/ollama-before.txt" after="$TMP/ollama-after.txt"
  local model
  model="$(ALKAHEST_HOME="$TMP/ollama-home" OLLAMA_MODELS="$store" "$BIN" ls --json </dev/null 2>/dev/null |
    python3 -c '
import json,sys
ms=[m for m in json.load(sys.stdin) if m.get("source")=="ollama" and m.get("loadable") and m.get("kind") in ("embeddings",) and m["size"]<1_000_000_000]
print(min(ms,key=lambda m:m["size"])["name"] if ms else "")')"
  store_fingerprint "$store" > "$before"
  echo "store $store, model $model, $(wc -l < "$before") fingerprint lines" > "$OUT/logs/ollama.store.log"
  step "ollama-models-visible" bash -c 'OLLAMA_MODELS="$1" ALKAHEST_HOME="$2" "$0" ls </dev/null | grep -q " ollama "' "$BIN" "$store" "$TMP/ollama-home"
  if [[ -z "$model" ]]; then
    skip "ollama-model-runs" "no small embedding model in the Ollama store"
  else
    mkdir -p "$TMP/ollama-home/data"
    [[ -d "$TMP/ollama-home/data/engines" ]] || cp -a "$HOME_E2E/data/engines" "$TMP/ollama-home/data/"
    step "serve-starts" start_server OLLAMA_MODELS="$store" ALKAHEST_HOME="$TMP/ollama-home"
    step "ollama-model-runs" bash -c '
      curl -sf "http://$ALKAHEST_HOST/api/embed" -d "{\"model\":\"$0\",\"input\":\"hi\"}" |
        python3 -c "import json,sys;d=json.load(sys.stdin);assert len(d[\"embeddings\"][0])>8"' "$model"
    step "sigterm-shutdown" stop_server
  fi
  store_fingerprint "$store" > "$after"
  step "ollama-store-unchanged" diff "$before" "$after"
  step "no-orphan-engines" no_orphans
}

leg_decision() {
  local store; store="$(ollama_store)" || { skip "decision" "no Ollama store found"; return; }
  local present
  present="$(OLLAMA_MODELS="$store" ALKAHEST_HOME="$TMP/ollama-home" "$BIN" ls --json </dev/null 2>/dev/null |
    python3 -c 'import json,sys;print(any(m["name"]==sys.argv[1] for m in json.load(sys.stdin)))' "$DECISION")"
  if [[ "$present" != "True" ]]; then skip "decision" "$DECISION is not in $store"; return; fi
  mkdir -p "$TMP/ollama-home/data"
  [[ -d "$TMP/ollama-home/data/engines" ]] || cp -a "$HOME_E2E/data/engines" "$TMP/ollama-home/data/"
  step "serve-starts" start_server OLLAMA_MODELS="$store" ALKAHEST_HOME="$TMP/ollama-home"
  [[ -n "$SERVER_PID" ]] || return
  step "systemone-ticket" bash -c '
    curl -sf "http://$ALKAHEST_HOST/v1/systemone" -d "$(python3 -c "
import json,sys
print(json.dumps({\"model\":sys.argv[1],\"state\":\"Customer message: I was charged twice for my order last week and nobody has replied.\",
 \"questions\":{\"route\":{\"type\":\"choice\",\"instructions\":\"Which team should handle this?\",\"criteria\":{\"billing\":None,\"shipping\":None,\"technical\":None}},
  \"angry\":{\"type\":\"noul\",\"instructions\":\"Is the customer angry?\"}}}))" "$0")" |
    python3 -c "import json,sys;d=json.load(sys.stdin);assert d[\"answers\"][\"route\"][\"choice\"]==\"billing\",d"' "$DECISION"
  step "decisions-endpoint" bash -c '
    curl -sf "http://$ALKAHEST_HOST/v1/decisions" -d "$(python3 -c "
import json,sys
print(json.dumps({\"model\":sys.argv[1],\"input\":\"I was charged twice for my order last week and nobody has replied.\",
 \"questions\":[{\"type\":\"choice\",\"name\":\"route\",\"instructions\":\"Which team should handle this?\",\"choices\":[{\"value\":\"billing\"},{\"value\":\"shipping\"},{\"value\":\"technical\"}]},
  {\"type\":\"predicate\",\"instructions\":\"Is the customer angry?\"}]}))" "$0")" |
    python3 -c "import json,sys;d=json.load(sys.stdin);assert d[\"answers\"][0][\"choice\"]==\"billing\",d;assert d[\"usage\"][\"total_tokens\"]>0,d"' "$DECISION"
  step "sigterm-shutdown" stop_server
  step "no-orphan-engines" no_orphans
}

leg_gpu() {
  if [[ "${ALKAHEST_TEST_GPU:-}" != "1" ]]; then skip "gpu" "set ALKAHEST_TEST_GPU=1"; return; fi
  command -v nvidia-smi >/dev/null || { skip "gpu" "nvidia-smi not found"; return; }
  # A separate home so the GPU build is the only engine and cannot be shadowed by the CPU one.
  local saved="$ALKAHEST_HOME"
  export ALKAHEST_HOME="$TMP/gpu-home"; mkdir -p "$ALKAHEST_HOME"
  step "engine-install-gpu" alk engines install llama-cpp ${ALKAHEST_E2E_GPU_VARIANT:+--variant "$ALKAHEST_E2E_GPU_VARIANT"} --with-runtime
  step "pull-chat" alk pull "$CHAT"
  step "serve-starts" start_server ALKAHEST_KEEP_ALIVE=30s
  if [[ -n "$SERVER_PID" ]]; then
    step "run-on-gpu" bash -c '"$0" run "$1" "Once upon a time" </dev/null | grep -q .' "$BIN" "$CHAT"
    step "ps-reports-vram" bash -c '"$0" ps --json </dev/null | python3 -c "
import json,sys
m=json.load(sys.stdin)[0]
print(m[\"processor\"], m[\"size_vram\"])
assert m[\"size_vram\"]>0 and \"GPU\" in m[\"processor\"], m"' "$BIN"
    step "sigterm-shutdown" stop_server
    step "no-orphan-engines" no_orphans
  fi
  step "nvml-vram-test" env ALKAHEST_TEST_GPU=1 cargo test -p alkahest --test gpu_vram --locked -- --nocapture
  export ALKAHEST_HOME="$saved"
}

leg_rust() {
  command -v cargo-nextest >/dev/null || { skip "nextest" "cargo-nextest not installed"; return; }
  # The opt-in tests that need an engine and the tiny models in the default home.
  step "pull-chat" alk pull "$CHAT"
  step "pull-embed" alk pull "$EMBED"
  step "engine-install" install_engine
  step "nextest-opt-in" env ALKAHEST_TEST_ENGINE=1 ALKAHEST_TEST_CONFORMANCE=1 \
    ALKAHEST_TEST_GPU="${ALKAHEST_TEST_GPU:-0}" \
    cargo nextest run --workspace --locked --profile ci \
      -E 'test(/real_engine|real_llama|embedding_mode|calibrat|answers_systemone/)'
}

# --- main -------------------------------------------------------------------------------------

echo "alkahest e2e: legs=$legs home=$HOME_E2E"
for leg in "${LEGS[@]}"; do
  CUR_LEG="$leg"
  echo "== leg $leg"
  case "$leg" in
    cpu) leg_cpu_body ;;
    conformance) leg_conformance ;;
    ollama) leg_ollama ;;
    decision) leg_decision ;;
    gpu) leg_gpu ;;
    rust) leg_rust ;;
    *) echo "unknown leg: $leg" >&2; FAILED=$((FAILED + 1)) ;;
  esac
done

write_junit
total=$(wc -l < "$OUT/cases.tsv")
echo "e2e: $total steps, $FAILED failed, $((SECONDS)) s; junit at $OUT/junit.xml"
[[ "$FAILED" == 0 ]]
