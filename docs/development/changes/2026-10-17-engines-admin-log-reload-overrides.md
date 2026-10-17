# Change: engines admin API: install log tail, recipe reload, overrides, listing flags (ALK-US-0107)

Implements [[ALK-US-0107]] ([[ALK-SP-0003]].R3-R5, [[ALK-SP-0005]].R6) for the desktop
Engines screen (design screen 15, [[2026-10-17-desktop-keys-engines-settings]]). Builds on
[[2026-10-17-admin-api]] and [[2026-10-17-recipe-format-engine-manager]]. Plan:
[[plan/alkahest-v1-plan]].

## What

`crates/alkahest-api/src/admin/`:

- `jobs.rs`: every job keeps its last `LOG_LINES` (2000) output lines in memory
  (`JobHandle::log`; `phase` logs the step, the outcome logs `done` / `cancelled` /
  `failed: ...`) and may name a log file (`JobHandle::set_log_path`, exposed as
  `Job::log_path`). `JobManager::log(id, lines) -> JobLog` reads the file's tail when it
  exists, else the in-memory lines. Route `GET /alkahest/v1/jobs/{id}/log?lines=` (default
  200, at most 2000; 404 for unknown jobs).
- `engines.rs`:
  - `EngineVariant` gained `bundled` (installed under a bundled root next to the binary, so it
    cannot be removed) and `download_bytes` (the recipe asset's `size` for this host).
  - `EngineManager::install_log_path` (default `None`): `RecipeEngineManager` returns the file
    a source build writes (`<engines_dir>/.logs/<id>-<dir>.log`, the recipes' default), which
    the install job exposes; the job also logs `installed {...}` with the result.
  - `EngineManager::reload` (default: listing count): `RecipeEngineManager` re-reads the
    recipe directories and returns engines, variant count, recipe dirs and the ignored files
    with reasons. Route `POST /alkahest/v1/engines/reload`.
  - Per-engine override: `GET` / `PUT /alkahest/v1/engines/{id}/override` (id parsed by
    `EngineId`, aliases accepted, unknown 404). `PUT {"path": abs | null, "extra_args": [..]}`
    writes `[engines.overrides.<id>]` with `toml_edit` through `settings::write_document`
    (comments kept, result validated, atomic); both empty removes the table. The answer has
    `running`, `saved` and `restart_required` (the engine registry is built at start).
- `context.rs`: `AdminContext::saved_overrides`.

`crates/alkahest-client` (new file `engine_admin.rs`, additive): `JobLog`,
`EngineOverride`, `EngineOverrideDoc`, `Client::{job_log, reload_recipes, engine_override,
set_engine_override}`; `admin::EngineVariant` gained `bundled` and `download_bytes`.

Docs: admin route table.

## Verification

- `crates/alkahest-api/tests/admin_api.rs#engine_admin_log_reload_and_override` (log tail of
  an install job, 404, listing flags, reload, override GET/PUT persisted and validated,
  removal by empty body).
- Unit: `admin::jobs::tests::job_logs_keep_phases_and_prefer_the_log_file`,
  `admin::engines::tests::{overrides_are_written_and_removed,
  recipe_listing_carries_download_sizes_and_reload_reports}`.
- `crates/alkahest-client/tests/client.rs#job_log_reload_and_engine_override`.
- fmt, clippy `-D warnings`, nextest api and client: passed.
