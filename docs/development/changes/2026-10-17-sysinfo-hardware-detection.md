# Hardware detection (alkahest-sysinfo)

Implements [[ALK-US-0006]]. Context: [[plan/alkahest-v1-plan]].

## What

Crate `alkahest-sysinfo`: CPU (model, cores, ISA flags), RAM (sysinfo crate), NVIDIA via NVML
opened with `libloading` (no link-time dependency; absence yields no GPUs), AMD via
`/sys/class/drm/card*/device`, Vulkan loader probe, Metal stub (macOS aarch64, `sysctl`).
`SystemInfo::accelerators()` returns `Accelerator` values (cpu, cuda{major.minor}, rocm,
vulkan, metal) best first.

## Files and symbols

- `src/probe.rs`: `HardwareProbe` trait, `SystemProbe`, `SystemInfo`, `GpuInfo`, `GpuVendor`,
  `detect`, `detect_with`.
- `src/nvidia.rs` (local `#[allow(unsafe_code)]` with SAFETY comments), `src/amd.rs`
  (`AmdSysfs`), `src/vulkan.rs`, `src/metal.rs`, `src/cpu.rs`, `src/accelerator.rs`.
- `tests/detect.rs`: fake-probe scenarios; real NVIDIA assertions only with
  `ALKAHEST_TEST_GPU=1`.

## Verification

Unit and integration tests pass with and without `ALKAHEST_TEST_GPU=1` (the dev machine has an
NVIDIA GPU); clippy `-D warnings` clean. The `alkahest sysinfo` CLI command (ALK-T-0024) is
left to the binary owner; it only needs `alkahest_sysinfo::detect()` and serde JSON.
