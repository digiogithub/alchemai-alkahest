# ADR 0007: fork engines next to mainline, and CUDA native builds

Status: proposed 2026-10-19 (implementation of [[ALK-US-0132]]; the orchestrator updates the
spec requirements). Specs: [[ALK-SP-0003]] (R2 selection, R5 locating installs),
[[ALK-SP-0011]] (R4 recipe validation, R6 native variants). Change record:
[[development/changes/2026-10-19-llama-cpp-fork-engines]].

## Context

Performance forks of llama.cpp (TurboQuant KV cache, multi-token prediction, new quant
kernels) speak the same `llama-server` command line and HTTP API as mainline, but are
experimental: they must run next to mainline, for the models a user chooses, without changing
what any other model runs on. Until now engine ids were a closed enum (`EngineId`), so a fork
could only replace mainline through `engines.overrides.llama-cpp.path`, for every model at
once.

Separately, the only native build was `cpu-native` (`-DGGML_NATIVE=ON`). On NVIDIA hosts the
useful native build is a CUDA one compiled for the local GPU architectures.

## Decision

1. **Engine family vs engine id.** `EngineId` stays the closed set of adapter families. A
   recipe may declare `base = "<family>"`; its `id` is then a *fork engine*
   (`EngineKey { family, fork }` in the engine crate). The fork runs through the family's
   adapter but has its own install directory (`<engines_dir>/<id>/<version>-<variant>/`), its
   own `engines.overrides.<id>`, and is never taken from `PATH`.
2. **Recipe validation (ALK-SP-0011.R4 extended).** A built-in id must not have `base`; any
   other id must have a `base` that is a canonical built-in id, and may not equal a built-in
   id or alias in any spelling (`llama.cpp`, `prism`, `sd`). The rule lives in
   `alkahest-recipes` (`parse_recipes_with`), so local recipe directories, git catalog syncs
   and `cargo xtask catalog-check` all apply it. The alias table moved there too and
   `EngineId::from_str` uses it.
3. **Selection (ALK-SP-0003.R2 extended, not changed).** The automatic rule is unchanged and
   only ever yields built-in engines. The per-model `engine` override additionally accepts a
   fork id (`engine=<fork>` or `engine=<fork>@<version>`). Names are resolved by the registry:
   built-in id or alias, then a fork defined in the live layered recipe catalog, then an
   installed fork (the install marker now records `base`). Anything else is
   `UnknownEngine`, listing the known engines and how to define a fork.
   `engines.auto_install` may install a fork for a model that names it.
4. **Native variants (ALK-SP-0011.R6 extended).** A `cuda` variant may be `native = true`
   without a `cuda = "x.y"` version (it compiles against the local toolkit). `llama-cpp` gets a
   source-only `cuda-native` variant (`-DGGML_CUDA=ON -DCMAKE_CUDA_ARCHITECTURES=native`).
   `--native` already picks the native variant of the best accelerator, so it now yields
   `cuda-native` on a CUDA host and `cpu-native` elsewhere. `--cuda-arch <list>` replaces (or
   adds) `-DCMAKE_CUDA_ARCHITECTURES` in any CUDA source build.
5. **Locating native installs (ALK-SP-0003.R5 refined).** Install directories ending in
   `-cpu-native` / `-cuda-native` are parsed as variant `cpu-native` / `cuda-native` (before,
   the suffix was read as part of the version). A native build scores one above the portable
   build of its backend, so an explicitly built `cuda-native` wins over `cuda12.8` and
   `cpu-native` over `cpu`.
6. **Per-model `extra_args`.** Appended after `engines.overrides.<id>.extra_args` (already the
   order); now parsed with shell-style quotes on the CLI and refused when they set a flag
   Alkahest manages (`--host`, `--port`, `-m`, `--model`, `--api-key`, `--api-key-file`, also
   `--flag=value`) at save time (CLI, admin API) and again at load time.

## Consequences

- Recipes in user directories or third-party catalogs with an unknown id and no `base` are now
  rejected (reported as ignored recipes). Before, they parsed but could never run, because no
  adapter existed for them.
- `alkahest ps` and logs show the family (`llama-cpp`) for a model running on a fork; `show`
  prints the configured engine.
- Built-in fork recipes would need to be added to the compiled-in list in
  `alkahest-recipes/src/catalog.rs`; catalog-only forks need no code change.

## Spec updates requested

- ALK-SP-0003.R2: add "fork engines are never selected automatically; the per-model engine
  setting may name a fork engine (`<id>[@<version>]`)".
- ALK-SP-0011.R4: add the `base` rule above.
- ALK-SP-0011.R6: native variants may target the GPU (`cuda-native`); `--native` picks the one
  for the best accelerator; `--cuda-arch` selects architectures.
