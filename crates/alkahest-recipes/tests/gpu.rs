//! Real downloads of GPU engine builds. Opt in with `ALKAHEST_TEST_GPU=1` (and a machine whose
//! recommended variant is a CUDA one); each run downloads 150-250 MB into a temp directory.
#![allow(clippy::unwrap_used)]

use alkahest_recipes::{AcceleratorKind, Catalog, Host, InstallOptions, install};
use tokio_util::sync::CancellationToken;

fn run(exe: &std::path::Path, dir: &std::path::Path, arg: &str) -> String {
    let out = std::process::Command::new(exe)
        .arg(arg)
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

/// Installs the CUDA variant the driver supports and checks that `llama-server` sees the GPU.
#[tokio::test]
async fn recommended_cuda_llama_server_lists_the_gpu() {
    if std::env::var("ALKAHEST_TEST_GPU").as_deref() != Ok("1") {
        return;
    }
    let system = alkahest_sysinfo::detect();
    let host = Host::current();
    let catalog = Catalog::builtin();
    let recipe = catalog.recommended("llama-cpp", &system, &host).unwrap();
    if recipe.accelerator != AcceleratorKind::Cuda {
        eprintln!(
            "recommended variant is {}, not CUDA: skipped",
            recipe.variant
        );
        return;
    }
    let engines = tempfile::tempdir().unwrap();
    let got = install(
        recipe,
        engines.path(),
        &InstallOptions::default(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    let version = run(&got.executable, &got.dir, "--version");
    assert!(version.contains("version"), "{version}");
    let devices = run(&got.executable, &got.dir, "--list-devices");
    assert!(devices.contains("CUDA0"), "{devices}");
}
