//! AMD GPU detection through Linux sysfs (`/sys/class/drm/card*/device`).

use std::path::{Path, PathBuf};

use crate::probe::{GpuInfo, GpuVendor};

/// PCI vendor id of AMD/ATI.
const AMD_VENDOR_ID: u64 = 0x1002;

/// Reader of the amdgpu sysfs interface, rooted at a configurable directory for tests.
#[derive(Debug, Clone)]
pub struct AmdSysfs {
    drm_root: PathBuf,
}

impl Default for AmdSysfs {
    fn default() -> Self {
        Self::new("/sys/class/drm")
    }
}

impl AmdSysfs {
    /// Creates a reader over the given `drm` directory (`/sys/class/drm` in production).
    pub fn new(drm_root: impl Into<PathBuf>) -> Self {
        Self {
            drm_root: drm_root.into(),
        }
    }

    /// Lists AMD GPUs with VRAM totals. Missing directories yield an empty list.
    pub fn gpus(&self) -> Vec<GpuInfo> {
        let Ok(entries) = std::fs::read_dir(&self.drm_root) else {
            return Vec::new();
        };
        let mut cards: Vec<(u32, PathBuf)> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                // `card0`, not `card0-DP-1` connector entries.
                let idx = name.strip_prefix("card")?.parse::<u32>().ok()?;
                Some((idx, e.path().join("device")))
            })
            .collect();
        cards.sort();
        cards
            .into_iter()
            .filter_map(|(idx, dev)| read_card(idx, &dev))
            .collect()
    }
}

fn read_card(index: u32, dev: &Path) -> Option<GpuInfo> {
    let vendor = parse_sysfs_u64(&std::fs::read_to_string(dev.join("vendor")).ok()?)?;
    if vendor != AMD_VENDOR_ID {
        return None;
    }
    let total = std::fs::read_to_string(dev.join("mem_info_vram_total"))
        .ok()
        .and_then(|s| parse_sysfs_u64(&s))?;
    let used = std::fs::read_to_string(dev.join("mem_info_vram_used"))
        .ok()
        .and_then(|s| parse_sysfs_u64(&s));
    let name = std::fs::read_to_string(dev.join("product_name"))
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::fs::read_to_string(dev.join("device"))
                .ok()
                .map(|d| format!("AMD GPU {}", d.trim()))
        })
        .unwrap_or_else(|| "AMD GPU".to_string());
    Some(GpuInfo {
        index,
        vendor: GpuVendor::Amd,
        name,
        vram_total_bytes: total,
        vram_free_bytes: used.map(|u| total.saturating_sub(u)),
        driver_version: None,
        cuda_version: None,
        compute_capability: None,
    })
}

/// Parses a sysfs integer, decimal or `0x` hexadecimal, with surrounding whitespace.
pub fn parse_sysfs_u64(text: &str) -> Option<u64> {
    let t = text.trim();
    match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => t.parse().ok(),
    }
}

/// Whether a ROCm/HIP runtime is installed (`/opt/rocm` or the HIP runtime library).
pub(crate) fn rocm_runtime_present() -> bool {
    if Path::new("/opt/rocm").exists() {
        return true;
    }
    ["libamdhip64.so", "libamdhip64.so.6", "libamdhip64.so.7"]
        .iter()
        .any(|n| open_lib(n))
}

#[allow(unsafe_code)]
fn open_lib(name: &str) -> bool {
    // SAFETY: the HIP runtime's initialisers are safe to run in any process; the handle is
    // dropped immediately and no symbol is resolved.
    unsafe { libloading::Library::new(name).is_ok() }
}
