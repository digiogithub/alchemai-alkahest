# Change: delete, copy and garbage collection (ALK-US-0013)

Implements [[ALK-US-0013]] / [[ALK-SP-0001]].R4 and R2 (delete scenario). Builds on
[[ALK-US-0012]] (catalog). Plan: [[plan/alkahest-v1-plan]].

## What

Crate `alkahest-store`, module `ops` (methods of `AlkahestStore`):

- `delete_model(&ModelName) -> Option<DeleteReport>`: exclusive lock, removes the manifest
  (case-insensitive lookup), prunes empty manifest directories, deletes only the blobs the
  manifest referenced that no remaining manifest uses (`blobs_removed`, `blobs_kept`,
  `reclaimed`).
- `gc(dry_run) -> GcReport`: exclusive lock; `blobs() - referenced_blobs()` is deleted,
  plus abandoned temp files; with `dry_run` it only reports.
- `clean_staging(older_than)`: removes `tmp/blob-*.partial` and `.tmp*` files older than the
  limit (default `STALE_STAGING_AGE`, 24 h) under the exclusive lock. Resumable downloads
  (`*-partial` + `.json` sidecar of `alkahest-fetch`) are deliberately not touched.
- `copy_model(src: &dyn ModelSource, entry, dst) -> CopyReport`: inside the store it writes
  the same manifest bytes under the new name (alias, same model id, no blob copied); from
  another store (Ollama) it **copies** every missing blob (streamed, hashed against the
  manifest digest, committed atomically; never hard-linked or symlinked) and writes a manifest
  with an added `Import` source record. Under the shared lock. An existing destination is
  replaced like `ollama cp`; the same model onto itself, cloud models and incomplete models
  are refused.

`Catalog::remove(&str)` resolves and deletes; a model of the Ollama store fails with
`StoreError::ReadOnly` ("use `ollama rm <name>`") via `ModelSource::ensure_writable` before any
file is touched. `Catalog::copy(source, destination)` validates the destination (a plain name;
no scope prefix, digest or pin) and copies.

CLI (`models_cmd.rs`): `alkahest rm <model>...` (continues after a failure, exit 1 if any
failed), `alkahest cp <src> <dst>`, new `alkahest gc [--dry-run]`; all support `--json`.

## Decisions

- Copy, not link, when importing from Ollama: a hard link would let a write through either
  name change the other store; copying costs disk but keeps the Ollama store strictly
  read-only and the copy independent of `ollama rm`.
- `rm` does not run a full GC: it deletes only the blobs of the deleted manifest, so
  unrelated orphans (in-flight work of other tools) are not collected by surprise. `gc` is the
  explicit sweep.

## Verification

`crates/alkahest-store/tests/delete_copy.rs` (8 tests): `shared_blob_kept` (spec scenario),
`delete_refuses_ollama_model` (spec scenario, trees compared byte for byte),
`copy_inside_the_store_is_an_alias`, `copy_from_ollama_copies_blobs_and_never_writes_there`
(inode and link-count check, copy survives deleting the source), cloud/bad destination
refusals, `gc_deletes_only_unreferenced_blobs`, stale staging cleanup. Binary tests
`rm_refuses_ollama_models`, `cp_rm_and_gc_round_trip`. fmt, clippy, nextest (262), deny,
check-deps, headless build pass.
