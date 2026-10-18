# Change: import models from a local GGUF file or folder (ALK-US-0127, CLI and API)

CLI and admin API half of [[ALK-US-0127]]; the desktop "Add from file / folder" half waits for
its canvas design and product owner approval. Adds [[ALK-SP-0002]].R7 to R10 (local import,
folders and Modelfiles, linked imports, admin route and `run <path>`). Reuses the Modelfile
parser and create logic of [[ALK-US-0087]] ([[2026-10-17-ollama-create-blobs-logprobs]]). Plan:
[[plan/alkahest-v1-plan]].

## What

`alkahest-store`:

- `import.rs` (new): `AlkahestStore::import_file(src, ImportMode, no_hardlink, progress)`
  (copy: streamed through a `BlobWriter`, hashed on the way; link: hashed, then a hard link, or
  a symbolic link to the absolute path when hard linking fails or the source lies under the
  Ollama store), `AlkahestStore::write_imported_model` (one model layer per GGUF shard named
  after the original file, projector layer + `vision`, or `SAFETENSORS_FILE` layers with a
  caller-supplied config; Modelfile text layers; `Import` source record; links layer),
  `LinkRecord` / `LinkedFile` (`application/vnd.alkahest.links.v1+json`: path, digest, size,
  mtime, how), `linked_files_problem` (stat check: missing / changed, named), and
  `is_current_link_of`.
- `local.rs`: `BlobWriter::replacing_links(mtime)` and `commit_reporting()`: a copy import
  replaces a blob that is only a link with a store-owned copy (same mtime, so linked models
  sharing it still pass); documented exception to "a blob always hashes to its name".
- `snapshot.rs` / `source.rs`: split GGUF views. `is_split_manifest`, `needs_view`;
  `materialise_snapshot` also links the shards of a split model under their file names;
  `ModelEntry::is_split()` (Alkahest store only), `snapshot_dir()` and `weights_path()` return
  `<store>/snapshots/<digest>/<first shard>`. This also fixes Hugging Face split pulls, whose
  first blob llama.cpp could not load (it finds sibling shards by name).
- `create.rs`: text layers factored into `push_text_layers` (shared with imports).
- `catalog.rs`: `CatalogEntry::ensure_loadable` checks linked files first;
  `destination_name` (public form of the plain destination check).
- `ops.rs`: `remove_blob` also deletes a dangling symbolic link.

`alkahest-engine` `launch.rs`: `from_catalog` rebuilds a missing split view like a missing
safetensors snapshot (`ensure_snapshot` takes the root from `snapshot_dir()`).

`alkahest-hf`: `safetensors::chat_template_of` is public (folder imports read the template the
same way pulls do).

`alkahest-api`:

- `import.rs` (new): `import_model(catalog, ImportRequest, progress, cancel) -> ImportSummary`,
  planning (file, shard set, folder, safetensors folder via `select_snapshot_files`, folder
  `Modelfile`, explicit Modelfile with `FROM` path or model), `ImportEvent` progress,
  `default_model_name`, `looks_like_local_model`, `current_linked_import`. Refused with clear
  messages: several models in one folder, incomplete shard sets, lone projector or
  safetensors file, non-GGUF files, `ADAPTER`.
- `admin/imports.rs` (new): `POST /alkahest/v1/models/import {path, name, link}` as an `import`
  job (new `JobKind::Import`, phases `hashing`/`copying`/`installing`, bytes progress, log
  lines); keyless loopback callers only (`local` and `Caller::Local`), 403 otherwise; absolute
  paths only.
- `ollama/create.rs`: `Modelfile`, `parse_modelfile`, `parameters_json` are `pub(crate)`.

`alkahest-client`: `ImportModelRequest`, `Client::import_model` (for the desktop phase).

`alkahest` CLI: `create <name> [--from <path>] [-f Modelfile] [--link]` (without either, a
`./Modelfile` like `ollama create`), `import <path> [--name] [--link]`, progress bars in the
pull style (`hashing`/`copying` per file plus a total) or JSON events with `--json`;
`run <path>` imports in link mode under the default name (skipped when an unchanged linked
import exists) and runs it.

Docs: user guide "Importing local files" (`guide/models.md`), API reference "Local imports",
CLI reference regenerated.

## Decisions

- `run ./model.gguf` imports automatically with `--link` (no prompt): no bytes are copied, the
  import is reported on stderr, and `rm` removes it without touching the file.
- "Verified on load" is a stat check (size + modification time) of every linked blob, cheap
  enough for each load; a full re-hash of gigabytes per load was rejected. A hard-linked blob
  no longer shared with the source (source deleted, or replaced by save-and-rename) is the
  store's own unchanged copy and passes.
- The admin route is limited to keyless loopback callers, as the story says; a loopback
  caller with a key (`require_on_loopback`) is refused too and uses the CLI.
- `ADAPTER` stays refused: no engine passes LoRA adapters yet (inbox item).

## Verification

- `crates/alkahest-api/tests/import.rs`: `import_a_gguf_file`, `source_file_untouched`,
  `ollama_store_untouched_by_import`, `split_and_multimodal_folder`, `modelfile_in_folder`,
  `create_from_a_modelfile`, `safetensors_folder`, `link_with_a_hard_link`,
  `linked_file_changed`, `linked_file_disappeared`, `copy_import_replaces_a_linked_blob`,
  `cancelled_import_leaves_nothing`, `api_import_from_loopback`,
  `api_import_refused_for_remote_callers`; unit tests in `import.rs` and
  `admin/imports.rs`.
- `crates/alkahest/tests/import.rs`: `import_a_gguf_file_from_the_cli`,
  `create_from_a_modelfile_with_the_cli`; `crates/alkahest/tests/run.rs#run_a_gguf_path`.
- `crates/alkahest-client/src/admin.rs#import_model_starts_a_job`.
- fmt, clippy `-D warnings`, nextest workspace, deny, check-deps, docs-cli `--check`, headless
  build: see the commit hand-off.

## Open

- Desktop Library "Add from file… / Add from folder…" (canvas design first, ALK-US-0081 flow).
- LoRA `ADAPTER` support in the engines.

## Follow-up: linked-file check on Windows (2026-10-18)

The Windows CI job failed `linked_file_changed`: `is_link_blob` only counts hard links on Unix,
so on Windows a hard-linked blob was taken for a store-owned copy and only its size was compared.
NTFS also serves size and times read by path from that name's directory entry, which is not
refreshed when the file is written through another hard link. `linked_files_problem` now reads
metadata through an open handle and recognises a hard link by comparing the blob's file identity
with the recorded source path (`same-file`, MIT/Unlicense, already in the lock file through
walkdir). `cloud_model_is_ignored_without_warning` (ALK-US-0126) rebuilds the tracing interest
cache so it also passes under `cargo test` with several tests in one process.
