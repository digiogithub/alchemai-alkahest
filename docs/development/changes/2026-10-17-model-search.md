# Model search (`alkahest search`)

Implements [[ALK-US-0021]] / [[ALK-SP-0002]].R6. Plan: [[plan/alkahest-v1-plan]].

## What

- `alkahest-ollama::library`: `OllamaLibrary::search(query, limit)` and the pure
  `parse_search_html`. ollama.com has no JSON search API, so the `/search?q=` HTML is parsed
  structurally (one `<li>` per model, `/library/<name>` link, `<p>` description, short
  `<span>` badges classified as size, capability, or count, `title="N downloads"`). No CSS
  classes or `x-test-*` attributes are relied on (the live page no longer has them). An
  unrecognisable layout returns `LibraryError::Layout` instead of an empty list. Module layout
  leaves room for `registry.rs` ([[ALK-US-0017]]).
- `alkahest-hf`: `SearchQuery.pipeline_tag` (task filter) sent as `pipeline_tag=`.
- `alkahest` bin: `search_cmd.rs` and `SearchArgs` in `cli.rs`: `--source all|hf|ollama`,
  `--gguf` (default) / `--safetensors` / `--all`, `--task`, `--sort downloads|likes|recent`,
  `--limit`. Output through `output::Listing` (SOURCE, NAME, DOWNLOADS, UPDATED, KIND,
  DESCRIPTION; JSON keys lower-case, raw counts). One failing source warns on stderr; the
  command fails only when all queried sources fail. `ALKAHEST_OLLAMA_LIBRARY` overrides the
  library site.

## Verification

Unit tests with a real-page fixture (`crates/alkahest-ollama/tests/fixtures/search_qwen.html`),
wiremock tests, CLI tests (`crates/alkahest/tests/search.rs`, scenario "hf search"), live test
behind `ALKAHEST_TEST_NET=1`. Full verification block green.

## Known gaps

- HF list endpoint omits `lastModified`, so UPDATED is empty for HF rows.
- Ollama tag counts are not on the search page; sizes are shown instead.
