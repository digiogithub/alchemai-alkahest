# Change: engines no longer outlive a killed server (ALK-US-0092, ALK-US-0093)

Fixes [[ALK-US-0092]] and [[ALK-US-0093]]. Builds on [[2026-10-17-engine-supervisor]] and
[[2026-10-17-graceful-shutdown]]. Plan: [[plan/alkahest-v1-plan]].

## What

Crate `alkahest-engine`, module `supervisor`:

- `orphan` (private): `spawn_tied_to_parent(Command)`. On Linux every engine gets
  `PR_SET_PDEATHSIG = SIGTERM` in a `pre_exec` hook (rustix `set_parent_process_death_signal`,
  one local `#[allow(unsafe_code)]` with a SAFETY comment: two raw syscalls, no allocation),
  and exits at once when it was already reparented between fork and `prctl`. The kernel ties
  the parent-death signal to the forking *thread*, so engines are forked from one
  process-lifetime thread (`alkahest-engine-spawner`) that enters the caller's tokio runtime;
  forking from a blocking-pool thread would kill the engine when that thread retires.
  Other platforms spawn plainly.
- `ledger`: `ProcessLedger` (`open`, `with_owner`, `record`, `forget`, `entries`,
  `sweep(grace) -> SweepReport`), `LedgerEntry { pid, start_time, exe, model, owner_pid,
  owner_start_time }`, `process_identity(pid)` (sysinfo start time and executable; zombies
  count as gone). JSON file written atomically, re-read on every update (servers may share a
  data directory). The sweep stops an engine (SIGTERM to its process group, SIGKILL after the
  grace) only when its owner server is gone (pid *and* start time) and the live process still
  has the recorded pid, start time and executable. Ollama's own `llama-server`, recycled pids
  and engines of another running server are never touched.
- `SpawnOptions.ledger: Option<Arc<ProcessLedger>>`: `spawn` records each engine right after
  exec, the monitor forgets it after the exit.
- Re-exports `ProcessLedger`, `LedgerEntry`, `SweepReport`.

Binary: `alkahest serve` opens `<data_dir>/run/engines.json`, sweeps it on the blocking pool
before building the scheduler (stopped engines are reported on stderr and in the log) and
passes the ledger to the `CatalogBackend` spawn options.

Tests (ALK-US-0093): `crates/alkahest/tests/serve.rs`, `ps.rs` and `serve_inference.rs` stop
`alkahest serve` with SIGTERM (`stop_server`, SIGKILL after 10 s as a fallback). The opt-in
`serve_inference.rs` chat part defaults to `qwen2.5-coder:0.5b` (`ALKAHEST_TEST_CHAT_MODEL`,
empty skips): decision models such as `tev1:0.8b` refuse chat since ALK-US-0089.

## Limitations

- macOS has no parent-death signal and Windows has no job object yet: a killed server leaves
  its engines running until the next `alkahest serve` sweeps them (inbox item).
- A SIGKILLed server's engine gets SIGTERM, not SIGKILL, so helper processes (vLLM workers)
  can be cleaned up by the engine itself; an engine that ignores SIGTERM is caught by the next
  sweep.

## Verification

- `crates/alkahest-engine/tests/orphans.rs`: `engine_dies_when_the_server_is_sigkilled`
  (re-executes the test binary as a "server" that launches the fake engine, SIGKILLs it,
  asserts the engine dies; fails without the `pre_exec` hook, checked),
  `engine_survives_the_spawning_thread` (blocking-pool thread retires after 20 ms, the engine
  keeps running), `stale_engines_are_stopped_on_startup` (only the orphan is stopped; recycled
  pid, foreign executable and live-owner entries survive), `supervisor_keeps_the_ledger`.
  Unit tests in `supervisor::ledger`.
- Real engine: `alkahest serve` on a scratch `ALKAHEST_HOME`, tiny-llamas loaded through
  `/api/generate`, `kill -9` of the server: the `llama-server` child was gone within a second.
  Opt-in `serve_inference.rs` (`ALKAHEST_TEST_ENGINE=1`, default chat model
  `qwen2.5-coder:0.5b`, embed `nomic-embed-text`): passed, the ledger was empty afterwards.
- `scripts/ci-local.sh`: passed.
