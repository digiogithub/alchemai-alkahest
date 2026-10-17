//! Hardware detection scenarios: fake probes by default, real GPU only with
//! `ALKAHEST_TEST_GPU=1`.

use alkahest_sysinfo::*;
use std::fs;

#[derive(Default)]
struct Fake {
    nvidia: Vec<GpuInfo>,
    amd: Vec<GpuInfo>,
    rocm: bool,
    vulkan: bool,
    metal: Option<MetalInfo>,
}

impl HardwareProbe for Fake {
    fn cpu_and_memory(&self) -> (CpuInfo, MemoryInfo) {
        (
            CpuInfo {
                model: "Fake CPU".into(),
                physical_cores: 4,
                logical_cores: 8,
                arch: "x86_64".into(),
                flags: vec!["avx2".into()],
            },
            MemoryInfo {
                total_bytes: 32 << 30,
                available_bytes: 20 << 30,
            },
        )
    }
    fn nvidia(&self) -> Vec<GpuInfo> {
        self.nvidia.clone()
    }
    fn amd(&self) -> Vec<GpuInfo> {
        self.amd.clone()
    }
    fn rocm_runtime_present(&self) -> bool {
        self.rocm
    }
    fn vulkan_present(&self) -> bool {
        self.vulkan
    }
    fn metal(&self) -> Option<MetalInfo> {
        self.metal.clone()
    }
}

fn nv(cuda: (u32, u32), free: u64) -> GpuInfo {
    GpuInfo {
        index: 0,
        vendor: GpuVendor::Nvidia,
        name: "RTX".into(),
        vram_total_bytes: 24 << 30,
        vram_free_bytes: Some(free),
        driver_version: Some("550.1".into()),
        cuda_version: Some(cuda),
        compute_capability: Some((8, 9)),
    }
}

#[test]
fn works_without_any_gpu_library() {
    let info = detect_with(&Fake::default());
    assert_eq!(info.accelerators(), vec![Accelerator::Cpu]);
    assert_eq!(info.best_accelerator().label(), "cpu");
    assert_eq!(info.free_accelerator_memory_bytes(), None);
}

#[test]
fn cuda_preferred_over_vulkan_and_labelled() {
    let info = detect_with(&Fake {
        nvidia: vec![nv((12, 4), 10 << 30)],
        vulkan: true,
        ..Default::default()
    });
    assert_eq!(
        info.accelerators(),
        vec![
            Accelerator::Cuda {
                major: 12,
                minor: 4
            },
            Accelerator::Vulkan,
            Accelerator::Cpu
        ]
    );
    assert_eq!(info.best_accelerator().label(), "cuda12.4");
    assert_eq!(info.free_accelerator_memory_bytes(), Some(10 << 30));
}

#[test]
fn rocm_requires_runtime() {
    let amd = GpuInfo {
        vendor: GpuVendor::Amd,
        vram_free_bytes: None,
        cuda_version: None,
        compute_capability: None,
        ..nv((0, 0), 0)
    };
    let without = detect_with(&Fake {
        amd: vec![amd.clone()],
        vulkan: true,
        ..Default::default()
    });
    assert_eq!(without.best_accelerator(), Accelerator::Vulkan);
    let with = detect_with(&Fake {
        amd: vec![amd],
        rocm: true,
        ..Default::default()
    });
    assert_eq!(with.best_accelerator(), Accelerator::Rocm);
}

#[test]
fn metal_wins_and_reports_unified_memory() {
    let info = detect_with(&Fake {
        metal: Some(MetalInfo {
            chip: "Apple M3".into(),
            unified_memory_bytes: 36 << 30,
        }),
        ..Default::default()
    });
    assert_eq!(info.best_accelerator(), Accelerator::Metal);
    assert_eq!(info.free_accelerator_memory_bytes(), Some(36 << 30));
}

