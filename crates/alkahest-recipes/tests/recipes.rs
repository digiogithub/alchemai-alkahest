//! Recipe format, discovery, variant selection and source builds (ALK-US-0049,
//! ALK-SP-0003.R3/R4/R5). Scenario names follow the spec.
#![allow(clippy::unwrap_used)]

use std::path::Path;

use alkahest_recipes::{
    BuildTools, Catalog, Host, InstallOptions, RecipeError, builtin_recipes, install,
    list_installed, parse_recipes, remove_installed, select_variant,
};
use alkahest_sysinfo::{CpuInfo, GpuInfo, GpuVendor, MemoryInfo, MetalInfo, SystemInfo};
use tokio_util::sync::CancellationToken;

const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// A multi-variant recipe for linux/x86_64 and macOS/arm64.
fn recipe_text(version: &str) -> String {
    let asset = |os: &str, arch: &str, name: &str| {
        format!(
            "[[variants.\"{name}\".assets]]\nos = \"{os}\"\narch = \"{arch}\"\n\
             url = \"https://example.org/{name}.tar.gz\"\nsha256 = \"{DIGEST}\"\n\
             format = \"tar.gz\"\n"
        )
    };
    format!(
        "id = \"eng\"\nversion = \"{version}\"\nexecutable = \"srv\"\n\n\
         [variants.cpu]\naccelerator = \"cpu\"\n{}{}\n\
         [variants.\"cuda12.4\"]\naccelerator = \"cuda\"\ncuda = \"12.4\"\n{}\n\
         [variants.\"cuda13.1\"]\naccelerator = \"cuda\"\ncuda = \"13.1\"\n{}\n\
         [variants.vulkan]\naccelerator = \"vulkan\"\n{}\n\
         [variants.rocm]\naccelerator = \"rocm\"\n{}\n\
         [variants.metal]\naccelerator = \"metal\"\n{}",
        asset("linux", "x86_64", "cpu"),
        asset("macos", "aarch64", "cpu"),
        asset("linux", "x86_64", "cuda12.4"),
        asset("linux", "x86_64", "cuda13.1"),
        asset("linux", "x86_64", "vulkan"),
        asset("linux", "x86_64", "rocm"),
        asset("macos", "aarch64", "metal"),
    )
}

fn cpu() -> CpuInfo {
    CpuInfo {
        model: String::new(),
        physical_cores: 4,
        logical_cores: 8,
        arch: "x86_64".into(),
        flags: vec![],
    }
}

fn gpu(vendor: GpuVendor, cuda: Option<(u32, u32)>) -> GpuInfo {
    GpuInfo {
        index: 0,
        vendor,
        name: "gpu".into(),
        vram_total_bytes: 8 << 30,
        vram_free_bytes: None,
        driver_version: None,
        cuda_version: cuda,
        compute_capability: None,
    }
}

fn system(gpus: Vec<GpuInfo>, vulkan: bool, rocm: bool, metal: bool) -> SystemInfo {
    SystemInfo {
        cpu: cpu(),
        memory: MemoryInfo::default(),
        gpus,
        vulkan,
        rocm_runtime: rocm,
        metal: metal.then(|| MetalInfo {
            chip: "M3".into(),
            unified_memory_bytes: 16 << 30,
        }),
    }
}

fn pick(sys: &SystemInfo, host: &Host) -> Option<String> {
    let rs = parse_recipes(&recipe_text("b1")).unwrap();
    let refs: Vec<_> = rs.iter().collect();
    select_variant(&refs, sys, host).map(|r| r.variant.clone())
}

fn linux() -> Host {
    Host::new("linux", "x86_64")
}

#[test]
fn recipe_validation_rejects_a_recipe_without_a_pinned_version() {
    let text = recipe_text("b1").replace("version = \"b1\"\n", "");
    let err = parse_recipes(&text).unwrap_err();
    assert!(matches!(err, RecipeError::Invalid { .. }), "{err}");
    assert!(err.to_string().contains("no pinned version"), "{err}");
}

