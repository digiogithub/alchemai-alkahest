# 2026-10-17 `alkahest sysinfo` command

Story [[ALK-US-0006]], task ALK-T-0024. Builds on [[2026-10-17-sysinfo-hardware-detection]].

## What

- `crates/alkahest/src/sysinfo_cmd.rs`: `run`, `render`. Human report: CPU model/arch, cores, key
  flags, RAM total/available, Metal unified memory, GPUs (name, VRAM total/free, driver, CUDA, CC),
  accelerators best first.
- `--json`: serialised `alkahest_sysinfo::SystemInfo` plus `accelerators` (labels, best first) and
  `best_accelerator`.
- `cli.rs`: `Sysinfo` no longer reports an owner story; `main.rs` dispatches it.

## Verification

`cargo test -p alkahest` (unit tests with a fake `SystemInfo`, assert_cmd tests with no GPU
assumptions: only the trailing `cpu` accelerator is asserted), clippy `-D warnings`.
See [[plan/alkahest-v1-plan]].
