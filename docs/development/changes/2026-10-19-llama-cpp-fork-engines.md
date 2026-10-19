# llama.cpp fork engines, CUDA native builds and per-model extra_args

Date: 2026-10-19. Related: [[ALK-US-0132]], [[ALK-SP-0003]], [[ALK-SP-0011]]. Decision:
[[development/decisions/0007-fork-engines-and-cuda-native]].

## What

**Fork engines.** A recipe may declare `base = "<family>"`. Its `id` is then a fork engine
that runs through the family's adapter, installs under its own id, has its own
`engines.overrides.<id>`, is never found on `PATH` and is never selected automatically; a model
opts in with `engine=<fork>` or `engine=<fork>@<version>`.

- `crates/alkahest-recipes/src/family.rs` (new): `BUILTIN_ENGINE_IDS`, `builtin_engine_id`
  (the alias table, shared with `EngineId::from_str`), `normalize_engine_name`,
  `check_fork_id`.
- `crates/alkahest-recipes/src/recipe.rs`: `RecipeFile::base`, `Recipe::base`, `check_base`
  (built-in ids must not have `base`; other ids need a canonical built-in `base` and may not
  shadow a built-in id or alias); a native `cuda` variant may omit `cuda = "x.y"`;
  `SourceBuild::{builds_cuda, with_cuda_architectures}`.
- `crates/alkahest-recipes/src/catalog.rs`: `Catalog::{fork_base, forks}`.
- `crates/alkahest-recipes/src/install.rs`: the install marker and `InstalledEngine` record
  `base`, so an installed fork runs without its recipe.
- `crates/alkahest-catalog/src/schema.rs`: `base` is a known recipe key (no "unknown key"
  warning in git catalogs).
- `crates/alkahest-engine/src/id.rs`: `EngineKey { family, fork }` (`builtin`, `fork`,
  `family`, `fork_id`, `as_str`, `path_lookup_allowed`, `install_command`); `EngineId::from_str`
  delegates to `alkahest_recipes::builtin_engine_id`.
- `crates/alkahest-engine/src/select.rs`: `EngineChoice::fork`, `EngineChoice::key`,
  `EngineResolver`, `builtin_resolver`, `parse_engine_setting_with`, `select_engine_with`;
  `select_engine` keeps its signature (built-in names only). The automatic rule is unchanged.
- `crates/alkahest-engine/src/locator.rs`: `LocateQuery::fork` / `with_fork`; `FsLocator`
  looks forks up in `<root>/<fork>/`, under `engines.overrides.<fork>`, never on `PATH`;
  `EngineLocator::installed_fork_family` (default `None`; `FsLocator` reads install markers).
  `split_install_dir` reads `-cpu-native` / `-cuda-native` as the variant; `variant_score`
  ranks a native build one above the portable build of its backend; `runs_on` treats
  `cpu-native` like `cpu`.
- `crates/alkahest-engine/src/registry.rs`: `EngineRegistry::{with_recipes,
  from_config_with_recipes, resolve_engine, locate_key}`; `select` resolves names through
  `resolve_engine` (built-in, recipe fork, installed fork; otherwise `UnknownEngine` listing
  the forks); `check_model_extra_args` runs in `plan` and `prepare`.
- `crates/alkahest-engine/src/install.rs`: `RecipeSource` (`BuiltinRecipes`, `Catalog`,
  `DynamicRecipes`); `EngineInstaller::install` takes an `EngineKey`; `RecipeInstaller`
  resolves through the layered catalog (`with_recipes`), so `engines.auto_install` installs
  forks.
- `crates/alkahest-engine/src/error.rs`: `NotInstalled.engine` and
  `AutoInstallFailed.engine` are `EngineKey` (the message names the fork and its install
  command); `OverridePathMissing.engine` is the configured name; `UnknownEngine` explains how
  to define a fork.
- `crates/alkahest/src/serve_cmd.rs`: `recipe_source` gives the registry the live layered
  recipe catalog (built-in, git catalogs, `recipes/` next to the binary, `<data dir>/recipes`).
