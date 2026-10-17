# Change: vLLM engine, python-venv recipes and safetensors memory estimate (ALK-US-0054)

Implements [[ALK-US-0054]] / [[ALK-SP-0003]].R2 (tasks ALK-T-0153, ALK-T-0154, ALK-T-0155).
Builds on [[2026-10-17-engine-trait-registry]], [[2026-10-17-engine-selection]],
[[2026-10-17-recipe-format-engine-manager]], [[2026-10-17-hf-safetensors-pull]]. Plan:
[[plan/alkahest-v1-plan]] section 5.4 (vLLM on Linux + NVIDIA and macOS Apple Silicon).

## What

- `alkahest-engine::vllm::VllmEngine` (replaces the baseline in `builtin`, which re-exports it):
  `vllm serve <snapshot dir> --host --port --served-model-name --dtype auto
  [--max-model-len] [--gpu-memory-utilization] [--max-num-seqs] [--tensor-parallel-size]
  [--default-chat-template-kwargs] [--chat-template] [--enable-auto-tool-choice
  --tool-call-parser <by model_type>] [--override-generation-config {sampling}]`, then
  `extra_args`. Embeddings use `--runner pooling --convert embed`, rerank `--runner pooling`.
  Health `GET /health` 2xx, 15 minute start timeout, 30 s stop grace. Environment disables vLLM
  usage stats and Hugging Face network access. `tool_call_parser(snapshot)` maps architectures.
  Platform rules already existed in `select` (Linux + CUDA, macOS arm64 + Metal, otherwise
  `UnsupportedPlatform` with a reason); a test pins them.
- `LaunchRequest` gained `gpu_memory_utilization` and `tensor_parallel_size`;
  `from_catalog` now uses `ModelEntry::weights_path()` (snapshot directory for safetensors)
  and rebuilds a missing directory view.
- `alkahest-recipes`: recipe kind **python venv** (`[variants.X.venv]`: `python`, pinned
  `packages`, `index_url`, `extra_index_urls`, `os`, `arch`). `uv` is used when present, else
  `python3 -m venv` + `pip`. The environment is built at its final path (venvs are not
  relocatable) and is complete only when the marker file is written last; failure removes it.
  `BuildTools` gained `uv` and `python`. `find_recipe(id, None)` falls back to the hardware
  choice for engines without a CPU variant. `engines ls` shows `(python package)`.
- `recipes/vllm.toml`: `vllm==0.11.0`; `cuda12.8` (Linux x86_64/aarch64) and an EXPERIMENTAL
  `metal` variant (macOS arm64, CPU build from sdist, spike ALK-T-0212 open).
- `alkahest-sched::safetensors` (new file): `SafetensorsProfile` (config.json + weight file
  sizes), `estimate_vllm` (weights + KV + 1.5 GiB runtime, 8% headroom, all VRAM),
  `gpu_memory_utilization`. `placement` uses it for vLLM on a GPU; `load` passes the fraction to
  the launch request so models share a GPU instead of vLLM claiming 90%.

## Decisions and caveats

- vLLM was NOT installed (multi-GB, product owner rule). Flags are from vLLM 0.11 docs and
  LocalAI's backend; `--runner`, `--convert` and `--default-chat-template-kwargs` should be
  confirmed with the opt-in real test (`ALKAHEST_TEST_VLLM=<binary>`,
  `ALKAHEST_TEST_VLLM_MODEL=<snapshot dir>`).
- The metal variant is unverified; ALK-T-0212 (spike) and ALK-T-0213 stay open.
- Memory constants are heuristics until calibrated against a real run (like llama.cpp's).

## Verification

`crates/alkahest-engine/tests/vllm.rs` (command line order, minimal request, parsers,
embeddings, template validation, GGUF refused, platform rules),
`crates/alkahest-recipes/tests/venv.rs` (fake uv and fake python/pip: in-place venv, fallback,
missing tools, failed install cleanup, platform refusal, validation, builtin vllm recipe),
unit tests in `alkahest-sched/src/safetensors.rs`. `scripts/ci-local.sh` and `cargo machete`
pass.
