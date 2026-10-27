# Start at login also starts the local server (ALK-US-0137)

Date: 2026-10-27. Story [[ALK-US-0137]], follow-up of [[ALK-US-0073]] and
[[2026-10-17-desktop-tray-autostart]]. Requirement [[ALK-SP-0008]].R2. Plan:
[[plan/alkahest-v1-plan]].

## What changed

- `crates/alkahest/src/cli.rs` (`DesktopArgs::start_server`), `crates/alkahest/src/main.rs`,
  `crates/alkahest-desktop/src/lib.rs` (`LaunchOptions::start_server`): new
  `alkahest desktop --start-server`.
- `crates/alkahest-desktop/src/autostart.rs`: the entries (XDG, LaunchAgent, `Run` key) run
  `desktop --start-server` (`ENTRY_ARGS`); `refresh` rewrites an enabled entry that differs
  from the current one (older version, moved executable). Called from `app::start`.
- `crates/alkahest-desktop/src/connection.rs`: `Machine::set_auto_start` /
  `Machine::take_auto_start` (true once, only when the first settled probe is `NotRunning`
  without error and this app may start servers); `ConnectionManager::with_auto_start`, and the
  run loop launches unless `ServerLauncher::external_service` reports a service.
- `crates/alkahest-desktop/src/launcher.rs`: `ServerLauncher::external_service` (default
  `None`), `LocalLauncher::external_service`, `installed_external_service` (system systemd unit
  under `/etc`, `/usr/lib`, `/lib`; launchd daemon or user agent `es.digio.alkahest`) and
  `windows_service_installed` (`sc.exe query alkahest`). The Linux user unit is not external:
  the launcher starts it with `systemctl --user start`.
- Settings caption and `docs/book/src/guide/desktop.md` describe the behaviour.

## Why

Product owner request: configuring start at login should bring the server up too, but never a
second server next to an installed service (at login a system service may still be binding its
port, so the app keeps probing instead of racing it).

## Verification

- Unit tests: `auto_start_fires_once_and_only_when_nothing_answers`,
  `start_at_login_starts_the_server_when_none_runs`,
  `start_at_login_leaves_the_server_to_an_installed_service`,
  `a_system_unit_counts_as_an_external_service_and_the_user_unit_does_not`,
  `an_old_entry_is_rewritten_with_start_server_and_a_disabled_one_is_left_alone`.
- Real run under `xvfb-run`, scratch `HOME`, `--host 127.0.0.1:11499`:
  - with `/etc/systemd/system/alkahest.service` present: log `a service runs the server; not
    starting another`, nothing listens on 11499;
  - with the unit hidden (`unshare -rm`, `/dev/null` bind-mounted over it): log `started a
    server method=alkahest serve · pid …`, `GET /alkahest/v1/info` answered on 11499.

## Limits

- Service detection is by installed file, not by its port: a system service bound to another
  port than the desktop's configured server still stops the automatic start (the user can press
  "Start server").
- The macOS and Windows paths compile only on those targets and were not run here.