#[test]
fn recipe_validation_names_the_broken_rule() {
    let bad_digest = recipe_text("b1").replacen(DIGEST, "XYZ", 1);
    assert!(
        parse_recipes(&bad_digest)
            .unwrap_err()
            .to_string()
            .contains("sha256")
    );
    let no_cuda = recipe_text("b1").replace("cuda = \"12.4\"\n", "");
    assert!(
        parse_recipes(&no_cuda)
            .unwrap_err()
            .to_string()
            .contains("cuda")
    );
    let nothing =
        "id = \"e\"\nversion = \"1\"\nexecutable = \"x\"\n[variants.cpu]\naccelerator = \"cpu\"\n";
    assert!(
        parse_recipes(nothing)
            .unwrap_err()
            .to_string()
            .contains("assets")
    );
    let escaping = recipe_text("b1").replace("executable = \"srv\"", "executable = \"../srv\"");
    assert!(parse_recipes(&escaping).is_err());
}

#[test]
fn llama_cpp_recipe_covers_the_published_gpu_builds_with_pinned_digests() {
    let catalog = Catalog::builtin();
    let variants = catalog.variants("llama-cpp", "b11669");
    let names: Vec<&str> = variants.iter().map(|r| r.variant.as_str()).collect();
    for want in [
        "cpu", "cuda12.4", "cuda12.8", "cuda13.4", "vulkan", "rocm", "metal",
    ] {
        assert!(names.contains(&want), "{want} missing from {names:?}");
    }
    for r in &variants {
        assert!(r.source.is_some(), "{} has no source steps", r.variant);
        for a in &r.assets {
            assert!(
                a.sha256.is_some() && !a.tofu,
                "{} {}/{}",
                r.variant,
                a.os,
                a.arch
            );
            assert!(a.size.is_some());
        }
    }
    let host = Host::new("linux", "x86_64");
    let linux_cuda = catalog
        .resolve("llama-cpp", None, Some("cuda12"), None, &host)
        .unwrap();
    assert_eq!(linux_cuda.variant, "cuda12.8");
    assert!(linux_cuda.asset_for(&host).is_some());
}

#[test]
fn builtin_recipes_all_parse_and_have_a_cpu_variant() {
    let all = builtin_recipes();
    assert!(!all.is_empty());
    for id in Catalog::builtin().engine_ids() {
        assert!(
            all.iter().any(|r| r.id == id && r.variant == "cpu"),
            "{id} has no cpu variant"
        );
    }
}

#[test]
fn cuda_pick_chooses_the_variant_the_driver_supports() {
    let host = linux();
    // Driver for CUDA 12.8: cuda12.4 fits, cuda13.1 does not.
    let sys = system(
        vec![gpu(GpuVendor::Nvidia, Some((12, 8)))],
        true,
        false,
        false,
    );
    assert_eq!(pick(&sys, &host).as_deref(), Some("cuda12.4"));
    // Driver for CUDA 13.2: the newest build wins.
    let sys = system(
        vec![gpu(GpuVendor::Nvidia, Some((13, 2)))],
        true,
        false,
        false,
    );
    assert_eq!(pick(&sys, &host).as_deref(), Some("cuda13.1"));
    // Driver too old for any CUDA build: Vulkan, then CPU.
    let sys = system(
        vec![gpu(GpuVendor::Nvidia, Some((11, 8)))],
        true,
        false,
        false,
    );
    assert_eq!(pick(&sys, &host).as_deref(), Some("vulkan"));
    let sys = system(
        vec![gpu(GpuVendor::Nvidia, Some((11, 8)))],
        false,
        false,
        false,
    );
    assert_eq!(pick(&sys, &host).as_deref(), Some("cpu"));
}

#[test]
fn variant_pick_follows_the_hardware_preference_order() {
    let amd = gpu(GpuVendor::Amd, None);
    let sys = system(vec![amd.clone()], true, true, false);
    assert_eq!(pick(&sys, &linux()).as_deref(), Some("rocm"));
    let sys = system(vec![amd], true, false, false);
    assert_eq!(pick(&sys, &linux()).as_deref(), Some("vulkan"));
    assert_eq!(
        pick(&system(vec![], false, false, false), &linux()).as_deref(),
        Some("cpu")
    );
    let mac = Host::new("macos", "aarch64");
    assert_eq!(
        pick(&system(vec![], false, false, true), &mac).as_deref(),
        Some("metal")
    );
}

