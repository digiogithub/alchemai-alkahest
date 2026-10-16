//! `alkahest sysinfo`: reports CPU, memory and accelerators from `alkahest-sysinfo`.

use std::io::{self, Write};
use std::process::ExitCode;

use alkahest_sysinfo::{SystemInfo, detect};
use anyhow::Result;
use serde_json::{Value, json};

/// CPU flags worth showing in the human report (the rest are in `--json`).
const KEY_FLAGS: [&str; 6] = ["avx2", "avx512f", "avx512vnni", "fma", "neon", "i8mm"];

/// Runs the command against the real hardware.
pub fn run(json: bool) -> Result<ExitCode> {
    let info = detect();
    let mut out = io::stdout().lock();
    render(&info, json, &mut out)?;
    Ok(ExitCode::SUCCESS)
}

/// Renders `info` as JSON or as a human report.
pub fn render<W: Write>(info: &SystemInfo, json: bool, out: &mut W) -> Result<()> {
    if json {
        serde_json::to_writer_pretty(&mut *out, &to_json(info))?;
        writeln!(out)?;
    } else {
        write_human(info, out)?;
    }
    Ok(())
}

/// The `SystemInfo` document plus the ranked accelerator labels, best first.
fn to_json(info: &SystemInfo) -> Value {
    let mut doc = serde_json::to_value(info).unwrap_or(Value::Null);
    let labels: Vec<String> = info.accelerators().iter().map(|a| a.label()).collect();
    if let Value::Object(map) = &mut doc {
        map.insert("accelerators".into(), json!(labels));
        map.insert(
            "best_accelerator".into(),
            json!(info.best_accelerator().label()),
        );
    }
    doc
}

/// Binary-unit size, one decimal.
fn fmt_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Human report.
fn write_human<W: Write>(info: &SystemInfo, out: &mut W) -> io::Result<()> {
    let cpu = &info.cpu;
    let model = if cpu.model.is_empty() {
        "unknown"
    } else {
        &cpu.model
    };
    writeln!(out, "CPU:    {model} ({})", cpu.arch)?;
    writeln!(
        out,
        "Cores:  {} physical, {} logical",
        cpu.physical_cores, cpu.logical_cores
    )?;
    let flags: Vec<&str> = KEY_FLAGS.into_iter().filter(|f| cpu.has_flag(f)).collect();
    writeln!(
        out,
        "Flags:  {}",
        if flags.is_empty() {
            "-".to_string()
        } else {
            flags.join(" ")
        }
    )?;
    writeln!(
        out,
        "RAM:    {} total, {} available",
        fmt_bytes(info.memory.total_bytes),
        fmt_bytes(info.memory.available_bytes)
    )?;
    if let Some(m) = &info.metal {
        writeln!(
            out,
            "Metal:  unified memory {}",
            fmt_bytes(m.unified_memory_bytes)
        )?;
    }
    if info.gpus.is_empty() {
        writeln!(out, "GPUs:   none detected")?;
    } else {
        writeln!(out, "GPUs:")?;
        for g in &info.gpus {
            let free = g.vram_free_bytes.map_or("unknown".to_string(), fmt_bytes);
            write!(
                out,
                "  [{}] {}  VRAM {} total, {} free",
                g.index,
                g.name,
                fmt_bytes(g.vram_total_bytes),
                free
            )?;
            if let Some(d) = &g.driver_version {
                write!(out, "  driver {d}")?;
            }
            if let Some((a, b)) = g.cuda_version {
                write!(out, "  CUDA {a}.{b}")?;
            }
            if let Some((a, b)) = g.compute_capability {
                write!(out, "  CC {a}.{b}")?;
            }
            writeln!(out)?;
        }
    }
    let accel: Vec<String> = info.accelerators().iter().map(|a| a.label()).collect();
    writeln!(out, "Accelerators (best first): {}", accel.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alkahest_sysinfo::{CpuInfo, GpuInfo, GpuVendor, MemoryInfo};

    fn fake() -> SystemInfo {
        SystemInfo {
            cpu: CpuInfo {
                model: "Test CPU".into(),
                physical_cores: 4,
                logical_cores: 8,
                arch: "x86_64".into(),
                flags: vec!["avx2".into(), "sse4.2".into()],
            },
            memory: MemoryInfo {
                total_bytes: 16 << 30,
                available_bytes: 8 << 30,
            },
            gpus: vec![GpuInfo {
                index: 0,
                vendor: GpuVendor::Nvidia,
                name: "RTX Test".into(),
                vram_total_bytes: 24 << 30,
                vram_free_bytes: Some(20 << 30),
                driver_version: Some("550.1".into()),
                cuda_version: Some((12, 4)),
                compute_capability: Some((8, 9)),
            }],
            vulkan: false,
            rocm_runtime: false,
            metal: None,
        }
    }

    #[test]
    fn human_report_lists_gpu_and_ranked_accelerators() {
        let mut buf = Vec::new();
        render(&fake(), false, &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains("Test CPU"));
        assert!(text.contains("24.0 GiB total, 20.0 GiB free"));
        assert!(text.contains("CUDA 12.4  CC 8.9"));
        assert!(text.contains("cuda12.4, cpu"));
    }

    #[test]
    fn json_report_has_accelerators_best_first() {
        let mut buf = Vec::new();
        render(&fake(), true, &mut buf).unwrap();
        let v: Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(v["accelerators"], json!(["cuda12.4", "cpu"]));
        assert_eq!(v["best_accelerator"], "cuda12.4");
        assert_eq!(v["gpus"][0]["name"], "RTX Test");
    }
}
