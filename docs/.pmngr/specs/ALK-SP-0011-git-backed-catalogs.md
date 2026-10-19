---
id: ALK-SP-0011
type: spec
title: Git-backed catalogs
status: backlog
author: mcp
labels: [catalog, recipes, models, security]
created: 2026-10-17T18:38:01Z
updated: 2026-10-19T15:33:31Z
links:
  - { kind: relates_to, target: ALK-US-0117 }
requirements:
  R1:
    status: backlog
    trace:
      code: [xtask/src/catalog.rs#check, catalog/catalog.toml]
      tests:
        - xtask/src/catalog_tests.rs#minimal_catalog_loads
        - xtask/src/catalog_tests.rs#stem_mismatch_rejected
        - xtask/src/catalog_tests.rs#other_files_are_ignored
        - xtask/src/catalog_tests.rs#the_repository_catalog_is_valid
  R2:
    status: backlog
    trace:
      code: [xtask/src/catalog.rs#check_manifest]
      tests:
        - xtask/src/catalog_tests.rs#newer_major_schema_refused_and_minor_accepted
  R3:
    status: backlog
    trace:
      code:
        - xtask/src/catalog.rs#check
        - crates/alkahest-catalog/src/content.rs#validate
        - crates/alkahest-catalog/src/schema.rs#strip_unknown
      tests:
        - xtask/src/catalog_tests.rs#strict_check_rejects_unknown_keys
        - crates/alkahest-catalog/tests/sync.rs#unknown_field_ignored_with_a_warning
        - crates/alkahest-catalog/tests/sync.rs#invalid_file_keeps_the_previous_snapshot
        - crates/alkahest-catalog/src/content.rs#every_failing_file_is_reported
  R4:
    status: backlog
    trace:
      code:
        - crates/alkahest-recipes/src/recipe.rs#parse_recipes_with
        - crates/alkahest-recipes/src/recipe.rs#RecipeOrigin
        - xtask/src/catalog.rs#check_engines
        - xtask/src/catalog.rs#check_drift
        - catalog/engines/llama-cpp-turboquant.toml
        - catalog/engines/llama-cpp-mtp.toml
        - catalog/engines/llama-cpp-kvstream.toml
      tests:
        - crates/alkahest-recipes/tests/dependencies.rs#catalog_recipe_equals_a_builtin_recipe
        - crates/alkahest-recipes/tests/dependencies.rs#tofu_refused_in_a_git_catalog
        - crates/alkahest-recipes/tests/dependencies.rs#plain_http_refused
        - crates/alkahest-recipes/tests/dependencies.rs#a_branch_source_rev_warns_in_a_git_catalog_only
        - xtask/src/catalog_tests.rs#tofu_refused_in_a_git_catalog
        - xtask/src/catalog_tests.rs#drift_between_recipes_and_catalog_engines_detected
        - crates/alkahest-recipes/tests/forks.rs#a_fork_recipe_declares_its_base_family
        - crates/alkahest-recipes/tests/forks.rs#an_unknown_id_without_base_is_rejected
        - crates/alkahest-recipes/tests/forks.rs#base_must_be_a_known_family
        - crates/alkahest-recipes/tests/forks.rs#a_fork_may_not_reuse_a_builtin_id_or_alias
        - crates/alkahest-recipes/tests/forks.rs#builtin_recipes_have_no_base
  R5:
    status: backlog
    trace:
      code:
        - crates/alkahest-recipes/src/recipe.rs#Dependency
        - crates/alkahest-recipes/src/install.rs#install
        - crates/alkahest/src/engines_cmd.rs#info
      tests:
        - crates/alkahest-recipes/tests/dependencies.rs#required_dependency_installed_with_the_engine
        - crates/alkahest-recipes/tests/dependencies.rs#optional_dependency_only_on_request
        - crates/alkahest-recipes/tests/dependencies.rs#dest_escaping_the_install_directory_refused
        - crates/alkahest-recipes/tests/dependencies.rs#companions_are_read_as_optional_dependencies
        - crates/alkahest-recipes/tests/companions.rs
  R6:
    status: backlog
    trace:
      code:
        - crates/alkahest-recipes/src/recipe.rs#select_native
        - crates/alkahest-recipes/src/install.rs#InstalledEngine
        - crates/alkahest/src/engines_cmd.rs#install_cmd
        - recipes/llama-cpp.toml
      tests:
        - crates/alkahest-recipes/tests/dependencies.rs#native_variant_never_auto_selected
        - crates/alkahest-recipes/tests/dependencies.rs#native_variant_with_assets_refused
        - crates/alkahest-recipes/tests/dependencies.rs#native_install_marked_stale_on_another_cpu
        - crates/alkahest-recipes/tests/dependencies.rs#a_native_build_records_the_cpu_features_it_was_built_for
        - crates/alkahest-recipes/tests/dependencies.rs#real_native_llama_cpp_build
        - crates/alkahest-recipes/tests/forks.rs#llama_cpp_has_a_source_only_cuda_native_variant
        - crates/alkahest-recipes/tests/forks.rs#native_picks_cuda_native_on_a_cuda_host_and_cpu_native_elsewhere
        - crates/alkahest-recipes/tests/forks.rs#cuda_arch_replaces_native_in_source_builds
        - crates/alkahest-recipes/tests/forks.rs#a_native_cuda_variant_needs_no_cuda_version_but_others_do
        - crates/alkahest-engine/tests/forks.rs#a_cuda_native_build_wins_over_the_portable_one
  R7:
    status: backlog
    trace:
      code:
        - crates/alkahest-catalog/src/model.rs#ModelEntry
        - crates/alkahest-catalog/src/model.rs#validate
        - crates/alkahest-catalog/src/pull.rs#plan_files
        - crates/alkahest-catalog/src/pull.rs#fetch_file
        - xtask/src/catalog.rs#check_model
      tests:
        - crates/alkahest-catalog/src/model.rs#a_valid_entry_passes
        - crates/alkahest-catalog/src/model.rs#missing_sha256_refused
        - crates/alkahest-catalog/src/model.rs#url_and_urls_exclusive
        - crates/alkahest-catalog/tests/pull.rs#mirrors_tried_in_order
        - xtask/src/catalog_tests.rs#missing_sha256_refused
        - xtask/src/catalog_tests.rs#url_and_urls_exclusive
  R8:
    status: backlog
    trace:
      code:
        - xtask/src/catalog.rs#generate_index
        - xtask/src/catalog.rs#check_index
        - catalog/index.toml
        - crates/alkahest-catalog/src/manifest.rs#Index
        - crates/alkahest-catalog/src/sync.rs#fetch_raw
        - crates/alkahest/src/catalog_cmd.rs#index
      tests:
        - xtask/src/catalog_tests.rs#stale_index_rejected_by_the_check
        - xtask/src/catalog_tests.rs#index_generation_is_deterministic
        - crates/alkahest-catalog/tests/sync.rs#unchanged_files_are_not_downloaded
        - crates/alkahest-catalog/tests/sync.rs#digest_mismatch_fails_the_sync
        - crates/alkahest-catalog/tests/sync.rs#index_path_outside_the_layout_fails
  R9:
    status: backlog
    trace:
      code:
        - crates/alkahest-core/src/config.rs#CatalogSource
        - crates/alkahest-catalog/src/edit.rs#add_source
        - crates/alkahest-catalog/src/edit.rs#remove_source
        - crates/alkahest/src/catalog_cmd.rs#run
      tests:
        - crates/alkahest-core/src/config.rs#default_official_catalog
        - crates/alkahest-core/src/config.rs#catalog_token_in_a_file_is_refused
        - crates/alkahest/tests/catalog.rs#default_official_catalog
        - crates/alkahest/tests/catalog.rs#add_and_remove_a_catalog
        - crates/alkahest-catalog/tests/sync.rs#pinned_catalog_never_refreshes
  R10:
    status: backlog
    trace:
      code:
        - crates/alkahest-catalog/src/snapshot.rs#install
        - crates/alkahest-catalog/src/sync.rs#sync
        - crates/alkahest-catalog/src/layers.rs#CatalogState
        - crates/alkahest/src/catalog_cmd.rs#prepare
        - crates/alkahest/src/serve_cmd.rs#spawn_catalog_refresh
      tests:
        - crates/alkahest-catalog/tests/sync.rs#refresh_skipped_when_the_commit_is_unchanged
        - crates/alkahest-catalog/tests/sync.rs#offline_uses_the_last_good_snapshot
        - crates/alkahest-catalog/tests/sync.rs#fresh_install_offline
        - crates/alkahest-catalog/tests/sync.rs#interrupted_sync_leaves_no_partial_snapshot
        - crates/alkahest/tests/catalog.rs#fresh_install_offline
  R11:
    status: backlog
    trace:
      code:
        - crates/alkahest-catalog/src/refs.rs#resolve
        - crates/alkahest-catalog/src/hosts.rs#raw_url
        - crates/alkahest-catalog/src/hosts.rs#archive_url
        - crates/alkahest-catalog/src/archive.rs#extract_catalog
        - crates/alkahest-catalog/src/http.rs#HttpClient
      tests:
        - crates/alkahest-catalog/tests/sync.rs#ref_resolved_without_git
        - crates/alkahest-catalog/tests/sync.rs#v0_fallback
        - crates/alkahest-catalog/tests/sync.rs#archive_fallback_without_index
        - crates/alkahest-catalog/tests/sync.rs#archive_commit_mismatch_refused
        - crates/alkahest/tests/catalog.rs#proxy_honoured
        - crates/alkahest/tests/catalog.rs#official_catalog_syncs_from_github
  R12:
    status: backlog
    trace:
      code:
        - crates/alkahest-recipes/src/catalog.rs#RecipeLayer
        - crates/alkahest-catalog/src/layers.rs#recipe_catalog
        - crates/alkahest-catalog/src/layers.rs#layered_models
        - crates/alkahest-catalog/src/layers.rs#search_models
        - crates/alkahest-catalog/src/pull.rs#resolve_model
        - crates/alkahest/src/engines_cmd.rs#load_catalog
        - crates/alkahest-api/src/admin/engines.rs#RecipeEngineManager
      tests:
        - crates/alkahest-catalog/tests/layers.rs#git_catalog_overrides_builtin
        - crates/alkahest-catalog/tests/layers.rs#user_directory_overrides_catalogs
        - crates/alkahest-catalog/tests/layers.rs#later_catalog_overrides_earlier
        - crates/alkahest-catalog/tests/layers.rs#disabled_catalog_contributes_nothing
        - crates/alkahest/tests/catalog.rs#engines_show_the_catalog_origin
        - crates/alkahest-catalog/tests/pull.rs#catalog_selected_explicitly
        - crates/alkahest-catalog/tests/pull.rs#search_lists_catalog_models
  R13:
    status: backlog
    trace:
      code:
        - crates/alkahest-store/src/reference.rs#CATALOG_HOST
        - crates/alkahest-store/src/catalog.rs#resolve
        - crates/alkahest-catalog/src/pull.rs#resolve_model
        - crates/alkahest-catalog/src/pull.rs#pull_catalog
        - crates/alkahest-api/src/pull.rs#PullService
        - crates/alkahest-ollama/src/pull.rs#pull_ollama
        - crates/alkahest/src/pull_cmd.rs#run
        - crates/alkahest/src/search_cmd.rs#build_listing
        - crates/alkahest-api/src/admin/discover.rs#catalog_search
      tests:
        - crates/alkahest-store/src/reference.rs#reference_parses_and_formats_back
        - crates/alkahest-store/tests/reference_props.rs#catalog_name_round_trips
        - crates/alkahest-store/tests/catalog.rs#catalog_model_without_tag_resolves_to_its_only_tag
        - crates/alkahest-catalog/tests/pull.rs#default_tag
        - crates/alkahest-catalog/tests/pull.rs#catalog_selected_explicitly
        - crates/alkahest-catalog/tests/pull.rs#wrong_digest_leaves_nothing
        - crates/alkahest-catalog/tests/pull.rs#a_failed_pull_removes_the_blobs_it_added
        - crates/alkahest-catalog/tests/pull.rs#unknown_catalog_entry
        - crates/alkahest/tests/catalog_models.rs#pulled_catalog_model_is_listed_and_shown
        - crates/alkahest/tests/catalog_models.rs#wrong_digest_leaves_nothing
        - crates/alkahest/tests/catalog_models.rs#unknown_catalog_entry
        - crates/alkahest-api/tests/admin_api.rs#catalog_search_lists_snapshot_models
  R14:
    status: backlog
    trace:
      code:
        - crates/alkahest-catalog/src/http.rs#KeyringCredentials
        - crates/alkahest-catalog/src/http.rs#Token
        - crates/alkahest-catalog/src/http.rs#host_token_env_var
        - crates/alkahest-catalog/src/archive.rs#extract_catalog
        - crates/alkahest-catalog/src/sync.rs#Limits
        - crates/alkahest-catalog/src/pull.rs#plan_files
        - crates/alkahest-catalog/src/pull.rs#rollback
      tests:
        - crates/alkahest-catalog/tests/sync.rs#sync_never_executes
        - crates/alkahest-catalog/tests/sync.rs#token_never_logged
        - crates/alkahest-catalog/tests/sync.rs#token_not_forwarded_across_hosts
        - crates/alkahest-catalog/tests/sync.rs#oversized_catalog_refused
        - crates/alkahest-catalog/tests/sync.rs#path_traversal_refused
        - crates/alkahest-catalog/src/pull.rs#model_file_first_and_token_only_for_its_host
        - crates/alkahest-catalog/src/pull.rs#host_matching
        - crates/alkahest-catalog/tests/pull.rs#wrong_digest_leaves_nothing
---

## Purpose

Let every Alkahest installation see a current catalog of engine recipes and model entries without a new release, by reading plain TOML catalogs kept in git repositories: the official one in the `catalog/` folder of `github.com/digiogithub/alchemai-alkahest` (ref `main`) and any extra catalogs the user adds, private ones included.

## Scope

Covers the catalog layout and file formats (manifest, engine recipes with the dependency and native extensions, model entries, index), `schema_version` handling, catalog source configuration, the snapshot cache and refresh policy, the fetch transport, the layer order with the built-in and local recipes, the `alkahest/<namespace>/<model>` catalog model names and the security rules. The recipe format itself is [[ALK-SP-0003]] (R3); model acquisition is [[ALK-SP-0002]]; the store and reference grammar are [[ALK-SP-0001]].

Fetch strategy: [[development/decisions/0006-catalog-fetch-strategy]]. Example catalog: [[research/catalog-format-example]]. Epic [[ALK-EP-0015]], format story [[ALK-US-0117]].

## Requirements

### ALK-SP-0011.R1 — Catalog layout

A catalog SHALL be a directory (the `subdir` of a git repository at one commit) holding `catalog.toml` (the manifest, R2), `index.toml` (the file index, R8), `engines/<id>.toml` (one engine recipe per file, R4) and `models/<namespace>/<model>.toml` (one model entry per file, R7). The file stem SHALL equal the engine `id` and the path SHALL equal the model `name`; a mismatch rejects the file. Files outside these paths (README, LICENSE, CI scripts) SHALL be ignored, never parsed and never executed. Names SHALL use lower-case ASCII letters, digits, `-`, `_` and `.` only.

#### Scenario: minimal catalog loads
- **GIVEN** a directory with `catalog.toml`, `index.toml`, `engines/llama-cpp.toml` and `models/acme/assistant.toml`
- **WHEN** it is loaded as a catalog
- **THEN** one engine `llama-cpp` and one model `acme/assistant` are listed

#### Scenario: stem mismatch rejected
- **GIVEN** `engines/llama.toml` containing `id = "llama-cpp"`
- **WHEN** the catalog is loaded
- **THEN** the file is rejected with an error naming the path, the stem and the id

#### Scenario: other files ignored
- **GIVEN** a catalog with `README.md` and `scripts/update.sh`
- **WHEN** it is loaded
- **THEN** neither file is read nor run, and no warning is emitted

### ALK-SP-0011.R2 — Catalog manifest and schema_version

`catalog.toml` SHALL carry `schema_version = "MAJOR.MINOR"` (this specification is `1.0`), `name` (a catalog name as in R1), `description`, and MAY carry `min_alkahest` (a semver requirement such as `>=0.3.0`), `homepage` and `maintainers`. A catalog whose major version is newer than the one the running Alkahest supports, or whose `min_alkahest` the running version does not satisfy, SHALL be refused as a whole with a message naming the catalog, the version it declares, the version supported and the remedy (upgrade Alkahest); the previous snapshot of that catalog SHALL stay in use. A newer minor version of the same major SHALL be accepted. A missing or malformed `schema_version` SHALL refuse the catalog.

#### Scenario: newer major refused clearly
- **GIVEN** a synced snapshot at schema `1.0` and a new commit whose `catalog.toml` says `schema_version = "2.0"`
- **WHEN** the catalog is synced
- **THEN** the sync fails with "catalog official uses schema 2.0, this Alkahest supports 1.x: upgrade Alkahest", and the `1.0` snapshot is still the one listed

#### Scenario: newer minor accepted
- **GIVEN** a catalog with `schema_version = "1.7"`
- **WHEN** an Alkahest supporting `1.0` loads it
- **THEN** the catalog loads and its entries are listed

#### Scenario: min_alkahest not satisfied
- **GIVEN** a catalog with `min_alkahest = ">=9.0.0"`
- **WHEN** Alkahest 0.x syncs it
- **THEN** the catalog is refused with a message naming the required and running versions

### ALK-SP-0011.R3 — Forward-compatible parsing and all-or-nothing validation

Files read from a git catalog SHALL be parsed leniently: unknown keys at any level SHALL be ignored and reported as warnings (file, key path) by `alkahest catalog ls` and the sync summary, so a catalog written for a newer minor version keeps working. Known keys with a wrong type or value, invalid TOML, and any rule violation of R1 to R8 SHALL fail validation. Validation of a fetched snapshot SHALL be all-or-nothing: if any file fails, the whole new snapshot is discarded, the previous snapshot stays in use, and every failing file is reported with its reason. `cargo xtask catalog-check` SHALL run in strict mode, in which unknown keys are errors, so the official catalog never carries typos. Local layers (`recipes/` next to the binary, the user directory) keep the strict parsing of [[ALK-SP-0003]] R3.

#### Scenario: unknown field ignored with a warning
- **GIVEN** `engines/llama-cpp.toml` with an extra key `variants.cpu.experimental_flag = true`
- **WHEN** the catalog is synced
- **THEN** the recipe loads and the sync summary warns `engines/llama-cpp.toml: unknown key variants.cpu.experimental_flag`

#### Scenario: invalid file keeps the previous snapshot
- **GIVEN** a good snapshot and a new commit in which `models/acme/assistant.toml` is not valid TOML
- **WHEN** the catalog is synced
- **THEN** the sync reports the file and the parse error, and every command keeps using the previous snapshot

#### Scenario: strict check rejects unknown keys
- **GIVEN** a catalog with a misspelt key `sha265`
- **WHEN** `cargo xtask catalog-check` runs on it
- **THEN** it fails naming the file and the key

### ALK-SP-0011.R4 — Engine recipes in catalogs

`engines/<id>.toml` SHALL use the recipe format of [[ALK-SP-0003]] R3 (`id`, pinned `version`, `executable`, `[variants.<name>]` with `assets`, `companions`, `source` or `venv`) plus the extensions of R5 and R6. In a git catalog every asset and dependency SHALL carry a `sha256`; `tofu = true` SHALL be refused there (it stays allowed only in local layers). Every download URL SHALL be `https`. A `source.rev` SHOULD be a commit id or a tag; a branch name SHALL produce a warning. A recipe whose `id` is not a built-in engine SHALL declare `base = "<built-in id>"` (a fork engine, ADR 0007); a built-in id SHALL NOT declare `base`; a fork id SHALL NOT equal a built-in id or alias in any spelling. These rules apply to local recipe directories, git catalog syncs and `cargo xtask catalog-check` alike.

#### Scenario: catalog recipe equals a built-in recipe
- **GIVEN** `catalog/engines/llama-cpp.toml` byte-identical to `recipes/llama-cpp.toml`
- **WHEN** both are parsed
- **THEN** they produce the same list of recipes

#### Scenario: tofu refused in a git catalog
- **GIVEN** a catalog recipe with an asset that has `tofu = true` and no `sha256`
- **WHEN** the catalog is synced
- **THEN** the file fails validation with "sha256 required in git catalogs (tofu is local only)"

#### Scenario: plain http refused
- **GIVEN** a catalog asset with `url = "http://example.org/x.tar.gz"`
- **WHEN** the catalog is validated
- **THEN** it fails naming the asset and the rule "url must be https"

#### Scenario: unknown engine id without base refused
- **GIVEN** a recipe `id = "my-llama"` without `base`
- **WHEN** it is parsed
- **THEN** it is rejected and the error suggests `base = "llama-cpp"`

### ALK-SP-0011.R5 — Recipe dependency downloads

A variant MAY declare `[[variants.<v>.dependencies]]`, each with `id` (unique within the variant), `os`, `arch`, `url` (https), `sha256` (mandatory), `size`, `format` (`tar.gz`, `zip` or `file`), `strip_components`, `dest` (a relative path inside the engine install directory, default `.`, never absolute and never containing `..`) and `optional` (default `false`). Required dependencies for the host platform SHALL be installed with the engine; optional ones only on request (`--with <id>`, and `--with-runtime` selects every optional dependency whose id starts with `cuda-` for compatibility). Dependencies SHALL be downloaded with `alkahest-fetch` (resumable, digest-checked), recorded in the install marker and removed with the engine. The existing `companions` table SHALL keep parsing and SHALL be read as optional dependencies.

#### Scenario: required dependency installed with the engine
- **GIVEN** a variant with a required dependency `vulkan-loader` for `windows/x86_64`
- **WHEN** the variant is installed on Windows x64
- **THEN** the dependency is downloaded, its sha256 verified, unpacked under `dest`, and listed in the install marker

#### Scenario: optional dependency only on request
- **GIVEN** a variant with an optional dependency `cuda-runtime`
- **WHEN** it is installed without `--with-runtime`
- **THEN** the dependency is not downloaded; with `--with-runtime` it is

#### Scenario: dest escaping the install directory refused
- **GIVEN** a dependency with `dest = "../../bin"`
- **WHEN** the recipe is validated
- **THEN** it fails with an error naming the dependency id and the rule

### ALK-SP-0011.R6 — Native per-machine variants

A variant MAY set `native = true`. A native variant SHALL have a `source` table and no `assets`, SHALL never be chosen by automatic variant selection, and SHALL be installed only by an explicit `alkahest engines install <id> --native` (or `--variant <name>` naming it, which implies `--from-source`). Its build SHALL target the host: `cpu-native` the host CPU (for llama.cpp `-DGGML_NATIVE=ON` plus the accelerator flags of the variant), `cuda-native` the host GPU (`-DGGML_CUDA=ON -DCMAKE_CUDA_ARCHITECTURES=native`; a native `cuda` variant needs no `cuda = "x.y"`). `--native` SHALL pick the native variant of the best accelerator present (`cuda-native` on a CUDA host, `cpu-native` otherwise). `--cuda-arch <list>` SHALL replace or add `-DCMAKE_CUDA_ARCHITECTURES` in any CUDA source build and be refused for non-CUDA builds. The install SHALL record the CPU feature set it was built for (from `alkahest-sysinfo`), and `alkahest engines ls` SHALL mark a native install whose recorded features are not all present on the current host.

#### Scenario: native variant never auto-selected
- **GIVEN** a recipe with variants `cpu` (assets) and `cpu-native` (`native = true`)
- **WHEN** `alkahest engines install llama-cpp` runs on a CPU-only host
- **THEN** the `cpu` variant is installed and no build runs

#### Scenario: native variant with assets refused
- **GIVEN** a variant with `native = true` and an `assets` list
- **WHEN** the recipe is validated
- **THEN** it fails with "a native variant is source-only"

#### Scenario: native install marked stale on another CPU
- **GIVEN** a native install recorded with features `avx512f`
- **WHEN** `alkahest engines ls` runs on a host without `avx512f`
- **THEN** the install is listed with a "built for another CPU" mark

#### Scenario: native on a CUDA host builds for the GPU
- **GIVEN** a host with an NVIDIA GPU and the `llama-cpp` recipe
- **WHEN** `alkahest engines install llama-cpp --native` runs
- **THEN** the `cuda-native` variant is built with `-DCMAKE_CUDA_ARCHITECTURES=native`

### ALK-SP-0011.R7 — Model entries

`models/<namespace>/<model>.toml` SHALL hold one model entry: `name` (`<namespace>/<model>`, equal to the path), `description`, `license` (SPDX id or `other`), optional `homepage` and `tags` (list of words used by search), and one or more `[[variants]]`. Each variant SHALL have `tag` (unique within the entry; the first variant is the default tag), optional `format` (`gguf` default, or `safetensors`), optional `quant`, optional `engine` (an engine id; [[ALK-SP-0003]] R2 selection applies when absent), either `template` (a named built-in template such as `chatml`) or `template_text` with `template_format` (`jinja` or `go`), optional `params` (a table of Ollama-style parameters: `num_ctx`, `temperature`, `stop`, ...), and one or more `[[variants.files]]`. Each file SHALL have `kind` (`model`, `projector`, `adapter`, `vae`, `text-encoder`, `tokenizer`, `config`), either `url` or `urls` (a non-empty list of https mirrors tried in order; plain `http` only to a loopback host, the rule of R9 for sources, for local mirrors and tests), `sha256` (mandatory), `size` (mandatory, bytes, greater than zero), optional `filename` (the name inside the store, default the last URL segment) and optional `auth` (a host name whose token is read from the keyring, R14). Exactly one file per variant SHALL have `kind = "model"`.

#### Scenario: model entry parses
- **GIVEN** `models/acme/assistant.toml` with two variants `q4_k_m` and `q8_0`, each with one model file
- **WHEN** the catalog is loaded
- **THEN** `acme/assistant` is listed with default tag `q4_k_m`, its engine, template, params and total size per tag

#### Scenario: missing sha256 refused
- **GIVEN** a model file without `sha256`
- **WHEN** the catalog is validated
- **THEN** it fails naming the entry, the tag and the file

#### Scenario: url and urls exclusive
- **GIVEN** a model file with both `url` and `urls`
- **WHEN** the catalog is validated
- **THEN** it fails with "set url or urls, not both"

#### Scenario: mirrors tried in order
- **GIVEN** a file with `urls = [A, B]` where A answers 503
- **WHEN** the model is pulled
- **THEN** the file is downloaded from B and verified against the same sha256

### ALK-SP-0011.R8 — Catalog index

`index.toml` SHALL list every catalog file of R1 except itself (`catalog.toml`, `engines/*.toml`, `models/**/*.toml`; other files are not listed and never fetched), as `[[files]]` with `path` (relative to the catalog root, forward slashes), `sha256` and `size`, plus `schema_version` equal to the manifest's. The index SHALL be generated by a tool (`alkahest catalog index <dir>` and `cargo xtask catalog-check --fix`), never by hand, and `catalog-check` SHALL fail when the index does not match the files. A sync SHALL download the index first and then only the files whose sha256 differs from the current snapshot, verifying each against the index; a file whose digest or size does not match SHALL fail the sync (R3, all-or-nothing). A path in the index that is absolute, contains `..`, or falls outside R1's layout SHALL fail the sync.

#### Scenario: unchanged files are not downloaded
- **GIVEN** a snapshot and a new commit in which only `models/acme/assistant.toml` changed
- **WHEN** the catalog is synced
- **THEN** exactly `index.toml` and that file are downloaded, and the other files are copied from the previous snapshot

#### Scenario: stale index rejected by the check
- **GIVEN** a catalog whose `engines/llama-cpp.toml` was edited without regenerating `index.toml`
- **WHEN** `cargo xtask catalog-check` runs
- **THEN** it fails naming the file and the expected and actual sha256

#### Scenario: digest mismatch fails the sync
- **GIVEN** a host that serves a file whose sha256 differs from the index
- **WHEN** the catalog is synced
- **THEN** the sync fails, nothing of the new snapshot is kept, and the previous snapshot stays in use

### ALK-SP-0011.R9 — Catalog sources configuration

Catalog sources SHALL be configured as an ordered list `[[catalogs]]` in the Alkahest configuration, each with `name` (unique, R1 characters), `url` (https git repository URL, or `file://` path of a plain directory for air-gapped installs and tests), `ref` (branch or tag, default `main`), `subdir` (default `.`), optional `pin` (a commit id; when set the catalog is frozen at that commit and never refreshed), optional `enabled` (default `true`) and optional `refresh_interval` (default `24h`). Out of the box the list SHALL hold one source `official` = `https://github.com/digiogithub/alchemai-alkahest`, ref `main`, subdir `catalog`; the user MAY disable it (`enabled = false`) but a configuration without any `[[catalogs]]` table SHALL still get it. `alkahest catalog add <name> <url> [--ref] [--subdir] [--pin]` and `alkahest catalog rm <name>` SHALL edit the configuration preserving comments; removing a catalog SHALL delete its snapshot. Configuration SHALL never hold a token.

#### Scenario: default official catalog
- **GIVEN** a fresh install with no `[[catalogs]]` in the configuration
- **WHEN** `alkahest catalog ls` runs
- **THEN** it lists `official` with url `https://github.com/digiogithub/alchemai-alkahest`, ref `main`, subdir `catalog`

#### Scenario: add and remove a catalog
- **WHEN** `alkahest catalog add acme https://git.example.org/acme/catalog --ref stable` runs, then `alkahest catalog rm acme`
- **THEN** the source is written after `official`, then removed together with `<data_dir>/catalogs/acme/`

#### Scenario: pinned catalog never refreshes
- **GIVEN** a source with `pin = "2d45ae95a500e22fa79fcd1c3c6c4ecdc4ea5a29"`
- **WHEN** its refresh interval expires
- **THEN** no network request is made and the snapshot stays at that commit

### ALK-SP-0011.R10 — Snapshot cache, refresh and offline behaviour

Each sync SHALL produce a snapshot under `<data_dir>/catalogs/<name>/`: the catalog files plus `snapshot.toml` with `url`, `ref`, `subdir`, `commit` (the full commit id the content came from), `fetched_at`, `checked_at` and the last error, if any. A new snapshot SHALL be written to a temporary directory and swapped in atomically only after R2, R3 and R8 validation pass. Commands that read catalogs (`serve`, `engines`, `search`, `pull`) SHALL refresh a catalog whose `checked_at` is older than its `refresh_interval` (default 24 h) in the background and SHALL never block a request or a command on the network; `alkahest catalog sync [<name>]` refreshes in the foreground. When the ref still points at the snapshot commit, only `checked_at` SHALL change. When the network is down, the host fails or validation fails, the last good snapshot SHALL stay in use and the error SHALL be recorded and shown once by the next command and in `catalog ls`. With no snapshot at all and no network, the catalog layer SHALL be empty and the built-in recipes SHALL be used, saying so once.

#### Scenario: refresh skipped when the commit is unchanged
- **GIVEN** a snapshot at commit C and a remote ref still at C
- **WHEN** the refresh interval expires and a command runs
- **THEN** only the ref is resolved, no catalog file is downloaded, and `checked_at` is updated

#### Scenario: offline uses the last good snapshot
- **GIVEN** a snapshot at commit C and no network
- **WHEN** `alkahest engines ls` runs after the interval expired
- **THEN** the entries of C are listed, the command does not wait for the network, and `catalog ls` shows the error and the age of C

#### Scenario: fresh install offline
- **GIVEN** no snapshot and no network
- **WHEN** `alkahest engines ls` runs
- **THEN** the built-in recipes are listed and one notice says the official catalog could not be fetched

#### Scenario: interrupted sync leaves no partial snapshot
- **GIVEN** a sync that fails after downloading half of the changed files
- **WHEN** the next command runs
- **THEN** the previous snapshot is complete and in use, and no temporary directory is left in `<data_dir>/catalogs/`

### ALK-SP-0011.R11 — Fetch transport

Catalog sync SHALL NOT require a `git` executable or a git library. It SHALL resolve `ref` to a full commit id with the git smart HTTP protocol (protocol v2 `ls-refs` with a `ref-prefix`, falling back to the v0 `info/refs` advertisement), then fetch content addressed by that commit id only: `index.toml` and the changed files through the raw file URL of the host (GitHub, GitLab and Gitea/Forgejo built in; a `raw_url` template with `{commit}` and `{path}` for other hosts), or, when the catalog has no `index.toml` or the host has no raw endpoint, the commit archive (`tar.gz`) of the host extracting only `subdir` (an `archive_url` template for other hosts). When the archive carries the commit id (the pax `comment` header written by `git archive`), it SHALL equal the resolved commit. All requests SHALL go through the same reqwest + rustls client configuration as `alkahest-fetch` (system proxy variables, user agent, timeouts, retries). The decision and alternatives are recorded in [[development/decisions/0006-catalog-fetch-strategy]].

#### Scenario: ref resolved without git
- **GIVEN** a host that answers a v2 `ls-refs` request for `refs/heads/main` with commit C
- **WHEN** the catalog is synced on a machine without `git` on `PATH`
- **THEN** the snapshot records commit C and every content URL requested contains C, never the branch name

#### Scenario: v0 fallback
- **GIVEN** a host that ignores `Git-Protocol: version=2` and returns a v0 advertisement
- **WHEN** the ref is resolved
- **THEN** the commit of `refs/heads/<ref>` (or the peeled commit of `refs/tags/<ref>`) is used

#### Scenario: archive fallback without index
- **GIVEN** a catalog repository without `index.toml` on a GitHub host
- **WHEN** it is synced
- **THEN** the commit archive is downloaded, only files under `subdir` are extracted, the pax commit equals C, and the snapshot validates as usual

#### Scenario: proxy honoured
- **GIVEN** `HTTPS_PROXY` pointing at a local proxy
- **WHEN** the catalog is synced
- **THEN** every catalog request goes through the proxy

### ALK-SP-0011.R12 — Layer order

Engine recipes SHALL be layered, lowest first: built-in recipes compiled into the binary < git catalogs in configured order (the first `[[catalogs]]` lowest, so a user catalog added after `official` overrides it) < `recipes/` next to the binary < `<data_dir>/recipes/`. A higher layer SHALL win on the same `(id, version, variant)` key; different versions of the same engine coexist. Model entries SHALL be layered the same way among git catalogs on the same `(name, tag)` key. `alkahest engines info` and `alkahest catalog ls` SHALL show which layer (and catalog name and commit) each entry comes from. A disabled or refused catalog SHALL contribute no entries.

#### Scenario: git catalog overrides built-in
- **GIVEN** built-in `llama-cpp b11669 cpu` and the official catalog with `llama-cpp b11669 cpu` at a different URL
- **WHEN** the recipes are loaded
- **THEN** the catalog URL is used and `engines info` names `catalog official@<commit>` as the origin

#### Scenario: user directory overrides catalogs
- **GIVEN** the same key in the official catalog and in `<data_dir>/recipes/`
- **WHEN** the recipes are loaded
- **THEN** the user directory entry wins

#### Scenario: later catalog overrides earlier
- **GIVEN** catalogs `official` then `acme`, both defining model `acme/assistant:q4_k_m`
- **WHEN** the models are loaded
- **THEN** the `acme` entry is used

### ALK-SP-0011.R13 — alkahest/ catalog model names

Catalog models SHALL be named with the pseudo-host `alkahest` in the model reference grammar of [[ALK-SP-0001]] (R3): `catalogname = "alkahest/" namespace "/" model [ ":" tag ]`, parsed like `hf.co/<org>/<repo>` (the first of three segments is the host; the namespace MAY contain `.` like a catalog name, R1). There is no `catalog:` prefix: `alkahest:` stays the store scope of ALK-SP-0001.R3, so `alkahest:alkahest/acme/assistant` means "the catalog model, looked up in the Alkahest store only". A missing tag, or the tag `latest` when the entry has no variant `latest`, SHALL select the entry's first variant, and the concrete variant tag SHALL be the stored tag. `alkahest pull` and `alkahest search` SHALL accept `--catalog <name>` to use one configured catalog; without it the highest layer that has the entry decides (R12). `alkahest pull alkahest/<...>` SHALL download every file of the variant with `alkahest-fetch` (resumable, parallel, mirror fallback, sha256- and size-checked) into the Alkahest store, never the Ollama store, and write a manifest named `alkahest/<namespace>/<model>:<tag>` whose config records engine, template, params and the origin (catalog name and commit). After a pull, `alkahest/<namespace>/<model>:<tag>` SHALL resolve to that local model, and `alkahest/<namespace>/<model>` SHALL resolve to it when it is the only local tag of that model. The host `alkahest` SHALL never be contacted as an OCI registry. A digest mismatch SHALL fail the pull and leave no manifest and no blob of the model in the store (verified downloads may stay in the partial download area for a resume).

#### Scenario: reference parses and formats back
- **WHEN** `alkahest/acme/assistant:q8_0` and `alkahest:alkahest/acme/assistant:q8_0` are parsed and formatted
- **THEN** host `alkahest`, name `acme/assistant`, tag `q8_0` (and scope `alkahest` for the second) round-trip to the same strings

#### Scenario: default tag
- **GIVEN** an entry whose first variant is `q4_k_m`
- **WHEN** `alkahest pull alkahest/acme/assistant` runs
- **THEN** the `q4_k_m` variant is pulled and stored as `alkahest/acme/assistant:q4_k_m`

#### Scenario: catalog selected explicitly
- **GIVEN** catalogs `official` then `acme`, both with `acme/assistant`
- **WHEN** `alkahest pull alkahest/acme/assistant --catalog official` runs
- **THEN** the `official` entry is pulled and its origin records `official`

#### Scenario: wrong digest leaves nothing
- **GIVEN** a fixture server that serves a model file whose sha256 differs from the entry
- **WHEN** the model is pulled
- **THEN** the pull fails naming the file, and `alkahest ls` does not list the model and no blob of it remains in the store

#### Scenario: unknown catalog entry
- **WHEN** `alkahest pull alkahest/acme/missing` runs
- **THEN** it fails with "no catalog entry acme/missing" and lists the catalogs searched

### ALK-SP-0011.R14 — Catalog security

Catalog content SHALL be treated as untrusted data. A sync SHALL only download and parse TOML: it SHALL never run a program, a build step, a script or a hook from a catalog, and SHALL never download an asset, dependency or model file; build steps run only on an explicit `alkahest engines install --from-source` or `--native`, and model files download only on an explicit `pull`. Every asset, dependency and model file reached through a catalog SHALL be verified against its mandatory `sha256` before it is unpacked or registered. Paths taken from a catalog (index paths, `dest`, `filename`, archive entries) SHALL be relative, free of `..` and absolute components, and SHALL never be followed through symlinks; archive entries that are symlinks or hard links SHALL be skipped. A catalog snapshot SHALL be bounded (default 16 MiB in total, 1 MiB per file) and exceeding it SHALL fail the sync. Credentials for private catalogs and for `auth` hosts SHALL come from the OS keyring (service `alkahest`, user `catalog:<name>` or `host:<host>`) or an environment variable named in the documentation, SHALL be sent only to the host they belong to (not across redirects to another host), SHALL never be written to configuration, snapshots or logs, and SHALL appear redacted in error messages. Content SHALL be fetched only by commit id (R11), and a source MAY be pinned to a commit (R9).

#### Scenario: sync never executes
- **GIVEN** a catalog whose recipe has a `source` table and whose repository contains executable scripts
- **WHEN** the catalog is synced
- **THEN** no child process is spawned and no asset is downloaded

#### Scenario: token never logged
- **GIVEN** a private catalog whose token is in the keyring and logging at `trace` level
- **WHEN** the catalog is synced and the host answers 401
- **THEN** neither the log nor the error message contains the token

#### Scenario: token not forwarded across hosts
- **GIVEN** a private catalog host that redirects a raw file request to another host
- **WHEN** the file is fetched
- **THEN** the redirected request carries no Authorization header

#### Scenario: oversized catalog refused
- **GIVEN** an index listing a 50 MiB file
- **WHEN** the catalog is synced
- **THEN** the sync fails before downloading it, and the previous snapshot stays in use

#### Scenario: path traversal refused
- **GIVEN** an archive with an entry `catalog/../../.bashrc`
- **WHEN** it is extracted
- **THEN** the sync fails and nothing is written outside the temporary snapshot directory
