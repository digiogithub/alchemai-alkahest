# Catalog admin API, catalog model detail and catalog-aware engines

Date: 2026-10-17. Story: [[ALK-US-0124]] (backend gaps of the desktop catalog design
[[ALK-US-0122]], `design/README.md` "Backend gaps for the catalog screens"). Spec:
[[ALK-SP-0011]] (R5, R6, R9, R10, R12, R14). Builds on [[2026-10-17-catalog-sync]],
[[2026-10-17-catalog-models]] and [[2026-10-17-recipe-dependencies-native]].

## What changed

- **`alkahest-catalog`**
  - New `manage` module, shared by the CLI and the admin API: `CatalogInfo` / `describe` (the
    `catalog ls --json` row: position, source fields, snapshot state, counts, warnings,
    status, `token_present`, `token_source`, `syncing`), `status_text`, `add_catalog`,
    `remove_catalog` (deletes the snapshot under the catalog lock, optionally the keyring
    token; `official` is disabled and keeps its snapshot), `edit_catalog`, the live
    `CatalogSet` (sources + limits a server reads), the `CatalogTokenStore` trait with
    `KeyringTokenStore` (`alkahest` / `catalog:<name>`) and `MemoryTokenStore`,
    `StoreCredentials` (environment, then the store), `token_source`, `AUTO_SYNC_ENV` and
    `auto_sync_enabled`.
  - `edit`: `SourcePatch` (`enabled`, `ref`, `subdir`, `pin` with `null` = unpin,
    `refresh_interval`; unknown keys refused) and `update_source` (key-by-key `toml_edit`
    edit, comments kept, result validated); `remove_source` disables `official` instead of
    removing it.
  - `sync`: `SyncStep` (`resolving ref`, `reading directory`, `downloading changed files n /
    m`, `extracting archive`, `validating`) reported through `SyncContext::with_progress`;
    `SyncContext::with_credentials`. `CatalogError::Keyring`.
- **`alkahest-recipes`**: `Catalog::resolve_native` (the CLI `--native` logic, now shared).
- **`alkahest-api`**
  - `admin::catalogs`: `GET/POST /alkahest/v1/catalogs`, `PATCH/DELETE
    /alkahest/v1/catalogs/{name}` (`?delete_token=true`), `POST /alkahest/v1/catalogs/sync`,
    `POST /alkahest/v1/catalogs/{name}/sync` (job kind `catalog_sync`, step as phase, files as
    `completed`/`total`), `PUT/DELETE /alkahest/v1/catalogs/{name}/token`,
    `GET /alkahest/v1/catalog/models/{ns}/{model}?catalog=`. Edits are serialised, written to
    the configuration file and applied at once to the shared `CatalogSet`. The add body has
    no `token` field (refused); the token PUT parses its body without echoing serde details.
    The name `sync` is reserved by the API (route clash).
  - Background refresh moved from `serve_cmd` into `admin::catalogs::spawn_refresh` /
    `refresh_pass`; it publishes a `catalog` event (`sync_failed`) on `/alkahest/v1/events`
    when a catalog that had no recorded error fails.
  - Engines: `EngineVariant` gains `origin` (`built-in`, `catalog:<name>`, `dir:<path>`),
    `origin_commit`, `native`, `native_stale`, `dependencies` (`id`, `optional`, `size`,
    `installed`); `EngineInstallRequest` gains `with` and `native` (validated before the job:
    unknown optional dependency, `native` with `variant`); `RecipeEngineManager::with_catalog_set`.
  - Server settings: `catalog_limits` / `catalog_limits_default` in the GET document; `PUT`
    accepts `catalog_limits` (bytes, `null` resets), checked before anything is written,
    applied live (no `restart_required`).
  - `PullService::with_catalog_set`, catalog search and model detail read the live set; the
    `PATCH` method is added to the route table and the CORS methods.
- **`alkahest-client`**: `catalogs`, `add_catalog`, `update_catalog`, `remove_catalog`,
  `sync_catalogs`, `sync_catalog`, `set_catalog_token`, `delete_catalog_token`,
  `set_catalog_limits`, `catalog_search`, `catalog_model`, `install_engine_with`
  (`EngineInstallOptions`), `ServerEvent::catalog`; `EngineVariant`/`EngineDependency` and
  `ServerSettingsDoc.catalog_limits` fields. `EngineInstall` itself is unchanged, so existing
  struct literals in the desktop keep compiling.
- **CLI**: `alkahest catalog enable|disable|edit` (`--ref`, `--subdir`, `--pin`/`--unpin`,
  `--refresh-interval`), `catalog token set|clear <name>` (hidden prompt or one stdin line,
  never an argument), `catalog rm --delete-token`; `catalog rm official` disables it; `catalog
  ls --json` is the shared `CatalogInfo` document (superset of the previous keys).
- **Docs**: `reference/api.md` section "Git catalogs" (routes, shapes, job steps, event,
  limits, install options), `guide/engines.md`, regenerated `reference/cli.md`.

## Security

- Tokens enter only through `PUT .../token` (admin scope; remote callers need an admin key,
  ALK-SP-0006) or `catalog token set` (stdin/prompt), are stored in the keyring, and are never
  returned, logged or echoed in errors. Test `catalog_token_logs.rs` captures every log line
  at trace level and every response of a token set + failing private sync and finds no token,
  while the host did receive an `Authorization` header after the anonymous 401.
- Configuration writes keep refusing token keys (existing `Config` validation).

## Verification

- `cargo fmt --all --check`: clean. `cargo clippy --workspace --all-targets --locked -- -D
  warnings`: clean.
