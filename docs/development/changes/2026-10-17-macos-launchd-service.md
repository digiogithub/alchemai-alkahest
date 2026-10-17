# macOS launchd service (`alkahest service` on macOS)

Story [[ALK-US-0047]] (task ALK-T-0135), requirement [[ALK-SP-0009.R1]] (and R2 on macOS).
Follows [[2026-10-17-systemd-service]]. Plan: [[plan/alkahest-v1-plan]].

## What changed

- `crates/alkahest/src/service_launchd.rs` (new), platform-independent generator plus apply:
  - `render_plist(JobSpec)`: `Label es.digio.alkahest`, `ProgramArguments [<bin>, serve]`,
    `RunAtLoad`, `KeepAlive` (restart like `Restart=always`), `ExitTimeOut 45` (above the 30 s
    drain), `SoftResourceLimits NumberOfFiles 4096` (macOS default 256), stdout/stderr file;
    daemon only: `UserName`/`GroupName _alkahest`, `EnvironmentVariables ALKAHEST_HOME` and `HOME`
    = `/Library/Application Support/Alkahest`, `WorkingDirectory`. XML-escaped values.
  - `plan_install`: system = `dscl` creation of the hidden `_alkahest` group and user when missing
    (first id free as uid and gid in 250..500, from `dscl . -list`), `mkdir`/`chown`/`chmod 0750`
    of the data and log directories, plist in `/Library/LaunchDaemons` then `chown root:wheel` +
    `chmod 0644` (launchd refuses other ownership), tolerated `launchctl bootout`, `enable`,
    `bootstrap system`. `--user` = LaunchAgent in `~/Library/LaunchAgents`, log dir
    `~/Library/Logs/Alkahest`, domain `gui/<uid>`; no account, no system change.
  - `plan_uninstall` (tolerated bootout, plist removed; account and data kept), `plan_start`
    (tolerated bootstrap + `kickstart`), `plan_stop` (`bootout`, so KeepAlive does not respawn),
    `status` via `launchctl print` (`parse_print`: `state = running`, `pid`), exit code 3 when not
    running, `--json` with `listen` (honours `https://` with TLS).
- `crates/alkahest/src/service_cmd.rs`: dispatcher by `--manager systemd|launchd|windows`
  (default: the native one; another platform's manager is refused unless `--dry-run` or
  `--root`), shared helpers `Step { argv, may_fail }`, `apply_steps`, `write_files`,
  `remove_file`, `service_binary`, `under`, `executes`, `shell_line` (quotes words with spaces).
  The systemd behaviour is unchanged.
- `cli.rs`: `ServiceManager` value enum, `ServiceArgs.manager`.
- `alkahest` crate: `rustix` (already pinned) on Unix for `getuid`.
- `docs/install.md`: "Running as a service" table.

## Decisions

- Label `es.digio.alkahest`, the desktop app id, so one reverse-DNS id names the product.
- `service stop` = `bootout` rather than `launchctl kill`: with `KeepAlive` a killed job is
  respawned at once, which is not what "stop" means. The job loads again at boot/login.
- Daemon account `_alkahest` (macOS convention for daemon users, hidden, shell `/usr/bin/false`,
  password `*`), never root (ALK-SP-0009.R2).

## Security checklist

| Threat | Mitigation | Proof |
|---|---|---|
| Server runs as root | `UserName _alkahest`, data dir 0750 owned by it | `service_launchd::tests::daemon_plist_runs_as_the_service_account_and_keeps_alive` |
| Tampered daemon plist | `chown root:wheel`, `chmod 0644` after writing | same test |
| Login shell for the service account | `UserShell /usr/bin/false`, `Password *`, hidden | code review (`create_daemon_account`) |
| Plist injection through odd paths | XML escaping of every string | `plist_is_well_formed`, `agent_goes_to_the_user_library_without_system_changes` |
| Accidental changes on the developer machine | `--dry-run`, `--root`, non-native manager refused without them | `tests/service.rs#another_platforms_manager_needs_dry_run_or_root`, `launchd_*` tests |

## Not verified

No macOS machine was used: the plist and commands are checked by unit tests and the binary
compiles and passes clippy for `aarch64-apple-darwin`. The scenario `status` (a running service
reports running and the listen address) needs a real macOS run.

## Verification

- `service_launchd::tests` (7 tests), `crates/alkahest/tests/service.rs` launchd tests (dry run,
  `--user` install into a root + status + uninstall, refusal without dry run).
- `cargo clippy --target aarch64-apple-darwin -p alkahest -p alkahest-api -- -D warnings` passes
  (zig as the C compiler for aws-lc-sys).
- `scripts/ci-local.sh` passed.
