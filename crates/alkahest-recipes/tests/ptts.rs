//! The built-in ptts-openai-server recipe (ALK-US-0139, ADR 0008): pinned Cargo source builds.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use alkahest_recipes::{Catalog, Host, parse_recipes};

const COMMIT: &str = "e851445158f07061f063e3edff0080f9426c0da2";

#[test]
fn builtin_ptts_recipe_is_pinned_and_built_with_cargo() {
    let catalog = Catalog::builtin();
    let linux = Host::new("linux", "x86_64");
    let cpu = catalog
        .resolve("ptts-openai-server", None, Some("cpu"), None, &linux)
        .unwrap();
    assert_eq!(cpu.tag, "v0.5.0");
    assert_eq!(cpu.license.as_deref(), Some("MIT OR Apache-2.0"));
    assert_eq!(
        cpu.executable_for(&linux),
        std::path::PathBuf::from("ptts-openai-server")
    );
    let c = cpu.cargo.as_ref().unwrap();
    assert_eq!(c.rev, COMMIT);
    assert_eq!(c.package, "ptts-openai-server");
    assert_eq!(c.bin_name(), "ptts-openai-server");
    assert!(c.features.is_empty());
    assert_eq!(c.tools, ["git", "cargo"]);
    assert!(cpu.source.is_none() && cpu.assets.is_empty());
    assert!(cpu.installable_on(&linux));
    assert!(cpu.needs_source_build(&linux));
    assert!(cpu.has_build_steps());
}

#[test]
fn every_ptts_variant_enables_its_feature() {
    let catalog = Catalog::builtin();
    let linux = Host::new("linux", "x86_64");
    for (variant, feature, tool) in [
        ("metal", "metal", None),
        ("cuda12.4", "cuda", Some("nvcc")),
        ("vulkan", "vulkan", Some("glslc")),
    ] {
        let r = catalog
            .resolve("ptts-openai-server", None, Some(variant), None, &linux)
            .unwrap();
        let c = r.cargo.as_ref().unwrap();
        assert_eq!(c.rev, COMMIT, "{variant}");
        assert_eq!(c.features, [feature], "{variant}");
        if let Some(tool) = tool {
            assert!(c.tools.iter().any(|t| t == tool), "{variant}");
        }
    }
}

#[test]
fn cargo_tables_follow_the_source_pinning_rules() {
    let head = "base = \"llama-cpp\"\n".to_owned()
        + "id = \"x\"\nversion = \"1\"\nexecutable = \"x\"\n[variants.cpu]\naccelerator = \"cpu\"\n";
    let ok = format!(
        "{head}[variants.cpu.cargo]\ngit = \"https://example.org/x.git\"\nrev = \"v1\"\npackage = \"x\"\n"
    );
    let parsed = parse_recipes(&ok).unwrap();
    let c = parsed[0].cargo.as_ref().unwrap();
    assert_eq!(c.tools, ["git", "cargo"]);

    for (from, to) in [
        ("https://example.org/x.git", "http://example.org/x.git"),
        ("rev = \"v1\"", "rev = \"-v1\""),
        ("package = \"x\"", "package = \"x y\""),
    ] {
        assert!(parse_recipes(&ok.replace(from, to)).is_err(), "{to}");
    }
    // A variant must still say how to install.
    assert!(parse_recipes(&head).is_err());
}

/// Runs `program` in `dir` and fails the test when it does not succeed.
fn sh(dir: &std::path::Path, program: &str, args: &[&str]) {
    let status = std::process::Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "{program} {args:?}");
}

#[tokio::test]
async fn cargo_recipe_installs_from_a_local_repository() {
    use alkahest_recipes::{InstallOptions, install};
    if std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipped: git is not installed");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"hello-srv\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("Cargo.lock"),
        "version = 3\n\n[[package]]\nname = \"hello-srv\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("src/main.rs"),
        "fn main() { println!(\"hi\"); }\n",
    )
    .unwrap();
    sh(&repo, "git", &["init", "-q"]);
    sh(&repo, "git", &["add", "-A"]);
    sh(
        &repo,
        "git",
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.org",
            "commit",
            "-q",
            "-m",
            "x",
        ],
    );
    sh(&repo, "git", &["tag", "v1"]);

    let text = "base = \"llama-cpp\"\nid = \"hello-srv\"\nversion = \"v1\"\nexecutable = \"hello-srv\"\n\
                [variants.cpu]\naccelerator = \"cpu\"\n[variants.cpu.cargo]\n\
                git = \"https://example.org/x.git\"\nrev = \"v1\"\npackage = \"hello-srv\"\n";
    let mut recipe = parse_recipes(text).unwrap().remove(0);
    // Validation wants https; the local path is set after parsing.
    recipe.cargo.as_mut().unwrap().git = repo.display().to_string();

    let engines = tmp.path().join("engines");
    let installed = install(
        &recipe,
        &engines,
        &InstallOptions::default(),
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(installed.from_source);
    assert!(installed.executable.is_file());
    assert!(!installed.dir.join("src").exists(), "checkout is deleted");
    let out = std::process::Command::new(&installed.executable)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hi");
}

/// The real thing: builds the pinned xn-ptts server. Needs network, Rust, libopus and libmp3lame.
#[tokio::test]
async fn real_ptts_openai_server_builds_when_opted_in() {
    use alkahest_recipes::{InstallOptions, install};
    if std::env::var("ALKAHEST_TEST_NET").as_deref() != Ok("1") {
        eprintln!("skipped: set ALKAHEST_TEST_NET=1");
        return;
    }
    let host = Host::current();
    let catalog = Catalog::builtin();
    let recipe = catalog
        .resolve("ptts-openai-server", None, Some("cpu"), None, &host)
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let installed = install(
        recipe,
        tmp.path(),
        &InstallOptions::default(),
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(installed.executable.is_file());
}
