# Change: end-to-end test suite (ALK-US-0079)

Implements [[ALK-US-0079]] (tasks ALK-T-0207 harness, ALK-T-0208 GPU suite). Builds on
[[2026-10-17-dialect-errors-conformance]], [[2026-10-17-graceful-shutdown]],
[[2026-10-17-orphaned-engines]], [[2026-10-17-scheduler-load-keep-alive]] and
[[2026-10-17-ollama-store-adapter]]. Plan: [[plan/alkahest-v1-plan]]. Guide: [[testing]].

## What

- `scripts/e2e.sh`: one entry point with legs `cpu`, `conformance`, `ollama`, `decision`, `gpu`,
  `rust` (`--legs all` expands to the first four, plus `gpu` when `ALKAHEST_TEST_GPU=1`). Fresh
  temporary `ALKAHEST_HOME`, ephemeral port, inherited `ALKAHEST_*` and `OLLAMA_MODELS` stripped.
  Every step is a JUnit test case (`target/e2e/junit.xml`, classname `e2e.<leg>`) with its log
  under `target/e2e/logs/`; legs without their prerequisite report `skipped`.
- `cpu`: engine install (reusable through `ALKAHEST_E2E_ENGINES_CACHE`), pull of tiny-llamas and
  all-minilm, `ls`, `show`, `cp`, `rm`, `run`, `ps`, `stop`, completions / generate /
  embeddings in the OpenAI and Ollama dialects, keep-alive unload, SIGTERM shutdown, no process
  whose command line contains the home survives.
- `conformance`: reuses `scripts/conformance.sh --url` against the live server and `smollm2:135m`
  (tiny-llamas has no chat template; tool-call assertions off through
  `ALKAHEST_CONFORMANCE_TOOLS=0`).
- `ollama`: fingerprint of the Ollama store (paths, sizes, mtimes, manifest hashes) before and
  after serving a model from it must be identical. `decision`: `/v1/systemone` and
  `/v1/decisions` with `tev1:0.8b`.
- `gpu`: separate home with the GPU build (`--with-runtime`), a model on the GPU, `ps` reports
  `size_vram > 0`, `gpu_vram` NVML test.
- `rust`: the existing scattered opt-in Rust tests through `cargo nextest run --profile ci` with
  the flags set.
- `docs/testing.md`: layers, legs, every `ALKAHEST_E2E_*` variable and every opt-in
  `ALKAHEST_TEST_*` flag.
- `.github/workflows/e2e-nightly.yml`: nightly `cpu,conformance` legs (not run here).

## Verification

Run on the reference host (RTX 4000 SFF Ada, CUDA 13, real Ollama store):

| Run | Steps | Result | Time |
|---|---|---|---|
| `--legs cpu` (engine download) | 22 | pass | 47 s (engine 16 s, pulls 12 s) |
| `--legs cpu,conformance,ollama,decision`, engine cached | 38 | pass | 75 s (28 s of it the smollm2 pull) |
| `ALKAHEST_TEST_GPU=1 --legs gpu` (CUDA engine download) | 8 | pass | 94 s (engine 73 s) |
| `--legs rust` | 4 (6 nextest tests) | pass | 36 s |

Findings while building it: `engines install` without `--variant` picks CUDA on a GPU host, so
the CPU legs pass `--variant cpu`; tiny-llamas answers `does not support chat`, which is correct.
