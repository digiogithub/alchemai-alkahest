---
id: ALK-SP-0002
type: spec
title: Model acquisition
status: backlog
labels: [pull, hf]
created: 2026-10-16T22:37:33Z
updated: 2026-10-17T23:09:44Z
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
        - crates/alkahest-hf/src/pull.rs#pull_hf_with
        - crates/alkahest-hf/src/safetensors.rs#select_snapshot_files
        - crates/alkahest-hf/src/safetensors.rs#pull_snapshot
        - crates/alkahest-store/src/snapshot.rs#materialise_snapshot
      tests:
        - crates/alkahest-hf/tests/pull.rs#quant_selection_downloads_only_that_file_and_the_projector
        - crates/alkahest-hf/tests/pull.rs#default_pull_picks_q4_k_m_and_names_the_model_after_it
        - crates/alkahest-hf/tests/pull.rs#gated_repository_without_token_explains_how_to_authenticate
        - crates/alkahest-hf/src/pull.rs#shards_are_all_selected_by_any_shard_file
        - crates/alkahest-hf/tests/safetensors.rs#safetensors_flag_downloads_the_snapshot_and_builds_the_directory
        - crates/alkahest-hf/tests/safetensors.rs#repository_without_gguf_is_pulled_as_a_snapshot_automatically
        - crates/alkahest-hf/tests/safetensors.rs#catalog_lists_the_snapshot_as_safetensors_for_vllm
        - crates/alkahest-hf/tests/safetensors.rs#delete_and_gc_remove_the_snapshot_view
        - crates/alkahest-hf/tests/safetensors.rs#corrupt_lfs_file_is_rejected
        - crates/alkahest-hf/tests/safetensors.rs#real_hub_pull_of_a_tiny_safetensors_repository
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
  R7:
    status: backlog
  R8:
    status: backlog
  R9:
    status: backlog
  R10:
    status: backlog
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

### ALK-SP-0002.R7 — Import a local GGUF file

WHEN the user runs `alkahest create <name> --from <file.gguf>` (or `alkahest import <file.gguf> [--name <name>]`), the system SHALL register the file as a model in the Alkahest store, copying it by default, with its format, family, quantisation and context length read from the GGUF header, a model name defaulting to the file stem, and blob digests equal to the sha256 of the source file. A shard of a split model given as the file SHALL import every shard. The source file SHALL never be moved, modified or deleted, and nothing SHALL be written under the Ollama store.

#### Scenario: import a GGUF file
- **WHEN** `alkahest import ./tiny-Q4_K_M.gguf` runs on a synthetic GGUF
- **THEN** `alkahest ls` lists `tiny-Q4_K_M` with the family and quantisation of the header and the weights blob digest equals the sha256 of the file

#### Scenario: source file untouched
- **WHEN** a file is imported in copy or link mode
- **THEN** its content hash, size and modification time are unchanged afterwards

#### Scenario: ollama store untouched by import
- **WHEN** a model is imported while an Ollama store is configured
- **THEN** no file under the Ollama store is created, modified or deleted

### ALK-SP-0002.R8 — Import a local model folder or Modelfile

WHEN the imported path is a folder, the system SHALL build one model from it: the shards of a split GGUF (`*-00001-of-0000N.gguf`) in order as one model that engines load through a view named after the original files, an `mmproj*.gguf` projector as the vision layer, and a `Modelfile` in the folder honoured (`TEMPLATE`, `PARAMETER`, `SYSTEM`, `LICENSE`); a Hugging Face safetensors folder (`config.json` plus `*.safetensors`) SHALL become a safetensors snapshot model for the vLLM engine. `alkahest create <name> -f Modelfile` SHALL accept `FROM ./file.gguf`, `FROM ./folder` (relative to the Modelfile) or a model name, as `ollama create -f` does. A folder holding several different models, an incomplete shard set or an instruction no engine applies (`ADAPTER`) SHALL be refused with a clear message.

#### Scenario: split and multimodal folder
- **WHEN** a folder with two shards `m-Q4_K_M-00001-of-00002.gguf`, `m-Q4_K_M-00002-of-00002.gguf` and `mmproj-m-f16.gguf` is imported
- **THEN** one model results with two weights layers in shard order, a projector layer, the `vision` capability, and a launch path whose file name is the first shard's name

#### Scenario: modelfile in folder
- **WHEN** the folder also holds a Modelfile with `SYSTEM`, `PARAMETER` and `TEMPLATE`
- **THEN** `alkahest show` returns that system prompt, those parameters and that template

#### Scenario: create from a modelfile
- **WHEN** `alkahest create mine -f ./Modelfile` runs with `FROM ./weights/tiny.gguf`
- **THEN** the model `mine` is created from that file with the Modelfile layers

#### Scenario: safetensors folder
- **WHEN** a folder with `config.json`, `tokenizer.json` and `model.safetensors` is imported
- **THEN** a safetensors model whose default engine is vLLM results, with a materialised snapshot directory

### ALK-SP-0002.R9 — Linked imports

WITH `--link`, the system SHALL register imported files without copying them: a hard link into the store when the source is on the same filesystem (and not under the Ollama store), otherwise a reference to the external file stored as a symbolic link, recording each file's path, digest, size and modification time in a links layer. Before an engine loads a linked model the system SHALL re-check each linked file (size and modification time) and refuse to load with a message naming the file when it changed or disappeared. A copy-mode import of a file whose blob is already present only as a link SHALL replace that link with a store-owned copy.

#### Scenario: link with a hard link
- **WHEN** a file on the store's filesystem is imported with `--link`
- **THEN** the weights blob and the source share one inode and no bytes are copied

#### Scenario: linked file changed
- **WHEN** a linked source file is modified in place after the import
- **THEN** loading the model fails with a message that names the file and says it changed

#### Scenario: linked file disappeared
- **WHEN** the external file of a symlinked import is deleted
- **THEN** loading the model fails with a message that names the file and says it is missing

### ALK-SP-0002.R10 — Import admin API and run of a local file

The admin API SHALL offer `POST /alkahest/v1/models/import {path, name, link}` that starts an `import` job reporting hashing and copying progress in bytes, allowed only for keyless loopback callers (others receive 403, since the path is read on the server machine). `alkahest run <path>` with a path to a local GGUF file or model folder SHALL import it in link mode under its default name (reusing an existing import of the same unchanged file) and then run it.

#### Scenario: api import from loopback
- **WHEN** a keyless loopback client posts a GGUF path to `/alkahest/v1/models/import`
- **THEN** it receives 202 with an `import` job that completes with the model name and the model is listed

#### Scenario: api import refused for remote callers
- **WHEN** a remote client with an admin key posts the same request
- **THEN** it receives 403 and nothing is imported

#### Scenario: run a gguf path
- **WHEN** `alkahest run ./tiny.gguf hi` runs against a server
- **THEN** the file is imported in link mode as `tiny` and the prompt is sent to model `tiny`
