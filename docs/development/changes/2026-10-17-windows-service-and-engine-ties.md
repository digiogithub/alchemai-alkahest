# Windows service, engine job objects and the macOS engine watchdog

Stories [[ALK-US-0048]] (tasks ALK-T-0136, ALK-T-0137, ALK-T-0221) and [[ALK-US-0094]],
requirement [[ALK-SP-0009.R1]] (R2 on Windows, R4 "no orphans" when the server is killed).
Builds on [[2026-10-17-systemd-service]], [[2026-10-17-macos-launchd-service]],
[[2026-10-17-orphaned-engines]] and [[2026-10-17-graceful-shutdown]]. Plan:
[[plan/alkahest-v1-plan]].

## What changed

### Windows service (`crates/alkahest/src/service_windows.rs`)

- `plan_install` (platform independent, `--manager windows` with `--dry-run`/`--root` anywhere):
  `sc.exe create alkahest binPath= "\"<exe>\" serve --service" start= delayed-auto
  obj= "NT AUTHORITY\LocalService" DisplayName= Alkahest`, `sc.exe description`, `sc.exe failure
  ... restart/5000/restart/5000/restart/30000`, `sc.exe failureflag 1`, `reg.exe add` of the
  service `Environment` value `ALKAHEST_HOME=%ProgramData%\Alkahest`, event source
  `Application\Alkahest` with `EventMessageFile = %SystemRoot%\System32\EventCreate.exe` and
  `TypesSupported 7`, data directory creation, `icacls <data> /grant *S-1-5-19:(OI)(CI)M`
  (LocalService by SID, language independent), `sc.exe start`. `plan_uninstall` (tolerated stop,
  delete, event source removal; data kept), start/stop, `status` from `sc.exe query`
  (`parse_query`, exit 3 when not running). `--user` is refused (services are machine-wide).
- Service entry `alkahest serve --service` (hidden flag; refused outside Windows):
  `windows-service` dispatcher; control handler maps Stop/Shutdown/Preshutdown to the server's
  stop future and reports `StopPending` (60 s hint); status `StartPending` (120 s hint) →
  `Running` (accepts STOP|SHUTDOWN) when the listener is bound → `Stopped` with a
  service-specific exit code 1 on failure.
- Event log: `eventlog::{info, error}` (`RegisterEventSourceW`/`ReportEventW`, source
  `Alkahest`): service starting, listening URL, stopped, failure with the error chain.
- `serve_cmd`: `ServeControl { stop, force, on_listening }` and `run_with(loaded, allow,
  control)`; the foreground run builds it from the OS signals, the service from the control
  handler. `alkahest serve` behaviour is unchanged.
- `service_cmd`: `--manager windows` dispatches here; `shell_line` escapes quotes inside quoted
  words.

### Engines tied to the server (`alkahest-engine`, `supervisor::orphan`)

- `spawn_tied_to_parent` now returns `(Child, Tie)`; the monitor keeps the `Tie` until the engine
  is reaped, then calls `terminate_tree()` and drops it.
- Windows (ALK-T-0221): each engine is created with `CREATE_NEW_PROCESS_GROUP` and assigned to its
  own job object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. The server holds the only handle:
  if it dies, the kernel kills the engine's whole tree. A graceful stop sends `CTRL_BREAK` to the
  engine's group (works when the server has a console), waits the grace, then
  `TerminateJobObject` (whole tree) and `TerminateProcess`.
- macOS and other non-Linux Unix (ALK-US-0094): a watchdog per engine, `/bin/sh -c <SCRIPT>
  alkahest-engine-watchdog <server> <engine>`, in its own process group. Once a second it checks
  that its parent is still the server (`ps -o ppid= -p $$`; covers a zombie server and pid reuse)
  and that the engine lives; when the server is gone it sends SIGTERM to the engine's process
  group and SIGKILL after 5 s. The supervisor kills and reaps the watchdog with the engine.
  `ALKAHEST_ENGINE_WATCHDOG=1` (`supervisor::WATCHDOG_ENV`) selects it on Linux too.
