//! Metal / Apple Silicon unified memory detection.

use serde::{Deserialize, Serialize};

/// Apple Silicon GPU description. The GPU shares system RAM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetalInfo {
    /// Chip name (`Apple M3 Max`), empty when unknown.
    pub chip: String,
    /// Unified memory size in bytes.
    pub unified_memory_bytes: u64,
}

/// Parses the output of `sysctl -n hw.memsize` (a decimal byte count, possibly with newline).
pub fn parse_sysctl_bytes(output: &str) -> Option<u64> {
    output.trim().parse().ok()
}

/// Detects Apple Silicon. Always `None` outside macOS on ARM.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub(crate) fn detect() -> Option<MetalInfo> {
    let sysctl = |key: &str| -> Option<String> {
        let out = std::process::Command::new("sysctl")
            .args(["-n", key])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let unified_memory_bytes = parse_sysctl_bytes(&sysctl("hw.memsize")?)?;
    Some(MetalInfo {
        chip: sysctl("machdep.cpu.brand_string").unwrap_or_default(),
        unified_memory_bytes,
    })
}

/// Non-Apple targets have no Metal device.
#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
pub(crate) fn detect() -> Option<MetalInfo> {
    None
}
