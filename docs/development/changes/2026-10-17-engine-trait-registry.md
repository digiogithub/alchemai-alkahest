# Change: Engine trait, locator and registry (ALK-US-0023)

Implements [[ALK-US-0023]] / [[ALK-SP-0003]].R1 and the install layout part of
[[ALK-SP-0003]].R5. Builds on [[ALK-US-0012]] (catalog) and [[ALK-US-0014]] (settings overlay).
Plan: [[plan/alkahest-v1-plan]].

## What

Crate `alkahest-engine` (first code in it):

- `id::EngineId` (`llama-cpp`, `prism-llama-cpp`, `vllm`, `whisper-cpp`,
  `stable-diffusion-cpp`; `FromStr` with aliases, `binary_names()`, `install_command()` ->
  `alkahest engines install <id>`, `path_lookup_allowed()`), `id::ApiDialect` (`LlamaServer`,
  `VllmOpenAi`, `WhisperServer`, `SdServer`).
- `capability::Capability` (chat, completion, embeddings, rerank, vision, speech_to_text,
  text_to_speech, image, decision, logits, tokenize), `Capabilities` set with
  `require(model, cap) -> Result<(), CapabilityError>` (the API maps it to 400),
  `kind_capabilities(kind, has_projector)`.
- `launch::LaunchRequest` (model, weights, projector, format, kind, quantization,
  needs_prism, effective `ModelSettings`, accelerator, loopback host, port) with
  `new`, `from_catalog(&CatalogEntry, &Effective, Accelerator, port)`, `with_port`.
- `launch::LaunchSpec` (program, args, env, working dir, host, port, `HealthCheck { path,
  ready: ReadyPredicate }`, startup timeout, stop grace, dialect), `prepend_library_dirs`
  (`LD_LIBRARY_PATH` / `DYLD_LIBRARY_PATH` / `PATH`, install dirs first), `base_url`,
  `command_line`.
- `engine::Engine` trait: `id`, `capabilities`, `formats`, `dialect`,
  `model_capabilities(req)` (engine set intersected with the kind's set), `check(req)`
  (format), `prepare(req, install) -> LaunchSpec`.
- `builtin`: baseline `LlamaCppEngine` (mainline and prism; `model_args` is the extension
  point for the full adapter of ALK-US-0025), `VllmEngine`, `WhisperCppEngine`,
  `StableDiffusionCppEngine` (whisper and sd command lines are provisional).
- `locator::EngineLocator` trait (the recipes crate will implement it) and `FsLocator`:
  config override path > `<engines_dir>/<id>/<version>-<variant>/` (newest compatible
  variant, `pin`, `preferred_version`) > `PATH` (never for the PrismML fork, whose binary
  name clashes with mainline). `EngineInstall` carries binary, version, variant, root,
  library dirs, source and `engines.overrides.<id>.extra_args`. Helpers
  `split_install_dir`, `compare_versions` (natural order), `is_executable`.
- `registry::EngineRegistry`: `with_builtin_engines(locator)`, `register` (adapters replace
  baselines), `get`, `engines`, `locate`, `installed`, `prepare(id, query, req) ->
  PreparedLaunch { engine, install, spec }`.

## Decisions

- Engines are stateless descriptions; health checking and stopping are data in the spec
  (`health`, `stop_grace`) that the supervisor (ALK-US-0024) acts on.
- Variant compatibility: exact accelerator label > older CUDA build on a newer driver > CPU
  or unlabelled builds; other GPU builds are skipped.
- Argument order: engine args, then `engines.overrides.<id>.extra_args`, then the per-model
  `extra_args`, so the most specific wins in last-value parsers.

## Verification

`crates/alkahest-engine/tests/registry.rs` (13 tests, incl. scenarios
`capabilities_drive_routing` and `two_versions_default_newest_and_pin`) and unit tests in
`id`, `capability`, `launch`, `locator`. `scripts/ci-local.sh` passes (fmt, clippy, nextest
329 tests, deny, check-deps, headless build, typos).
