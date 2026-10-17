# Change: multi-file diffusion manifests and a video capability (ALK-US-0100, partial)

Part of [[ALK-US-0100]] ([[ALK-SP-0003]].R2, [[ALK-SP-0005]].R4). Follows
[[2026-10-17-sd-image-generation]] and the recommendation of
[[2026-10-17-video-generation-spike]] (multi-file manifests before any video endpoint).
Plan: [[plan/alkahest-v1-plan]].

## What

- `alkahest-store`:
  - `manifest::media_type::DIFFUSION_{VAE, CLIP_L, CLIP_G, T5XXL, LLM}`
    (`application/vnd.alkahest.diffusion.*.v1`) and `DiffusionComponent` (ids, media
    types, `parse`, `guess_from_file_name`, `is_text_encoder`).
  - `ModelEntry::components()`: component blob paths in engine order.
  - `CreateLayers::components`: `create_model` adds (or, on a model base, replaces) one
    layer per component; with components the weights blob may be a single safetensors file
    (sniffed) and the config carries `image-generation`, so the catalog classifies the model
    as an image model unless the GGUF header says video (`wan`, ...).
- `alkahest-engine`:
  - `LaunchRequest::components` (filled by `from_catalog`).
  - `StableDiffusionCppEngine::model_args`: `--vae`, `--clip_l`, `--clip_g`, `--t5xxl`,
    `--llm` from the components (flags checked against the docs of the pinned
    `master-970-2f016e8`); with a text encoder the weights go to `--diffusion-model`, a VAE
    alone keeps `-m`.
  - `Capability::Video`; `kind_capabilities(Video)` is `{Video}`, so image requests no longer
    reach video models; sd.cpp offers `{Image, Video}`.
- `alkahest-api` `/api/create`: `files` may hold the weights plus diffusion components,
  recognised by file name (`ae`/`*vae*`, `clip_l`, `clip_g`, `*t5*`) or named
  `<component>:<file>`; duplicates, several weights files, projectors and safetensors
  language models are refused. Components may also extend a `from` model.
- Memory: the scheduler already charges a diffusion model by `ModelEntry::size` (all blobs,
  components included) with the ggml image/video workload overhead.

## Not done (left on the story)

- Pulling multi-file models directly from the Hub (today: download the files, upload with
  `/api/blobs`, create), `/v1/images/edits` (img2img), `url` responses, and the video
  endpoint (deferred by the spike).

## Verification

- `crates/alkahest-api/tests/ollama.rs#create_a_multi_file_diffusion_model` (four uploads,
  create, catalog kind `image`, three component layers, launch request -> sd-server flags).
- `crates/alkahest-engine/tests/sd_cpp.rs#multi_file_models_pass_their_components`,
  `capability::tests::video_models_do_not_serve_image_requests`,
  `ollama::create::tests::diffusion_components_are_split_from_the_weights`.
- No real sd-server run: no sd.cpp build is installed on this machine (the opt-in
  `crates/alkahest/tests/serve_images.rs` covers single-file models).
- fmt, clippy `-D warnings`, nextest engine, store, api: passed.
