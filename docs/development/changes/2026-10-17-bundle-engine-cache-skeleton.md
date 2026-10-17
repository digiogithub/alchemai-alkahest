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
Deleting only the stale macOS x86_64 cache was not enough: run 39051689441 fixed that job, but
the aarch64 macOS and Windows jobs then hit the caches the previous run had saved and failed the
same way. All `v0-rust-release-*` caches were deleted and run 39053828405 passed every job and
created the `Alkahest 0.1.1` draft release. Any tag cut before `05f4b6b` must be rebuilt with
the release caches cleared.

## Verification

`cargo test -p xtask --locked`: 54 passed. `cargo clippy -p xtask --all-targets --locked -- -D warnings`: clean.