#[test]
fn two_versions_coexist_and_the_pin_or_newest_is_used() {
    let mut catalog = Catalog::default();
    catalog.extend(parse_recipes(&recipe_text("b100")).unwrap());
    catalog.extend(parse_recipes(&recipe_text("b90")).unwrap());
    assert_eq!(catalog.versions("eng"), vec!["b100", "b90"]);
    let host = linux();
    let newest = catalog
        .resolve("eng", None, Some("cpu"), None, &host)
        .unwrap();
    assert_eq!(newest.tag, "b100");
    let pinned = catalog
        .resolve("eng", Some("b90"), Some("cpu"), None, &host)
        .unwrap();
    assert_eq!(pinned.tag, "b90");
    assert_eq!(
        pinned.install_dir(Path::new("/e")),
        Path::new("/e/eng/b90-cpu")
    );
    assert!(matches!(
        catalog.resolve("eng", Some("b1"), None, None, &host),
        Err(RecipeError::UnknownVersion { .. })
    ));
    assert!(matches!(
        catalog.resolve("eng", None, Some("tpu"), None, &host),
        Err(RecipeError::UnknownVariant { .. })
    ));
    assert!(matches!(
        catalog.resolve("nope", None, None, None, &host),
        Err(RecipeError::UnknownEngine(_))
    ));
}

#[test]
fn cuda_major_alias_resolves_to_the_newest_build_of_that_major() {
    let mut catalog = Catalog::default();
    catalog.extend(parse_recipes(&recipe_text("b1")).unwrap());
    let r = catalog
        .resolve("eng", None, Some("cuda12"), None, &linux())
        .unwrap();
    assert_eq!(r.variant, "cuda12.4");
}

#[test]
fn discovery_layers_directories_over_the_builtin_recipes() {
    let dir = tempfile::tempdir().unwrap();
    // Overrides one variant of a built-in engine's version with a different digest.
    let builtin = Catalog::builtin();
    let first = builtin.recipes().first().unwrap().clone();
    std::fs::write(
        dir.path().join("mine.toml"),
        recipe_text("b7").replace("\"eng\"", &format!("\"{}\"", first.id)),
    )
    .unwrap();
    std::fs::write(dir.path().join("broken.toml"), "id = ").unwrap();
    std::fs::write(dir.path().join("notes.txt"), "ignored").unwrap();
    let (catalog, issues) = builtin.with_dir(dir.path());
    assert_eq!(issues.len(), 1);
    assert!(issues[0].path.ends_with("broken.toml"));
    assert!(catalog.versions(&first.id).contains(&"b7".to_string()));
    let (none, issues) = Catalog::default().with_dir(Path::new("/nonexistent-dir-xyz"));
    assert!(issues.is_empty() && none.recipes().is_empty());
}

