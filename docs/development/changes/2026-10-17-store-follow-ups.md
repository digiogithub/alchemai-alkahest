# Change: store follow-ups (ALK-US-0082)

Implements [[ALK-US-0082]]; follow-ups of [[ALK-US-0009]], [[ALK-US-0010]]. See also
[[ALK-US-0012]], [[ALK-US-0013]]. Plan: [[plan/alkahest-v1-plan]].

## What

1. Ollama cloud models (config `remote_host`, no weights): kept in the catalog and marked.
   `CatalogEntry::source_label()` is `ollama-cloud` (the `SOURCE` column and the `source` JSON
   field), `ModelInfo::remote` / `loadable = false`, `not_loadable_reason()`,
   `ensure_loadable()` and `Catalog::resolve_loadable()` fail with the new
   `StoreError::NotLoadable` ("it is an Ollama cloud model served remotely..."). `show` and
   `ls` still work on them, the scheduler / `run` must use `resolve_loadable`. Models with
   missing blobs are not loadable either.
2. Stale `tmp/` cleanup: `AlkahestStore::open` removes `blob-*.partial` and `.tmp*` files older
   than `STALE_STAGING_AGE` (24 h) through `try_clean_staging`, which takes the store lock with
   a non-blocking `try_lock`: if a pull, import or GC holds the lock, nothing is touched.
   Resumable downloads (`*-partial` with `.json` sidecar) are never removed. `gc` and
   `clean_staging` do the same under the blocking exclusive lock.
3. Hex digest prefixes: done in [[ALK-US-0012]] (`Catalog::resolve`: a bare hex string of 4 to
   64 characters that is not an installed model name matches the manifest digest or weights
   digest; ambiguity is `StoreError::AmbiguousModel`).

## Verification

`tests/catalog.rs`: `ollama_cloud_models_are_marked_and_not_loadable`,
`incomplete_models_are_not_loadable`, `resolves_digest_and_hex_prefixes`,
`ambiguous_digest_prefix_is_reported`. `tests/delete_copy.rs`:
`opening_the_store_removes_stale_temp_files`,
`opening_the_store_leaves_temp_files_while_a_writer_holds_the_lock`, stale staging sweep.
fmt, clippy, nextest (278), deny, check-deps and the headless build pass.
