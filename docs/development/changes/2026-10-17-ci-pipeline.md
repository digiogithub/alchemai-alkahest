# CI pipeline (ALK-US-0002)

## What

- `.github/workflows/ci.yml`: jobs `lint` (fmt, clippy `-D warnings`, `cargo xtask check-deps`,
  `cargo deny`, typos), `test` (matrix ubuntu/macOS/windows: headless build
  `--no-default-features`, `cargo nextest --profile ci`, JUnit artifact
  `target/nextest/ci/junit.xml`), `machete`, and `desktop` (`--features desktop`, off until the
  repository variable `DESKTOP_CI` is `true`; waits for ALK-US-0064).
- Toolchain comes from `rust-toolchain.toml` (`rustup show active-toolchain || rustup toolchain install`);
  `Swatinem/rust-cache` on every build job.
- `scripts/ci-local.sh`: the same verification block locally (one command for the orchestrator).

## Verification

`actionlint .github/workflows/ci.yml` clean; `scripts/ci-local.sh` passes (fmt, clippy, nextest ci
profile, deny, check-deps, headless build, typos).

## Deviations

Remote is assumed to be GitHub (placeholder repository URL). The binary-size report of
ALK-T-0008 is not included.

Links: [[ALK-US-0002]], [[plan/alkahest-v1-plan]].
