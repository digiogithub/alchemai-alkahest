# Change: PrismML llama.cpp recipe for Bonsai ternary models (ALK-US-0051)

Implements [[ALK-US-0051]] (tasks ALK-T-0146 recipe, ALK-T-0220 type ids, ALK-T-0065 E2E) for
[[ALK-SP-0003]] R2 and R4. Builds on [[2026-10-17-recipe-format-engine-manager]] and
[[2026-10-17-mainline-llama-cpp-recipes]]. Plan: [[plan/alkahest-v1-plan]].

## What

- `recipes/prism-llama-cpp.toml`: engine `prism-llama-cpp`, version `prism-b10777-e3de715`
  (commit e3de715222b2667c54a267441159a5729f11155e, tag present in PrismML-Eng/llama.cpp).
  Variants `cpu` (ubuntu x64/arm64, macOS arm64/x64, Windows x64/arm64), `cuda12.4` (Linux and
  Windows), `cuda12.8` (Linux), `cuda13.3` (Linux and Windows), `vulkan`, `rocm` (Linux ROCm 7.2,
  Windows HIP Radeon), `metal` (macOS arm64), `sycl`. Asset names follow
  `/www/Pips/llm/Bonsai-demo/scripts/download_binaries.sh`; digests and sizes from the GitHub
  releases API. Source steps per variant use the tag, target `llama-server`, and the same flag
  sets as the mainline recipe (the Bonsai demo's `build_cuda_linux.sh` uses `-DGGML_CUDA=ON`
  with Ninja). Registered as a built-in recipe in `alkahest-recipes/src/catalog.rs`.
- Library layout: the archives are mainline-shaped (wrapper directory stripped, `llama-server`
  beside `libggml-*.so`, `RUNPATH=$ORIGIN`), so no `library_dirs` entry is needed; the engine
  crate's locator also adds the install directory to the child's library path.
- ALK-T-0220: `GGML_TYPE_PQ2_0 = 142`, `GGML_TYPE_PTQ1_0 = 143` and `GGML_TYPE_COUNT = 144`
  confirmed in the fork's `ggml/include/ggml.h` at the pin, so `PTQ1_0_ID` was right. Block
  geometry fixed from `ggml/src/ggml-common.h`: `block_q2_0` 64 elements / 18 bytes (was 32 / 10),
  `block_pq2_0` 128 / 34 (was 256 / 68, same ratio), `block_ptq1_0` 128 / 28 (24 trit bytes + 2
  high-trit bytes + fp16 scale; was 256 / 56, same ratio). Only Q2_0 changed in effect (0.281 vs
  0.3125 bytes per weight). Test `bonsai_type_ids_and_block_geometry_match_the_fork`.
- ALK-T-0065: `crates/alkahest-engine/tests/bonsai_e2e.rs` (opt-in `ALKAHEST_TEST_GPU=1` and
  `ALKAHEST_TEST_NET=1`) resolves the pulled model, lets `EngineRegistry::plan` choose the
  engine, launches it through the supervisor and sends a chat request.

## Verification

- Model: `prism-ml/Ternary-Bonsai-2-2B-gguf` `PQ2_0` is 517 MB (+ 365 MB projector), pulled with
  `alkahest pull hf.co/prism-ml/Ternary-Bonsai-2-2B-gguf:PQ2_0` into a temporary
  `ALKAHEST_HOME`. `alkahest engines install prism-llama-cpp` chose `cuda12.8` for the RTX 4000
  Ada (driver CUDA 13.0, so `cuda13.3` was not eligible) and installed 208 MB.
- `ALKAHEST_TEST_GPU=1 ALKAHEST_TEST_NET=1 cargo test -p alkahest-engine --test bonsai_e2e`:
  the plan selected `PrismLlamaCpp`, the server became healthy, and "What is the capital of
  France? Answer in one word." was answered `"Paris"` (1.3 s end to end). The temporary home was
  deleted afterwards.
- `scripts/ci-local.sh` passes.

## Not done

ALK-T-0147 (Bonsai thinking defaults, KV q4_0 option) and ALK-T-0148 (benchmark against the
llama-swap setup, ~80 tok/s with the 9B model) stay in the backlog; the 9B model and mmproj
were not downloaded.
