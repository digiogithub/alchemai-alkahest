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

## Security review hardening

A review of the first commit (6306717) found ten problems. Each is fixed, with the regression
test named after it:

1. **Unbounded decompression** (high). The archive is now read in raw tar mode. GNU
   long-name/link and pax headers are parsed here and each is capped at 64 KiB. The gzip stream
   is wrapped in a reader that fails beyond four times `max_archive_bytes`. Tests:
   `archive.rs#huge_gnu_long_name_is_refused`, `#decompression_bomb_is_refused`,
   `#pax_long_paths_are_honoured`.
2. **Blocking work in async code.** Lock, snapshot reads, extraction, validation, staging and the
   keyring lookup run on `spawn_blocking`. The deadline is part of `SyncContext` and fires on a
   current-thread runtime. The token is looked up only after a 401/403, so a public catalog never
   touches the keyring. Tests: `sync.rs#sync_deadline_fires`,
   `#public_catalog_never_reads_the_keyring`, `#token_never_logged` (anonymous first),
   `#token_not_forwarded_across_hosts` (now through a host that requires the token).
3. **Host confusion with backslashes.** `host_of` and `split_url` use `reqwest::Url`
   (`host_str` + `port_or_known_default`), and tokens are bound to `host:port`. The
   configuration refuses `@`, `\`, whitespace and control characters in `url`, `raw_url` and
   `archive_url`. Tests: `hosts.rs#backslash_userinfo_does_not_leak_the_token`,
   `config.rs#catalog_sources_are_validated`.
4. **Non-atomic swap.** Snapshot content now lives in immutable `v-<commit>-<nanos>`
   directories, and `snapshot.toml` names the current one. One rename switches content and
   metadata together, and readers re-read the pointer if a version vanishes. Cleanup removes
   only versions that are not current and older than 10 minutes. Test:
   `snapshot.rs#cleanup_never_deletes_the_current_snapshot`,
   `#the_pointer_switches_versions_atomically`.
5. **Stealable lock.** The lock is now an advisory OS lock (`std::fs::File::try_lock`, no new
   dependency). It is never stolen, and the OS releases it when its holder dies. Every write of
   `snapshot.toml` happens under it. Test: `snapshot.rs#a_running_sync_lock_is_not_stolen`.
6. **A day without a catalog after one failure.** A failed check is retried after 15 minutes
   (`RETRY_AFTER_ERROR`, or the refresh interval if shorter). Timeouts are recorded by the sync
   itself, under the lock, and `record_timeout` is gone. Test:
   `sync.rs#failed_sync_is_retried_after_a_short_delay`.
7. **Optional commit check.** Archives from GitHub, GitLab and Gitea must carry the pax commit.
   A custom `archive_url` without it only produces a warning. Test:
   `archive.rs#archive_without_commit_is_refused_for_known_hosts`.
8. **`subdir` escapes.** Each component must be `[A-Za-z0-9._-]` and must not start with `.`.
   Test: `config.rs#catalog_sources_are_validated` (`%2e%2e`, `?`, `#`).
9. **Size overflow.** Per-file checks come first, then a `checked_add` sum. Test:
   `sync.rs#huge_index_sizes_do_not_overflow`.
10. **Background child.** It runs in its own process group on Unix and with
    `DETACHED_PROCESS | CREATE_NO_WINDOW` on Windows. It is not spawned while a sync holds the
    lock. There is no automated test; this was checked by hand.

The snapshot limits are now configurable in `[catalog_limits]` (`max_snapshot_bytes`,
`max_file_bytes`, `max_archive_bytes`) and documented in the generated configuration
reference. Test: `config.rs#catalog_limits_are_configurable`.

## Open points

- GitLab/Gitea raw and archive authentication uses `Authorization: Bearer`; verified only
  against the wiremock host, not a real private GitLab or Gitea.
