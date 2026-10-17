# Change: llama-server engine adapter (ALK-US-0025, ALK-T-0222)

Implements [[ALK-US-0025]] / [[ALK-SP-0003]].R2 and [[ALK-SP-0010]].R2, plus task ALK-T-0222
of [[ALK-US-0026]]. Builds on [[2026-10-17-engine-trait-registry]],
[[2026-10-17-engine-supervisor]], [[2026-10-17-bundled-cpu-llama-cpp]] and
[[development/decisions/0001-chat-template-strategy]]. Plan: [[plan/alkahest-v1-plan]].

## What

Crate `alkahest-engine`:

- `llama_cpp::LlamaCppEngine` (mainline and prism; moved out of `builtin`):
  `model_args(&LaunchRequest) -> Result<Vec<String>>` builds `-m`, `--mmproj`, `--alias`, `-c`
  (context times slots), `-np`, `-ngl` (All and GPU auto map to 999; CPU auto passes nothing),
  `-fa on|off`, `--cache-type-k/v`, `-t`, the template flags, server-side sampling defaults
  (`--temp --top-p --top-k --min-p --repeat-penalty --presence-penalty -n -s`), then
  `--no-webui --metrics --slots`. Embeddings: `--embeddings`; rerank: `--reranking --pooling
  rank`; neither gets `--jinja`. `prepare` prefixes `--host/--port` and appends
  `engines.overrides.<id>.extra_args` then per-model `extra_args` last. Health is
  `GET /health` 2xx (503 while loading). The prism fork needs no extra flags
  (`/www/Pips/llm/config.yaml` uses only mainline ones).
- Template (ADR 0001): `TemplateSource` (`Gguf`, `OverrideFile`, `OverrideInline`, `Builtin`,
  `label()` for `show`), `template_source(req)`, `builtin_template_for_go(go_template)` marker
  based mapping of Ollama Go templates to llama.cpp built-ins, `BUILTIN_TEMPLATES` (b11669
  `--help` list). The `template` setting is classified as inline Jinja, built-in name or file;
  a missing file is `InvalidEngineSetting`. `LaunchRequest` gained `num_parallel`, `threads`,
  `builtin_template`.
- ALK-T-0222: `FsLocator::from_config` now includes `alkahest_recipes::bundled_roots`
  (dependency direction: engine depends on recipes, recipes does not depend on engine; kept
  because recipes only needs `alkahest-fetch`). `EngineInstaller` trait, `RecipeInstaller`,
  `EngineRegistry::from_config`, `with_auto_install`, `plan_or_install` (async), error
  `AutoInstallFailed`. New config flag `engines.auto_install` (default false) in
  `alkahest-core`.

## Verification

Flags checked against `llama-server --help` of b11669 installed with `alkahest engines
install llama-cpp`. Tests: `tests/llama_cpp.rs` (argument list, ADR 0001 precedence, extra args
order, prism, format refusal) and `tests/auto_install.rs`. Opt-in real runs
(`ALKAHEST_TEST_ENGINE=1`, tiny-llamas `Q4_0` pulled into a temp `ALKAHEST_HOME`, and
`ALKAHEST_TEST_EMBED_GGUF` = nomic-embed-text blob read from the Ollama store): launch through
`launch`, `/v1/models` lists the alias, `/completion` generates, `/metrics` and `/slots` answer,
`/v1/embeddings` returns a vector, `stop` is clean. `scripts/ci-local.sh` passes.

## Notes

`--fit` (default on in b11669) is left alone so unset context/offload still adapts to device
memory; pass `--fit off` through `extra_args` for exact reproducibility.
