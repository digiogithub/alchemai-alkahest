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

/// `nvmlProcessInfo_t` (the v2 layout used by `nvmlDeviceGetComputeRunningProcesses_v3`).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NvmlProcessInfo {
    pid: c_uint,
    used_gpu_memory: u64,
    gpu_instance_id: c_uint,
    compute_instance_id: c_uint,
}

/// `NVML_VALUE_NOT_AVAILABLE`: the driver does not report this process's memory (containers,
/// WSL, missing permissions).
const NVML_VALUE_NOT_AVAILABLE: u64 = u64::MAX;
/// `NVML_ERROR_INSUFFICIENT_SIZE`: the process array was too small; the count is updated.
const NVML_ERROR_INSUFFICIENT_SIZE: c_int = 7;

type ProcessesFn = unsafe extern "C" fn(*mut c_void, *mut c_uint, *mut NvmlProcessInfo) -> c_int;

/// A loaded, initialised NVML that stays open, for cheap repeated memory queries.
///
/// [`detect`] opens and closes NVML on every call, which is right for a one-off hardware
/// report but too heavy for the scheduler's per-load memory snapshot. A session keeps the
/// library and the device handles, and calls `nvmlShutdown` when dropped.
pub(crate) struct Session {
    shutdown: ShutdownFn,
    memory_fn: MemoryFn,
    processes_fn: Option<ProcessesFn>,
    /// Device handles (`nvmlDevice_t`), kept as integers so the session is `Send + Sync`.
    devices: Vec<usize>,
    /// Keeps the symbols above valid; dropped after `Drop::drop` has called `nvmlShutdown`.
    _lib: libloading::Library,
}

impl Session {
    /// Opens NVML, or `None` when it is absent, fails to initialise or lists no device.
    #[allow(unsafe_code)]
    pub(crate) fn open() -> Option<Self> {
        // SAFETY: loading NVML runs its initialisers, which are safe in any process.
        let lib = LIBRARY_NAMES
            .iter()
            .find_map(|n| unsafe { libloading::Library::new(*n).ok() })?;
        // SAFETY: every symbol type matches nvml.h; the copied function pointers stay valid
        // because `lib` is stored in the session and outlives them.
        unsafe {
            let init: InitFn = *lib.get::<InitFn>(b"nvmlInit_v2\0").ok()?;
            let shutdown: ShutdownFn = *lib.get::<ShutdownFn>(b"nvmlShutdown\0").ok()?;
            let count_fn: CountFn = *lib.get::<CountFn>(b"nvmlDeviceGetCount_v2\0").ok()?;
            let handle_fn: HandleFn = *lib
                .get::<HandleFn>(b"nvmlDeviceGetHandleByIndex_v2\0")
                .ok()?;
            let memory_fn: MemoryFn = *lib.get::<MemoryFn>(b"nvmlDeviceGetMemoryInfo\0").ok()?;
            let processes_fn: Option<ProcessesFn> = lib
                .get::<ProcessesFn>(b"nvmlDeviceGetComputeRunningProcesses_v3\0")
                .ok()
                .map(|s| *s);
            if init() != 0 {
                return None;
            }
            let mut count: c_uint = 0;
            let mut devices = Vec::new();
            if count_fn(&mut count) == 0 {
                for i in 0..count {
                    let mut handle: *mut c_void = std::ptr::null_mut();
                    if handle_fn(i, &mut handle) == 0 && !handle.is_null() {
                        devices.push(handle as usize);
                    }
                }
            }
            if devices.is_empty() {
                let _ = shutdown();
                return None;
            }
            Some(Self {
                shutdown,
                memory_fn,
                processes_fn,
                devices,
                _lib: lib,
            })
        }
    }

    /// `(total, free)` bytes of every device, in index order; a device that fails to answer
    /// is skipped.
    #[allow(unsafe_code)]
    pub(crate) fn memory(&self) -> Vec<(u64, u64)> {
        self.devices
            .iter()
            .filter_map(|&h| {
                let mut mem = NvmlMemory::default();
                // SAFETY: `h` is a handle NVML returned and the session keeps NVML initialised;
                // `mem` matches `nvmlMemory_t`.
                (unsafe { (self.memory_fn)(h as *mut c_void, &mut mem) } == 0)
                    .then_some((mem.total, mem.free))
            })
            .collect()
    }

    /// VRAM the compute process `pid` holds, summed over devices; `None` when the process is
    /// on no device, the driver does not report per-process memory, or NVML lacks the call.
    #[allow(unsafe_code)]
    pub(crate) fn process_vram(&self, pid: u32) -> Option<u64> {
        let query = self.processes_fn?;
        let mut total: Option<u64> = None;
        for &h in &self.devices {
            // The process list can grow between the size query and the fetch: retry with the
            // count NVML reports, a few times.
            let mut capacity: c_uint = 0;
            let mut buf: Vec<NvmlProcessInfo> = Vec::new();
            for _ in 0..4 {
                let mut count = capacity;
                // SAFETY: `buf` holds `capacity` elements (a null-like empty buffer when 0, in
                // which case NVML only writes the needed count); the struct matches
                // `nvmlProcessInfo_t`.
                let rc = unsafe { query(h as *mut c_void, &mut count, buf.as_mut_ptr()) };
                if rc == 0 {
                    buf.truncate(count as usize);
                    break;
                }
                if rc != NVML_ERROR_INSUFFICIENT_SIZE {
                    buf.clear();
                    break;
                }
                capacity = count.saturating_add(8);
                buf = vec![NvmlProcessInfo::default(); capacity as usize];
            }
            for p in buf.iter().take(capacity as usize).filter(|p| p.pid == pid) {
                if p.used_gpu_memory != NVML_VALUE_NOT_AVAILABLE {
                    total = Some(total.unwrap_or(0).saturating_add(p.used_gpu_memory));
                }
            }
        }
        total
    }
}

impl Drop for Session {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: NVML was initialised in `open` and no query runs after this point.
        let _ = unsafe { (self.shutdown)() };
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
