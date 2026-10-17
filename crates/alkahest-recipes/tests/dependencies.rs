//! Recipe dependencies and native per-machine variants (ALK-US-0119, ALK-SP-0011.R4 to R6).
//! Scenario names follow the spec. Downloads go to a local HTTP server with tiny fixtures.
//!
//! The one test that really compiles llama.cpp for the host CPU is opt-in:
//! `ALKAHEST_TEST_BUILD=1` (needs git, cmake, ninja and network access).
#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::path::Path;

use alkahest_recipes::{
    ArchiveFormat, BuildTools, Host, InstallOptions, Recipe, RecipeError, RecipeOrigin, install,
    parse_recipes, parse_recipes_with, remove_installed, select_native, select_variant,
};
use alkahest_sysinfo::{CpuInfo, MemoryInfo, SystemInfo};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FIXTURE: &str = include_str!("fixtures/catalog/engines/llama-cpp.toml");
const BUILTIN_LLAMA: &str = include_str!("../../../recipes/llama-cpp.toml");
const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o755);
    for (name, body) in files {
        w.start_file(*name, opts).unwrap();
        w.write_all(body).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn cpu_only_system(flags: &[&str]) -> SystemInfo {
    SystemInfo {
        cpu: CpuInfo {
            model: String::new(),
            physical_cores: 4,
            logical_cores: 8,
            arch: "x86_64".into(),
            flags: flags.iter().map(|f| f.to_string()).collect(),
        },
        memory: MemoryInfo::default(),
        gpus: vec![],
        vulkan: false,
        rocm_runtime: false,
        metal: None,
    }
}

// ---- ALK-SP-0011.R4 -------------------------------------------------------------------------

#[test]
fn the_catalog_fixture_parses_in_both_origins() {
    for origin in [RecipeOrigin::Local, RecipeOrigin::GitCatalog] {
        let parsed = parse_recipes_with(FIXTURE, origin).unwrap();
        let native = parsed
            .recipes
            .iter()
            .find(|r| r.variant == "cpu-native")
            .unwrap();
        assert!(native.native && native.assets.is_empty() && native.source.is_some());
        let vulkan = parsed
            .recipes
            .iter()
            .find(|r| r.variant == "vulkan")
            .unwrap();
        assert_eq!(vulkan.dependencies[0].id, "vulkan-loader");
        assert_eq!(vulkan.dependencies[0].format, ArchiveFormat::File);
        assert!(!vulkan.dependencies[0].optional);
        let cuda = parsed
            .recipes
            .iter()
            .find(|r| r.variant == "cuda12.8")
            .unwrap();
        assert!(cuda.dependencies[0].optional);
    }
}

#[test]
fn catalog_recipe_equals_a_builtin_recipe() {
    let local = parse_recipes_with(BUILTIN_LLAMA, RecipeOrigin::Local).unwrap();
    let catalog = parse_recipes_with(BUILTIN_LLAMA, RecipeOrigin::GitCatalog).unwrap();
    assert_eq!(local.recipes, catalog.recipes);
    assert_eq!(parse_recipes(BUILTIN_LLAMA).unwrap(), catalog.recipes);
    assert!(catalog.warnings.is_empty(), "{:?}", catalog.warnings);
}

fn asset_recipe(extra_asset: &str) -> String {
    format!(
        "id = \"e\"\nversion = \"1\"\nexecutable = \"srv\"\n[variants.cpu]\naccelerator = \"cpu\"\n\
         [[variants.cpu.assets]]\nos = \"linux\"\narch = \"x86_64\"\nformat = \"zip\"\n{extra_asset}\n"
    )
}

#[test]
fn tofu_refused_in_a_git_catalog() {
    let text = asset_recipe("url = \"https://example.org/x.zip\"\ntofu = true");
    assert!(parse_recipes_with(&text, RecipeOrigin::Local).is_ok());
    let err = parse_recipes_with(&text, RecipeOrigin::GitCatalog)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("sha256 required in git catalogs (tofu is local only)"),
        "{err}"
    );
}

#[test]
fn plain_http_refused() {
    let text = asset_recipe(&format!(
        "url = \"http://example.org/x.tar.gz\"\nsha256 = \"{DIGEST}\""
    ));
    for origin in [RecipeOrigin::Local, RecipeOrigin::GitCatalog] {
        let err = parse_recipes_with(&text, origin).unwrap_err().to_string();
        assert!(err.contains("asset for linux/x86_64"), "{err}");
        assert!(err.contains("url must be https"), "{err}");
    }
}

