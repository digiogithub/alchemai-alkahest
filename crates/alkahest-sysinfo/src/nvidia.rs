//! NVIDIA detection through NVML, loaded at runtime (no link-time dependency).

use std::ffi::{c_char, c_int, c_uint, c_void};

use crate::probe::{GpuInfo, GpuVendor};

/// NVML shared library names to try, in order.
const LIBRARY_NAMES: &[&str] = &[
    "libnvidia-ml.so.1",
    "nvml.dll",
    "C:\\Program Files\\NVIDIA Corporation\\NVSMI\\nvml.dll",
];

/// `nvmlMemory_t`.
#[repr(C)]
#[derive(Default)]
struct NvmlMemory {
    total: u64,
    free: u64,
    used: u64,
}

type InitFn = unsafe extern "C" fn() -> c_int;
type ShutdownFn = unsafe extern "C" fn() -> c_int;
type DriverVersionFn = unsafe extern "C" fn(*mut c_char, c_uint) -> c_int;
type CudaDriverVersionFn = unsafe extern "C" fn(*mut c_int) -> c_int;
type CountFn = unsafe extern "C" fn(*mut c_uint) -> c_int;
type HandleFn = unsafe extern "C" fn(c_uint, *mut *mut c_void) -> c_int;
type NameFn = unsafe extern "C" fn(*mut c_void, *mut c_char, c_uint) -> c_int;
type MemoryFn = unsafe extern "C" fn(*mut c_void, *mut NvmlMemory) -> c_int;
type CapabilityFn = unsafe extern "C" fn(*mut c_void, *mut c_int, *mut c_int) -> c_int;

/// Splits NVML's packed CUDA driver version (`12040`) into `(12, 4)`.
pub(crate) fn split_cuda_version(packed: i32) -> (u32, u32) {
    let p = packed.max(0) as u32;
    (p / 1000, (p % 1000) / 10)
}

/// Converts a NUL-terminated C buffer to a `String`.
fn c_buf_to_string(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).trim().to_string()
}

/// Lists NVIDIA GPUs, or an empty list when NVML is absent or fails to initialise.
pub(crate) fn detect() -> Vec<GpuInfo> {
    query().unwrap_or_default()
}

/// Runs the NVML query sequence. Returns `None` on any failure so the caller degrades to
/// "no NVIDIA GPU".
#[allow(unsafe_code)]
fn query() -> Option<Vec<GpuInfo>> {
    // SAFETY: loading NVML runs its initialisers, which are safe in any process.
    let lib = LIBRARY_NAMES
        .iter()
        .find_map(|n| unsafe { libloading::Library::new(*n).ok() })?;
    // SAFETY: every symbol type below matches the NVML C API (nvml.h); buffers are sized as
    // NVML documents (96 bytes for names, 80 for the driver version); `lib` outlives every use
    // of the resolved symbols; `nvmlShutdown` is called after the last query.
    unsafe {
        let init: libloading::Symbol<InitFn> = lib.get(b"nvmlInit_v2\0").ok()?;
        let shutdown: libloading::Symbol<ShutdownFn> = lib.get(b"nvmlShutdown\0").ok()?;
        let count_fn: libloading::Symbol<CountFn> = lib.get(b"nvmlDeviceGetCount_v2\0").ok()?;
        let handle_fn: libloading::Symbol<HandleFn> =
            lib.get(b"nvmlDeviceGetHandleByIndex_v2\0").ok()?;
        let name_fn: libloading::Symbol<NameFn> = lib.get(b"nvmlDeviceGetName\0").ok()?;
        let mem_fn: libloading::Symbol<MemoryFn> = lib.get(b"nvmlDeviceGetMemoryInfo\0").ok()?;
        let drv_fn: Option<libloading::Symbol<DriverVersionFn>> =
            lib.get(b"nvmlSystemGetDriverVersion\0").ok();
        let cuda_fn: Option<libloading::Symbol<CudaDriverVersionFn>> =
            lib.get(b"nvmlSystemGetCudaDriverVersion\0").ok();
        let cap_fn: Option<libloading::Symbol<CapabilityFn>> =
            lib.get(b"nvmlDeviceGetCudaComputeCapability\0").ok();

        if init() != 0 {
            return None;
        }
        let result = (|| {
            let driver_version = drv_fn.as_ref().and_then(|f| {
                let mut buf = [0u8; 80];
                (f(buf.as_mut_ptr().cast(), buf.len() as c_uint) == 0)
                    .then(|| c_buf_to_string(&buf))
            });
            let cuda_version = cuda_fn.as_ref().and_then(|f| {
                let mut v: c_int = 0;
                (f(&mut v) == 0).then(|| split_cuda_version(v))
            });
            let mut count: c_uint = 0;
            if count_fn(&mut count) != 0 {
                return None;
            }
            let mut gpus = Vec::new();
            for i in 0..count {
                let mut handle: *mut c_void = std::ptr::null_mut();
                if handle_fn(i, &mut handle) != 0 {
                    continue;
                }
                let mut name_buf = [0u8; 96];
                let name = if name_fn(handle, name_buf.as_mut_ptr().cast(), 96) == 0 {
                    c_buf_to_string(&name_buf)
                } else {
                    "NVIDIA GPU".to_string()
                };
                let mut mem = NvmlMemory::default();
                if mem_fn(handle, &mut mem) != 0 {
                    continue;
                }
                let compute_capability = cap_fn.as_ref().and_then(|f| {
                    let (mut maj, mut min): (c_int, c_int) = (0, 0);
                    (f(handle, &mut maj, &mut min) == 0).then_some((maj as u32, min as u32))
                });
                gpus.push(GpuInfo {
                    index: i,
                    vendor: GpuVendor::Nvidia,
                    name,
                    vram_total_bytes: mem.total,
                    vram_free_bytes: Some(mem.free),
                    driver_version: driver_version.clone(),
                    cuda_version,
                    compute_capability,
                });
            }
            Some(gpus)
        })();
        let _ = shutdown();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuda_version_is_split() {
        assert_eq!(split_cuda_version(12040), (12, 4));
        assert_eq!(split_cuda_version(11080), (11, 8));
        assert_eq!(split_cuda_version(-1), (0, 0));
    }
}
