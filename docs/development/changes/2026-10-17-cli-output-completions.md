# CLI output conventions and shell completions (ALK-T-0014, ALK-T-0015)

## What

- `crates/alkahest/src/output.rs`: `Listing` (headers + rows, `push_row`, `render`), `Format`
  (`Table`, `Json`, `Format::from_json_flag`). Table: upper-case headers, two-space column gap,
  no trailing spaces. JSON: array of objects keyed by lower-case headers. Exit codes: 0 ok,
  1 runtime failure, 2 usage (clap).
- Global `--json` flag and `alkahest completions <bash|zsh|fish|elvish|powershell>` in `main.rs`.
- `clap_complete =4.6.8` pinned in workspace deps; `serde_json` used by the renderer.
- `cargo-machete` ignore for the optional `alkahest-desktop` dependency.

## Verification

Unit tests in `output.rs` and `main.rs`; integration `crates/alkahest/tests/completions.rs`;
`scripts/ci-local.sh` passes.

Links: [[ALK-US-0004]], [[ALK-US-0002]], [[plan/alkahest-v1-plan]].
