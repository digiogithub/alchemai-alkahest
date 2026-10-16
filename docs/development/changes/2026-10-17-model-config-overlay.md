# Change: per-model configuration overlay (ALK-US-0014, ALK-T-0073)

Implements [[ALK-US-0014]] / [[ALK-SP-0010]].R4 and task ALK-T-0073 of [[ALK-US-0022]]
(recommended defaults layer, [[ALK-SP-0010]].R3 storage). Builds on [[ALK-US-0012]].
Plan: [[plan/alkahest-v1-plan]].

## What

Crate `alkahest-store`, module `overlay`:

- `ModelSettings`: every tunable as an optional field (context_length, kv_cache_type_k/v,
  gpu_layers, flash_attention, template, tools_enabled, temperature, top_p, top_k, min_p,
  repeat_penalty, presence_penalty, max_tokens, seed, stop, keep_alive, engine, extra_args,
  chat_template_kwargs). One type for every layer and for the result. `set_key` / `unset_key`
  take the CLI spelling with Ollama aliases (`num_ctx`, `num_gpu`, `num_predict`),
  `ModelSettings::global_defaults(&Config)` builds the global layer (scheduler `keep_alive`).
- Resolver: `resolve(Layers { request, model, recommended, global }) -> Effective`, field by
  field, request > model > recommended > global; `Effective { settings, origins }` records the
  `Layer` that decided each field (`origin("context_length")`), `engine_or(automatic)`.
- `ModelOverlays` (`in_data_dir(data_dir)`, files below `<data_dir>/model-config/`):
  `get`, `put`, `update(name, closure)`, `model_path`, `recommended`, `set_recommended`,
  `set_recommended_json`, `ensure_recommended(name, make)` (write only when missing: the
  "first resolved" path), `remove_recommended`, and
  `resolve(name, request, global, thinking, fallback_recommended)`.
- `RecommendedLayer` is the persisted recommended-defaults layer (ALK-T-0073). It has the same
  serde shape as `alkahest_hf::RecommendedParams`; `alkahest-hf` is only a dev-dependency of the
  store (a test round-trips the real type) so the store does not link the HF client. The pull
  code calls `set_recommended_json(&name, &serde_json::to_string(&params)?)`. `to_settings(
  Some(true|false))` applies the thinking / non-thinking sampling set over the general one.
- `Catalog::overlays()`, `Catalog::with_overlays()`, `Catalog::effective_settings(entry,
  request, global, thinking)` (adds `tools_enabled` derived from the weights when no layer
  sets it). `Catalog::remove` drops the model's recommended layer and keeps the user overlay.

CLI: `alkahest config model <model> [show | set KEY=VALUE... | unset KEY... | path]`, `--json`.
`show` prints the overlay, the recommended layer and the effective settings with the layer of
each value. The model may be installed in either store or only a name.

## Decisions and deviations

- Storage is a file per model in Alkahest's data dir, keyed by the lower-cased name, not an
  `application/vnd.alkahest.config.v1+json` manifest layer as the story text suggests. The
  orchestrator asked for the keyed file: it works for read-only Ollama models, a hand edit never
  changes a model digest, and the overlay survives `rm` + `pull`. The `ALKAHEST_CONFIG` layer
  type stays reserved and unused.
- The user overlay (`<tag>.toml`, hand-editable) and the machine-written recommended layer
  (`<tag>.recommended.json`) are separate files so pulls never overwrite user edits.
- The trained context length is not injected as a default context (it would exhaust memory);
  it comes in only through `RecommendedParams.context_length` when the card recommends one.

## Verification

`tests/overlay.rs` (7 tests: `precedence` is the spec scenario, Ollama model untouched, invalid
file reports its path, unset removes the file, HF `RecommendedParams` round trip, ensure-once,
remove keeps the overlay), unit tests of the resolver, keys and thinking sets in `overlay.rs`,
bin test `config_model_set_show_unset`. fmt, clippy, nextest (274), deny, check-deps, headless
build pass.
