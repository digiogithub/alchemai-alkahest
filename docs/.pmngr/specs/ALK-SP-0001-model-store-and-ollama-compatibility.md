---
id: ALK-SP-0001
type: spec
title: Model store and Ollama compatibility
status: backlog
labels: [store]
created: 2026-10-16T22:37:32Z
updated: 2026-10-17T23:14:44Z
requirements:
  R1:
    status: backlog
    trace:
      code:
        - crates/alkahest-store/src/local.rs#AlkahestStore
        - crates/alkahest-store/src/local.rs#BlobWriter
        - crates/alkahest-store/src/manifest.rs#Manifest
        - crates/alkahest-store/src/manifest.rs#media_type
        - crates/alkahest-store/src/source.rs#ModelSource
      tests:
        - crates/alkahest-store/tests/alkahest_store.rs#blob_written_atomically
        - crates/alkahest-store/tests/alkahest_store.rs#digest_mismatch_leaves_no_blob_and_no_partial
        - crates/alkahest-store/tests/alkahest_store.rs#manifest_layout_and_round_trip
        - crates/alkahest-store/tests/alkahest_store.rs#manifest_requires_every_blob
        - crates/alkahest-store/tests/alkahest_store.rs#concurrent_writers_are_safe
        - crates/alkahest-store/tests/alkahest_store.rs#exclusive_lock_blocks_writers
  R2:
    status: backlog
    trace:
      code:
        - crates/alkahest-store/src/ollama.rs#OllamaStore
        - crates/alkahest-store/src/catalog.rs#Catalog
        - crates/alkahest-store/src/source.rs#ModelEntry
        - crates/alkahest-store/src/reference.rs#ModelName::is_ollama_cloud
        - crates/alkahest-store/src/error.rs#StoreError::CloudModel
        - crates/alkahest-api/src/pull.rs#PullService::pull
      tests:
        - crates/alkahest-store/tests/ollama_store.rs#ollama_model_is_listed
        - crates/alkahest-store/tests/ollama_store.rs#delete_refuses_ollama_model
        - crates/alkahest-store/tests/ollama_store.rs#never_writes
        - crates/alkahest-store/tests/ollama_store.rs#works_on_read_only_store
        - crates/alkahest-store/tests/ollama_store.rs#resolves_manifests_v2_before_legacy
        - crates/alkahest-store/tests/ollama_store.rs#legacy_public_host_aliases
        - crates/alkahest-store/src/ollama.rs#ollama_module_has_no_write_calls
        - crates/alkahest-store/tests/delete_copy.rs#delete_refuses_ollama_model
        - crates/alkahest/tests/models.rs#rm_refuses_ollama_models
        - crates/alkahest/tests/models.rs#ollama_model_is_listed
        - crates/alkahest-store/tests/catalog.rs#cloud_model_is_ignored
        - crates/alkahest-store/tests/ollama_cloud.rs#cloud_model_is_ignored_in_listings
        - crates/alkahest-store/tests/ollama_cloud.rs#cloud_model_is_ignored_on_run_show_pull
        - crates/alkahest-store/tests/ollama_cloud.rs#cloud_model_is_ignored_without_warning
        - crates/alkahest-store/tests/ollama_cloud.rs#cloud_model_is_ignored_and_ollama_store_untouched
        - crates/alkahest-api/tests/cloud_models.rs#cloud_model_is_ignored_in_api_listings
        - crates/alkahest-api/tests/cloud_models.rs#cloud_model_show_is_a_clear_404
        - crates/alkahest-api/tests/cloud_models.rs#cloud_model_pull_is_refused_with_404
  R3:
    status: backlog
    trace:
      code:
        - crates/alkahest-store/src/reference.rs#ModelRef
        - crates/alkahest-store/src/reference.rs#ScopedRef
        - crates/alkahest-store/src/reference.rs#ModelName
        - crates/alkahest-store/src/reference.rs#HfRef
        - crates/alkahest-store/src/digest.rs#Digest
      tests:
        - crates/alkahest-store/src/reference.rs#short_name_resolves
        - crates/alkahest-store/src/reference.rs#hf_spellings
        - crates/alkahest-store/src/reference.rs#scope_prefixes
        - crates/alkahest-store/tests/reference_props.rs#model_ref_round_trips
        - crates/alkahest-store/tests/reference_props.rs#scoped_ref_round_trips
        - crates/alkahest-store/tests/reference_props.rs#parser_never_panics
  R4:
    status: backlog
    trace:
      code:
        - crates/alkahest-store/src/ops.rs#delete_model
        - crates/alkahest-store/src/ops.rs#gc
        - crates/alkahest-store/src/ops.rs#copy_model
      tests:
        - crates/alkahest-store/tests/delete_copy.rs#shared_blob_kept
        - crates/alkahest-store/tests/delete_copy.rs#gc_deletes_only_unreferenced_blobs
        - crates/alkahest-store/tests/delete_copy.rs#copy_from_ollama_copies_blobs_and_never_writes_there
  R5:
    status: backlog
    trace:
      code:
        - crates/alkahest-store/src/catalog.rs#Catalog
        - crates/alkahest-store/src/meta.rs#ModelInfo
        - crates/alkahest/src/models_cmd.rs#ls
      tests:
        - crates/alkahest-store/tests/catalog.rs#json_listing
        - crates/alkahest/tests/models.rs#json_listing
