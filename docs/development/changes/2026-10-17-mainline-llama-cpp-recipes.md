# Change: mainline llama.cpp recipes for every accelerator (ALK-US-0050)

Implements [[ALK-US-0050]] / [[ALK-SP-0003]] R4 on top of [[ALK-US-0049]]
([[2026-10-17-recipe-format-engine-manager]]). Plan: [[plan/alkahest-v1-plan]].

## What

`recipes/llama-cpp.toml` now pins release `b11669` of ggml-org/llama.cpp with variants:

| Variant | Assets | Notes |
|---|---|---|
| `cpu` | linux x64/arm64, macOS arm64/x64, Windows x64/arm64 | as before |
| `cuda12.8` | linux x64 | needs CUDA 12 runtime libs on the host |
| `cuda12.4` | windows x64 | CUDA DLLs from the separate cudart zip or a toolkit |
| `cuda13.4` | linux x64/arm64, windows x64 | needs CUDA 13 runtime |
| `vulkan` | linux x64/arm64, windows x64/arm64 | |
| `rocm` | linux x64, windows x64 (ROCm 10.0) | |
| `metal` | macOS arm64 | the standard macOS arm64 archive has Metal built in |
| `sycl` | linux x64, windows x64 (fp16) | install on request only |

Every sha256 and size comes from the `digest`/`size` fields of the GitHub releases API
(`GET /repos/ggml-org/llama.cpp/releases/tags/b11669`); nothing was downloaded just to hash it and
no asset uses `tofu`. The file was generated from that JSON, then reviewed.

Every variant also has a `source` table (T-0144): git `https://github.com/ggml-org/llama.cpp.git`
at `b11669`, target `llama-server`, tools git/cmake/ninja, flags `-DBUILD_SHARED_LIBS=ON
-DLLAMA_CURL=OFF` plus `-DGGML_CUDA=ON` / `-DGGML_VULKAN=ON` / `-DGGML_HIP=ON` /
`-DGGML_METAL=ON` / `-DGGML_SYCL=ON`; Linux variants add `-DCMAKE_BUILD_WITH_INSTALL_RPATH=ON
-DCMAKE_INSTALL_RPATH=$ORIGIN` so the server finds its libraries after being moved.

### CUDA runtime libraries

The upstream CUDA archives do not contain `libcudart`/`libcublas`. Linux: they must be installed
(`ldd libggml-cuda.so` on the reference host resolves them from `/lib/x86_64-linux-gnu`); the
`cudart-llama-b11669-bin-ubuntu-cuda-*.tar.gz` bundles are 440-594 MB and are not installed by
the recipe. Windows: separate `cudart-llama-bin-win-cuda-*.zip` (391-424 MB). The recipe records
this in each variant's `notes`, shown by `alkahest engines info llama-cpp`. Installing a
companion archive next to the main one would need a recipe feature (multiple archives per asset);
filed as an inbox item.

## Verification

- Reference host: RTX 4000 SFF Ada, driver 580.173.02 (CUDA 13.0). `alkahest engines ls`
  recommends `cuda12.8` (13.4 needs a newer driver). `engines install llama-cpp` into a temporary
  `ALKAHEST_HOME` installed `b11669-cuda12.8`; `llama-server --version` printed
  `version: 0.7.0-dev (build 11669, commit 620d2225c)` and `--list-devices` printed
  `CUDA0: NVIDIA RTX 4000 SFF Ada Generation (20017 MiB, ...)`.
- Real source build of the `cpu` variant (`engines install llama-cpp --variant cpu
  --from-source` into a temporary home, git + cmake + ninja on the host): succeeded, the
  installed `llama-server --version` printed `version: 0.7.0-dev (build 1, commit 620d222)`.
  The CUDA/Vulkan/HIP/Metal/SYCL flag sets were not compiled (no toolchains exercised); they are
  the documented upstream options.
- Test `crates/alkahest-recipes/tests/gpu.rs#recommended_cuda_llama_server_lists_the_gpu`
  (opt-in `ALKAHEST_TEST_GPU=1`) automates that; passed in 18 s.
- `crates/alkahest-recipes/tests/recipes.rs#llama_cpp_recipe_covers_the_published_gpu_builds_with_pinned_digests`.
- `scripts/ci-local.sh` passes.
