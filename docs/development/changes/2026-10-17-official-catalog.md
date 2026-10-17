# Official catalog and its CI validation (ALK-US-0121)

Implements [[ALK-US-0121]] for [[ALK-SP-0011]] R1, R2, R3 (strict check), R4 (check side) and R8,
following [[0006-catalog-fetch-strategy]] and [[research/catalog-format-example]].

## What changed

- `catalog/` seeded: `catalog.toml` (schema 1.0, name `official`), `engines/` (the five recipes
  copied from `recipes/`), `models/qwen/qwen2.5-0.5b-instruct.toml` (Apache-2.0, variants `q4_k_m`
  491400032 bytes and `q8_0` 675710816 bytes; URLs pinned to the HF commit
  `9217f5db79a2...`, sha256 and size from the HF tree API) and a generated `index.toml`.
- `xtask/src/catalog.rs`: `catalog-check [<dir>] [--net] [--fix]` and
  `catalog-index [<dir>] [--sync-recipes]`. Engines are validated with
  `alkahest_recipes::parse_recipes_with(.., GitCatalog)` (sha256 mandatory, https, no tofu); the
  manifest, model entries and index use local `deny_unknown_fields` serde structs (strict mode).
  Checks: name characters, file stem equals id, path equals model name, unique keys, one `model`
  file per variant, url xor urls, https, sha256 and size mandatory, index complete and current.
  `--net` (or `ALKAHEST_TEST_NET=1`) sends HEAD requests (curl) for the declared size of every
  engine asset, dependency and model file, and downloads and hashes files up to 8 MiB.
- Drift guard: `catalog/engines/` is the source of truth. `catalog-check` on the repository
  catalog fails if `recipes/*.toml` differs byte for byte; `catalog-index --sync-recipes` (also
  run by `catalog-check --fix`) copies the engines over `recipes/`. Chosen over generating
  `recipes/` at build time because `include_str!` consumers keep reading plain committed files.
- `engines-check` reads `catalog/engines/` and falls back to `recipes/`.
- CI: `lint` job runs `cargo xtask catalog-check` (offline) on every push and pull request.
- User guide page `docs/book/src/guide/catalogs.md` (authoring workflow).
- `xtask` now depends on `alkahest-recipes`, `serde`, `sha2`, `hex`, `semver` (all already
  pinned in the workspace).

## Verification

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
`cargo nextest run --workspace --locked` (1212 passed), `cargo xtask check-deps`,
`cargo xtask catalog-check` and `--net`, `cargo xtask docs-cli --check`,
`cargo xtask notice --check`, headless build: all pass. `cargo deny check`: advisories fail on
RUSTSEC-2026-0370/0371 as on main (not addressed here).
