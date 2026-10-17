# Change: Linux systemd service (ALK-US-0045)

Implements [[ALK-US-0045]] (tasks ALK-T-0131, ALK-T-0132, ALK-T-0133) for [[ALK-SP-0009]].R1,
R2 and R3 on Linux. Plan: [[plan/alkahest-v1-plan]].

## What

- `alkahest service install|uninstall|start|stop|status` replace the stub
  (`crates/alkahest/src/service_cmd.rs`, `ServiceAction`/`ServiceArgs` in `cli.rs`). Options:
  `--user`, `--dry-run`, `--root <dir>`, `--bin <path>`.
- Templates `packaging/systemd/alkahest.service` (system) and `alkahest.user.service`
  (embedded with `include_str!`). System unit: `User=alkahest`, `Restart=always`,
  `ExecStart=<bin> serve`, `HOME`/`ALKAHEST_HOME=/var/lib/alkahest`, `StateDirectory=alkahest`,
  `SupplementaryGroups` limited to the groups that exist (`render`, `video`, and `ollama` for
  read access to a system Ollama store), `TimeoutStopSec=45` (above the 30 s drain), journald
  for logs (R3, no log file setting). The system configuration file is read by the server as
  usual.
- Planning is separate from applying: `plan_install`, `plan_uninstall`, `render_system_unit`,
  `render_user_unit` are pure; install creates the user with `useradd --system` (or
  `usermod --append --groups` when it exists), `install -d` for the state directory, writes the
  unit, `systemctl daemon-reload` and `enable --now`. `--dry-run` prints files and commands;
  `--root` writes below the directory, reads `etc/group` and `etc/passwd` from it and runs no
  command. `uninstall` keeps the user and its data. `status` reports running and the listen
  URL (`--json` supported); exit code 3 when inactive, as `systemctl is-active`.

## Not done

macOS launchd and Windows service (R1 other platforms) belong to other stories. No unit was
installed and no user created on the development machine; verified through the plan and temp
roots only, so a real boot test (scenario `reboot`) is still to be run on a VM.

## Verification

Unit tests in `service_cmd.rs` and `crates/alkahest/tests/service.rs` (dry run, root install
and uninstall, `--user` into a temp XDG dir, status JSON). `scripts/ci-local.sh` passes.