#[test]
fn a_branch_source_rev_warns_in_a_git_catalog_only() {
    let text = |rev: &str| {
        format!(
            "id = \"e\"\nversion = \"1\"\nexecutable = \"srv\"\n[variants.cpu]\naccelerator = \"cpu\"\n\
             [variants.cpu.source]\ngit = \"https://example.org/e.git\"\nrev = \"{rev}\"\n"
        )
    };
    let warned = parse_recipes_with(&text("main"), RecipeOrigin::GitCatalog).unwrap();
    assert_eq!(warned.warnings.len(), 1);
    assert!(warned.warnings[0].contains("looks like a branch"));
    for pinned in ["b11669", "v1.2.3", "e3de715", "prism-b10777-e3de715"] {
        let ok = parse_recipes_with(&text(pinned), RecipeOrigin::GitCatalog).unwrap();
        assert!(ok.warnings.is_empty(), "{pinned}: {:?}", ok.warnings);
    }
    let local = parse_recipes_with(&text("main"), RecipeOrigin::Local).unwrap();
    assert!(local.warnings.is_empty());
}

// ---- ALK-SP-0011.R5 -------------------------------------------------------------------------

fn dep_recipe(dep_extra: &str) -> String {
    format!(
        "id = \"e\"\nversion = \"1\"\nexecutable = \"srv\"\n[variants.cpu]\naccelerator = \"cpu\"\n\
         [variants.cpu.source]\ngit = \"https://example.org/e.git\"\nrev = \"abc1234\"\n\
         [[variants.cpu.dependencies]]\nid = \"dep\"\nos = \"linux\"\narch = \"x86_64\"\n\
         url = \"https://example.org/d.zip\"\nsha256 = \"{DIGEST}\"\nformat = \"zip\"\n{dep_extra}\n"
    )
}

#[test]
fn dest_escaping_the_install_directory_refused() {
    for dest in [
        "../../bin",
        "/etc",
        "a/../../b",
        "C:/Windows",
        "\\\\x",
        "a\\\\..\\\\..\\\\b",
    ] {
        let err = parse_recipes(&dep_recipe(&format!("dest = \"{dest}\"")))
            .unwrap_err()
            .to_string();
        assert!(err.contains("dependency `dep`"), "{err}");
        assert!(
            err.contains("must be a relative path without `..`"),
            "{err}"
        );
    }
    assert!(parse_recipes(&dep_recipe("dest = \"bin/lib\"")).is_ok());
}

#[test]
fn dependency_rules_are_enforced() {
    let no_sha = dep_recipe("").replace(&format!("sha256 = \"{DIGEST}\"\n"), "");
    assert!(parse_recipes(&no_sha).is_err());
    let http = dep_recipe("").replace("https://example.org/d.zip", "http://example.org/d.zip");
    assert!(
        parse_recipes(&http)
            .unwrap_err()
            .to_string()
            .contains("url must be https")
    );
    let twice = format!(
        "{}[[variants.cpu.dependencies]]\nid = \"dep\"\nos = \"linux\"\narch = \"x86_64\"\n\
         url = \"https://example.org/d.zip\"\nsha256 = \"{DIGEST}\"\nformat = \"zip\"\n",
        dep_recipe("")
    );
    assert!(
        parse_recipes(&twice)
            .unwrap_err()
            .to_string()
            .contains("not unique")
    );
}

struct Fixture {
    server: MockServer,
    recipe: Recipe,
    host: Host,
}