- Admin API (`crates/alkahest-api/src/admin/engines.rs`): `EngineVariant::base`;
  `EngineInstallRequest::cuda_arch`; `GET`/`PUT /alkahest/v1/engines/{id}/override` accept a
  fork id known to the engine manager (`known_engine_id`). Client
  (`crates/alkahest-client/src/admin.rs`): `EngineVariant::base`,
  `EngineInstallOptions::cuda_arch`. Desktop (`crates/alkahest-desktop/src/engines.rs`): a
  fork's card says "fork of <family> · opt-in per model: engine=<id>".
- CLI (`crates/alkahest/src/engines_cmd.rs`, `cli.rs`): `engines info` names the family of a
  fork and how to opt in; `engines install --cuda-arch <list>` and `--dry-run`.

**CUDA native build.** `catalog/engines/llama-cpp.toml` (synced to `recipes/` with
`cargo xtask catalog-index --sync-recipes`) has a source-only `cuda-native` variant: the flags
of the other llama.cpp source builds plus `-DGGML_CUDA=ON -DCMAKE_CUDA_ARCHITECTURES=native`,
`native = true`. `alkahest engines install llama-cpp --native` picks it on a CUDA host
(`Catalog::resolve_native` already tried accelerators best first) and `cpu-native` elsewhere;
`--variant cuda-native --from-source` works too. `--cuda-arch 86;89` replaces `native`
(`SourceBuild::with_cuda_architectures`; refused for non-CUDA builds). PrismML was left
unchanged.

**Per-model `extra_args`.** Already appended after the engine-level `extra_args` by every
adapter. Now: `alkahest config model <m> set "extra_args=--a 'b c'"` splits with shell-style
quotes (`alkahest_store::split_args`, as the desktop field does); `check_extra_args` refuses
`--host`, `--port`, `-m`, `--model`, `--api-key`, `--api-key-file` (also `=` forms) in
`ModelSettings::set_key`, in the admin API model settings validation and at load
(`check_model_extra_args`). `alkahest show` prints the model's engine setting and its engine
arguments (`engine_setting`, `extra_args` in `--json`).

**Docs.** `docs/book/src/guide/engines.md` (fork engines with a recipe skeleton, native
builds, `--cuda-arch`, benchmarking procedure, per-model `extra_args`), `guide/catalogs.md`
(`base`), `guide/models.md` (`extra_args`, forks), regenerated `reference/cli.md` and
`reference/configuration.md`.

## Why

Performance forks must be tried per model without touching what other models run on (story
[[ALK-US-0132]]); keeping `EngineId` as the adapter family and adding a key for forks kept the
churn small: existing configuration strings, API ids and `EngineId` matches are unchanged.

## Verification

- New tests: `crates/alkahest-recipes/tests/forks.rs` (base parse and validation in local and
  git origins, built-in id and alias reuse, install dir layout, `cuda-native` flags and never
  auto-selected, `--native` → `cuda-native` on CUDA / `cpu-native` otherwise, `--cuda-arch`
  replacement and refusal, marker `base`), `crates/alkahest-engine/tests/forks.rs` (automatic
  selection never picks a fork, `engine=` and `engine=<fork>@<version>` pick it, not
  installed → install command, never from `PATH`, unknown engine error, installed fork without
  recipe, fork overrides and argument order, managed flags refused), store unit tests
  (quoting, managed flags), `alkahest_recipes::family` and `alkahest_engine::id` unit tests,
  locator unit tests (native dir names and scores), CLI tests (`engines ls/info/install
  --dry-run` with a fork recipe, `--cuda-arch` dry run, `config model set extra_args`, `show`),
  admin API (model settings refuse managed flags), xtask `a_fork_engine_needs_a_known_base`,
  desktop fork card.
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
  the desktop clippy run, `cargo nextest run --workspace --locked`, `cargo deny check`,
  `cargo xtask check-deps`, `cargo xtask catalog-check catalog`,
  `cargo build -p alkahest --no-default-features --locked`: all pass.
- `alkahest engines install llama-cpp --variant cuda-native --cuda-arch "86;89" --dry-run`
  prints `-DGGML_CUDA=ON -DCMAKE_CUDA_ARCHITECTURES=86;89`; no real source build was run.

## Not done

- Fork example recipes (criterion 6) are added separately; a fork meant to be compiled into
  the binary must also be added to the `BUILTIN` list in `crates/alkahest-recipes/src/catalog.rs`.
- `alkahest ps` reports the family id for a model running on a fork; the desktop model settings
  "Engine" choice still lists only the built-in engines (a fork is set with the CLI or the
  admin API).
