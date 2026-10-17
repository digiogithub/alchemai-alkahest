//! Python-package (virtual environment) recipes: the vLLM recipe format and installer
//! (ALK-US-0054, ALK-SP-0003.R3). The installers are fake `uv` / `python3` scripts, so no
//! package is downloaded.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use alkahest_recipes::{
    BuildTools, Catalog, Host, InstallOptions, Recipe, RecipeError, install, list_installed,
    parse_recipes,
};
use alkahest_sysinfo::{CpuInfo, GpuInfo, GpuVendor, MemoryInfo, MetalInfo, SystemInfo};
use tokio_util::sync::CancellationToken;

const RECIPE: &str = r#"
id = "pyeng"
version = "1.2.3"
executable = "bin/pyeng"
[variants.cuda12]
accelerator = "cuda"
cuda = "12.8"
[variants.cuda12.venv]
python = "3.12"
packages = ["pyeng==1.2.3"]
extra_index_urls = ["https://example.org/simple"]
os = ["linux"]
"#;

fn recipe() -> Recipe {
    parse_recipes(RECIPE).unwrap().remove(0)
}

#[cfg(unix)]
fn script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Fake `uv`: `venv <dir>` makes `<dir>/bin/python`; `pip install --python <py> ...` makes the
/// `pyeng` console script next to it. Every call is appended to `uv-args.txt`.
#[cfg(unix)]
fn fake_uv(dir: &Path, fail_install: bool) -> PathBuf {
    let uv = dir.join("uv");
    let install = if fail_install {
        "echo 'ERROR: no matching distribution' >&2; exit 1".to_string()
    } else {
        "bin=$(dirname \"$4\"); printf '#!/bin/sh\\necho pyeng\\n' > \"$bin/pyeng\"; chmod +x \"$bin/pyeng\"".to_string()
    };
    script(
        &uv,
        &format!(
            "echo \"$@\" >> {log}\ncase \"$1\" in\n\
             venv) for a; do d=$a; done; mkdir -p \"$d/bin\"; : > \"$d/bin/python\";;\n\
             pip) {install};;\nesac",
            log = dir.join("uv-args.txt").display()
        ),
    );
    uv
}

/// Fake `python3`: `-m venv <dir>` makes `<dir>/bin/pip`, which creates the console script.
#[cfg(unix)]
fn fake_python(dir: &Path) -> PathBuf {
    let py = dir.join("python3");
    script(
        &py,
        &format!(
            "d=$3; mkdir -p \"$d/bin\"\n\
             printf '#!/bin/sh\\necho \"$@\" >> {log}\\nb=$(dirname \"$0\")\\n\
             printf \"#!/bin/sh\\\\n\" > \"$b/pyeng\"; chmod +x \"$b/pyeng\"\\n' > \"$d/bin/pip\"\n\
             chmod +x \"$d/bin/pip\"",
            log = dir.join("pip-args.txt").display()
        ),
    );
    py
}

fn options(tools: BuildTools) -> InstallOptions {
    InstallOptions {
        host: Some(Host::new("linux", "x86_64")),
        build_tools: tools,
        ..InstallOptions::default()
    }
}

