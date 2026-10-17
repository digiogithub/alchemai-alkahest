//! The built-in stable-diffusion.cpp recipe (ALK-US-0059, ALK-SP-0003.R4): pinned release assets
//! with digests, source builds with submodules.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use alkahest_recipes::{Catalog, Host};

#[test]
fn builtin_recipe_pins_the_release_assets() {
    let catalog = Catalog::builtin();
    let linux = Host::new("linux", "x86_64");
    let cpu = catalog
        .resolve("stable-diffusion-cpp", None, Some("cpu"), None, &linux)
        .unwrap();
    assert_eq!(cpu.tag, "master-970-2f016e8");
    let asset = cpu.asset_for(&linux).unwrap();
    assert!(asset.url.contains("/releases/download/master-970-2f016e8/"));
    assert_eq!(asset.sha256.as_deref().map(str::len), Some(64));
    // The archive is flat: no wrapper directory to strip.
    assert_eq!(asset.strip_components, 0);
    assert!(!cpu.needs_source_build(&linux));
    assert_eq!(
        cpu.executable_for(&linux),
        std::path::PathBuf::from("sd-server")
    );

    // Every variant can be built from source, and the repository needs its submodules.
    for v in ["cpu", "cuda12.4", "vulkan", "rocm", "metal"] {
        let r = catalog
            .resolve("stable-diffusion-cpp", None, Some(v), None, &linux)
            .unwrap();
        let src = r.source.as_ref().unwrap();
        assert!(src.submodules, "{v}");
        assert_eq!(src.targets, ["sd-server"], "{v}");
    }
}

#[test]
fn linux_cuda_is_a_source_build_and_windows_cuda_is_prebuilt() {
    let catalog = Catalog::builtin();
    let cuda = |host: &Host| {
        catalog
            .resolve("stable-diffusion-cpp", None, Some("cuda12.4"), None, host)
            .unwrap()
            .clone()
    };
    let linux = Host::new("linux", "x86_64");
    let windows = Host::new("windows", "x86_64");
    assert!(cuda(&linux).needs_source_build(&linux));
    assert!(
        cuda(&linux)
            .source
            .as_ref()
            .unwrap()
            .tools
            .iter()
            .any(|t| t == "nvcc")
    );
    assert!(!cuda(&windows).needs_source_build(&windows));
}
