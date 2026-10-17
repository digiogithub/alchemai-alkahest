# Change: engine process supervisor (ALK-US-0024)

Implements [[ALK-US-0024]] / [[ALK-SP-0004]].R6 (crash detection side) and the child
environment part of [[ALK-SP-0003]].R5. Builds on [[ALK-US-0023]]. Plan:
[[plan/alkahest-v1-plan]].

## What

Crate `alkahest-engine`, module `supervisor`:

- `launch(make_spec: FnMut(u16) -> Result<LaunchSpec>, &SpawnOptions) -> EngineHandle`:
  picks a loopback port (`PortPicker`, default `EphemeralPorts` binding `127.0.0.1:0`),
  checks it is free, spawns, waits for readiness and retries on a new port when the port was
  taken or the engine exits with a bind failure (`looks_like_bind_failure`); any other failure
  stops the process and is returned. `spawn(spec, &opts)` starts without waiting.
- Spawning: `tokio::process`, `kill_on_drop`, stdin null, own process group on unix
  (`process_group(0)`), spec env (library search path) and working dir.
- `EngineHandle` (clone): `engine`, `spec`, `pid`, `port`, `base_url`, `started_at`, `state`,
  `subscribe() -> watch::Receiver<EngineState>`, `logs`, `try_exit`, `wait_ready`, `exited`,
  `stop`, `stop_with_grace`, `resource_usage(&ResourceSampler)`.
- `EngineState`: `Starting`, `Ready`, `Stopping`, `Exited(ExitInfo)`; `ExitInfo { code,
  signal, requested, stderr_tail }` with `crashed()`, `success()`, display
  (`exited with code 3`, `was killed by signal 9`).
- A monitor task owns the child: on crash it records exit status and stderr tail and logs a
  warning; on stop it sends SIGTERM to the process group, SIGKILL after `spec.stop_grace`
  (Windows: `TerminateProcess` at once); after any exit it kills stragglers left in the group.
  Dropping every handle stops the engine; a drop guard kills the group if the runtime shuts
  down first.
- `health::http_get`: minimal HTTP/1.1 GET on loopback (no HTTP client dependency).
- `logs::LogBuffer`: ring of the last N lines of both pipes (`tail`, `tail_text`), mirrored
  to tracing (`alkahest_engine::output`, debug) and optionally to a per-model log file
  (`SpawnOptions.log_file`).
- `usage::ResourceSampler`: RSS and CPU of the process tree via `sysinfo`.
- `restart::RestartTracker` / `RestartPolicy` / `RestartDecision`: crash-loop guard the
  scheduler consults before reloading (exponential backoff, give up after N crashes in a
  window).
- `kill::{terminate_group, kill_group}` over `rustix::process::kill_process_group` (safe API).
- New `EngineError` variants: `Spawn`, `StartupTimeout`, `ExitedBeforeReady`, `Stopped`,
  `PortUnavailable`, `Io`.
- Test support binary `alkahest-fake-engine` (`src/bin/`, std only): `/health` with
  configurable ready delay, crash, immediate exit, a forked sleeper and llama-server's bind
  error wording.

## Decisions

- New pinned dependency `rustix =1.1.5` (unix only, features `std`, `process`; already in the
  lock through `tempfile`): process-group signals without local `unsafe`.
- The supervisor reports crashes; failing in-flight requests with 502 and reloading on the
  next request is the scheduler's part of ALK-SP-0004.R6.
- Windows has no process group handling yet: only the direct child is terminated. Job
  objects are filed as a follow-up.

## Verification

`crates/alkahest-engine/tests/supervisor.rs` (14 tests: ready and clean stop with log capture
and log file, health timeout, exit before ready, scenario `crash`, kill detection, port
collision before and after spawn, ports exhausted, missing binary, SIGTERM-to-SIGKILL
escalation, process group kill, drop stops the engine, resource usage, library path reaches
the child), unit tests in `supervisor`, `logs`, `health`, `restart`, `usage`. Five
consecutive runs green. `scripts/ci-local.sh` passes (nextest 352 tests).
