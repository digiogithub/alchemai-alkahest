# Change: speech to text with whisper.cpp (ALK-US-0057)

Implements [[ALK-US-0057]] (tasks ALK-T-0159, ALK-T-0160) / [[ALK-SP-0005]].R4 and
[[ALK-SP-0003]].R2. Builds on [[2026-10-17-engine-trait-registry]],
[[2026-10-17-engine-selection]], [[2026-10-17-recipe-format-engine-manager]],
[[2026-10-17-llama-server-adapter]], [[2026-10-17-openai-api]],
[[2026-10-17-model-kind-classifier]] and [[2026-10-17-hf-gguf-pull]]. Plan:
[[plan/alkahest-v1-plan]].

## What

- Recipe `recipes/whisper-cpp.toml`: pinned to v1.9.5 (commit d1be6fd). Upstream publishes **no**
  prebuilt binaries for the release, so every variant is a source build: `cpu` (shared libs,
  `GGML_CPU_ALL_VARIANTS` + `GGML_BACKEND_DL`, RPATH `$ORIGIN`), `cuda12.4` (needs `nvcc`),
  `vulkan` (needs `glslc`), `metal`. Target `whisper-server` only.
- `alkahest-recipes`: `Recipe::needs_source_build(host)`; `install` (and the CLI message) builds
  from source on its own when a recipe has no prebuilt asset for the host, so
  `alkahest engines install whisper-cpp` needs no `--from-source`. `tag_number` now packs dotted
  versions (`v1.10.0` sorts after `v1.9.5`, `0.11.0` after `0.9.2`); `bNNNN` tags are unchanged.
- `alkahest-gguf`: `ModelFormat::Ggml` (`ggml`, label `GGML`); `classify` maps `ggml-*.bin` files
  and the manifest capability `transcription` to (SpeechToText, Ggml).
- `alkahest-store`: `ModelInfo::derive` reads `model_format = "ggml"`.
- `alkahest-hf`: module `ggml` (`ggml_variants`, `select_ggml`, `pull_ggml`). A repository with
  `ggml-*.bin` files and no GGUF pulls as `hf.co/<org>/<repo>:<name>` where the name is the file
  without `ggml-`/`.bin` (`tiny.en`, `base-q5_1`); the selector may also be the file name. No
  selector prefers `base`, then `small`, then `tiny`, else the smallest. The manifest is one
  model layer plus the source layer, config `model_format = "ggml"`,
  `capabilities = ["transcription"]`.
- `alkahest-engine::whisper_cpp::WhisperCppEngine` (replaces the baseline): `-m --host --port
  -t <n> [-ng] -l auto [--convert --tmp-dir <tmp>]` then extra args. `--convert` is passed when
  `ffmpeg` is on `PATH` (checked at launch; `with_ffmpeg(bool)` for tests). Health is
  `GET /health` 2xx (503 while the model loads). Only `ModelFormat::Ggml` loads.
- `alkahest-api`, new module `audio` (`audio::routes()`, one merge line in `inference_routes()`):
  `POST /v1/audio/transcriptions` and `/v1/audio/translations` (multipart: `file`, `model`,
  `language`, `prompt`, `response_format` json/text/srt/vtt/verbose_json, `temperature`,
  `timestamp_granularities[]`) proxied to `whisper-server /inference`. No language means
  `auto` (whisper-server defaults to English). `verbose_json` is reshaped to OpenAI's
  (`task, language, duration, text, segments[, words]`, engine extras dropped); `text`, `srt`
  and `vtt` are `text/plain`. A file whisper-server cannot decode answers 400 explaining WAV
  versus ffmpeg. Workspace features: axum `multipart`, reqwest `multipart`.

## Verified against the real binary

`whisper-server --help` of the v1.9.5 build (flags above), the `/inference` field names and
`/health` behaviour read from `examples/server/server.cpp` of the pinned tag. The old provisional
command line (`--model`, GGUF format) is gone.

## Verification

- Unit/integration: `crates/alkahest-api/tests/audio.rs` (scenario `transcription` and 6 more
  against a fake whisper-server), `crates/alkahest-engine/tests/whisper_cpp.rs`,
  `crates/alkahest-hf/tests/ggml.rs`, `crates/alkahest-recipes/tests/whisper.rs`, classifier test
  `whisper_ggml_files_are_speech_to_text_in_ggml`. `scripts/ci-local.sh` passes.
- Real, opt-in (`ALKAHEST_TEST_ENGINE=1 cargo test -p alkahest --test serve_audio`): in a temporary
  `ALKAHEST_HOME`, `engines install whisper-cpp --variant cpu` (source build), `pull
  hf.co/ggerganov/whisper.cpp:tiny.en` (75 MB), `serve`, the `jfk.wav` sample through the
  transcriptions endpoint in json, verbose_json, srt and vtt. Passed in 39 s; the temporary home
  is deleted by the test.

## Open

- Prebuilt whisper binaries (Windows, macOS) can be added to the recipe as upstream publishes
  them; until then every platform needs git, cmake, ninja and a C++ toolchain.
- GPU variants (`cuda12.4`, `vulkan`, `metal`) are defined but only the CPU build was run here.
  The scheduler has no memory profile for ggml models (it accounts the file size as RAM and
  0 VRAM); a GPU build still offloads by itself, and `gpu_layers = 0` in the model settings
  passes `-ng`.