#[test]
fn amd_sysfs_reads_vram_and_skips_other_vendors() {
    let root = tempfile::tempdir().unwrap();
    let mk = |card: &str, vendor: &str, total: &str| {
        let dev = root.path().join(card).join("device");
        fs::create_dir_all(&dev).unwrap();
        fs::write(dev.join("vendor"), format!("{vendor}\n")).unwrap();
        fs::write(dev.join("mem_info_vram_total"), format!("{total}\n")).unwrap();
        fs::write(dev.join("mem_info_vram_used"), "1073741824\n").unwrap();
        fs::write(dev.join("device"), "0x744c\n").unwrap();
    };
    mk("card0", "0x10de", "8589934592");
    mk("card1", "0x1002", "25769803776");
    fs::create_dir_all(root.path().join("card1-DP-1")).unwrap();
    let gpus = AmdSysfs::new(root.path()).gpus();
    assert_eq!(gpus.len(), 1);
    assert_eq!(gpus[0].index, 1);
    assert_eq!(gpus[0].vram_total_bytes, 24 << 30);
    assert_eq!(gpus[0].vram_free_bytes, Some(23 << 30));
    assert_eq!(gpus[0].name, "AMD GPU 0x744c");
    assert!(AmdSysfs::new(root.path().join("missing")).gpus().is_empty());
}

#[test]
fn parsers() {
    assert_eq!(parse_sysfs_u64("0x1002\n"), Some(0x1002));
    assert_eq!(parse_sysfs_u64(" 42 "), Some(42));
    assert_eq!(parse_sysfs_u64("x"), None);
    assert_eq!(parse_sysctl_bytes("17179869184\n"), Some(16 << 30));
}

#[test]
fn system_info_round_trips_json() {
    let info = detect_with(&Fake {
        nvidia: vec![nv((12, 4), 1)],
        ..Default::default()
    });
    let json = serde_json::to_string(&info).unwrap();
    let back: SystemInfo = serde_json::from_str(&json).unwrap();
    assert_eq!(back, info);
}

#[test]
fn real_probe_never_panics_and_reports_cpu_and_ram() {
    let info = detect();
    assert!(info.cpu.logical_cores >= 1);
    assert!(info.memory.total_bytes > 0);
    assert!(info.accelerators().contains(&Accelerator::Cpu));
}

#[test]
fn real_nvidia_gpu_when_enabled() {
    if std::env::var("ALKAHEST_TEST_GPU").as_deref() != Ok("1") {
        return;
    }
    let info = detect();
    let gpu = info
        .gpus
        .iter()
        .find(|g| g.vendor == GpuVendor::Nvidia)
        .expect("an NVIDIA GPU");
    assert!(gpu.vram_total_bytes > 0);
    assert!(gpu.cuda_version.is_some());
    assert!(matches!(info.best_accelerator(), Accelerator::Cuda { .. }));
}

#[test]
fn memory_monitor_reads_ram_without_a_full_detection() {
    let monitor = MemoryMonitor::new();
    let a = monitor.snapshot();
    assert!(a.total_ram > 0 && a.available_ram > 0 && a.available_ram <= a.total_ram);
    // Repeated snapshots reuse the open NVML session and stay consistent.
    let b = monitor.snapshot();
    assert_eq!(a.total_ram, b.total_ram);
    assert_eq!(a.total_vram, b.total_vram);
    assert_eq!(a.gpu_count, b.gpu_count);
    // A process that holds no GPU memory (this test) has no per-process VRAM.
    assert_eq!(monitor.process_vram(std::process::id()), None);
}

#[test]
fn real_memory_monitor_sees_the_gpu_when_enabled() {
    if std::env::var("ALKAHEST_TEST_GPU").as_deref() != Ok("1") {
        return;
    }
    let monitor = MemoryMonitor::new();
    let m = monitor.snapshot();
    assert!(m.gpu_count > 0 && m.total_vram > 0 && m.free_vram <= m.total_vram);
    // Same device list as the full detection.
    let info = detect();
    let nvidia_total: u64 = info
        .gpus
        .iter()
        .filter(|g| g.vendor == GpuVendor::Nvidia)
        .map(|g| g.vram_total_bytes)
        .sum();
    assert!(m.total_vram >= nvidia_total);
}
