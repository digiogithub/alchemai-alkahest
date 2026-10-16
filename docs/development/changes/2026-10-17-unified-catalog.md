# Change: unified catalog and `alkahest ls` / `show` (ALK-US-0012)

Implements [[ALK-US-0012]] / [[ALK-SP-0001]].R5. Builds on [[ALK-US-0009]], [[ALK-US-0010]],
[[ALK-US-0011]]. Plan: [[plan/alkahest-v1-plan]].

## What

Crate `alkahest-store`:

- `catalog::Catalog` merges the Alkahest store and the read-only Ollama store.
  `Catalog::new(alkahest, ollama, meta_cache_dir)` / `Catalog::open(&ResolvedPaths)` (an
  unreadable Ollama store becomes a `warnings()` entry, not an error), `list()` (one row per
  name, newest first), `list_origin(origin)`, `find(origin, &name)`, `resolve(&str)`,
  `resolve_loadable(&str)`, `detail(&CatalogEntry)`, `alkahest()`, `ollama()`, `source(origin)`.
- Merge rule: on a name clash the Alkahest model is listed and resolved, the hidden copy is
  recorded in `CatalogEntry::shadows`; `ollama:<name>` selects the Ollama copy.
- `resolve` accepts short names (Ollama defaults), `ollama:` / `alkahest:` prefixes, `name@sha256:`
  pins, every Hugging Face spelling (`hf.co/o/r:Q4_K_M`, `hf://`, URLs; without a selector the
  only pulled quant of the repo), `sha256:<hex>`, and bare hex prefixes (>= 4 chars) of the
  manifest digest (the `ID` column) or the weights blob. Failures are
  `StoreError::ModelNotFound` (with "did you mean other tags") or `StoreError::AmbiguousModel`.
- `CatalogEntry { entry: ModelEntry, info: ModelInfo, shadows }`, `source_label()`
  (`alkahest`, `ollama`, `ollama-cloud`), `summary() -> ModelSummary` (serde: name, reference,
  id, digest, size, modified RFC 3339, source, kind, format, quantization, family, architecture,
  parameters, context_length, embedding_length, capabilities, engine, loadable, complete,
  also_in, model_path), `not_loadable_reason()` / `ensure_loadable()`.
- `meta::ModelInfo` derived from the Ollama config blob plus GGUF facts: format, kind
  (`alkahest_gguf::classify` on the header, vision upgrade for projectors), architecture,
  family, parameters (`format_parameters`), quantisation, context/embedding length,
  capabilities, `default_engine()` (ALK-SP-0003.R2 rule: PQ2_0/PTQ1_0 -> `prism-llama-cpp`,
  safetensors -> `vllm`, STT -> `whisper-cpp`, image/video -> `stable-diffusion-cpp`, else
  `llama-cpp`).
- GGUF header facts (`GgufFacts`) are cached as `gguf-<blob hex>.json` in
  `<cache_dir>/gguf-meta` (keyed by the weights digest, so never stale); nothing is written in
  either model store (tested).
- `ModelDetail` (template with GGUF fallback, system, licence, params, messages, source info),
  `parameter_lines()` and `modelfile()`.
- New `StoreError` variants: `ModelNotFound`, `AmbiguousModel`, `NotLoadable`.

Binary: `models_cmd.rs`. `alkahest ls` prints NAME ID SIZE MODIFIED SOURCE KIND FORMAT QUANT
(decimal sizes, relative ages); `--json` prints `ModelSummary` objects. `alkahest show <model>`
prints the sections Model, Capabilities, Parameters, System, Template, License, Source, Path;
`--modelfile`, `--template`, `--parameters`, `--license`, `--system`, `--json`.

## Decisions and deviations

- The metadata cache is under the Alkahest *cache* dir (`ResolvedPaths::cache_dir`), not the data
  dir: it is derived data that can be deleted at any time.
- "De-duplicating identical digests" is implemented as de-duplication by name across stores (the
  same model present in both stores is listed once). Two different names with one manifest
  digest are aliases (`cp`) and both stay listed.
- `ls --json` does not use the generic string-only `Listing`; it emits typed values (size in
  bytes, nullable fields).

## Verification

`crates/alkahest-store/tests/catalog.rs` (9 tests incl. scenario `json_listing`),
`crates/alkahest/tests/models.rs` (4 tests: `ollama_model_is_listed`, `json_listing`, show
sections and flags), unit tests for sizes, ages, parameters. fmt, clippy `-D warnings`,
nextest (252), deny, check-deps and the headless build pass.