/// Fake `git` and `cmake` scripts; `cmake --build` produces `build/bin/srv` plus a library.
#[cfg(unix)]
fn fake_tools(dir: &Path, fail_build: bool) -> BuildTools {
    use std::os::unix::fs::PermissionsExt;
    let git = dir.join("git");
    let cmake = dir.join("cmake");
    let args_file = dir.join("cmake-args.txt");
    std::fs::write(
        &git,
        "#!/bin/sh\nif [ \"$1\" = init ]; then mkdir -p .git; fi\nexit 0\n",
    )
    .unwrap();
    let build_body = if fail_build {
        "echo 'compile error: boom' >&2; exit 3".to_string()
    } else {
        "mkdir -p build/bin; printf '#!/bin/sh\\necho built\\n' > build/bin/srv; \
         chmod +x build/bin/srv; echo lib > build/bin/libx.so"
            .to_string()
    };
    std::fs::write(
        &cmake,
        format!(
            "#!/bin/sh\necho \"$@\" >> {}\ncase \"$1\" in --build) {build_body};; esac\nexit 0\n",
            args_file.display()
        ),
    )
    .unwrap();
    for p in [&git, &cmake] {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    BuildTools { git, cmake }
}

fn source_recipe() -> alkahest_recipes::Recipe {
    let text = "id = \"eng\"\nversion = \"b5\"\nexecutable = \"srv\"\nlibrary_dirs = [\".\"]\n\
                [variants.cuda12]\naccelerator = \"cuda\"\ncuda = \"12.4\"\n\
                [variants.cuda12.source]\ngit = \"https://example.org/eng.git\"\nrev = \"abc123\"\n\
                cmake_flags = [\"-DGGML_CUDA=ON\"]\ntargets = [\"srv\"]\ntools = [\"git\", \"cmake\"]\n";
    parse_recipes(text).unwrap().remove(0)
}

#[cfg(unix)]
#[tokio::test]
async fn source_build_runs_the_steps_logs_and_keeps_only_the_products() {
    let tools_dir = tempfile::tempdir().unwrap();
    let engines = tempfile::tempdir().unwrap();
    let opts = InstallOptions {
        from_source: true,
        build_tools: fake_tools(tools_dir.path(), false),
        ..InstallOptions::default()
    };
    let got = install(
        &source_recipe(),
        engines.path(),
        &opts,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(got.from_source);
    assert!(got.dir.ends_with("eng/b5-cuda12"));
    assert!(got.executable.is_file());
    assert!(got.dir.join("libx.so").is_file());
    assert!(!got.dir.join("src").exists(), "checkout must be removed");
    assert_eq!(got.library_dirs, vec![got.dir.join(".")]);
    let args = std::fs::read_to_string(tools_dir.path().join("cmake-args.txt")).unwrap();
    assert!(args.contains("-DGGML_CUDA=ON"), "{args}");
    assert!(args.contains("--target srv"), "{args}");
    let log = std::fs::read_dir(engines.path().join(".logs"))
        .unwrap()
        .count();
    assert_eq!(log, 1);
    assert_eq!(list_installed(engines.path()).len(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn failed_source_build_reports_the_log_and_installs_nothing() {
    let tools_dir = tempfile::tempdir().unwrap();
    let engines = tempfile::tempdir().unwrap();
    let opts = InstallOptions {
        from_source: true,
        build_tools: fake_tools(tools_dir.path(), true),
        ..InstallOptions::default()
    };
    let err = install(
        &source_recipe(),
        engines.path(),
        &opts,
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    let RecipeError::BuildFailed {
        step, tail, log, ..
    } = &err
    else {
        panic!("{err}");
    };
    assert_eq!(step, "cmake build");
    assert!(tail.contains("boom"), "{tail}");
    assert!(log.is_file());
    assert!(list_installed(engines.path()).is_empty());
    assert!(!engines.path().join("eng/b5-cuda12.partial").exists());
}

#[tokio::test]
async fn source_build_needs_its_tools_and_a_source_table() {
    let engines = tempfile::tempdir().unwrap();
    let mut recipe = source_recipe();
    recipe.source.as_mut().unwrap().tools = vec!["definitely-not-a-tool-xyz".into()];
    let opts = InstallOptions {
        from_source: true,
        ..InstallOptions::default()
    };
    let err = install(&recipe, engines.path(), &opts, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, RecipeError::BuildToolMissing(_)), "{err}");

    recipe.source = None;
    let err = install(&recipe, engines.path(), &opts, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, RecipeError::NoSource { .. }), "{err}");
}

#[cfg(unix)]
#[tokio::test]
async fn remove_deletes_only_marked_installs_of_the_selected_version() {
    let tools_dir = tempfile::tempdir().unwrap();
    let engines = tempfile::tempdir().unwrap();
    let opts = InstallOptions {
        from_source: true,
        build_tools: fake_tools(tools_dir.path(), false),
        ..InstallOptions::default()
    };
    let mut other = source_recipe();
    other.tag = "b6".into();
    for r in [source_recipe(), other] {
        install(&r, engines.path(), &opts, &CancellationToken::new())
            .await
            .unwrap();
    }
    std::fs::create_dir_all(engines.path().join("eng/stray")).unwrap();
    assert_eq!(list_installed(engines.path()).len(), 2);
    let gone = remove_installed(engines.path(), "eng", Some("b5"), None).unwrap();
    assert_eq!(gone.len(), 1);
    assert_eq!(list_installed(engines.path())[0].tag, "b6");
    assert!(engines.path().join("eng/stray").exists());
    assert!(matches!(
        remove_installed(engines.path(), "eng", Some("b5"), None),
        Err(RecipeError::NotInstalled(_))
    ));
}
