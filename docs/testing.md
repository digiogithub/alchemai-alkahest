# Testing Alkahest

Related: [[ALK-US-0079]], [[plan/alkahest-v1-plan]], [[2026-10-17-e2e-test-suite]].

Three layers, from cheap to expensive:

| Layer | Command | Needs |
|---|---|---|
| Unit and in-process integration | `cargo nextest run --workspace --locked` (or `scripts/ci-local.sh`) | nothing (fake llama-server, synthetic GGUF) |
| Opt-in Rust tests | the same command with the flags below | a real engine, models, sometimes a GPU or network |
| End to end | `scripts/e2e.sh` | the built binary, network for the first run |

`cargo nextest run --profile ci` writes `target/nextest/ci/junit.xml`; `scripts/e2e.sh` writes
`target/e2e/junit.xml`. Both are ingested by `gintrack spec ingest` so scenarios and requirement
coverage see them.

## The end-to-end entry point

```sh
scripts/e2e.sh                      # cpu leg
scripts/e2e.sh --legs cpu,conformance,ollama,decision
scripts/e2e.sh --legs all           # + gpu when ALKAHEST_TEST_GPU=1
scripts/e2e.sh --list               # the header of the script: legs and variables
```

Every run uses a fresh temporary `ALKAHEST_HOME` (deleted afterwards; `ALKAHEST_E2E_KEEP=1`
keeps it), a server on an ephemeral loopback port, and no inherited `ALKAHEST_*` or
`OLLAMA_MODELS` configuration. A step is one JUnit test case (`classname` `e2e.<leg>`); its
output is in `target/e2e/logs/<leg>.<step>.log`.

| Leg | What it proves |
|---|---|
| `cpu` | CPU llama.cpp install, `pull` of a tiny completion model and an embedding model, `ls`, `show` (text and JSON), `cp`, `rm`, `serve` start, `run` one-shot, `ps`, `/v1/completions`, `/api/generate`, `/v1/embeddings`, `/api/embed`, unknown-model 404, `stop`, keep-alive unload (5 s), SIGTERM shutdown, no orphan engine process |
| `conformance` | the `ollama`, `openai` and `anthropic` Python SDK scripts (`scripts/conformance.sh --url`) against the live server and a small chat model (tool-call assertions off, tiny models cannot call tools) |
| `ollama` | an existing Ollama store is only read: a fingerprint of every file (path, size, mtime) and of every manifest hash is identical before and after listing and running a model from it |
| `decision` | `/v1/systemone` and `/v1/decisions` with `tev1:0.8b` from the Ollama store |
| `gpu` | GPU engine build with `--with-runtime`, a model loaded on the GPU (`ps` shows `size_vram > 0`), NVML per-process VRAM test; needs `ALKAHEST_TEST_GPU=1` |
| `rust` | the opt-in Rust tests below, through nextest, against the pulled models |

Legs whose prerequisite is absent (no Ollama store, no `tev1`, no GPU, no python) report
`skipped`, not failed. The exit status is non-zero when a step failed.

Environment of `scripts/e2e.sh`:

| Variable | Default | Meaning |
|---|---|---|
| `ALKAHEST_E2E_BIN` | `target/debug/alkahest` | binary under test (built when missing) |
| `ALKAHEST_E2E_ENGINES_CACHE` | unset | directory holding a previous engines install; copied instead of downloading, filled on first use |
| `ALKAHEST_E2E_VARIANT` | `cpu` | engine variant of the cpu, conformance and rust legs |
| `ALKAHEST_E2E_CHAT` | `hf.co/ggml-org/tiny-llamas:Q4_0` | completion model (19 MB) |
| `ALKAHEST_E2E_CHAT_TEMPLATE_MODEL` | `smollm2:135m` | chat-capable model for the conformance leg |
| `ALKAHEST_E2E_EMBED` | `all-minilm` | embedding model |
| `ALKAHEST_E2E_DECISION` | `tev1:0.8b` | decision model, taken from the Ollama store |
| `ALKAHEST_E2E_OLLAMA_MODELS` | `$OLLAMA_MODELS`, `/usr/share/ollama/.ollama/models`, `~/.ollama/models` | store of the ollama and decision legs (read only) |
| `ALKAHEST_E2E_GPU_VARIANT` | best for the machine | variant of the gpu leg |
| `ALKAHEST_E2E_OUT` | `target/e2e` | report directory |
| `ALKAHEST_E2E_KEEP` | unset | `1` keeps the temporary home |
| `ALKAHEST_CONFORMANCE_TOOLS` | `0` in the leg | `1` asserts tool calls (needs a tool-capable model) |
| `ALKAHEST_CONFORMANCE_PACKAGES` | `ollama openai anthropic` | pip packages (pin versions here) |

