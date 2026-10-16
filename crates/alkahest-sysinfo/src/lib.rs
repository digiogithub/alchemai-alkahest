//! Hardware discovery: CPU, RAM and accelerators (NVIDIA through NVML loaded at runtime, AMD,
//! Vulkan, Metal), feeding engine choice and scheduling.
//!
//! Every probe is best effort and never fails: a machine without any GPU library simply
//! reports no accelerator beyond the CPU. All detection goes through the [`HardwareProbe`]
//! trait so that tests (and other crates) can inject fake hardware; [`SystemProbe`] is the real
//! implementation and [`detect`] is the one-call entry point.
//!
//! No GPU library is linked at build time: NVML and the Vulkan loader are opened with
//! `libloading` when present.

mod accelerator;
mod amd;
mod cpu;
mod metal;
mod nvidia;
mod probe;
mod vulkan;

pub use accelerator::Accelerator;
pub use amd::{AmdSysfs, parse_sysfs_u64};
pub use cpu::{CpuInfo, MemoryInfo};
pub use metal::{MetalInfo, parse_sysctl_bytes};
pub use probe::{GpuInfo, GpuVendor, HardwareProbe, SystemInfo, SystemProbe, detect, detect_with};
