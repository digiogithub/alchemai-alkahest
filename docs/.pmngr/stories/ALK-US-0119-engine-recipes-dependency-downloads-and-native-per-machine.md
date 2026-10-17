---
id: ALK-US-0119
type: story
title: "Engine recipes: dependency downloads and native per-machine builds"
status: in_review
priority: high
parent: ALK-EP-0015
milestone: ALK-M-0006
author: mcp
labels: [catalog, recipes, engine]
estimate: 5
created: 2026-10-17T18:30:04Z
updated: 2026-10-17T19:01:14Z
started: 2026-10-17T18:43:22Z
links:
  - { kind: implements, target: ALK-SP-0011.R4 }
  - { kind: implements, target: ALK-SP-0011.R5 }
  - { kind: implements, target: ALK-SP-0011.R6 }
---

## Description

Extend the recipe format so a catalog can describe everything an engine needs on one machine.

1. Dependencies: `[[variants.<v>.dependencies]]` with `id`, `os`, `arch`, `url`, `sha256`, `size`,
   `format`, `strip_components`, `dest` (relative to the engine dir), `optional`. Generalises the
   current CUDA-only `--with-runtime` (cudart/cublas) to any library set (ROCm, oneAPI, Vulkan
   loader on Windows). Downloaded with `alkahest-fetch` (resumable, parallel, digest-checked).
2. Native builds: a variant flagged `native = true` is source-only and compiled for the host CPU
   (llama.cpp `-DGGML_NATIVE=ON`, plus accelerator flags). `alkahest engines install <id> --native`
   picks it. The install records the CPU features it was built for (from `alkahest-sysinfo`) and
   `engines ls` marks a native build that no longer matches the host.

## Acceptance Criteria

- `alkahest engines install llama-cpp --native` builds with `GGML_NATIVE=ON` on Linux, macOS and
  Windows (build tests behind `ALKAHEST_TEST_GPU=1` / an opt-in build flag).
- `alkahest engines install llama-cpp --variant cuda12.8 --with-runtime` keeps working, now through
  the generic dependency list; existing recipes migrate.
- Dependencies are verified by sha256, resume after interruption, and are removed with the engine.
- `engines info` shows dependencies and native variants; the JSON output carries them.
- Spec ALK-SP-0003 requirements updated (and an ADR if behaviour changes).

## Notes

Built-in `recipes/*.toml` remain the offline fallback.
