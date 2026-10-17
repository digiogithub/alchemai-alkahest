# Graceful shutdown and signal handling

Story [[ALK-US-0046]], requirement [[ALK-SP-0009.R4]]; also closes task ALK-T-0103 of
[[ALK-US-0033]]. Builds on [[2026-10-17-api-server-skeleton]]. Plan:
[[plan/alkahest-v1-plan]].

## What changed

- `crates/alkahest-api/src/shutdown.rs`:
  - `ShutdownHooks` (cloneable shared registry): `register(name, || async {..})`,
    `run(timeout)` runs each hook once, newest first, each bounded by the timeout,
    returning `(name, HookOutcome::{Completed, TimedOut})`. The scheduler story registers
    engine teardown here, through `AppState::shutdown_hooks()`.
  - `stop_signal()`: installs handlers eagerly and returns a future resolving to the
    signal name. Unix: SIGTERM, SIGINT. Windows: ctrl-c, ctrl-break, console close, system
    shutdown (cross-checked with `cargo check --target x86_64-pc-windows-gnullvm`). The
    Windows service wrapper (ALK-US-0045) passes its own stop future to `serve_with`.
- `crates/alkahest-api/src/server.rs`: `serve_with(listener, app, shutdown, ServeOptions,
  ShutdownHooks) -> ShutdownReport { drained, hooks }`. On the stop future: the listener
  closes (no new connections), in-flight requests drain up to
  `ServeOptions::drain_timeout`, connections still open are abandoned (axum runs them as
  spawned tasks, cancelled when the runtime shuts down at process exit), then hooks run.
  `ServeOptions::from_config` (drain from `server.shutdown_timeout_secs`, default 30 s;
  hook budget 15 s). `serve(..)` stays as the hook-less convenience.
- `alkahest-core`: `server.shutdown_timeout_secs` (default 30).
- `alkahest serve`: `stop_signal()` installed before binding; `serve_with` with the state's
  hooks; the key usage flush is registered first so it runs last; prints
  `alkahest serve: stopped` and exits 0.

## Security / robustness checklist

| Risk | Mitigation | Proof |
|---|---|---|
| Orphan engine processes after stop | hooks run after drain, even on drain timeout | `tests/shutdown.rs#no_orphans` (scenario), `drain_timeout_bounds_the_wait_and_hooks_still_run` |
| Stop hangs on a stuck stream | drain timeout, per-hook timeout | `drain_timeout_bounds_the_wait_and_hooks_still_run`, `shutdown.rs#hooks_run_newest_first_once_and_time_out` |
| Requests cut mid-flight on a normal stop | graceful drain | `in_flight_requests_finish_and_new_ones_are_refused` |
| New requests accepted while stopping | listener closed on signal | same test (connect fails afterwards) |
| Signal arriving during startup lost | handlers installed before bind | code review (`serve_cmd.rs`) |
| Wrong exit code for service managers | exit 0 on SIGTERM/SIGINT | `crates/alkahest/tests/serve.rs#sigterm_stops_the_server_with_exit_code_0`, `sigint_...` |
| Lost usage counters on stop | final flush as last hook | `serve_cmd.rs`; `last_used_shows_in_keys_ls_and_the_audit_log` |

Not done here: a second signal does not force an immediate exit (the drain timeout bounds
the wait instead).

## Verification

`scripts/ci-local.sh`: fmt, clippy, nextest (404 passed, 1 skipped), deny, check-deps,
headless build, typos: all passed. `cargo machete` clean. Windows cross-check of
`alkahest-api` passes.
