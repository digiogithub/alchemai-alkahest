//! The built-in whisper.cpp recipe (ALK-US-0057, ALK-SP-0003.R4): pinned, source-only.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use alkahest_recipes::{Catalog, Host};

#[test]
fn builtin_whisper_recipe_is_pinned_and_built_from_source() {
    let catalog = Catalog::builtin();
    let linux = Host::new("linux", "x86_64");
    let cpu = catalog
        .resolve("whisper-cpp", None, Some("cpu"), None, &linux)
        .unwrap();
    assert_eq!(cpu.tag, "v1.9.5");
    assert_eq!(
        cpu.executable_for(&linux),
        std::path::PathBuf::from("whisper-server")
    );
    let src = cpu.source.as_ref().unwrap();
    assert_eq!(src.rev, "v1.9.5");
    assert_eq!(src.targets, ["whisper-server"]);
    // No upstream binaries: the installer builds it, with no flag needed.
    assert!(cpu.assets.is_empty());
    assert!(cpu.installable_on(&linux));
    assert!(cpu.needs_source_build(&linux));

    // The GPU variants need their toolchain checked before the build starts.
    let cuda = catalog
        .resolve("whisper-cpp", None, Some("cuda12.4"), None, &linux)
        .unwrap();
    assert!(
        cuda.source
            .as_ref()
            .unwrap()
            .tools
            .iter()
            .any(|t| t == "nvcc")
    );
    let vulkan = catalog
        .resolve("whisper-cpp", None, Some("vulkan"), None, &linux)
        .unwrap();
    assert!(
        vulkan
            .source
            .as_ref()
            .unwrap()
            .tools
            .iter()
            .any(|t| t == "glslc")
    );
}

#[test]
fn prebuilt_recipes_do_not_build_from_source_on_their_own() {
    let catalog = Catalog::builtin();
    let linux = Host::new("linux", "x86_64");
    let llama = catalog
        .resolve("llama-cpp", None, Some("cpu"), None, &linux)
        .unwrap();
    assert!(!llama.needs_source_build(&linux));
}
