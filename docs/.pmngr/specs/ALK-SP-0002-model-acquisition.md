---
id: ALK-SP-0002
type: spec
title: Model acquisition
status: backlog
labels: [pull, hf]
created: 2026-10-16T22:37:33Z
updated: 2026-10-16T23:54:14Z
requirements:
  R1:
    status: backlog
    trace:
      code:
        - crates/alkahest-ollama/src/registry.rs#RegistryClient
        - crates/alkahest-ollama/src/pull.rs#pull_ollama
        - crates/alkahest-ollama/src/transfer.rs#Transfer
        - crates/alkahest/src/pull_cmd.rs#run
      tests:
        - crates/alkahest-ollama/tests/pull.rs#pull_from_registry_installs_manifest_and_all_layers
        - crates/alkahest-ollama/tests/pull.rs#manifest_is_not_installed_when_a_blob_fails
        - crates/alkahest-ollama/tests/pull.rs#manifest_list_resolves_to_the_llamacpp_entry
        - crates/alkahest-ollama/tests/pull.rs#model_in_the_ollama_store_is_reused_unless_forced
        - crates/alkahest/tests/pull.rs#pull_json_installs_the_manifest_and_prints_events
  R2:
    status: backlog
    trace:
      code:
        - crates/alkahest-hf/src/pull.rs#select_files
        - crates/alkahest-hf/src/pull.rs#pull_hf
      tests:
        - crates/alkahest-hf/tests/pull.rs#quant_selection_downloads_only_that_file_and_the_projector
        - crates/alkahest-hf/tests/pull.rs#default_pull_picks_q4_k_m_and_names_the_model_after_it
        - crates/alkahest-hf/tests/pull.rs#gated_repository_without_token_explains_how_to_authenticate
        - crates/alkahest-hf/src/pull.rs#shards_are_all_selected_by_any_shard_file
  R3:
    status: backlog
    trace:
      code:
        - crates/alkahest-hf/src/token.rs#resolve_token
        - crates/alkahest-hf/src/client.rs#HfClient
      tests:
        - crates/alkahest-hf/tests/client.rs#token_is_sent_as_bearer_to_the_endpoint_only
        - crates/alkahest-hf/tests/client.rs#no_token_means_anonymous_requests
        - crates/alkahest-hf/src/token.rs#tests
  R4:
    status: backlog
    trace:
      code:
        - crates/alkahest-fetch/src/download.rs#Downloader
        - crates/alkahest-fetch/src/plan.rs#plan_parts
      tests:
        - crates/alkahest-fetch/tests/download.rs#resume_downloads_only_missing_bytes_and_digest_matches
        - crates/alkahest-fetch/tests/download.rs#digest_mismatch_fails_and_removes_partial_files
  R5:
    status: backlog
  R6:
    status: backlog
    trace:
      code:
        - crates/alkahest-ollama/src/library.rs#OllamaLibrary
        - crates/alkahest/src/search_cmd.rs#run
        - crates/alkahest-hf/src/client.rs#HfClient
      tests:
        - crates/alkahest/tests/search.rs#hf_search
        - crates/alkahest-ollama/src/library.rs#parses_fixture_models
---

## Purpose

Download models from the Ollama registry and Hugging Face reliably, fast and resumably, with an optional Hugging Face token.

## Scope

Covers registry and Hub clients, file selection, transfer, integrity and progress reporting, and search. Store layout is [[ALK-SP-0001]].

Plan: [[plan/alkahest-v1-plan]]

## Requirements

### ALK-SP-0002.R1 — Ollama registry pull

WHEN the user pulls an Ollama name, the system SHALL fetch the manifest (resolving manifest lists), download missing blobs following CDN redirects, and install the manifest only after every blob is verified.

#### Scenario: pull from registry
- **WHEN** `alkahest pull llama3.2` runs on an empty store
- **THEN** manifest and all layers are installed and `alkahest ls` lists the model

### ALK-SP-0002.R2 — Hugging Face file selection

WHEN the user pulls a Hugging Face GGUF repository, the system SHALL select the file matching the requested quantization (default `Q4_K_M`, then the closest available), include all shards of a split GGUF and the matching `mmproj` file; for a safetensors repository it SHALL download the snapshot files a vLLM engine needs.

#### Scenario: quant selection
- **WHEN** the user pulls `hf.co/org/repo:Q8_0`
- **THEN** only the Q8_0 file (and mmproj if present) is downloaded

### ALK-SP-0002.R3 — Optional Hugging Face token

The system SHALL read a Hugging Face token from `HF_TOKEN`, the configuration file or the OS keyring, SHALL send it only to the configured Hub endpoint (`HF_ENDPOINT`, default `https://huggingface.co`) and SHALL never log it.

#### Scenario: gated repository
- **WHEN** a token with access is configured and a gated repo is pulled
- **THEN** the download succeeds

#### Scenario: no token
- **WHEN** no token is configured
- **THEN** public repositories download anonymously

### ALK-SP-0002.R4 — Resumable parallel transfer

The downloader SHALL split large blobs into parallel HTTP Range parts, persist per-part state in partial files, retry stalled or failed parts, resume after interruption and verify the sha256 before the atomic rename.

#### Scenario: resume
- **WHEN** a pull is interrupted at 60% and restarted
- **THEN** only missing bytes are downloaded and the final digest matches

### ALK-SP-0002.R5 — Progress reporting

Pulls SHALL report progress per layer (total, completed, rate) as a terminal progress bar in the CLI and as NDJSON status lines compatible with Ollama `/api/pull`.

#### Scenario: api progress
- **WHEN** a client posts to `/api/pull` with stream true
- **THEN** it receives NDJSON lines ending with `{"status":"success"}`

### ALK-SP-0002.R6 — Model search

The system SHALL search Hugging Face (filters: GGUF, task, sort by downloads or recency) and the Ollama library, returning name, description, size or quantizations and download counts.

#### Scenario: hf search
- **WHEN** `alkahest search qwen --source hf --gguf` runs
- **THEN** it lists matching GGUF repositories
