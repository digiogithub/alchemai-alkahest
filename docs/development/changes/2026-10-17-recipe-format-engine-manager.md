# Change: recipe format v2 and engine manager (ALK-US-0049)

Implements [[ALK-US-0049]] / [[ALK-SP-0003]] R3, R4, R5. Builds on [[ALK-US-0026]] (bundled CPU
llama.cpp). Plan: [[plan/alkahest-v1-plan]].

## What

- Recipe file format: one file per engine **version**, `id`, `version` (required, pinned),
  `executable`, optional `library_dirs`, and `[variants.<name>]` tables with
  `accelerator = cpu|cuda|rocm|vulkan|metal|sycl`, `cuda = "12.4"` (CUDA runtime the build
  needs), `notes`, `[[variants.<name>.assets]]` (os, arch, url, sha256 or `tofu = true`, size,
  format, strip_components) and/or `[variants.<name>.source]` (git, rev, cmake_flags, targets,
  tools, bin_dir). `parse_recipes` validates and flattens to one `Recipe` per variant. A recipe
  without `version` is rejected with "no pinned version".
- `recipes/llama-cpp-cpu.toml` became `recipes/llama-cpp.toml` (same pin b11669, `cpu` variant).
- `Catalog` (`catalog.rs`): built-in + `with_dir` layers (`recipes/` next to the binary, then
  `<data dir>/recipes`), later sources replace the same `(id, version, variant)`; unusable files
  are reported as `DiscoveryIssue`, not fatal. `resolve(id, version?, variant?, system?, host)`;
  explicit variant is the exact name or `cudaN` for the newest CUDA N build; `recommended`.
- `select_variant(variants, &SystemInfo, &Host)`: accelerators best first (Metal, CUDA, ROCm,
  Vulkan, CPU). CUDA variants qualify when their `cuda` version is not above the driver's maximum;
  the newest qualifying wins. SYCL is never picked automatically.
- Installer: `install` handles prebuilt (SHA-256 verified; `tofu` assets hash on first use and
  record `tofu: true` plus the digest in the marker) and `from_source` (`build.rs`: git init +
  shallow fetch of `rev`, cmake configure with `-G Ninja` when `ninja` is a listed tool, cmake
  build, products of `bin_dir` moved to the install dir, checkout removed, output in
  `<engines>/.logs/<id>-<version>-<variant>.log`, failure shows the log tail; kill on cancel).
  `BuildTools` overrides git/cmake for tests. `remove_installed` deletes only marked installs.
  `InstalledEngine` gains `library_dirs` and `from_source`.
- CLI: `engines ls` (installed, then every variant with state `recommended`/`available`/
  `unsupported on this host`/`(source only)`), `engines install <id> [--variant] [--version]
  [--from-source] [--force]`, `engines rm <id> [--version] [--variant]`, `engines info <id>`.
- `alkahest-engine` (minimal): `is_variant_label` also accepts `sycl`.
- `tag_number` now takes the first digit run (`prism-b10777-e3de715` is 10777).

## Decisions

- Spec R4 says `--build`; the story and CLI use `--from-source`. Spec text unchanged.
- No `engines use`: per-model pins already ride on `engine = "llama-cpp@b11669"`
  ([[ALK-US-0027]]) and the default is the newest version.
- Source builds copy no cmake RPATH logic: a recipe that needs one passes
  `-DCMAKE_BUILD_WITH_INSTALL_RPATH=ON` in `cmake_flags` or lists `library_dirs`.
- Recipe directories are the binary's `recipes/` and the user data dir only, never the working
  directory, because a recipe makes Alkahest download and run code.

## Verification

`scripts/ci-local.sh` passes. New tests: `crates/alkahest-recipes/tests/recipes.rs` (12 tests:
scenarios `recipe validation`, `cuda pick`, `two versions`, hardware order, discovery, source
build with fake git/cmake scripts, failing build, missing tools, remove) and `tests/tofu.rs`.
