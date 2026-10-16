---
id: ALK-SP-0001
type: spec
title: Model store and Ollama compatibility
status: backlog
labels: [store]
created: 2026-10-16T22:37:32Z
updated: 2026-10-16T22:37:32Z
requirements:
  R1:
    status: backlog
  R2:
    status: backlog
  R3:
    status: backlog
  R4:
    status: backlog
  R5:
    status: backlog
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

WHEN an Ollama store exists at `$OLLAMA_MODELS`, `~/.ollama/models` or `/usr/share/ollama/.ollama/models`, the store SHALL expose its models by resolving `manifests-v2/` before legacy `manifests/` (hosts `registry.ollama.ai` and `ollama.com`) and SHALL never create, modify or delete a file inside it.

#### Scenario: Ollama model is listed
- **WHEN** Ollama has `llama3.2:latest` pulled
- **THEN** `alkahest ls` lists it with source `ollama` and its size

#### Scenario: delete refuses Ollama model
- **WHEN** the user runs `alkahest rm` on an Ollama-sourced model
- **THEN** the command fails with a message pointing to `ollama rm` and no file changes

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
