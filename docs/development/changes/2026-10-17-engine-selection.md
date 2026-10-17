# Change: engine selection rules (ALK-US-0027)

Implements [[ALK-US-0027]] / [[ALK-SP-0003]].R2. Builds on [[ALK-US-0023]] and
[[ALK-US-0024]]. Plan: [[plan/alkahest-v1-plan]].

## What

Crate `alkahest-engine`, module `select`:

- `select_engine(&LaunchRequest, &Platform) -> EngineChoice { engine, pin,
  preferred_version, reason: SelectionReason }`. Rules: per-model `engine` setting wins
  (`<id>` or `<id>@<version>`, `parse_engine_setting`); otherwise safetensors -> `vllm`,
  speech to text -> `whisper-cpp`, image/video -> `stable-diffusion-cpp`, PQ2_0/PTQ1_0
  (`needs_prism` or the quant label, `is_prism_quant`) -> `prism-llama-cpp` preferring
  `PRISM_PIN = "prism-b10777-e3de715"`, other GGUF -> `llama-cpp`.
- `Platform { os: Os, aarch64, accelerator }` (`Platform::current(&SystemInfo)`),
  `vllm_unsupported_reason()`: vLLM only on Linux with CUDA or macOS on Apple Silicon (Metal);
  elsewhere `EngineError::UnsupportedPlatform`, for rule and override alike.
- `EngineRegistry::select` and `EngineRegistry::plan(&req, &platform) -> EnginePlan { choice,
  engine, install }` (format check, locate with the choice's pin/preference);
  `EnginePlan::prepare(&req)` is what the scheduler passes to `launch` per port.
- A missing engine is `EngineError::NotInstalled` with the reason and the command:
  "engine `prism-llama-cpp` is not installed (needed for PQ2_0 weights); install it with
  `alkahest engines install prism-llama-cpp`".

Also (follow-up to [[ALK-US-0023]] after rebasing on [[ALK-US-0026]]):
`FsLocator::with_extra_roots(roots)` searches bundled engine roots
(`alkahest_recipes::bundled_roots`) after the engines directory with the same
`<id>/<version>-<variant>/` layout; on equal versions the engines directory wins, matching
`alkahest_recipes::locate`. The recipes install layout (`llama-cpp/b11669-cpu/llama-server`)
is found by `FsLocator` as is.

## Decisions

- Version pins ride on the existing `engine` setting (`llama-cpp@b6500`) instead of a new
  overlay field, so ALK-SP-0003.R5 "each model uses the version its configuration pins"
  needs no store change.
- The PrismML pin is a preference, not a requirement: a newer fork build is used when the
  tested one is not installed; an explicit `prism-llama-cpp@<version>` makes it strict.
- The rule duplicates `alkahest_store::ModelInfo::default_engine` (what `alkahest ls`
  shows) because the scheduler works from a `LaunchRequest`; a test enumerates every format,
  kind and quant combination to keep both in agreement.

## Verification

`crates/alkahest-engine/tests/selection.rs` (8 tests, plus `bundled_roots_are_searched_after_the_engines_dir` in `registry.rs`: scenario `bonsai_routing` launches the
fake engine as `prism-llama-cpp` end to end, rules, vLLM platforms, override, not-installed
messages, format check, preferred prism build, agreement with `default_engine`) and unit tests
in `select`. `scripts/ci-local.sh` passes.