/// A prebuilt `eng` zip plus a required `loader` file dependency, an optional `extra` archive
/// unpacked into `lib/` and an optional `cuda-runtime` archive, all served locally.
async fn fixture() -> Fixture {
    let host = Host::current();
    let srv = format!("srv{}", std::env::consts::EXE_SUFFIX);
    let engine = zip_of(&[(srv.as_str(), b"#!/bin/sh\necho hi\n")]);
    let loader = b"loader-bytes".to_vec();
    let extra = zip_of(&[("extra.bin", b"x")]);
    let cudart = zip_of(&[("cudart.bin", b"c")]);
    let server = MockServer::start().await;
    for (p, body) in [
        ("/engine.zip", &engine),
        ("/loader.dll", &loader),
        ("/extra.zip", &extra),
        ("/cudart.zip", &cudart),
    ] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;
    }
    let text = format!(
        "id = \"eng\"\nversion = \"b1\"\nexecutable = \"srv\"\n[variants.cpu]\naccelerator = \"cpu\"\n\
         [[variants.cpu.assets]]\nos = \"{os}\"\narch = \"{arch}\"\nurl = \"https://example.org/engine.zip\"\n\
         sha256 = \"{es}\"\nformat = \"zip\"\n\
         [[variants.cpu.dependencies]]\nid = \"loader\"\nos = \"{os}\"\narch = \"{arch}\"\n\
         url = \"https://example.org/loader.dll\"\nsha256 = \"{ls}\"\nformat = \"file\"\n\
         [[variants.cpu.dependencies]]\nid = \"extra\"\nos = \"{os}\"\narch = \"{arch}\"\n\
         url = \"https://example.org/extra.zip\"\nsha256 = \"{xs}\"\nformat = \"zip\"\ndest = \"lib\"\noptional = true\n\
         [[variants.cpu.dependencies]]\nid = \"cuda-runtime\"\nos = \"{os}\"\narch = \"{arch}\"\n\
         url = \"https://example.org/cudart.zip\"\nsha256 = \"{cs}\"\nformat = \"zip\"\noptional = true\n\
         [[variants.cpu.dependencies]]\nid = \"other-os\"\nos = \"plan9\"\narch = \"{arch}\"\n\
         url = \"https://example.org/never.zip\"\nsha256 = \"{ls}\"\nformat = \"zip\"\n",
        os = host.os,
        arch = host.arch,
        es = sha(&engine),
        ls = sha(&loader),
        xs = sha(&extra),
        cs = sha(&cudart),
    );
    // Recipes must be https; the test server is plain http, so swap the URLs after parsing.
    let mut recipe = parse_recipes(&text).unwrap().remove(0);
    recipe.assets[0].url = format!("{}/engine.zip", server.uri());
    for d in &mut recipe.dependencies {
        let name = d.url.rsplit('/').next().unwrap().to_string();
        d.url = format!("{}/{name}", server.uri());
    }
    Fixture {
        server,
        recipe,
        host,
    }
}

async fn hits(f: &Fixture, url_path: &str) -> usize {
    f.server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == url_path && r.method == wiremock::http::Method::GET)
        .count()
}

