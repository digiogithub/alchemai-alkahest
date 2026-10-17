# Change: recipe companion archives (CUDA runtime) (ALK-US-0086)

Implements [[ALK-US-0086]] on top of [[ALK-US-0049]] ([[2026-10-17-recipe-format-engine-manager]])
and [[2026-10-17-mainline-llama-cpp-recipes]]. Spec: [[ALK-SP-0003]]. Plan: [[plan/alkahest-v1-plan]].

## What

- Recipe format: `[[variants.<name>.companions]]` with `name`, `os`, `arch`, `url` (https),
  `sha256` (mandatory, never trust-on-first-use), `size`, `format`, `strip_components`.
  `Companion` (with `as_asset`), `Recipe::companions`, `Recipe::companions_for(host)`.
- Install (`alkahest-recipes/src/install.rs`): `InstallOptions::with_runtime` unpacks the
  companions of the host into the same engine directory (before the marker is written, so a
  failed or corrupt companion leaves nothing). On an engine already installed without them only
  the companions are fetched and the marker updated (`add_companions`); a repeat run downloads
  nothing. The marker and `InstalledEngine` record `companions` (names). The download/unpack
  code moved into `fetch_and_unpack`, shared by the engine archive and companions.
- CLI: `alkahest engines install <id> --with-runtime`; `engines info` lists the companion and
  its size per variant; after an install a missing CUDA runtime prints a warning with the remedy.
- Detection (`alkahest-recipes/src/runtime.rs`): `check_cuda_runtime(variant, os, install_dir,
  library_dirs)` looks for `libcudart.so.N` / `libcublas.so.N` (Windows `cudart64_N.dll`,
  `cublas64_N.dll`) in the engine directory, `LD_LIBRARY_PATH`/`PATH`, the usual system library
  and toolkit directories and `CUDA_PATH`. `RecipeError::CudaRuntimeMissing` names the missing
  libraries and the two fixes. `alkahest-engine`: `EngineInstall::check_runtime` runs at the
  start of `prepare` of llama.cpp, whisper.cpp and stable-diffusion.cpp
  (`EngineError::RuntimeMissing`), so the failure appears before launch.
- Recipes with companions (digests and sizes from the GitHub releases API):
  `llama-cpp` cuda12.8 (Linux x64, 594 MB), cuda12.4 (Windows, 391 MB), cuda13.4 (Linux x64
  440 MB, Linux arm64 552 MB, Windows 423 MB); `prism-llama-cpp` cuda12.4 and cuda13.3 (Windows
  only: the fork publishes no Linux cudart); `stable-diffusion-cpp` cuda12.4 (Windows, 563 MB).

## Findings

Upstream publishes Linux cudart archives (`cudart-llama-b11669-bin-ubuntu-cuda-*.tar.gz`) with
one wrapper directory (`strip_components = 1`) holding `libcublas.so.N` and friends (checked by
streaming the first 4 MB of the archives); the Windows zips are flat (central directory read by
range request: `cudart64_N.dll`, `cublas64_N.dll`, `cublasLt64_N.dll`). Nothing larger than a
few MB was downloaded.

## Verification

`scripts/ci-local.sh` passes. New tests `crates/alkahest-recipes/tests/companions.rs` use tiny
zip fixtures on a local server: skipped unless requested, `with_runtime` unpacks and satisfies
the detection, late install fetches only the companion and is idempotent, corrupt digest leaves
no directory, recipe validation, built-in recipes pin companions below 600 MB, detection errors.
The real archives were not downloaded; the Linux layout of the 12.8 archive is inferred from
its first entry (`libcublas.so.12`).
