# Bundled CPU llama.cpp: recipe, installer, `engines ls|install`

Refs: [[ALK-US-0026]], [[ALK-SP-0003]] R6, [[ALK-US-0049]] (full recipe system, later),
[[plan/alkahest-v1-plan]]

## What

- `recipes/llama-cpp-cpu.toml`: mainline llama.cpp pinned to release `b11669`, six platform
  assets (linux x86_64/aarch64, macOS arm64/x64, Windows x64/arm64) with SHA-256 and size.
  Digests come from the GitHub release API; linux-x64 and windows-x64 re-verified by manual
  download.
- `alkahest-recipes`: `Recipe`/`Asset`/`Host` data model (`RecipeKind` and `ArchiveFormat` are
  `non_exhaustive` so ALK-US-0049 can add source builds), `builtin_recipes`, `find_recipe`;
  `install` (download with `alkahest-fetch`, digest verified, unpack into a `.partial`
  sibling, marker file, atomic rename, idempotent); `extract` (tar.gz via `flate2`+`tar`, zip,
  strip components, refuses `..`, absolute names, escaping symlinks, hard links);
  `list_installed`, `locate` (newest tag, data dir wins ties), `bundled_roots`.
- Install layout: `<engines_dir>/llama-cpp/b11669-cpu/` with `.alkahest-engine.json`.
- CLI: `alkahest engines ls [--json]` and `alkahest engines install <id> [--variant] [--force]`
  replace the stubs (`crates/alkahest/src/engines_cmd.rs`); the bandwidth cap from `[pull]`
  applies.
- New workspace pins: `flate2 =1.1.9`, `tar =0.4.45` (no default features), `zip =9.0.3`
  (`deflate` only). `zip` 2.4.2 is yanked and 8.6.0 trips RUSTSEC-2026-0369, hence 9.0.3.

## Running the engine

Verified on Linux x86-64: `llama-server` carries `RUNPATH=$ORIGIN`, so it needs no
`LD_LIBRARY_PATH`; the `libggml-cpu-*` backends are loaded from its own directory. The
supervisor ([[ALK-US-0024]]) should still use the install directory as working directory.
`llama-server --version` printed `version: 0.7.0-dev (build 11669, commit 620d2225c)`.

## Not done / next

- `xtask bundle` should copy `<engines_dir>/llama-cpp/b11669-cpu/` into the packages under
  `engines/` next to the binary (`bundled_roots` already looks there); packaging stories
  own that.
- A GPU-less fresh install still runs `engines install llama-cpp` once unless the package
  bundles it; the engine adapter should auto-install on first use as a fallback.
- macOS dylib search (`@loader_path`) and Windows were not run, only unpacked in tests.

## Verification

`scripts/ci-local.sh` passes. `ALKAHEST_TEST_NET=1 cargo test -p alkahest-recipes --test
install real_cpu_llama_server_runs` downloads the real asset, installs it and runs
`llama-server --version`. Manual: `alkahest engines install llama-cpp` then `engines ls`.
