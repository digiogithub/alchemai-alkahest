# Catalog sync: git catalogs, snapshots and the catalog CLI

Date: 2026-10-17. Story: [[ALK-US-0118]]. Spec: [[ALK-SP-0011]] (R3, R8, R9, R10, R11, R12,
R14). Decision: [[0006-catalog-fetch-strategy]] (option C, implemented as written). Builds on
[[ALK-US-0117]] (format), [[ALK-US-0119]] (`parse_recipes_with`, dependencies, native variants)
and [[ALK-US-0121]] (official `catalog/`, `cargo xtask catalog-check`).

## What changed

- **New crate `alkahest-catalog`** (no new third-party dependencies; reqwest/rustls, flate2,
  tar, sha2, keyring, toml_edit were already in the workspace):
  - `refs` resolves a branch or tag to a commit over git smart HTTP: v2 `ls-refs` with
    `ref-prefix` (and `peel`), falling back to the v0 `info/refs` advertisement. No git binary,
    no git library.
  - `hosts` builds raw-file and commit-archive URLs for GitHub, GitLab and Gitea/Forgejo, or
    from `raw_url` / `archive_url` templates; `redact_url` strips credentials and queries.
  - `http::HttpClient` is configured like `alkahest-fetch` (user agent, connect timeout, system
    proxy variables), bounds every body, retries 5xx/429/connection errors. `Token` is redacted
    and zeroized; `KeyringCredentials` reads `ALKAHEST_CATALOG_TOKEN_<NAME>` then the keyring
    (`alkahest` / `catalog:<name>`); a token is sent only to the repository's hosts and reqwest
    drops it on cross-host redirects.
  - `sync()` runs one sync: pin or resolved commit; unchanged commit only touches `checked_at`;
    otherwise `index.toml` at the commit, then only files whose sha256 differs from the
    snapshot, each verified (digest and size) against the index; without an index (404) the
    commit archive, extracting only catalog files under `subdir`, refusing `..`/absolute
    entries, skipping links, checking the pax `comment` commit. `file://` sources read a plain
    directory. Limits: 16 MiB per snapshot, 1 MiB per file, 256 MiB per archive (`Limits`).
  - `content::validate` is all-or-nothing over R1-R8: manifest (`schema_version` major,
    `min_alkahest`), index (paths, digests, completeness), engine files through
    `parse_recipes_with(.., RecipeOrigin::GitCatalog)` (stem = id, tofu refused), model entries
    (R7 rules). Lenient mode strips unknown keys with warnings (`schema` key trees); strict mode
    rejects them.
  - `snapshot` writes `<data_dir>/catalogs/<name>/` through a temporary directory and an atomic
    swap, with a per-catalog lock; a failed sync records `last_error` in `snapshot.toml`
    (shown once) and keeps the previous content.
  - `layers::recipe_catalog` layers built-in < git catalogs (configured order) < recipe
    directories; `layered_models` / `find_model` expose model entries on `(name, tag)` for
    [[ALK-US-0120]]. `edit` adds/removes `[[catalogs]]` with `toml_edit`, keeping comments.
- **`alkahest-core`**: `Config.catalogs: Vec<CatalogSource>` (`[[catalogs]]`, default the
  official catalog), validation (names, https/`file://`/loopback-http URLs, no credentials in
  URLs, refs, subdir, full-commit pins, templates, `refresh_interval`), and a refusal of
  `token`/`password` keys in catalog entries.
- **`alkahest-recipes`**: `RecipeLayer` and `Catalog::extend_layer` / `layer_of`, so every recipe
  knows its origin.
- **CLI**: `alkahest catalog ls|sync [name]|add|rm|index`; `engines ls` gains an `origin`
  column and `engines info` a `from <layer>` line (and `origin` in JSON). Reading commands call
  `catalog_cmd::prepare`: a never-fetched catalog is fetched once in the foreground (10 s
  deadline, then the built-in recipes and one notice); a stale one is refreshed by a detached
  `alkahest catalog sync --background`. `serve` refreshes due catalogs in a background task
  every 10 minutes and the admin engine manager reads the snapshots. `ALKAHEST_CATALOG_AUTO_SYNC=0`
  turns automatic refresh off.
- **xtask docs**: `r#ref` fields and `Vec<Struct>` sections (`catalogs[].name`) are rendered;
  `docs/book/src/reference/{cli,configuration}.md` regenerated; user guide section "Catalogs:
  recipes without a new release" in `guide/engines.md`.

## Verification

- `cargo fmt --all --check`: clean. `cargo clippy --workspace --all-targets --locked -- -D
  warnings`: clean.
- `cargo nextest run --workspace --locked`: 1281 passed, 1 skipped.
- `ALKAHEST_TEST_NET=1 cargo test -p alkahest --test catalog official_catalog_syncs_from_github`:
  passed against github.com (commit `c1c1877bc8fb`, 29 recipe variants, 1 model).
- `cargo deny check`: bans, licences, sources ok; advisories fail on the pre-existing
  RUSTSEC-2026-0370/0371 (zbus_names, zvariant unmaintained, through gpui), unrelated.
- `cargo xtask check-deps`, `cargo build -p alkahest --no-default-features --locked`,
  `cargo xtask docs-cli --check`, `cargo xtask catalog-check`: ok.

Scenario tests: `crates/alkahest-catalog/tests/sync.rs`, `tests/layers.rs`,
`crates/alkahest/tests/catalog.rs`, plus unit tests in `alkahest-core::config` and the catalog
crate modules.

## Open points

- Size limits are configurable in code (`Limits`) but not yet from the configuration file.
- GitLab/Gitea raw and archive authentication uses `Authorization: Bearer`; verified only
  against the wiremock host, not a real private GitLab or Gitea.
- The background refresh of CLI commands is a detached child process; on Windows it is not
  started with `DETACHED_PROCESS`, so it may briefly share the console.
