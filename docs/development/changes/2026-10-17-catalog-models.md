# Catalog models: `alkahest/<namespace>/<model>` pull and search

Date: 2026-10-17. Story: [[ALK-US-0120]]. Spec: [[ALK-SP-0011]] (R7, R12 model part, R13,
R14 model downloads). Decision: [[0006-catalog-fetch-strategy]], product owner review of
2026-10-17, point 3 (pseudo-host `alkahest`, no `catalog:` prefix, `--catalog <name>`). Builds
on [[ALK-US-0118]] (sync, snapshots, `layered_models` / `find_model`) and [[ALK-US-0121]]
(official catalog, `cargo xtask catalog-check`).

## Spec

- ALK-SP-0011.R13 rewritten ("alkahest/ catalog model names"): names are
  `alkahest/<namespace>/<model>[:<tag>]`, parsed like `hf.co/<org>/<repo>`; `alkahest:` stays
  the store scope of ALK-SP-0001.R3 (`alkahest:alkahest/acme/x` = catalog model, Alkahest store
  only); `--catalog <name>` on `pull` and `search`; a missing tag or `latest` (when the entry
  has no `latest` variant) selects the first variant and the concrete tag is stored; the bare
  name resolves to the only local tag; the host is never contacted as a registry; a digest
  mismatch leaves no manifest and no blob. New scenario "catalog selected explicitly".
- ALK-SP-0011.R7: plain `http` model URLs are accepted only for loopback hosts (the R9 rule for
  sources; local mirrors and tests); `size` must be greater than zero.
- Still to reconcile: the Scope paragraph of ALK-SP-0011 still says "the `catalog:` model
  reference" (left untouched; only R13 was in the remit of this story).

## What changed

- **`alkahest-store`**: `CATALOG_HOST = "alkahest"`; `ModelName::is_catalog`,
  `catalog_entry`, `has_default_tag`; catalog namespaces accept `.` like catalog names.
  `Catalog::resolve` maps `alkahest/<ns>/<model>` (no tag) to the only local tag of that model
  (ambiguous with two). `SourceKind::Catalog`. `RecommendedLayer` gains optional `stop`,
  `engine` and `template` (additive; the Hugging Face JSON still parses), carried into
  `ModelSettings` by `to_settings`.
- **`alkahest-catalog`**: new `pull` module. `resolve_model` (catalog or layer order, default
  tag, precise "no catalog entry ... (searched: ...)", "no tag ...; available: ...", "no catalog
  named ..." errors). `pull_catalog` downloads each file through `alkahest_pull::Transfer`
  (alkahest-fetch: resumable, parallel ranges, sha256 and size verified), mirrors in order
  (cancellation stops, any other failure moves on), bearer token only for files with `auth` and
  only to URLs on that host (`CredentialSource::host_token`: `ALKAHEST_HOST_TOKEN_<HOST>`, then
  keyring `alkahest` / `host:<host>`). Manifest: one layer per file (model, projector, adapter,
  vae; tokenizer/config/text-encoder as `application/vnd.alkahest.catalog.file.v1`), a Go
  `TEMPLATE` layer, an Ollama `params` layer, the source layer (`kind = catalog`, revision =
  commit) and an `application/vnd.alkahest.config.v1+json` layer (`CatalogModelConfig`: origin
  catalog/commit/entry/tag, engine, template, params, licence). Engine, template (named or
  Jinja) and known params become the model's recommended defaults, so `run` and `serve` use
  them while a user overlay wins. A failed pull writes no manifest and removes the blobs it
  added that no manifest references. `search_models` filters layered rows by words over name,
  description, licence, tags, tag and quant. `ModelFile.sha256`/`size` parse with defaults so
  a missing value is reported by `validate` with entry, tag and file.
- **`alkahest-ollama`**: `pull_ollama` refuses catalog names (`PullError::CatalogName`), so the
  host `alkahest` is never contacted.
- **`alkahest-api`**: `PullService` routes `alkahest/...` names to `pull_catalog`
  (`PullOptions.catalog`, now `Clone` instead of `Copy`; `PullSummary::Installed.origin`).
  Admin: `POST /alkahest/v1/pulls` accepts `catalog`; new
  `GET /alkahest/v1/catalog/search?q=&catalog=&limit=` (local snapshots only, one row per tag
  with `reference`, `default`, size, quant, licence, tags, engine, catalog, commit). Additive.
- **CLI**: `alkahest pull <ref> [--catalog <name>]` (catalog names run `catalog_cmd::prepare`
  first; `--catalog` with a non-catalog name is an error); `alkahest search --source catalog`
  and `--catalog <name>`; `all` lists catalog rows first. The search listing gains `size` and
  `license` columns (filled for catalog rows). `ls` shows the `alkahest/...` name.
- **`alkahest-core`**: `is_loopback_http` is public (shared rule).
- **xtask**: `catalog-check` parses model entries with `alkahest_catalog::ModelEntry` and runs
  its `validate`, so sync and the strict check share one schema; the local serde structs are
  gone.
- **Docs**: `guide/catalogs.md` "Finding and pulling catalog models", `guide/models.md`
  reference table, `reference/api.md` discovery row, `reference/cli.md` regenerated.

## Verification

- `cargo fmt --all --check`: clean. `cargo clippy --workspace --all-targets --locked -- -D
  warnings`: clean.
- `cargo nextest run --workspace --locked`: 1310 passed, 1 skipped.
- `cargo xtask check-deps`, `cargo xtask catalog-check`, `cargo xtask docs-cli --check`,
  `cargo build -p alkahest --no-default-features --locked`: ok.
- `cargo deny check`: bans, licences, sources ok; advisories fail on the pre-existing
  RUSTSEC-2026-0370/0371 (zbus_names, zvariant through gpui), unrelated.

Scenario tests: `crates/alkahest-catalog/tests/pull.rs` (`default_tag`,
`mirrors_tried_in_order`, `wrong_digest_leaves_nothing`,
`a_failed_pull_removes_the_blobs_it_added`, `catalog_selected_explicitly`,
`unknown_catalog_entry`, `search_lists_catalog_models`), `crates/alkahest/tests/catalog_models.rs`
(pull + `ls` + `show` + `search` through the binary, wrong digest, unknown entry and `--catalog`
rules), `crates/alkahest-store` (`reference_parses_and_formats_back`, property test
`catalog_name_round_trips`, `catalog_model_without_tag_resolves_to_its_only_tag`),
`crates/alkahest-api/tests/admin_api.rs#catalog_search_lists_snapshot_models`.

## Open points

- Safetensors catalog variants are listed but refused by `pull` (no snapshot layout yet).
- A Go `template_text` is stored as the `TEMPLATE` layer but, like Ollama Go templates
  elsewhere, does not change the llama.cpp template when the GGUF has its own.
- With several mirrors, a failing mirror is retried with the downloader's normal retry budget
  (up to about 30 s for 5xx) before the next mirror is tried.
- `alkahest search` without `--source` now does the first-fetch of a never-synced catalog (10 s
  deadline), like `engines ls`; tests set `ALKAHEST_CATALOG_AUTO_SYNC=0`.
- The blob rollback after a failed pull runs under the shared store lock; a concurrent pull of
  the very same blob that has not written its manifest yet could lose it, leaving that model
  with a missing blob until it is pulled again. Unlikely (same file, overlapping pulls, one
  failing); GC has no such window because it takes the lock exclusively.
