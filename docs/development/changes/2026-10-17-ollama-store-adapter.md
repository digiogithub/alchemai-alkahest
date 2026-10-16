# Change: read-only Ollama store adapter (ALK-US-0010)

Implements [[ALK-US-0010]] / [[ALK-SP-0001]].R2. Builds on [[ALK-US-0009]] and
[[ALK-US-0011]]. Plan: [[plan/alkahest-v1-plan]].

## What

Crate `alkahest-store`, module `ollama`: `OllamaStore` implementing `ModelSource`.

- `OllamaStore::open(root)` (never creates; `NoStore` / `PermissionDenied` with a hint to join
  the `ollama` group or disable reuse) and `OllamaStore::discover(&ResolvedPaths)` (uses
  `paths.ollama_store` from `store.ollama.path` / `OLLAMA_MODELS` / `~/.ollama/models` /
  `/usr/share/ollama/.ollama/models`; `Ok(None)` when disabled or missing).
- Resolution order per name (`manifest_candidates`): `manifests-v2/<host>` (public host spelled
  `ollama.com`, port hosts `%3A`-encoded), then legacy `manifests/registry.ollama.ai` and
  `manifests/ollama.com` (legacy port hosts also unencoded), then a case-insensitive scan.
  Listing walks `manifests-v2` then `manifests`, dedupes by normalised name (first wins).
- Documents: plain image manifests, manifest lists (prefers runners `llamacpp`/`ggml` or
  format `gguf`, skips entries whose blob is not local) and manifest references; `"layers":
  null` accepted. Symlinked entries must point at a `sha256-<hex>` blob of the same store
  whose content matches the name; plain entries must resolve inside their tree.
- Layers: model, projector, adapter, template, system, params, messages, license (plus draft,
  embed, tensor, json) via `LayerKind`; config blob parsed into `ModelConfig`.
- `ensure_writable` always fails with `StoreError::ReadOnly` pointing to `ollama rm <name>`.
- `ModelConfig.remote_host/remote_model` and `ModelEntry::is_remote()` flag Ollama cloud
  models (`name:cloud`, no weights) found in the real store.

## Read-only proof

- No write API in the module; unit test `ollama_module_has_no_write_calls` scans the source.
- `tests/ollama_store.rs`: tree snapshot (path, kind, size, mtime, mode, link target) before
  and after every operation (`never_writes`, `delete_refuses_ollama_model`), the same
  operations on a `chmod 0555/0444` fixture (`works_on_read_only_store`),
  `missing_store_is_never_created`, `store_without_manifest_trees_is_empty`.
- Opt-in `ALKAHEST_TEST_OLLAMA_STORE=1`: listed the real `/usr/share/ollama/.ollama/models`
  on the dev machine (22 models: library, `hf.co/...`, cloud), metadata unchanged.

## Verification

16 + 1 integration tests (spec scenarios `ollama_model_is_listed`,
`delete_refuses_ollama_model`; v2-before-legacy, host aliases, case-insensitive lookup,
manifest list selection, every layer kind, hf.co and port hosts, symlink escape and tampered
manifest blob rejected, permission hint, cloud models). fmt, clippy `-D warnings`, nextest,
deny, check-deps and the headless build pass.

## Follow-ups

- The `rm` command itself (ALK-US-0013) must call `ModelSource::ensure_writable` first.
- The catalog (ALK-US-0012) should hide or mark `is_remote()` entries.
