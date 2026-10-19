//! Fork engines (`base`), the CUDA native variant and `--cuda-arch` (ALK-US-0132).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use alkahest_recipes::{
    Catalog, Host, InstallOptions, RecipeOrigin, list_installed, parse_recipes, parse_recipes_with,
};
use alkahest_sysinfo::{Accelerator, CpuInfo, GpuInfo, GpuVendor, MemoryInfo, SystemInfo};

/// A machine with one NVIDIA GPU whose driver supports `cuda`, or none.
fn machine(cuda: Option<(u32, u32)>) -> SystemInfo {
    SystemInfo {
        cpu: CpuInfo {
            model: String::new(),
            physical_cores: 4,
            logical_cores: 8,
            arch: "x86_64".into(),
            flags: vec![],
        },
        memory: MemoryInfo::default(),
        gpus: cuda
            .map(|c| GpuInfo {
                index: 0,
                vendor: GpuVendor::Nvidia,
                name: "gpu".into(),
                vram_total_bytes: 24 << 30,
                vram_free_bytes: None,
                driver_version: None,
                cuda_version: Some(c),
                compute_capability: None,
            })
            .into_iter()
            .collect(),
        vulkan: false,
        rocm_runtime: false,
        metal: None,
    }
}

/// A source-only recipe with the given header lines.
fn fork(header: &str) -> String {
    format!(
        "{header}\nversion = \"b1\"\nexecutable = \"llama-server\"\n\
         [variants.cpu]\naccelerator = \"cpu\"\n\
         [variants.cpu.source]\ngit = \"https://example.org/fork.git\"\nrev = \"b1\"\n\
         cmake_flags = [\"-DGGML_NATIVE=OFF\"]\n\
         [variants.cuda-native]\naccelerator = \"cuda\"\nnative = true\n\
         [variants.cuda-native.source]\ngit = \"https://example.org/fork.git\"\nrev = \"b1\"\n\
         cmake_flags = [\"-DGGML_CUDA=ON\", \"-DCMAKE_CUDA_ARCHITECTURES=native\"]\n"
    )
}

fn invalid_reason(text: &str) -> String {
    parse_recipes(text).unwrap_err().to_string()
}

#[test]
fn a_fork_recipe_declares_its_base_family() {
    let rs = parse_recipes(&fork("id = \"llama-cpp-turboquant\"\nbase = \"llama-cpp\"")).unwrap();
    assert_eq!(rs.len(), 2);
    assert!(
        rs.iter()
            .all(|r| r.base.as_deref() == Some("llama-cpp") && r.id == "llama-cpp-turboquant")
    );
    // The install directory is the fork's own: <engines_dir>/<id>/<version>-<variant>/.
    let cpu = rs.iter().find(|r| r.variant == "cpu").unwrap();
    assert_eq!(
        cpu.install_dir(std::path::Path::new("/e")),
        std::path::Path::new("/e/llama-cpp-turboquant/b1-cpu")
    );
    // Git catalogs apply the same rule.
    assert!(
        parse_recipes_with(
            &fork("id = \"llama-cpp-turboquant\"\nbase = \"llama-cpp\""),
            RecipeOrigin::GitCatalog
        )
        .is_ok()
    );
}

#[test]
fn an_unknown_id_without_base_is_rejected() {
    let why = invalid_reason(&fork("id = \"turbo\""));
    assert!(why.contains("no `base`"), "{why}");
    assert!(why.contains("base = \"llama-cpp\""), "{why}");
    let why = parse_recipes_with(&fork("id = \"turbo\""), RecipeOrigin::GitCatalog)
        .unwrap_err()
        .to_string();
    assert!(why.contains("no `base`"), "{why}");
}

#[test]
fn base_must_be_a_known_family() {
    let why = invalid_reason(&fork("id = \"turbo\"\nbase = \"ollama\""));
    assert!(why.contains("not a built-in engine family"), "{why}");
    assert!(why.contains("llama-cpp"), "{why}");
    let why = invalid_reason(&fork("id = \"turbo\"\nbase = \"llama.cpp\""));
    assert!(why.contains("canonical id `llama-cpp`"), "{why}");
}

#[test]
fn a_fork_may_not_reuse_a_builtin_id_or_alias() {
    let why = invalid_reason(&fork("id = \"llama-cpp\"\nbase = \"llama-cpp\""));
    assert!(why.contains("cannot have a `base`"), "{why}");
    for alias in ["prism", "llama", "sd", "whisper"] {
        let why = invalid_reason(&fork(&format!("id = \"{alias}\"\nbase = \"llama-cpp\"")));
        assert!(why.contains("built-in engine"), "{alias}: {why}");
        let why = invalid_reason(&fork(&format!("id = \"{alias}\"")));
        assert!(why.contains("alias"), "{alias}: {why}");
    }
    let why = invalid_reason(&fork("id = \"Turbo\"\nbase = \"llama-cpp\""));
    assert!(why.contains("lower-case"), "{why}");
}

#[test]
fn builtin_recipes_have_no_base() {
    let c = Catalog::builtin();
    assert!(c.recipes().iter().all(|r| r.base.is_none()));
    assert!(c.forks().is_empty());
}

#[test]
fn catalog_reports_forks_and_their_family() {
    let mut c = Catalog::builtin();
    c.extend(parse_recipes(&fork("id = \"llama-cpp-turboquant\"\nbase = \"llama-cpp\"")).unwrap());
    assert_eq!(c.fork_base("llama-cpp-turboquant"), Some("llama-cpp"));
    assert_eq!(c.fork_base("llama-cpp"), None);
    assert_eq!(
        c.forks(),
        vec![("llama-cpp-turboquant".to_string(), "llama-cpp".to_string())]
    );
}

