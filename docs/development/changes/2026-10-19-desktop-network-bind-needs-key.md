# Desktop: a network bind needs an API key, and startup errors reach the user

Date: 2026-10-19. Related: [[ALK-US-0131]], [[ALK-SP-0006]], [[ALK-SP-0008]].

## What

**Blocking the bind before it is saved.** "Allow access from other machines" (screen 14, API
keys & network; the Settings screen links to it for network settings) is refused while
`auth.require_key_for_remote` is on and no active API key exists. Nothing is saved. The network
card then shows an inline explanation (`BIND_NEEDS_KEY`) with a secondary "Create a key"
button that opens the new-key form. The same banner shows when a network bind is already saved
(set by hand, or the last key was revoked), so the next restart's failure is explained in
advance. Once a key exists the banner goes away and the toggle works.

- `crates/alkahest-desktop/src/keys.rs`: `bind_needs_key`, `BIND_NEEDS_KEY`,
  `KeysState::{active_keys, toggle_remote, bind_blocked, on_key_added, on_key_revoked}`, field
  `bind_refused`. `NetworkDraft::patch` takes the active key count and refuses a switch to a
  network bind without one. `is_loopback_host` now parses addresses (`127.0.0.0/8`, `::1`,
  `[::1]`, `localhost`); `0.0.0.0`, `::` and names such as `127.example.com` are not loopback.
- `crates/alkahest-desktop/src/views/keys.rs`: the toggle goes through `toggle_remote`; the
  banner with "Create a key"; the server's duplicate "no active API key" warning is hidden
  while the banner shows; created and revoked keys update the count.

The active key count comes from `GET /alkahest/v1/settings/network` (`active_keys`, already
served) and is kept current by the view when keys are created or revoked. No server API
changed.

**Startup errors in the connection state.** A server the desktop starts and that exits before
answering now ends the start at once with its own error text, instead of a 30 s wait followed
by "did not answer".

- `crates/alkahest-desktop/src/launcher.rs`: the detached `alkahest serve` writes its stderr
  to `<log_dir>/desktop-serve.stderr.log` (truncated on every launch; stdin and stdout stay
  null). The reaper thread records the exit status of the last spawned pid.
  `ServerLauncher::startup_failure` (default `None`) returns, for a process launch that exited,
  `the server exited while starting: <last error line>`; for the systemd user unit, when
  `systemctl --user is-failed` says `failed`, the summary of `journalctl --user -u
  alkahest.service` (or a pointer to that command). New `LocalLauncher::with_unit`,
  `with_stderr_log`, `startup_error_summary` (last `error:` line and up to two lines after it,
  ANSI codes stripped, 600 characters at most), `unit_failure_message`.
- `crates/alkahest-desktop/src/connection.rs`: `ConnectionManager::check_startup` asks the
  launcher after each probe while `Starting`; a failure goes through
  `Machine::on_launch_failed`, so the service card shows "Service not running" with the
  server's message and offers "Start server". A systemd start that times out points at
  `journalctl --user -u alkahest.service`.
- `crates/alkahest-desktop/src/lib.rs` (`LaunchOptions::serve_stderr_log`),
  `crates/alkahest-desktop/src/app.rs`, `crates/alkahest/src/main.rs`: the log path comes from
  the resolved `log_dir`.

## Why

Reproduced on v1.0.2: with no key, switching to `0.0.0.0` was saved, the restart stopped the
working server, the new one refused to start (ALK-SP-0006, correct) and the desktop only showed
it as unreachable. The product owner wants the network bind not to happen without a key, and
the desktop to say so before it takes effect. The previous working configuration is kept
because the save is refused. `alkahest serve` is unchanged: it still refuses a non-loopback
bind without keys unless `--allow-unauthenticated`.

## Verification

New tests: `keys::tests::{loopback_hosts, a_network_bind_without_keys_is_blocked,
the_toggle_waits_for_a_key_and_the_card_offers_one}`,
`launcher::tests::{the_summary_keeps_the_last_error_line, an_early_exit_reports_the_servers_error,
a_running_server_is_not_a_failure}`,
`connection::tests::{an_early_exit_on_restart_shows_the_servers_error,
a_systemd_start_timeout_points_at_the_journal}`.

- `cargo fmt --all --check`: clean.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: clean; also with
  `--features alkahest/desktop` for `alkahest` and `alkahest-desktop`.
- `cargo nextest run --workspace --locked`: 1401 passed, 1 skipped.
- `cargo test -p alkahest-desktop --features gui-test --locked`: 158 passed.
- `cargo deny check`, `cargo xtask check-deps`, `cargo xtask tokens --check --contrast`,
  `cargo build -p alkahest --no-default-features --locked`: OK.

## Notes

The detached server logs to stderr as well as to its log files, so the stderr file grows with
the server's log for as long as it runs; it is truncated on the next launch from the desktop.
