# Change: scheduler on-demand load and keep-alive (ALK-US-0028)

Implements [[ALK-US-0028]] / [[ALK-SP-0004]].R1 and R2, plus the scheduler side of
[[ALK-SP-0004]].R5 (stop) and R6 (crash handling). Builds on [[ALK-US-0023]], [[ALK-US-0024]],
[[ALK-US-0027]], [[ALK-US-0012]] and [[ALK-US-0014]]. Plan: [[plan/alkahest-v1-plan]].

## What

Crate `alkahest-sched`:

- `backend::ModelBackend` trait: `resolve(model, request_settings) -> ResolvedModel` and
  `launch(&ResolvedModel, LaunchRequest) -> Arc<dyn EngineInstance>` (both boxed futures).
  `ResolvedModel { key, name, digest, request: LaunchRequest, engine, dialect, capabilities,
  profile: Option<ModelProfile>, weights_bytes, projector_bytes, plan }`.
- `backend::EngineInstance` trait (pid, port, base_url, try_exit, exit_watch, stop,
  resource_usage), implemented for `alkahest_engine::EngineHandle`. `exit_watch` uses a state
  receiver so watching never keeps an engine alive.
- `backend::CatalogBackend::new(catalog, registry, platform, global)` with
  `with_spawn_options`, `with_log_dir` (`<dir>/<model>.log`) and `resolve_blocking`:
  `Catalog::resolve_loadable` -> `Catalog::effective_settings` -> `LaunchRequest::from_catalog`
  -> `EngineRegistry::plan` -> GGUF header profile; launch through `alkahest_engine::launch`
  with `EnginePlan::prepare` per port. Resolution runs on the blocking pool.
- `scheduler::Scheduler::new(backend, SchedulerOptions)` (needs a tokio runtime; starts the
  keep-alive reaper): `acquire(model, &request_settings, keep_alive) -> Lease`, `unload(model)`
  (name, `ollama:`/`alkahest:` key, `:latest` default tag or digest prefix), `unload_all()`,
  `shutdown()` (refuses new work, stops every engine), `loaded()`, `options()`.
- `SchedulerOptions::from_config(&SchedulerConfig)` (keep_alive, max_loaded, num_parallel,
  max_queue, `default_context` 4096, `load_timeout` 300 s, `restart: RestartPolicy`).
- `Lease`: model, key, digest, base_url, port, pid, engine, dialect, capabilities, settings
  (effective), context_length (per slot), launch_request, `check()` and `lost().await` (502
  `EngineCrashed` / 503 `Unloaded`). Dropping it ends the request.
- `error::SchedError` with `status_code()` (404/400/502/503/500) and `root()` for the shared
  `LoadFailed` error every waiter of one load receives.
- `launch_request` / `context_length`: explicit context (settings, else 4096 capped by the
  trained context) and `num_parallel` slots for the engine.

Behaviour:

- One load per model: the first request inserts a loading entry and spawns the load task;
  later requests wait on the runner's status channel. The load is not cancelled when the
  requesting client goes away; waiting requests are counted and un-counted by a guard.
- Keep-alive: request value > per-model setting > global; the latest request's value is used
  for the idle period after it; `0` unloads after the request, negative never unloads. The
  reaper task sleeps until the earliest deadline (tokio clock).
- A request whose load settings differ (context, KV cache types, GPU layers, flash attention,
  template, engine, extra args, slots, digest) marks the instance stale: no new leases, unload
  when idle, then reload. Sampling options and `keep_alive` never reload.
- Crash: the exit watcher removes the model, records the crash in a `RestartTracker`; leases
  see `EngineCrashed` with the stderr tail; the next request reloads after the backoff or gets
  `CrashLoop` (502) once the policy gives up.
- Stop / shutdown: waiting requests fail with `Unloaded` (503); engines are stopped and
  awaited.

## Decisions

- The scheduler talks to engines through `ModelBackend` / `EngineInstance` so timing tests
  run on tokio's paused clock with in-process fakes; one end-to-end test uses the real
  catalog, registry and supervisor with `alkahest-fake-engine`.
- `num_parallel` reaches the engine through `LaunchRequest::num_parallel` (added by the
  llama-server adapter, [[ALK-US-0025]]), which turns it into `-c <ctx * slots> -np <slots>`.
  The first version of this story passed a leading `--parallel` extra argument; it was
  replaced after merging the adapter.
- The default context (4096, Ollama's) is always passed explicitly so the engine never
  allocates the trained context the memory estimate did not count.

## Verification

`crates/alkahest-sched/tests/scheduler.rs` (20 tests, scenarios `single_load`, `idle_unload`,
`crash`, `stop`, plus keep-alive 0 / forever / precedence / reset, reload on settings change,
no reload on sampling change, crash loop, shared load failure, cancellation, shutdown, drop),
`tests/end_to_end.rs` (`idle_unload_stops_the_real_engine_process`), unit tests in `error`,
`backend`, `scheduler`. `scripts/ci-local.sh` passes (nextest 410 passed), `cargo machete`
clean.