#[cfg(unix)]
#[tokio::test]
async fn venv_install_with_uv_creates_the_environment_in_place() {
    let tools = tempfile::tempdir().unwrap();
    let engines = tempfile::tempdir().unwrap();
    let opts = options(BuildTools {
        uv: fake_uv(tools.path(), false),
        python: PathBuf::from("/nonexistent/python3"),
        ..BuildTools::default()
    });
    let got = install(&recipe(), engines.path(), &opts, &CancellationToken::new())
        .await
        .unwrap();
    assert!(got.dir.ends_with("pyeng/1.2.3-cuda12"));
    assert_eq!(got.executable, got.dir.join("bin/pyeng"));
    assert!(got.executable.is_file());
    assert!(!got.from_source);
    let args = std::fs::read_to_string(tools.path().join("uv-args.txt")).unwrap();
    assert!(args.contains("venv --python 3.12"), "{args}");
    assert!(args.contains("pip install --python"), "{args}");
    assert!(
        args.contains("--extra-index-url https://example.org/simple"),
        "{args}"
    );
    assert!(args.contains("pyeng==1.2.3"), "{args}");
    assert_eq!(list_installed(engines.path()).len(), 1);
    // Idempotent: a complete install is returned as is.
    let again = install(&recipe(), engines.path(), &opts, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(again.dir, got.dir);
}

#[cfg(unix)]
#[tokio::test]
async fn venv_install_falls_back_to_python_and_pip_without_uv() {
    let tools = tempfile::tempdir().unwrap();
    let engines = tempfile::tempdir().unwrap();
    let opts = options(BuildTools {
        uv: PathBuf::from("/nonexistent/uv"),
        python: fake_python(tools.path()),
        ..BuildTools::default()
    });
    let got = install(&recipe(), engines.path(), &opts, &CancellationToken::new())
        .await
        .unwrap();
    assert!(got.executable.is_file());
    let args = std::fs::read_to_string(tools.path().join("pip-args.txt")).unwrap();
    assert!(
        args.contains("install") && args.contains("pyeng==1.2.3"),
        "{args}"
    );
}

#[tokio::test]
async fn venv_install_needs_uv_or_python() {
    let engines = tempfile::tempdir().unwrap();
    let opts = options(BuildTools {
        uv: PathBuf::from("/nonexistent/uv"),
        python: PathBuf::from("/nonexistent/python3"),
        ..BuildTools::default()
    });
    let err = install(&recipe(), engines.path(), &opts, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, RecipeError::BuildToolMissing(_)), "{err}");
    assert!(list_installed(engines.path()).is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn failed_pip_install_reports_the_log_and_leaves_nothing_behind() {
    let tools = tempfile::tempdir().unwrap();
    let engines = tempfile::tempdir().unwrap();
    let opts = options(BuildTools {
        uv: fake_uv(tools.path(), true),
        ..BuildTools::default()
    });
    let err = install(&recipe(), engines.path(), &opts, &CancellationToken::new())
        .await
        .unwrap_err();
    let RecipeError::BuildFailed { step, tail, .. } = &err else {
        panic!("{err}");
    };
    assert_eq!(step, "uv pip install");
    assert!(tail.contains("no matching distribution"), "{tail}");
    assert!(!engines.path().join("pyeng/1.2.3-cuda12").exists());
}

#[tokio::test]
async fn venv_install_refuses_a_platform_the_recipe_excludes() {
    let engines = tempfile::tempdir().unwrap();
    let mut opts = options(BuildTools::default());
    opts.host = Some(Host::new("windows", "x86_64"));
    let err = install(&recipe(), engines.path(), &opts, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, RecipeError::UnsupportedHost { .. }), "{err}");
}

#[test]
fn venv_tables_are_validated() {
    let bad = |venv: &str| {
        let text = format!(
            "id = \"e\"\nversion = \"1\"\nexecutable = \"bin/e\"\n[variants.cpu]\n\
             accelerator = \"cpu\"\n[variants.cpu.venv]\n{venv}\n"
        );
        parse_recipes(&text).unwrap_err().to_string()
    };
    assert!(bad("packages = [\"vllm\"]").contains("pinned"));
    assert!(bad("packages = [\"--pre\"]").contains("pinned"));
    assert!(bad("packages = []").contains("empty"));
    assert!(bad("packages = [\"a==1\"]\nindex_url = \"http://x\"").contains("https"));
}

fn nvidia(cuda: (u32, u32)) -> SystemInfo {
    SystemInfo {
        cpu: CpuInfo {
            model: String::new(),
            physical_cores: 4,
            logical_cores: 8,
            arch: "x86_64".into(),
            flags: vec![],
        },
        memory: MemoryInfo::default(),
        gpus: vec![GpuInfo {
            index: 0,
            vendor: GpuVendor::Nvidia,
            name: "gpu".into(),
            vram_total_bytes: 24 << 30,
            vram_free_bytes: None,
            driver_version: None,
            cuda_version: Some(cuda),
            compute_capability: None,
        }],
        vulkan: false,
        rocm_runtime: false,
        metal: None::<MetalInfo>,
    }
}

#[test]
fn builtin_vllm_recipe_is_pinned_and_follows_the_hardware() {
    let catalog = Catalog::builtin();
    let linux = Host::new("linux", "x86_64");
    let r = catalog
        .resolve("vllm", None, None, Some(&nvidia((12, 8))), &linux)
        .unwrap();
    assert_eq!(r.variant, "cuda12.8");
    assert_eq!(r.tag, "0.11.0");
    assert_eq!(r.venv.as_ref().unwrap().packages, ["vllm==0.11.0"]);
    // A driver older than CUDA 12.8 gets no vLLM build; the installer says so.
    assert!(
        catalog
            .resolve("vllm", None, None, Some(&nvidia((12, 4))), &linux)
            .is_err()
    );
    // No Windows build.
    assert!(!r.installable_on(&Host::new("windows", "x86_64")));
    // Apple Silicon has the experimental metal variant.
    let metal = catalog
        .resolve(
            "vllm",
            None,
            Some("metal"),
            None,
            &Host::new("macos", "aarch64"),
        )
        .unwrap();
    assert!(metal.installable_on(&Host::new("macos", "aarch64")));
    assert!(!metal.installable_on(&Host::new("macos", "x86_64")));
}