#[test]
fn llama_cpp_has_a_source_only_cuda_native_variant() {
    let c = Catalog::builtin();
    let host = Host::new("linux", "x86_64");
    let r = c
        .resolve("llama-cpp", None, Some("cuda-native"), None, &host)
        .unwrap();
    assert!(r.native);
    assert!(r.assets.is_empty());
    assert_eq!(r.cuda, None);
    let src = r.source.as_ref().unwrap();
    assert!(src.cmake_flags.iter().any(|f| f == "-DGGML_CUDA=ON"));
    assert!(
        src.cmake_flags
            .iter()
            .any(|f| f == "-DCMAKE_CUDA_ARCHITECTURES=native")
    );
    // Same flags as the other llama.cpp source builds, minus the portable-CPU ones.
    let cuda = c
        .resolve("llama-cpp", None, Some("cuda12.8"), None, &host)
        .unwrap();
    for f in &cuda.source.as_ref().unwrap().cmake_flags {
        assert!(src.cmake_flags.contains(f), "cuda-native lacks {f}");
    }
    // Never picked automatically, even on a CUDA machine.
    let sys = machine(Some((13, 4)));
    let auto = c
        .resolve("llama-cpp", None, None, Some(&sys), &host)
        .unwrap();
    assert!(!auto.native);
}

#[test]
fn native_picks_cuda_native_on_a_cuda_host_and_cpu_native_elsewhere() {
    let c = Catalog::builtin();
    let host = Host::new("linux", "x86_64");
    let cuda_host = machine(Some((12, 4)));
    assert!(matches!(
        cuda_host.best_accelerator(),
        Accelerator::Cuda { .. }
    ));
    let r = c
        .resolve_native("llama-cpp", None, &cuda_host, &host)
        .unwrap();
    assert_eq!(r.variant, "cuda-native");
    let cpu_host = machine(None);
    let r = c
        .resolve_native("llama-cpp", None, &cpu_host, &host)
        .unwrap();
    assert_eq!(r.variant, "cpu-native");
}

#[test]
fn cuda_arch_replaces_native_in_source_builds() {
    let c = Catalog::builtin();
    let host = Host::new("linux", "x86_64");
    let r = c
        .resolve("llama-cpp", None, Some("cuda-native"), None, &host)
        .unwrap();
    let src = r.source.as_ref().unwrap();
    let built = src.with_cuda_architectures("86;89").unwrap();
    let arch: Vec<&String> = built
        .cmake_flags
        .iter()
        .filter(|f| f.starts_with("-DCMAKE_CUDA_ARCHITECTURES="))
        .collect();
    assert_eq!(arch, ["-DCMAKE_CUDA_ARCHITECTURES=86;89"]);
    assert_eq!(built.cmake_flags.len(), src.cmake_flags.len());
    // Commas are accepted as separators.
    let built = src.with_cuda_architectures("89,90a").unwrap();
    assert!(
        built
            .cmake_flags
            .contains(&"-DCMAKE_CUDA_ARCHITECTURES=89;90a".to_string())
    );
    // A CUDA build without the flag gets it appended.
    let cuda = c
        .resolve("llama-cpp", None, Some("cuda12.8"), None, &host)
        .unwrap();
    let csrc = cuda.source.as_ref().unwrap();
    let built = csrc.with_cuda_architectures("89").unwrap();
    assert_eq!(
        built.cmake_flags.last().map(String::as_str),
        Some("-DCMAKE_CUDA_ARCHITECTURES=89")
    );
    // Not a CUDA build, or not an architecture list.
    let cpu = c
        .resolve("llama-cpp", None, Some("cpu"), None, &host)
        .unwrap();
    assert!(
        cpu.source
            .as_ref()
            .unwrap()
            .with_cuda_architectures("89")
            .is_err()
    );
    assert!(src.with_cuda_architectures("89 -DEVIL=1").is_err());
    assert!(src.with_cuda_architectures("").is_err());
    assert!(src.with_cuda_architectures(";").is_err());
}

#[test]
fn a_native_cuda_variant_needs_no_cuda_version_but_others_do() {
    let missing = "id = \"llama-cpp-x\"\nbase = \"llama-cpp\"\nversion = \"b1\"\n\
                   executable = \"llama-server\"\n[variants.cuda]\naccelerator = \"cuda\"\n\
                   [variants.cuda.source]\ngit = \"https://example.org/x.git\"\nrev = \"b1\"\n";
    let why = invalid_reason(missing);
    assert!(
        why.contains("cuda = \\\"12.4\\\"") || why.contains("needs `cuda"),
        "{why}"
    );
}

#[test]
fn an_install_of_a_fork_records_its_base() {
    // A marker written by an install (simulated) carries `base`; list_installed returns it.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("llama-cpp-turboquant").join("b1-cpu");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("llama-server"), b"").unwrap();
    std::fs::write(
        root.join(alkahest_recipes::MARKER_FILE),
        r#"{"id":"llama-cpp-turboquant","base":"llama-cpp","variant":"cpu","tag":"b1",
            "executable":"llama-server","source":"git x@b1","sha256":""}"#,
    )
    .unwrap();
    let found = list_installed(dir.path());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].base.as_deref(), Some("llama-cpp"));
    let _ = InstallOptions::default();
}
