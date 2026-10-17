# Change: image generation with stable-diffusion.cpp (ALK-US-0059)

Implements [[ALK-US-0059]] (tasks ALK-T-0163, ALK-T-0164) / [[ALK-SP-0005]].R4 and
[[ALK-SP-0003]].R2. Builds on [[2026-10-17-whisper-stt]], [[2026-10-17-engine-selection]],
[[2026-10-17-recipe-format-engine-manager]], [[2026-10-17-hf-gguf-pull]],
[[2026-10-17-model-kind-classifier]] and [[2026-10-17-openai-api]]. Plan:
[[plan/alkahest-v1-plan]].

## What

- Recipe `recipes/stable-diffusion-cpp.toml`, pinned to release master-970-2f016e8. Prebuilt
  archives with the release's sha256 for `cpu` (Linux x64, macOS arm64, Windows x64), `vulkan`
  (Linux, Windows), `rocm` (Linux, Windows), `metal` (the macOS archive) and `cuda12.4` (Windows
  prebuilt; Linux has none upstream, so it is built from source with `nvcc`). Archives are flat
  (`strip_components = 0`) and `sd-server` has RUNPATH `$ORIGIN`. Every variant also has source
  steps (`-DSD_CUDA=ON`, `-DSD_VULKAN=ON`, ...).
- `alkahest-recipes`: `SourceBuild.submodules` (`git submodule update --init --recursive
  --depth 1` after checkout; stable-diffusion.cpp vendors ggml, libwebp and libwebm that way).
- `alkahest-engine::sd_cpp::StableDiffusionCppEngine` (replaces the baseline; `builtin.rs` now only
  lists the adapters): `-m <file> -l <host> --listen-port <port> [-t n] [--backend cpu]
  [-s seed]`, then extra args. Formats GGUF and single-file safetensors; a snapshot directory
  or ggml is refused with an explanation. Health: `GET /` with a 2xx (sd-server has no
  `/health` and listens only after its context exists).
- Selection: image, video and speech kinds are now chosen before the safetensors-means-vLLM rule
  (a single-file diffusion checkpoint is not vLLM's); `ModelInfo::default_engine` follows, and the
  agreement test also covers `ModelFormat::Ggml`.
- Classification: `kind_from_gguf` recognises stable-diffusion.cpp's metadata-less conversions
  by their tensor names (`model.diffusion_model.`, `first_stage_model.`); the Hub pull writes
  the manifest capability `image-generation` for repositories tagged `sd.cpp`,
  `stable-diffusion.cpp` or `text-to-image`; `ModelInfo::derive` turns the capability into the
  Image kind and `capabilities_of` lists it.
- `alkahest-api`, new module `imagegen` (`imagegen::routes()`, one merge line; the existing
  `images` module is the vision-input helper): `POST /v1/images/generations` with `model`,
  `prompt`, `n` (1-10), `size` (`WxH`, multiples of 8, 64-4096, or `auto`), `response_format`
  (`b64_json` only, `url` answers 400), `output_format`, `output_compression`, plus the
  extensions `negative_prompt`, `steps`, `seed`, `cfg_scale`, `guidance`, `sampler`, `scheduler`,
  `clip_skip`. sd-server's OpenAI route takes only prompt/n/size/output options, so the
  extensions travel as JSON in `<sd_cpp_extra_args>` tags appended to the prompt (documented in
  its `examples/server/api.md`). **A client prompt that contains the tag is rejected**: the block
  can reach engine options (LoRA and image paths) a caller must not choose.

## Verified against the real binary

`sd-server --help` of the release (flags above); `routes_openai.cpp` and `api.md` of the tag for
the request and response shape and the extra-args schema. The route is synchronous in the engine
(a mutex serialises generations).

## Verification

- Tests: `crates/alkahest-api/tests/imagegen.rs` (5, fake sd-server) and unit tests in
  `imagegen/generations.rs`, `crates/alkahest-engine/tests/sd_cpp.rs`,
  `crates/alkahest-recipes/tests/stable_diffusion.rs`, `crates/alkahest-hf/tests/ggml.rs`
  (`sd_cpp_repositories_are_image_models`), classifier test
  `metadata_less_stable_diffusion_checkpoints_are_images`. `scripts/ci-local.sh` passes.
- Real, opt-in (`ALKAHEST_TEST_ENGINE=1 cargo test -p alkahest --test serve_images`):
  prebuilt `cpu` archive installed through `engines install` (sha256 verified), SD-Turbo
  (`Green-Sky/SD-Turbo-GGUF:sd_turbo-f16-q8_0.gguf`, 2.0 GB, no GGUF metadata) pulled, 512x512
  at 4 steps through the API: a valid 512x512 PNG of a red fox, 16 s on CPU. The same with the
  `cuda12.4` variant built from source on this machine (RTX 4000 Ada, nvcc present): 3.6 s end to
  end. The temporary homes and the 2 GB model were deleted.

## Open

- Multi-file models (Flux, SD3: separate text encoders and VAE) need the extra components in the
  model's `extra_args`; Alkahest pulls only the main weights file. The `/v1/images/edits`
  endpoint and `url` responses are not implemented.
- Video generation: the `video` kind selects this engine but `sd-server` is started for image
  generation only; see [[ALK-US-0060]] and `docs/research/video-generation.md`.
- Memory: the scheduler has no estimate for diffusion models (file size as RAM, 0 VRAM).
