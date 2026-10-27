# Cargo source builds in recipes and the ptts-openai-server recipe (ALK-US-0139)

Date: 2026-10-27. Story [[ALK-US-0139]]. Decision [[0008-phonon-tts-engine]] (engine, licence
handling), licence policy [[0004-licence-policy-and-notices]]. Plan: [[plan/alkahest-v1-plan]].

## What changed

- `crates/alkahest-recipes/src/recipe.rs`: new `CargoBuild` (`git`, `rev`, `package`,
  `features`, `bin`, `tools`; `bin_name()`), a `[variants.<v>.cargo]` table on `VariantDef`,
  carried into `Recipe::cargo`. Validation: one of assets/source/venv/cargo; `cargo` excludes
  `source` and `venv` and native; same https/ssh and ref rules as `source` (factored into
  `check_git_ref`), names checked, branch-like `rev` warns in git catalogs.
  `Recipe::has_build_steps()` (source or cargo) now drives `needs_source_build` and
  `installable_on`.
- `crates/alkahest-recipes/src/build.rs`: `BuildTools::cargo`; `git_checkout` (the shallow
  fetch/checkout helper, now shared with the CMake path); pure `cargo_build_args`
  (`build --release --locked -p <pkg> [--features a,b]`); `build_cargo` runs it with
  `CARGO_TARGET_DIR=<checkout>/target` (new `Step::env`), copies `target/release/<bin>[.exe]`
  to the work dir root and deletes the checkout. Missing tools give `BuildToolMissing`;
  cancellation goes through the existing `run()`.
- `crates/alkahest-recipes/src/install.rs`: `install` dispatches to `build_cargo` for cargo
  recipes; `ENGINE_LICENSES` gains `ptts-openai-server`. `catalog.rs`: recipe in `BUILTIN`.
- `crates/alkahest-api/src/admin/engines.rs`: `source_build`, `unsupported` state and the
  `from_source` check use `has_build_steps()` so cargo recipes are offered and installable.
- `recipes/ptts-openai-server.toml` and its source of truth `catalog/engines/` copy
  (`cargo xtask catalog-index --sync-recipes`, `catalog/index.toml` regenerated). Variants
  `cpu`, `metal`, `cuda12.4` (keeps the `cudaX.Y` naming and the mandatory `cuda` version of
  the schema, like whisper-cpp), `vulkan`. Pinned to commit
  `e851445158f07061f063e3edff0080f9426c0da2` (tag v0.5.0).
- `crates/alkahest-catalog/src/schema.rs`: `CARGO` shape so the catalog accepts the table.
- `packaging/licenses/ptts-openai-server.txt` (verbatim LICENSE-MIT + LICENSE-APACHE),
  `THIRD_PARTY_LICENSES` regenerated. `xtask/src/notice.rs` `check_engines` accepts
  `MIT OR Apache-2.0` / `Apache-2.0 OR MIT`; `xtask/src/engines.rs` test counts 9 pins.
- Tests: `crates/alkahest-recipes/tests/ptts.rs` (recipe parse, all variants, validation, a
  local-repository install through the real cargo path, and a real xn-ptts build behind
  `ALKAHEST_TEST_NET=1`), unit tests in `build.rs` (argv, bin name, missing cargo), notice
  licence acceptance test, `tests/recipes.rs` licence check accepts the dual licence.

## Why

ADR 0008: the Phonon TTS server is a Rust program that upstream does not ship as a binary, so
recipes need a Cargo build kind. LAME stays user-installed and dynamically linked.

## Verification

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`
  clean.
- `cargo nextest run --workspace --locked`: 1462 run, 1462 passed, 1 skipped.
- `cargo xtask check-deps`: OK; `cargo xtask notice --check`: OK (9 engines).
- `cargo build -p alkahest --no-default-features --locked`: OK.
- Real install with a scratch `ALKAHEST_HOME`: `alkahest engines install ptts-openai-server
  --variant cpu` built in about 1m25s, installed `.../v0.5.0-cpu/ptts-openai-server`, and
  `ptts-openai-server --help` prints "OpenAI-compatible speech server for Phonon" (links
  libopus and libmp3lame dynamically).
