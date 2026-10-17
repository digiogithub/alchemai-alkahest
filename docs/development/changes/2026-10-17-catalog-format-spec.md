# Change: git catalog format spec, fetch strategy ADR and fixture catalog

Date: 2026-10-17. Story [[ALK-US-0117]], epic [[ALK-EP-0015]], plan
[[development/plans/ALK-EP-0015-git-catalogs]].

## What

- New gintrack spec [[ALK-SP-0011]] "Git-backed catalogs" with requirements R1 to R14: layout,
  manifest and `schema_version`, lenient parsing with all-or-nothing validation, engine recipes
  in catalogs, dependency downloads, native variants, model entries, file index, source
  configuration (default `official` = this repository, `main`, `catalog/`), snapshot cache and
  24 h background refresh with last-good fallback, fetch transport, layer order, `catalog:`
  model reference, security. Every requirement has Given/When/Then scenarios for the later
  stories to turn into tests.
- ADR [[development/decisions/0006-catalog-fetch-strategy]]: HTTPS only. Ref resolved with
  git smart HTTP v2 `ls-refs` (v0 fallback), content fetched by commit id as raw files listed in
  a generated `index.toml`, commit archive as fallback. Rejected: `git` CLI (not present on
  Windows/macOS by default, separate proxy/credential handling) and `gix` (+4.6 MB stripped,
  +97 crates measured, no partial clone in the high-level API so ~9.3 MB per changed commit).
- Fixture catalog `crates/alkahest-recipes/tests/fixtures/catalog/` (`catalog.toml`,
  `index.toml`, `engines/llama-cpp.toml` with dependencies and a native variant,
  `models/acme/assistant.toml`, an ignored `README.md`) and the model blob
  `crates/alkahest-recipes/tests/fixtures/catalog-blobs/assistant-q4_k_m.gguf`.
- [[research/catalog-format-example]]: the format by example.
- Stories linked with `implements` (gintrack `item link`): [[ALK-US-0118]] R3 R8 R9 R10 R11 R12
  R14; [[ALK-US-0119]] R4 R5 R6; [[ALK-US-0120]] R7 R12 R13 R14; [[ALK-US-0121]] R1 R2 R3 R4 R8;
  [[ALK-US-0117]] relates to the spec.

## Why

Every later story of the epic depends on the format and the transport; fixing them first lets
0118, 0119 and 0120 run in parallel against one fixture.

## Verification

- All fixture TOML files parse (`tomllib`); `index.toml` digests computed with `sha256sum`.
- Size probe and wire measurements recorded in the ADR (probe crate in the session scratchpad,
  not committed).
- No Rust source changed; `cargo fmt --all --check` passes.