CI: `.github/workflows/e2e-nightly.yml` runs the `cpu` and `conformance` legs every night on a
hosted runner (engine cached). The GPU and Ollama legs run on the reference host:

```sh
ALKAHEST_TEST_GPU=1 scripts/e2e.sh --legs all
```

## Opt-in flags of the Rust tests

Every flag defaults to off; a test without its flag prints `skipped` and passes. These are the
tests the `rust` leg selects; run any of them directly with `cargo nextest run -p <crate>`.

| Flag | Enables | Related variables |
|---|---|---|
| `ALKAHEST_TEST_ENGINE=1` | tests against a real `llama-server` in the default `ALKAHEST_HOME` (`alkahest engines install llama-cpp --variant cpu` first): `alkahest-engine` `llama_cpp`, `alkahest` `serve_inference`, `serve_decisions`, `serve_vision`, `serve_audio`, `serve_images`, `calibration` | `ALKAHEST_TEST_GGUF`, `ALKAHEST_TEST_EMBED_GGUF`, `ALKAHEST_TEST_LLAMA_SERVER`, `ALKAHEST_TEST_MODEL`, `ALKAHEST_TEST_CHAT_MODEL`, `ALKAHEST_TEST_EMBED_MODEL`, `ALKAHEST_TEST_RERANK_MODEL`, `ALKAHEST_TEST_DECISION_MODEL`, `ALKAHEST_TEST_VISION_MODEL`, `ALKAHEST_TEST_WAV`, `ALKAHEST_TEST_HOME`, `ALKAHEST_TEST_SD_VARIANT`, `ALKAHEST_TEST_CALIBRATION_GGUF` |
| `ALKAHEST_TEST_NET=1` | real Hugging Face and registry traffic (`alkahest-hf`, Bonsai) | `ALKAHEST_TEST_HF_REF`, `ALKAHEST_TEST_HF_SAFETENSORS` |
| `ALKAHEST_TEST_GPU=1` | NVML, CUDA llama-server VRAM (`gpu_vram`), GPU detection, Bonsai end to end (also needs NET) | |
| `ALKAHEST_TEST_CONFORMANCE=1` | SDK conformance over the in-process server (`alkahest-api` `conformance`) | `ALKAHEST_CONFORMANCE_PYTHON`, `scripts/conformance.sh` sets both |
| `ALKAHEST_TEST_OLLAMA_STORE=1` | read-only Ollama store test over the real store | |
| `ALKAHEST_TEST_VLLM=<binary>` | real vLLM run | `ALKAHEST_TEST_VLLM_MODEL` |

Network and GPU tests never download more than a few hundred MB by default; the 2 GB
stable-diffusion model of `serve_images` stays in `ALKAHEST_TEST_HOME` when you set it.

## Conventions

- A test named after a spec scenario keeps the scenario name (snake case).
- Tests that start `alkahest serve` stop it with SIGTERM so engines are reaped; a SIGKILL leaves
  orphan `llama-server` processes.
- Nothing under an Ollama store is ever written; tests that read one prove it with a
  fingerprint comparison.
