# Change: pull safetensors snapshots for vLLM (ALK-US-0020)

Implements [[ALK-US-0020]] / [[ALK-SP-0002]].R2 (safetensors half). Builds on
[[2026-10-17-hf-hub-client]], [[2026-10-17-hf-gguf-pull]], [[2026-10-17-m1-pull-glue]] and
[[2026-10-17-unified-catalog]]. Plan: [[plan/alkahest-v1-plan]].

## What

- `alkahest-hf::safetensors`: `select_snapshot_files` (allow patterns: `config.json`,
  `generation_config.json`, `tokenizer*`, `chat_template*`, `preprocessor*`, `*.safetensors`
  and their index, tiktoken/jinja/merges/vocab files; root only; `consolidated*` duplicates and
  LoRA adapters dropped), `pull_snapshot`, `SnapshotFacts::from_config` (family, context from
  `max_position_embeddings` also under `text_config`, hidden size, precision `BF16`/`F16`/`AWQ`,
  capabilities from `pipeline_tag`, `vision_config` and chat template presence).
- `alkahest-hf::pull`: `PullFormat { Auto, Gguf, Safetensors }`, `pull_hf_with`; `pull_hf` is
  `Auto`: GGUF when the repository has any, a snapshot when it has none, `:safetensors`
  selector forces a snapshot. Stored as `hf.co/org/repo:safetensors`, pinned to the commit sha.
- Verification: LFS files by sha256 (resumable downloader); small non-LFS files by size and
  hashed on arrival (trust on first use, listed in the config blob under
  `alkahest_unverified_files`); non-LFS files above 32 MiB are refused. No SHA-1 dependency
  was added, so the git blob id is not checked.
- `HfClient::file_bytes`.
- `alkahest-store` (additive): `media_type::SAFETENSORS_FILE`
  (`application/vnd.alkahest.safetensors.file.v1`, `name` = repository path),
  `snapshot.rs` (`AlkahestStore::materialise_snapshot`, `snapshot_path`, `snapshots_dir`,
  `remove_snapshot`; relative symlinks under `snapshots/<manifest digest>/`, built in a temp dir
  and renamed), `ModelEntry::is_snapshot` / `snapshot_dir` / `weights_path` (catalog
  `model_path` now reports the snapshot directory), `delete_model` and `gc` remove orphan views,
  `ModelInfo::derive` treats snapshot layers like tensor layers (format safetensors, loadable).
- `alkahest-api` (one field) and CLI: `PullOptions::safetensors`, `alkahest pull --safetensors`.

## Decisions

- Symlinks, not hard links: deleting a blob can never leave a hidden copy; views are removed
  with their manifest. Aliases made by `cp` share the view of the same manifest digest.
- Capabilities use `chat` (not `completion`) for templated models because the store classifier
  maps `completion` alone to a base model.

## Verification

`crates/alkahest-hf/tests/safetensors.rs` (wiremock: snapshot download and directory view,
automatic fallback, GGUF preferred, catalog row FORMAT safetensors / engine vllm, delete and gc,
corrupt LFS file rejected) and unit tests in `safetensors.rs`; opt-in
`real_hub_pull_of_a_tiny_safetensors_repository` (`ALKAHEST_TEST_NET=1`, defaults to
`hf-internal-testing/tiny-random-LlamaForCausalLM`) passes. `scripts/ci-local.sh` passes.
