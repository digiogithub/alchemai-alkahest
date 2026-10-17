# Bundle: reinstall the engine when the CI cache left an empty skeleton

Date: 2026-10-17. Commit `05f4b6b`. Links: [[2026-10-17-macos-packages]], [[plan/alkahest-v1-plan]].

## What happened

The v0.1.1 release produced no draft: the `draft release` job was skipped because
`macos (x86_64-apple-darwin)` failed its smoke test in both runs (tag push 39028880082 and
workflow_dispatch 39031245321):

```
ls: target/packager/Alkahest.app/Contents/Resources/engines/llama-cpp: No such file or directory
```

That job was the only one with a `Swatinem/rust-cache` hit. rust-cache prunes files under
`target/` before saving, so the restore brought back `target/bundle/engines/llama-cpp/` as
empty directories. `ensure_engine` only checked `dest.is_dir()`, printed
`bundled engine: reusing ...` and cargo-packager bundled a tree with no files.

## Change

- `xtask/src/bundle.rs`: new `engine_staged` (a `llama-server` or `llama-server.exe` file must
  exist below the engine dir); `ensure_engine` reuses only then, otherwise reinstalls (or, for a
  cross build, refuses as before).
- Test `an_empty_engine_skeleton_is_not_staged`.

## Recovery of v0.1.1

The release workflow builds the tag's tree, and the tag predates the fix; the tag was not moved.
The stale cache `v0-rust-release-x86_64-apple-darwin-*` was deleted and `release` re-dispatched
for `v0.1.1` (run 39051689441), so the job installs the engine from scratch.

## Verification

`cargo test -p xtask --locked`: 54 passed. `cargo clippy -p xtask --all-targets --locked -- -D warnings`: clean.
