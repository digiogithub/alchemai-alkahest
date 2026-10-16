# Ollama registry client and `alkahest pull`

Story [[ALK-US-0017]], requirement [[ALK-SP-0002]] R1. Plan: [[plan/alkahest-v1-plan]].

## What changed

- `crates/alkahest-ollama/src/registry.rs`: `RegistryClient`. `GET /v2/<ns>/<model>/manifests/<tag>`
  with the Docker v2 and Ollama manifest-list `Accept` headers; `resolve_manifest` turns a
  manifest list into the image manifest to install (runner `llamacpp`/`ggml` with `gguf`
  format first, else the first entry; inline entries and blob references both handled, blob
  manifests verified against their digest). `resolve_blob_url` reads the `307` `Location` of
  `/v2/<ns>/<model>/blobs/<digest>` once, so the multi-part downloader talks to the CDN
  directly. Typed `RegistryError` (404, 401/403, status with the query string stripped
  because CDN URLs are signed).
- `crates/alkahest-ollama/src/transfer.rs`: `Transfer`, `BlobSpec`, `PullEvent`, `PullProgress`.
  One step that brings a blob into the store: skip when present with the right size, else
  download with `alkahest-fetch` into `AlkahestStore::staging_dir()/dl-<hex>` (stable name,
  so an interrupted pull resumes), `adopt_file`. Shared with the Hugging Face pull
  ([[ALK-US-0019]]).
- `crates/alkahest-ollama/src/pull.rs`: `pull_ollama`. Reuses a complete model of the
  read-only Ollama store (`PullOutcome::ReusedOllama`) unless `PullOptions::force`; otherwise
  holds `lock_shared` from the first blob until the manifest is written, downloads missing
  blobs, then `write_manifest_bytes` with the registry bytes verbatim (digest preserved).
- `crates/alkahest/src/pull_cmd.rs` + `cli.rs`/`main.rs`: `alkahest pull <ref> [--insecure]
  [--force|--copy]`; indicatif per-blob bars plus an aggregate bar on stderr; `--json` prints
  one event object per line on stdout (`plan`, `skipped`, `started`, `progress` throttled to
  250 ms, `finished`, `installing`, `done`).

## Why

Acceptance: manifest list resolution, blob download through CDN redirects, manifest installed
only after all blobs verified, works against registry.ollama.ai.

## Verification

- `crates/alkahest-ollama/tests/pull.rs` (wiremock registry with 307 to a Range-capable CDN):
  `pull_from_registry_installs_manifest_and_all_layers` (scenario "pull from registry"),
  `second_pull_skips_present_blobs`, `manifest_is_not_installed_when_a_blob_fails`,
  `manifest_list_resolves_to_the_llamacpp_entry`, `unknown_model_is_a_clear_not_found`,
  `model_in_the_ollama_store_is_reused_unless_forced`; opt-in
  `real_registry_pull_of_a_tiny_model` (`ALKAHEST_TEST_NET=1`).
- `crates/alkahest/tests/pull.rs`: CLI `--json` end to end over plain HTTP.
- Manual: real `alkahest pull all-minilm:22m` against registry.ollama.ai into a temp home,
  second run downloads 0 bytes.
- fmt, clippy `-D warnings`, nextest, `cargo deny check`, `check-deps`, headless build: green.

## Notes

`alkahest pull` no longer reports "not implemented"; `Command::owner` and the unbuilt-commands
test were updated. Registry authentication (private models, push) is out of scope.
