# 2026-10-17 Binary size report

Story [[ALK-US-0002]], task ALK-T-0008. See [[plan/alkahest-v1-plan]].

- `scripts/binary-size.sh`: builds `alkahest` release, `--no-default-features --locked`, prints
  the size and exits 1 above `ALKAHEST_SIZE_BUDGET_MIB` (default 60).
- `scripts/ci-local.sh`: new optional `--size` flag runs it; `--no-deny` still works (options are
  now parsed in a loop).
- CI workflow: new `size` job after `lint`.

Verified by running the script locally.