- `cargo nextest run --workspace --locked`: 1326 passed, 1 skipped.
- `cargo xtask check-deps`, `cargo xtask catalog-check`, `cargo xtask docs-cli --check`,
  `cargo build -p alkahest --no-default-features --locked`: ok.
- `cargo deny check`: bans, licences, sources ok; advisories fail on the pre-existing
  RUSTSEC-2026-0370/0371/0372 (zbus/zvariant crates through gpui), unrelated.

Tests: `crates/alkahest-api/tests/catalogs.rs` (admin scope on every route, list/add/patch/
remove with validation and comment preservation, sync job steps and failure, token stored
and never returned, environment token source, model detail, engines fields and install
`with`/`native`, catalog limits round trip, first background failure announced once and
streamed as SSE, typed client end to end), `tests/catalog_token_logs.rs`,
`crates/alkahest-catalog/src/manage.rs` unit tests, `tests/sync.rs#sync_reports_its_steps`,
`crates/alkahest/tests/catalog.rs` (`enable_disable_edit_and_official_is_only_disabled`,
`catalog_token_is_never_an_argument`).

## Open points

- A `file://` catalog can be added through the API by any admin-scoped caller (it only reads
  catalog TOML files from that directory); restrict to loopback callers if that is a concern.
- After `PATCH` of `ref`/`subdir`/`pin` the client should start a sync; the server does not
  sync on its own.
- "Discover All keeps catalog rows first" is a desktop ordering rule; `GET catalog/search`
  returns catalog rows only, so the desktop phase of ALK-US-0122 implements the ordering.
- CLI edits made while `serve` runs are not seen by the running server's live set until a
  restart (API edits are).

## Security review hardening

An independent review of bb5db5f found six problems. Each is fixed, and each fix has a
regression test named after the problem:

1. **Token re-targeting** (medium-high). A keyring token was keyed only by the catalog name.
   Removing a catalog and adding the same name with another URL sent the token to that new
   host after a 401.
   - Tokens are now stored as `{"v":1,"host":"<host:port>","token":...}`. A bare legacy
     value still reads, as an unbound token.
   - `manage::TokenPolicy` returns a bound token only for its `host:port`.
   - An environment token, or an unbound legacy one, goes only to the source of that name
     and host that the server's startup configuration names. In the CLI, that is the
     configuration file.
   - `POST catalogs` answers 409 when a token for the name belongs to another host, unless
     the request sets `replace_token: true`, which deletes that token.
   - The listing reports `token_source: "mismatch"`.
   - `CredentialSource::catalog_token` now takes the source.
   - Tests: `manage.rs#token_retargeting_to_another_host_is_refused`,
     `#keyring_values_round_trip_and_legacy_values_read_unbound`,
     `tests/catalogs.rs#token_retargeting_to_another_host_is_refused`,
     `#environment_token_is_not_given_to_a_runtime_source`.
2. **CSRF and DNS rebinding on loopback** (medium-high, pre-existing for every admin route).
   A browser guard now sits in the authentication middleware (`AuthPolicy::browser_guard`).
   It refuses a keyless caller of an admin route in three cases:
   - the `Origin` is neither the server's own nor an explicit CORS origin (`*` does not
     count);
   - `Sec-Fetch-Site` is `cross-site` or `same-site`;
   - for a loopback caller, `Host` is not `localhost` (or `*.localhost`), a loopback IP or
     the configured bind host.

   In addition, admin API bodies must be `application/json`. As an optional hardening,
   `file://` catalogs can be added only by loopback callers. ALK-SP-0006.R4 was updated
   with a new scenario.
   - Tests: `tests/catalogs.rs#csrf_and_dns_rebinding_on_loopback_are_refused`,
     `#configured_cors_origin_may_call_admin_routes`,
     `#file_catalogs_are_added_only_from_loopback`.
3. **Limits without an upper bound** (low). `CatalogLimits::validate` sets ceilings of
   256 MiB per file, 1 GiB per snapshot and 4 GiB per archive. Both the configuration and
   the API apply them.
   - Tests: `config.rs#catalog_limits_have_upper_bounds`,
     `tests/catalogs.rs#catalog_limits_have_upper_bounds`.
4. **Removal broke the lock guarantee** (low).
   - The lock file is no longer unlinked.
   - `SyncContext::with_source_check` stops a sync once the lock is held if the source is
     gone or its URL or folder changed. The stopped sync writes nothing, not even an error
     record.
   - The server's jobs and its background refresh use this check.
   - Tests: `alkahest-catalog/tests/sync.rs#removed_source_is_not_synced_after_the_lock`,
     `tests/catalogs.rs#remove_keeps_the_lock_and_removed_sources_are_not_synced`.
5. **Token set or delete racing a removal** (low). Both now run under `catalog_edit` and
   re-check the source against the file.
   - Test: `#token_set_rechecks_the_source_under_the_edit_lock`.
6. **CLI edits drifting from the running server** (low). Every API edit first re-reads
   `[[catalogs]]` from the configuration file (`manage::sources_from_file`) and refreshes
   the live set.
   - Tests: `manage.rs#sources_are_reread_from_the_file`,
     `tests/catalogs.rs#cli_edits_are_kept_by_api_edits`.

Residual risks:

- The CLI trusts its own configuration file for environment and unbound legacy tokens.
  Someone who rewrites that file through the admin API, with a name whose legacy or
  environment token exists in the CLI's environment, could still have a later CLI sync send
  that token. Bound tokens, meaning every token set since this fix, are not affected.
- The browser guard covers admin routes only. The Content-Type rule applies only to
  `/alkahest/...`; the Ollama `/api/pull` and similar routes keep accepting any content type
  from local clients.
