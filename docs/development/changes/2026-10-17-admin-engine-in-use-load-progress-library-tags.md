# Change: engine removal in use, load progress, Ollama library tags (ALK-US-0099)

Implements points 1-3 of [[ALK-US-0099]] ([[ALK-SP-0005]].R6, [[ALK-SP-0002]].R6,
[[ALK-SP-0004]].R7). Follows [[2026-10-17-admin-api]] and
[[2026-10-17-scheduler-runtime-metrics]]. Plan: [[plan/alkahest-v1-plan]].

## What

- **Engine removal refused while in use.** `alkahest_sched::EngineInstance::executable()`
  (default `None`; `EngineHandle` returns its `LaunchSpec::program`) feeds the new
  `ModelStatus::executable`. `EngineManager::install_dirs(engine, version, variant)` (default
  empty; `RecipeEngineManager` lists the matching managed installs) names what a removal
  would delete; `DELETE /alkahest/v1/engines/{id}` answers 409 (new
  `ErrorKind::Conflict`, OpenAI code `conflict`, Anthropic `invalid_request_error`) naming
  the loaded models whose executable lies under one of those directories.
- **Per-model load progress.** `ModelStatus::load_progress: Option<f32>` while
  `state == loading`: 0 during admission, then `0.05 + 0.9 * elapsed / expected` capped at
  0.95, `expected` = weights + projector at `LOAD_BYTES_PER_SECOND` (1 GiB/s), at least 1 s
  (`alkahest_sched::load_progress`). llama-server reports no load progress, so it is an
  estimate. `RunnerStats::launch_started` marks the end of admission. The `/events` stream
  now ticks every second while a model loads so the estimate advances.
- **Ollama library tags.** `alkahest_ollama::tags` (`LibraryTag`, `parse_tags_html`) and
  `OllamaLibrary::tags(name)` read `/library/<name>/tags` (user models `/<user>/<name>/tags`):
  every `href=".../<name>:<tag>"` starts a tag; the text up to the next tag gives the 12-hex
  digest, download size (+ bytes), context window, inputs and age; quantisation and
  parameter size come from the tag name. Names are validated (no `..`, at most one `/`).
  Admin route `GET /alkahest/v1/ollama/tags?name=` (404 for unknown models, 400 for bad
  names, 502 on layout or transport errors).
- **Client** (new files, additive): `alkahest_client::LibraryTag`, `Client::ollama_tags`
  (`library_tags.rs`), a shared `Client::call_json` helper (`rest.rs`); `RuntimeModel`
  gained `load_progress` and `executable`.
- Docs: admin route table.

Not done here: point 4 (a desktop "Restart server" action) is desktop work.

## Verification

- `crates/alkahest-api/tests/admin_api.rs#engine_removal_is_refused_while_a_loaded_model_uses_it`,
  the discovery test now also covers `ollama/tags` (fixture, 404, 400).
- `crates/alkahest-sched/tests/metrics.rs#{loading_models_report_estimated_progress,
  load_progress_estimate_is_capped}`.
- `crates/alkahest-ollama/src/tags.rs` tests: hand-written fixture
  `tests/fixtures/tags_smollm2.html`, unknown markup, user models, wiremock route (404,
  layout error, invalid name); `live_library_tags` against ollama.com with
  `ALKAHEST_TEST_NET=1`: passed.
- `crates/alkahest-client/tests/client.rs#ollama_tags_and_load_progress`.
- fmt, clippy `-D warnings`, nextest for api, sched, ollama, client: passed.
