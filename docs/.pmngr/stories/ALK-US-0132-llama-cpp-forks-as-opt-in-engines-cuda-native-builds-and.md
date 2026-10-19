---
id: ALK-US-0132
type: story
title: llama.cpp forks as opt-in engines, CUDA native builds and per-model engine arguments
status: in_review
priority: high
parent: ALK-EP-0010
milestone: ALK-M-0004
author: mcp
labels: [recipes, engine]
estimate: 8
created: 2026-10-19T15:00:30Z
updated: 2026-10-19T15:35:06Z
started: 2026-10-19T15:00:30Z
---

## Description

The product owner wants to run performance forks of llama.cpp (KV-cache compression such as TurboQuant, MTP + TBQ4 fused flash attention, adaptive KV streaming) on NVIDIA Ada GPUs (RTX 4000 Ada, SM 8.9) next to mainline, and to build llama.cpp optimised for the local GPU architecture.

Today engine ids are a closed enum (`EngineId` in `alkahest-engine/src/id.rs`), so a recipe for a fork cannot get its own id; the only options are a global binary override (`[engines.overrides.llama-cpp] path`) or a hand-made extra version of the `llama-cpp` recipe. Fork-specific server flags can only be set per engine (`extra_args`), not per model.

## Acceptance Criteria

1. **Fork engines.** A recipe may declare `base = "llama-cpp"` (the adapter / API dialect it speaks: llama-server). Such a recipe has its own id (e.g. `llama-cpp-turboquant`), its own install directory `<engines_dir>/<id>/<version>-<variant>/`, and appears in `engines ls/info/install/rm`, the admin API and the desktop Engines view. Recipes from `<data dir>/recipes/` and catalogs may define such engines.
2. **Never automatic.** A fork engine is never chosen by automatic engine selection (ALK-SP-0003.R2); it runs a model only when the model's `engine` setting names it (`engine=llama-cpp-turboquant` or `@<version>`). `engines.auto_install` may install it for such a model.
3. **Validation.** A recipe with an unknown id and no `base`, or a `base` that is not a known engine family, is rejected with a clear error; a fork may not reuse a built-in id. `catalog-check` applies the same rule.
4. **CUDA native variant.** The `llama-cpp` recipe (and the source of fork recipes) offers a source-only `cuda-native` variant: `-DGGML_CUDA=ON -DCMAKE_CUDA_ARCHITECTURES=native` (plus the usual flags), `native = true`, never auto-selected, installed with `alkahest engines install llama-cpp --native` on a CUDA host (or `--variant cuda-native --from-source`). An explicit architecture can be passed (`--cuda-arch 89` or recipe override) for machines where `native` detection is not possible.
5. **Per-model engine arguments.** A model setting `extra_args` (list of strings) is appended to the engine command line after the engine-level `extra_args`; settable via `alkahest config model <m> set extra_args=...`, the admin API and visible in `alkahest show`. Arguments that Alkahest itself manages (`--host`, `--port`, `-m`/`--model`, `--api-key`) are rejected.
6. **Example recipes.** The official catalog (`catalog/engines/`, synced to `recipes/`) carries source-only, experimental fork recipes for the forks the research step verifies (candidates: openalchemy/llama.cpp TurboQuant KV cache; the Indras-Mirror CUDA MTP + TBQ4 fork), pinned to a commit, with notes on the server flags they need. Unverifiable forks are left out and recorded in the research document.
7. **Docs.** `docs/book/src/guide/engines.md` documents fork engines, `cuda-native`, per-model `extra_args`, and a short benchmarking recipe (same model, seed, temperature 0, context; compare decode t/s, prompt t/s, VRAM, TTFT against mainline).
8. **Tests** for: recipe parsing/validation of `base`, selection never picking a fork, explicit `engine=` picking it, install dir layout, cuda-native flags, per-model extra_args ordering and the rejected-flag list.

## Notes

- Research input: product owner's preliminary study (2026-10-19) on llama.cpp forks for RTX 4000 Ada: upstream with SM 89 as baseline; achyudh/llama.cpp and troed/llama.cpp-adaptive-kv-streaming for long context; openalchemy/llama.cpp and atomic-llama-cpp-turboquant for KV compression; Indras-Mirror CUDA MTP + TBQ4 fork for throughput with Qwen MTP models; llamAmpere (Ampere-tuned) and gfx906 (AMD) not applicable. Recorded in `docs/research/llama-cpp-forks.md`.
- Related: ALK-SP-0003 (engine selection), ALK-SP-0011.R6 (native variants), EP-0015 (catalogs).