---

## Purpose

Keep models on disk in one content-addressed store and use models already pulled by a local Ollama install without copying or modifying them.

## Scope

Covers the Alkahest store layout, the read-only Ollama adapter, model reference grammar, listing, delete, copy and garbage collection. Downloading is in [[ALK-SP-0002]].

Plan: [[plan/alkahest-v1-plan]]

## Requirements

### ALK-SP-0001.R1 — Content-addressed store layout

The store SHALL keep blobs as `blobs/sha256-<hex>` and manifests as Docker distribution v2 JSON under `manifests/<host>/<namespace>/<name>/<tag>` below `$ALKAHEST_MODELS` (default: the platform data directory), using the Ollama layer media types plus `application/vnd.alkahest.*` types for Alkahest-only layers.

#### Scenario: blob written atomically
- **WHEN** a blob download completes and its sha256 matches
- **THEN** the blob is renamed into `blobs/` and no partial file remains

### ALK-SP-0001.R2 — Read-only Ollama store adapter

WHEN an Ollama store exists at `$OLLAMA_MODELS`, `~/.ollama/models` or `/usr/share/ollama/.ollama/models`, the store SHALL expose its models by resolving `manifests-v2/` before legacy `manifests/` (hosts `registry.ollama.ai` and `ollama.com`) and SHALL never create, modify or delete a file inside it. Ollama cloud models (a `cloud` or `*-cloud` tag, or a config naming `remote_host` / `remote_model`, typically with `"layers": null`) SHALL NOT be listed, SHALL be logged at most once per model and process at debug level, and a reference to one SHALL fail with "Ollama cloud models are not supported by Alkahest" (404 in the API dialects) (ALK-US-0126).

#### Scenario: Ollama model is listed
- **WHEN** Ollama has `llama3.2:latest` pulled
- **THEN** `alkahest ls` lists it with source `ollama` and its size

#### Scenario: delete refuses Ollama model
- **WHEN** the user runs `alkahest rm` on an Ollama-sourced model
- **THEN** the command fails with a message pointing to `ollama rm` and no file changes

#### Scenario: cloud model is ignored
- **WHEN** the Ollama store holds `glm-5.1:cloud` (manifest `"layers": null`, config with `remote_host` and `"model_families": null`) next to `llama3.2:latest`
- **THEN** `alkahest ls`, `/api/tags`, `/v1/models` and the admin model list show only `llama3.2:latest`, no warning is logged, `run` / `show` / `pull` of `glm-5.1:cloud` fail with "Ollama cloud models are not supported by Alkahest" (404 in the API) and no file in the Ollama store changes

### ALK-SP-0001.R3 — Model reference grammar

The CLI and API SHALL accept `[host/][namespace/]name[:tag]` for Ollama-style names, `hf.co/<org>/<repo>[:<quant or file>]` for Hugging Face, and the prefixes `ollama:` and `alkahest:` to disambiguate, defaulting host `registry.ollama.ai`, namespace `library` and tag `latest`.

#### Scenario: short name resolves
- **WHEN** the user passes `qwen3`
- **THEN** it resolves to `registry.ollama.ai/library/qwen3:latest`

### ALK-SP-0001.R4 — Delete and garbage collection

WHEN a model is deleted, the store SHALL remove its manifest and SHALL delete only the blobs no remaining Alkahest manifest references.

#### Scenario: shared blob kept
- **WHEN** two tags share one weights blob and one tag is deleted
- **THEN** the shared blob stays on disk

### ALK-SP-0001.R5 — Unified catalog

The catalog SHALL list every model from all sources with name, source, digest, size, format (gguf, safetensors), quantization, family, kind and modification time, de-duplicating identical digests.

#### Scenario: json listing
- **WHEN** `alkahest ls --json` runs
- **THEN** each entry carries all listed fields