#[tokio::test]
async fn required_dependency_installed_with_the_engine() {
    let f = fixture().await;
    let engines = tempfile::tempdir().unwrap();
    let got = install(
        &f.recipe,
        engines.path(),
        &InstallOptions::default(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(got.dependencies, vec!["loader".to_string()]);
    assert_eq!(
        std::fs::read(got.dir.join("loader.dll")).unwrap(),
        b"loader-bytes"
    );
    assert_eq!(hits(&f, "/extra.zip").await, 0);
    assert_eq!(hits(&f, "/cudart.zip").await, 0);
    assert!(!engines.path().join(".downloads/loader.dll").exists());
    // Removed with the engine: nothing of it is left behind.
    remove_installed(engines.path(), "eng", None, None).unwrap();
    assert!(!got.dir.exists());
}

#[tokio::test]
async fn optional_dependency_only_on_request() {
    let f = fixture().await;
    let engines = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    let got = install(
        &f.recipe,
        engines.path(),
        &InstallOptions::default(),
        &cancel,
    )
    .await
    .unwrap();
    assert!(!got.dir.join("cudart.bin").exists());
    let engine_hits = hits(&f, "/engine.zip").await;

    // `--with-runtime` selects the `cuda-*` optional dependencies, on the installed engine.
    let opts = InstallOptions {
        with_runtime: true,
        ..Default::default()
    };
    let got = install(&f.recipe, engines.path(), &opts, &cancel)
        .await
        .unwrap();
    assert!(got.dir.join("cudart.bin").is_file());
    assert!(!got.dir.join("lib/extra.bin").exists());
    assert_eq!(
        hits(&f, "/engine.zip").await,
        engine_hits,
        "engine refetched"
    );

    // `--with <id>` names any optional dependency; it lands under its `dest`.
    let opts = InstallOptions {
        with: vec!["extra".into()],
        with_runtime: true,
        ..Default::default()
    };
    let got = install(&f.recipe, engines.path(), &opts, &cancel)
        .await
        .unwrap();
    assert!(got.dir.join("lib/extra.bin").is_file());
    assert_eq!(got.dependencies.len(), 3);
    assert_eq!(got.companions, got.dependencies);
    // Idempotent: nothing is downloaded again.
    let (extra_hits, cuda_hits) = (hits(&f, "/extra.zip").await, hits(&f, "/cudart.zip").await);
    assert!(extra_hits > 0 && cuda_hits > 0);
    install(&f.recipe, engines.path(), &opts, &cancel)
        .await
        .unwrap();
    assert_eq!(hits(&f, "/extra.zip").await, extra_hits);
    assert_eq!(hits(&f, "/cudart.zip").await, cuda_hits);
    // A dependency of another platform is never touched.
    assert!(
        f.recipe
            .dependencies_for(&f.host)
            .iter()
            .all(|d| d.id != "other-os")
    );
}

#[tokio::test]
async fn a_dependency_with_a_wrong_digest_fails_the_install_and_leaves_nothing() {
    let mut f = fixture().await;
    f.recipe.dependencies[0].sha256 = "0".repeat(64);
    let engines = tempfile::tempdir().unwrap();
    let opts = InstallOptions {
        fetch: alkahest_fetch::FetchConfig {
            max_retries: 0,
            ..Default::default()
        },
        ..Default::default()
    };
    let err = install(&f.recipe, engines.path(), &opts, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, RecipeError::Fetch { .. }), "{err}");
    assert!(alkahest_recipes::list_installed(engines.path()).is_empty());
    assert!(!f.recipe.install_dir(engines.path()).exists());
}

#[test]
fn companions_are_read_as_optional_dependencies() {
    let host = Host::new("linux", "x86_64");
    let text = format!(
        "id = \"e\"\nversion = \"1\"\nexecutable = \"srv\"\n[variants.cuda12]\naccelerator = \"cuda\"\ncuda = \"12.4\"\n\
         [variants.cuda12.source]\ngit = \"https://example.org/e.git\"\nrev = \"abc1234\"\n\
         [[variants.cuda12.companions]]\nname = \"cuda-runtime\"\nos = \"linux\"\narch = \"x86_64\"\n\
         url = \"https://example.org/c.zip\"\nsha256 = \"{DIGEST}\"\nformat = \"zip\"\n"
    );
    let r = parse_recipes(&text).unwrap().remove(0);
    let deps = r.dependencies_for(&host);
    assert_eq!(deps.len(), 1);
    assert!(deps[0].optional && deps[0].id == "cuda-runtime" && deps[0].dest == ".");
    assert!(r.selected_dependencies(&host, &[], false).is_empty());
    assert_eq!(r.selected_dependencies(&host, &[], true).len(), 1);
    assert_eq!(
        r.selected_dependencies(&host, &["cuda-runtime".into()], false)
            .len(),
        1
    );
}

#[test]
fn built_in_recipes_keep_their_cuda_runtime_as_an_optional_dependency() {
    let host = Host::new("linux", "x86_64");
    let llama = alkahest_recipes::find_recipe("llama-cpp", Some("cuda12.8")).unwrap();
    let deps = llama.dependencies_for(&host);
    assert!(deps.iter().any(|d| d.id == "cuda-runtime" && d.optional));
}

// ---- ALK-SP-0011.R6 -------------------------------------------------------------------------

fn native_text() -> String {
    "id = \"eng\"\nversion = \"b5\"\nexecutable = \"srv\"\nlibrary_dirs = [\".\"]\n\
     [variants.cpu]\naccelerator = \"cpu\"\n\
     [[variants.cpu.assets]]\nos = \"linux\"\narch = \"x86_64\"\nurl = \"https://example.org/e.zip\"\n\
     sha256 = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\nformat = \"zip\"\n\
     [variants.cpu-native]\naccelerator = \"cpu\"\nnative = true\n\
     [variants.cpu-native.source]\ngit = \"https://example.org/eng.git\"\nrev = \"abc123\"\n\
     cmake_flags = [\"-DGGML_NATIVE=ON\"]\ntargets = [\"srv\"]\ntools = [\"git\", \"cmake\"]\n"
        .to_string()
}

#[test]
fn native_variant_never_auto_selected() {
    let rs = parse_recipes(&native_text()).unwrap();
    let refs: Vec<_> = rs.iter().collect();
    let host = Host::new("linux", "x86_64");
    let sys = cpu_only_system(&["avx2"]);
    assert_eq!(select_variant(&refs, &sys, &host).unwrap().variant, "cpu");
    // Even when the portable variant is not installable, the native one is not a fallback.
    let only_native: Vec<_> = refs.iter().copied().filter(|r| r.native).collect();
    assert!(select_variant(&only_native, &sys, &host).is_none());
    assert_eq!(
        select_native(&refs, &sys, &host).unwrap().variant,
        "cpu-native"
    );
    // The built-in catalog follows the same rule.
    let catalog = alkahest_recipes::Catalog::builtin();
    let rec = catalog.recommended("llama-cpp", &sys, &host).unwrap();
    assert!(!rec.native);
    assert!(
        catalog
            .resolve("llama-cpp", None, Some("cpu-native"), Some(&sys), &host)
            .unwrap()
            .native
    );
}

#[test]
fn native_variant_with_assets_refused() {
    let text = native_text().replace(
        "native = true\n",
        "native = true\n[[variants.cpu-native.assets]]\nos = \"linux\"\narch = \"x86_64\"\n\
         url = \"https://example.org/n.zip\"\n\
         sha256 = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\nformat = \"zip\"\n",
    );
    // The `assets` array has to come before the `source` table of the same variant.
    let err = parse_recipes(&text).unwrap_err().to_string();
    assert!(err.contains("a native variant is source-only"), "{err}");
    let no_source = native_text().replace(
        "[variants.cpu-native.source]",
        "[variants.cpu-native.venv]\npackages = [\"a==1\"]\n[variants.cpu-native.x]",
    );
    assert!(parse_recipes(&no_source).is_err());
}

#[test]
fn native_install_marked_stale_on_another_cpu() {
    let engine = |flags: Option<Vec<String>>| alkahest_recipes::InstalledEngine {
        id: "eng".into(),
        variant: "cpu-native".into(),
        tag: "b5".into(),
        dir: "/x".into(),
        executable: "/x/srv".into(),
        library_dirs: vec![],
        from_source: true,
        dependencies: vec![],
        companions: vec![],
        native_cpu_flags: flags,
    };
    let host: Vec<String> = ["avx", "avx2"].map(String::from).to_vec();
    let built = |f: &[&str]| Some(f.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert!(engine(built(&["avx2", "avx512f"])).native_stale(&host));
    assert!(!engine(built(&["avx", "avx2"])).native_stale(&host));
    assert!(!engine(None).native_stale(&host));
}

#[cfg(unix)]
fn fake_tools(dir: &Path) -> BuildTools {
    use std::os::unix::fs::PermissionsExt;
    let git = dir.join("git");
    let cmake = dir.join("cmake");
    std::fs::write(
        &git,
        "#!/bin/sh\nif [ \"$1\" = init ]; then mkdir -p .git; fi\nexit 0\n",
    )
    .unwrap();
    std::fs::write(
        &cmake,
        "#!/bin/sh\ncase \"$1\" in --build) mkdir -p build/bin; \
         printf '#!/bin/sh\\necho built\\n' > build/bin/srv; chmod +x build/bin/srv;; esac\nexit 0\n",
    )
    .unwrap();
    for p in [&git, &cmake] {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    BuildTools {
        git,
        cmake,
        ..BuildTools::default()
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_native_build_records_the_cpu_features_it_was_built_for() {
    let r = parse_recipes(&native_text())
        .unwrap()
        .into_iter()
        .find(|r| r.native)
        .unwrap();
    let tools = tempfile::tempdir().unwrap();
    let engines = tempfile::tempdir().unwrap();
    let opts = InstallOptions {
        build_tools: fake_tools(tools.path()),
        cpu_flags: Some(vec!["avx2".into(), "avx512f".into()]),
        ..Default::default()
    };
    let got = install(&r, engines.path(), &opts, &CancellationToken::new())
        .await
        .unwrap();
    assert!(got.from_source);
    assert_eq!(
        got.native_cpu_flags,
        Some(vec!["avx2".to_string(), "avx512f".to_string()])
    );
    // Persisted: a fresh scan of the engines directory sees the same features.
    let listed = alkahest_recipes::list_installed(engines.path());
    assert_eq!(listed[0].native_cpu_flags, got.native_cpu_flags);
    assert!(listed[0].native_stale(&["avx2".to_string()]));
}

/// Really compiles llama.cpp with `-DGGML_NATIVE=ON`; opt-in (`ALKAHEST_TEST_BUILD=1`).
#[tokio::test]
async fn real_native_llama_cpp_build() {
    if std::env::var("ALKAHEST_TEST_BUILD").as_deref() != Ok("1") {
        eprintln!("skipped: set ALKAHEST_TEST_BUILD=1 (needs git, cmake, ninja, network)");
        return;
    }
    let r = alkahest_recipes::find_recipe("llama-cpp", Some("cpu-native")).unwrap();
    let engines = tempfile::tempdir().unwrap();
    let got = install(
        &r,
        engines.path(),
        &InstallOptions::default(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(got.executable.is_file());
    assert!(got.native_cpu_flags.is_some());
}
