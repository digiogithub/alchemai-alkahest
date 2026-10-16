# 2026-10-17 Hugging Face Hub client

Story [[ALK-US-0018]], requirement [[ALK-SP-0002]] R3. Plan: [[plan/alkahest-v1-plan]].

## What

Crate `alkahest-hf` (depends on `alkahest-core` for `HfConfig`):

- `HfClient`: `search_models`, `model_info` (`?blobs=true`, optional revision), `repo_files`,
  `tree` (recursive, `Link rel=next` pagination), `readme`, `model_card`, `resolve_url`,
  `bearer_for(url)` (token only for the configured endpoint, never CDN hosts).
- Token: `resolve_token(env, store)` = `HfConfig::token` then `KeyringStore` (service `alkahest`,
  user `hf-token`); broken keyring counts as anonymous. `Token` is redacted in `Debug`/`Display`.
- Endpoint: `resolve_endpoint`: config / `ALKAHEST_HF_ENDPOINT`, else `HF_ENDPOINT`, else public Hub.
- Errors: 401 `Unauthorized`, 403 `Forbidden`, 404 `NotFound` (with `x-error-code`), 429
  `RateLimited`; auth errors carry the `HF_TOKEN` hint.
- `gguf`: `detect_quant`, `gguf_variants` (groups `-0000N-of-0000M` shards, excludes mmproj),
  `pick_variant` (default preference Q4_K_M first, explicit quant exact, else smallest),
  `mmproj_files`, `pick_mmproj`.
- `ModelCard::parse`: YAML front matter via `serde_norway` (new pin `=0.9.42`).

## Verification

15 unit tests and 12 wiremock tests with recorded-shape fixtures (`tests/fixtures`); one
`#[ignore]` real-Hub test gated by `ALKAHEST_TEST_NET=1`. fmt, clippy `-D warnings`,
`cargo deny check`, `check-deps`, headless build, full workspace tests all green.
