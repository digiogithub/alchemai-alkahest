# Ignore Ollama cloud models

Date: 2026-10-18. Story: [[ALK-US-0126]] (builds on [[ALK-US-0082]] item 1). Spec:
[[ALK-SP-0001]] requirement R2 (scenario "cloud model is ignored" added).

## Why

Product owner decision: Alkahest does not support Ollama cloud models (`*:cloud`). Newer
Ollama writes their config blob with `"model_families": null`; the strict `ModelConfig` parse
failed at line 1 column 58 and every listing logged
`WARN unreadable model config blob ... invalid type: null, expected a sequence`, and the models
still showed up as `ollama-cloud` rows.

## What changed

- `alkahest-store/src/reference.rs` `ModelName::is_ollama_cloud`: tag `cloud` or `*-cloud` on
  `registry.ollama.ai` / `ollama.com`.
- `alkahest-store/src/source.rs` `ModelEntry::build`: cloud detection happens before and
  independently of the strict config parse (name, or a tolerant JSON pre-parse of
  `remote_host` / `remote_model`); new field `ModelEntry::remote`, returned by `is_remote()`.
  A config that fails to parse on a cloud model is not warned about; each cloud model is logged
  once per process at `debug` (`log_cloud_model_once`).
- `alkahest-store/src/manifest.rs` `ModelConfig`: `model_families` and `capabilities` read
  `null` as empty (`null_as_default`).
- `alkahest-store/src/error.rs` `StoreError::CloudModel`: "<name> is an Ollama cloud model;
  Ollama cloud models are not supported by Alkahest".
- `alkahest-store/src/catalog.rs`: `list` / `list_origin` drop cloud entries (`supported`), so
  `alkahest ls`, `/api/tags`, `/v1/models`, the admin model list and the desktop Library no
  longer show them; `find` (hence `resolve`, `resolve_loadable`, `copy`, `remove`, `show`, the
  scheduler's `CatalogBackend`) answers `CloudModel` for a cloud name whether or not a store
  holds it.
- `alkahest-api/src/service.rs` `HttpError::from_store` / `from_sched` and
  `alkahest-sched/src/error.rs` `SchedError::status_code`: `CloudModel` is a 404
  (`ErrorKind::ModelNotFound`: OpenAI `model_not_found`, Anthropic `not_found_error`) carrying the
  explanation instead of "try pulling it first".
- `alkahest-api/src/pull.rs` `PullService::pull`: refuses a cloud name with `PullError::NotFound`
  before opening any store (CLI `pull` and `/api/pull`).
- The Ollama store stays read-only: no new code path writes anywhere.

## Verification

Tests named after the scenario: `alkahest-store/tests/ollama_cloud.rs`
(`cloud_model_is_ignored_in_listings`, `..._on_run_show_pull`, `..._without_warning`,
`..._and_ollama_store_untouched`, `cloud_model_config_with_null_lists_parses`),
`alkahest-store/tests/catalog.rs#cloud_model_is_ignored` (replaces the ALK-US-0082 "marked"
test), `alkahest-api/tests/cloud_models.rs` (tags, `/v1/models`, `/api/show` 404, `/api/pull`
404, error shapes per dialect).

`cargo fmt --check`, `cargo clippy --workspace --all-targets --locked -D warnings`,
`cargo nextest run --workspace --locked` (1370 passed, 1 skipped), `cargo deny check`,
`cargo xtask check-deps`, `cargo build -p alkahest --no-default-features --locked`: all pass.

The fixture config reproduces the reported column-58 failure byte for byte; it was
reconstructed from Ollama's `ConfigV2` field order because this machine's Ollama store
(`/usr/share/ollama/.ollama/models`) holds no cloud models.
