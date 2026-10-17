# Recipe dependencies and native per-machine builds (ALK-US-0119)

Story [[ALK-US-0119]], requirements R4 to R6 of [[ALK-SP-0011]] (git-backed catalogs).

## What changed

- `crates/alkahest-recipes/src/recipe.rs`
  - `Dependency` (`[[variants.<v>.dependencies]]`: id, os, arch, url, sha256, size, format
    `tar.gz|zip|file`, strip_components, dest, optional). Validation: unique id, mandatory
    lower-case hex sha256, https, `dest` relative without `..`; errors name the dependency id.
  - `Companion` keeps parsing and is read as an optional dependency
    (`Recipe::dependencies_for`, `Recipe::selected_dependencies`).
  - `native = true` variants: source-only ("a native variant is source-only"), excluded from
    `select_variant`; `select_native` picks the one matching the best accelerator.
  - `RecipeOrigin::{Local, GitCatalog}` and `parse_recipes_with` returning `ParsedRecipes`
    (recipes + warnings). `GitCatalog` refuses `tofu`/missing sha256 ("sha256 required in git
    catalogs (tofu is local only)") and warns on a branch-like `source.rev` (heuristic: not a
    hex commit id and no digit). This is the entry point ALK-US-0118 calls when syncing.
  - `ArchiveFormat::File` (dependencies only; refused for assets and companions).
- `install.rs`: required dependencies install with the engine, optional ones with
  `--with <id>` (`InstallOptions::with`) or `--with-runtime` (every `cuda-*`), also on an
  already installed engine; recorded in the marker (`dependencies`, alias `companions` for old
  markers); removed with the engine directory. Native builds record `native_cpu_flags` (from
  the new `alkahest_sysinfo::detect_cpu_flags`); `InstalledEngine::native_stale`.
- `archive.rs`: `file` format copies the download into the destination directory.
- CLI: `engines install --native` (conflicts with `--variant`) and `--with <id>`; `engines ls`
  marks a native build with "built for another CPU"; `engines info` lists dependencies and the
  native variant (JSON carries `dependencies` and `native` via the serialized recipe).
- Recipes: `cpu-native` (`-DGGML_NATIVE=ON`) in `recipes/llama-cpp.toml` and
  `recipes/prism-llama-cpp.toml`. Existing `companions` were left as they are (they keep
  parsing as optional dependencies, `--with-runtime` unchanged).

## Verification

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
`cargo nextest run --workspace --locked` (1196 passed), `cargo xtask check-deps`,
`cargo xtask notice --check`, `cargo build -p alkahest --no-default-features --locked`,
`cargo xtask docs-cli --check` (regenerated). `cargo deny check`: advisories fail on
RUSTSEC-2026-0370/0371 (zbus crates unmaintained), identical without this change; bans,
licenses and sources pass.

Tests: `crates/alkahest-recipes/tests/dependencies.rs` (scenario names follow the spec).
The real llama.cpp native build is opt-in: `ALKAHEST_TEST_BUILD=1` (git, cmake, ninja,
network); without it the test returns early.

## Notes

ALK-SP-0003 needed no change: ALK-SP-0011 R5/R6 cover the new behaviour and do not
contradict it.
