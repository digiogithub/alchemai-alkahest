# Change: Alkahest content-addressed store (ALK-US-0009)

Implements [[ALK-US-0009]] / [[ALK-SP-0001]].R1. Builds on [[ALK-US-0011]] (references).
Plan: [[plan/alkahest-v1-plan]].

## What

Crate `alkahest-store`:

- `manifest`: `Manifest` (Docker distribution v2), `Descriptor`, `LayerKind` and
  `media_type::*` for every Ollama layer (model, projector, adapter, template, system, params,
  messages, license, draft, embed, prompt, tensor, json, config) plus the Alkahest extension
  layers `application/vnd.alkahest.source.v1+json` (`SourceInfo`, `SourceKind`) and
  `application/vnd.alkahest.config.v1+json` (opaque here; schema owned by the per-model
  configuration story). `ManifestDocument` also parses Ollama manifest lists and manifest
  references (needed by the Ollama adapter and the registry pull). `ModelConfig` mirrors
  Ollama's `ConfigV2` and keeps unknown fields.
- `local::AlkahestStore` at `ResolvedPaths::models_dir`: `blobs/sha256-<hex>`,
  `manifests/<host>/<ns>/<model>/<tag>`, `tmp/` staging, `.lock`.
  - `blob_writer(expected) -> BlobWriter` (`io::Write`, hashes while writing, `commit()`
    verifies, fsyncs, renames; drop deletes the temp file), `put_blob`, `put_json_blob`,
    `adopt_file(src, digest)` for finished resumable downloads, `verify_blob`, `has_blob`,
    `blob_size`, `blobs()`.
  - `write_manifest` / `write_manifest_bytes` (bytes kept verbatim so registry digests hold;
    refuses unless every blob is present with the declared size; atomic replace),
    `read_manifest` (exact, then case-insensitive), `list_manifests`, `referenced_blobs`.
  - `lock_shared` / `lock_exclusive` (`std::fs::File::lock*`, advisory, cross-process).
- `source::ModelSource` trait (`origin`, `root`, `list`, `get`, `blob_path`,
  `read_text_layer`, `ensure_writable`) and `ModelEntry` / `LayerInfo`, implemented by
  `AlkahestStore`; the Ollama adapter (ALK-US-0010) implements it next.
- `error::StoreError` (permission errors mapped to `PermissionDenied` with a hint).

## Decisions

- Same on-disk layout as Ollama's legacy store (`manifests/`, `%3A`-encoded host ports), so
  export/import is a file copy; Alkahest never writes `manifests-v2/` symlinks.
- The store holds only concrete image manifests; lists and references are resolved by the
  pull (ALK-SP-0002.R1) before installing.
- Blobs and manifests are chmod 0644 (temp files start 0600) so a service user and a desktop
  user can share a store.
- GC contract for ALK-US-0013: pulls hold `lock_shared` from the first blob to the manifest
  write; GC takes `lock_exclusive`, then `referenced_blobs()` vs `blobs()`.
- New regular dependencies (all already pinned): `serde`, `serde_json`, `sha2`, `tempfile`,
  `tracing`, `alkahest-core`.

## Verification

`tests/alkahest_store.rs` (17 tests), including the spec scenario `blob_written_atomically`,
mismatch/drop leave no partial, dedupe, missing/size-mismatched blobs refused, atomic
replacement, concurrent writers (8 threads), exclusive lock blocks shared, `ModelSource`
listing, `open_resolved`. Unit tests for manifest/config/list/reference parsing. fmt, clippy
`-D warnings`, nextest, deny, check-deps and the headless build pass.
