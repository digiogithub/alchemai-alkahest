//! The accelerator summary used by recipes and engine selection.

use serde::{Deserialize, Serialize};

/// A compute backend the machine can run an engine on.
///
/// Recipes pick a prebuilt engine variant from this; the scheduler picks a device list from
/// [`crate::SystemInfo::gpus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Accelerator {
    /// No usable GPU: CPU inference only. Always present in a summary.
    Cpu,
    /// NVIDIA CUDA, with the highest CUDA version the installed driver supports.
    Cuda {
        /// CUDA major version (12 for CUDA 12.4).
        major: u32,
        /// CUDA minor version (4 for CUDA 12.4).
        minor: u32,
    },
    /// AMD ROCm / HIP.
    Rocm,
    /// Any Vulkan 1.x capable device through the system loader.
    Vulkan,
    /// Apple Metal (Apple Silicon unified memory).
    Metal,
}

impl Accelerator {
    /// Stable short label used in recipe names and CLI output (`cuda12.4`, `rocm`, `cpu`).
    pub fn label(&self) -> String {
        match self {
            Accelerator::Cpu => "cpu".to_string(),
            Accelerator::Cuda { major, minor } => format!("cuda{major}.{minor}"),
            Accelerator::Rocm => "rocm".to_string(),
            Accelerator::Vulkan => "vulkan".to_string(),
            Accelerator::Metal => "metal".to_string(),
        }
    }

    /// Whether this backend offloads to a GPU.
    pub fn is_gpu(&self) -> bool {
        !matches!(self, Accelerator::Cpu)
    }
}
