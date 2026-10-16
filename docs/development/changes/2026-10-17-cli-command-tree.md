# Change: CLI command tree skeleton

Story [[ALK-US-0004]]; builds on [[ALK-US-0003]] and [[plan/alkahest-v1-plan]].

## What

`crates/alkahest/src/cli.rs` defines the clap tree: `pull`, `search`, `ls` (alias `list`), `ps`,
`rm`, `show`, `cp`, `run`, `stop`, `serve`, `desktop`, `keys`, `engines`, `service`, `sysinfo`,
`config show|path`. Global flags: `--host`, `--config`, `-v`, `--log-level`, `--json`.
`Command::owner()` maps each unbuilt command to its story; `main.rs` prints
`alkahest <cmd>: not implemented yet (ALK-US-xxxx)` and exits 2. `config show|path` work now
(text with `# source:` comments, or JSON). `desktop` without the `desktop` feature explains how
to rebuild and exits 1. Logging goes to stderr; only `serve` enables the file appender.

Owners: pull 0017, search 0021, ls/show 0012, ps/stop 0031, rm/cp 0013, run 0039, serve 0033,
desktop 0064, keys 0041, engines 0049, service 0045, sysinfo 0006.

## Not done

Shell completions (task ALK-T-0015) need `clap_complete`, not yet pinned. Table output
conventions (ALK-T-0014) arrive with the first listing command.

## Verification

fmt, clippy `-D warnings` (also with `--features desktop`), nextest (46 tests, `assert_cmd` suite
in `crates/alkahest/tests/cli.rs`), deny, check-deps, headless build all pass.