- Linux keeps `PR_SET_PDEATHSIG`; every platform keeps the ledger sweep at startup.
- Workspace pins: `windows-sys =0.61.2` (the line tokio and windows-service already use; features
  Foundation, Security, Security_Authorization, Storage_FileSystem, System_Console,
  System_EventLog, System_JobObjects, System_Threading) and `windows-service =0.8.1` (MIT OR
  Apache-2.0, Mullvad). Both Windows-only dependencies. `THIRD_PARTY_LICENSES` regenerated.
- `docs/install.md`: Windows row and the engine-tie paragraph.

## Decisions

- `sc.exe`/`reg.exe`/`icacls.exe` command plans instead of SCM API calls for install: the same
  plan is printed by `--dry-run`, reviewed by tests on Linux, and executed on Windows, exactly as
  for systemd and launchd. `windows-service` is used where an API is unavoidable: the service
  entry (dispatcher, control handler, status).
- `NT AUTHORITY\LocalService`, not LocalSystem: least privilege (ALK-SP-0009.R2); the data
  directory gets an explicit modify grant.
- `EventCreate.exe` as message file: its message table renders `%1`, so events read as plain text
  without shipping a message DLL.
- macOS watchdog as a shell loop instead of a kqueue helper process: nothing extra to ship or
  locate (engines are third-party binaries, so a parent-pipe trick inside the engine is not
  possible), one-second latency, the ledger sweep remains the backstop. A fork-without-exec
  watchdog inside `pre_exec` was rejected: it would inherit every open descriptor of the server
  (client sockets would never see EOF).
- One job object per engine (not one per server): a stop terminates exactly that engine's tree.

## Security checklist

| Threat | Mitigation | Proof |
|---|---|---|
| Service runs with SYSTEM rights | `obj= NT AUTHORITY\LocalService` | `service_windows::tests::install_registers_a_delayed_auto_service_as_local_service` |
| Unquoted service path hijack (`C:\Program Files\...`) | `binPath` quotes the executable | same test, `tests/service.rs#windows_dry_run_prints_the_sc_commands` |
| Data directory writable by everyone / not writable by the service | only a LocalService modify grant added to the ProgramData ACL | same test (icacls line) |
| Engines (and their children) left running after a crash or kill | Windows job object (kill on close), macOS watchdog, Linux parent-death signal, startup sweep | `tests/orphans.rs#engine_dies_when_the_server_is_sigkilled`, `#engine_dies_with_the_server_through_the_watchdog`, `orphan::tests::watchdog_kills_the_engine_group_when_the_server_dies` |
| Watchdog kills a foreign process | parent-pid check (not just `kill -0`), group of the recorded engine only, watchdog killed when the engine is reaped | `watchdog_exits_when_the_engine_is_gone`, code review |
| Accidental changes on the developer machine | non-native managers only with `--dry-run`/`--root`; `serve --service` refused outside Windows | `tests/service.rs#another_platforms_manager_needs_dry_run_or_root`, `#service_entry_is_windows_only` |

## Not verified

No Windows or macOS machine was used. The Windows code (`entry`, `eventlog`, job objects) and the
macOS path pass `cargo clippy -D warnings` for `x86_64-pc-windows-gnullvm` and
`aarch64-apple-darwin` (zig as the C compiler for aws-lc-sys); the plans and the watchdog script
are tested on Linux. To run on Windows: install/start/stop/status as administrator, Event Viewer
entries, killing `alkahest.exe` in Task Manager takes `llama-server.exe` with it.

## Verification

- New tests: `service_windows::tests` (5), `tests/service.rs` (`windows_dry_run_prints_the_sc_commands`,
  `service_entry_is_windows_only`), `orphan::tests` (2), `tests/orphans.rs`
  (`engine_dies_with_the_server_through_the_watchdog`; `engine_dies_when_the_server_is_sigkilled`
  now runs on every Unix).
- `scripts/ci-local.sh`: passed (1025 tests). `cargo machete` clean for `alkahest`,
  `alkahest-engine`.
- Cross clippy: `-p alkahest -p alkahest-engine` for Windows (lib/bin), `-p alkahest -p
  alkahest-engine -p alkahest-api --all-targets` for macOS.
