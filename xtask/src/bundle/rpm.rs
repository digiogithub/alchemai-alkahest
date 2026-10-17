//! `.rpm` packages (ALK-US-0105). cargo-packager has no rpm target, so the packages are built
//! here with the pure-Rust `rpm` crate (MIT or Apache-2.0): no `rpmbuild`, no spec file, and so
//! the same code runs on every CI image and on a developer machine.
//!
//! The layout is the one the `.deb` gets from cargo-packager, so documentation and support are
//! shared: the binary in `/usr/bin`, everything else under `/usr/lib/alkahest/` (the directory
//! `alkahest_recipes::bundled_roots` looks at: `engines/`, `licenses/`, `systemd/`, `NOTICE`,
//! `THIRD_PARTY_LICENSES`), the licence under `/usr/share/licenses/<package>/`, and for the
//! desktop flavour a menu entry and the hicolor icons. Like the `.deb`, a package carries no
//! scriptlets and creates no user or service (ALK-SP-0009.R2): `sudo alkahest service install`
//! does that on request.
//!
//! Two packages, because the desktop build links GPUI and needs Vulkan and X11/Wayland
//! libraries a server does not: `alkahest` (desktop, CLI and server) and `alkahest-headless`
//! (CLI and server, no GUI libraries). Each conflicts with the other: both own `/usr/bin/alkahest`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};
use rpm::{BuildConfig, CompressionType, Dependency, FileOptions, PackageBuilder};
use serde_json::Value;

/// Which package to build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavour {
    /// `alkahest-headless`: CLI and server, no GUI libraries.
    Headless,
    /// `alkahest`: the desktop build.
    Desktop,
}

impl Flavour {
    /// The rpm package name.
    fn package_name(self) -> &'static str {
        match self {
            Flavour::Headless => "alkahest-headless",
            Flavour::Desktop => "alkahest",
        }
    }

    /// The package this one cannot be installed next to.
    fn conflicts_with(self) -> &'static str {
        match self {
            Flavour::Headless => "alkahest",
            Flavour::Desktop => "alkahest-headless",
        }
    }
}

/// Runtime libraries of the desktop build, as Fedora, RHEL and openSUSE name them in `Requires`
/// (the `.deb` list in `Cargo.toml` is the Debian spelling of the same set). GPUI renders through
/// Vulkan, so the loader is mandatory; a driver is only recommended because `mesa-vulkan-drivers`
/// is not the right one on NVIDIA machines.
const DESKTOP_REQUIRES: &[&str] = &[
    "vulkan-loader",
    "libxkbcommon-x11",
    "libxkbcommon",
    "fontconfig",
    "libwayland-client",
    "alsa-lib",
];

/// Everything the builder needs besides the flavour.
#[derive(Debug, Clone)]
pub struct Context {
    /// Workspace root.
    pub root: PathBuf,
    /// `target/bundle`, holding the staged `engines/`.
    pub bundle_dir: PathBuf,
    /// Where the package is written.
    pub out_dir: PathBuf,
    /// Workspace version (semver, possibly with a pre-release suffix).
    pub version: String,
    /// Target triple; decides the rpm architecture.
    pub triple: String,
    /// `SOURCE_DATE_EPOCH`, for reproducible builds.
    pub source_date: Option<u64>,
    /// `[package.metadata.packager]` of `crates/alkahest/Cargo.toml`, kebab-case as written.
    pub packager: Value,
}

/// The rpm architecture of a Linux target triple.
fn rpm_arch(triple: &str) -> Result<&'static str> {
    match triple.split('-').next() {
        Some("x86_64") => Ok("x86_64"),
        Some("aarch64") => Ok("aarch64"),
        other => bail!("no rpm architecture for {other:?} (target {triple})"),
    }
}

/// Splits a semver into the rpm `Version` and `Release`. RPM forbids `-` in a version and sorts a
/// pre-release after the final release it precedes unless it gets a release below the final
/// one's `1`, so `1.2.0-rc.1` becomes version `1.2.0`, release `0.rc.1` (the Fedora convention
/// for pre-releases).
fn split_version(semver: &str) -> (String, String) {
    match semver.split_once('-') {
        Some((base, pre)) => (
            base.to_owned(),
            format!("0.{}", pre.replace(['-', '+'], ".")),
        ),
        None => (
            semver.split('+').next().unwrap_or(semver).to_owned(),
            "1".to_owned(),
        ),
    }
}

/// Fills the few `{{...}}` placeholders of `packaging/linux/alkahest.desktop`, the template
/// cargo-packager renders for the `.deb`, so the menu entry is identical: `{{categories}}`,
/// `{{comment}}`, `{{exec}}` and `{{icon}}`; the `{{#if comment}}` block is kept when a comment
/// is given and dropped otherwise.
fn render_desktop_entry(
    template: &str,
    categories: &str,
    comment: Option<&str>,
    exec: &str,
    icon: &str,
) -> String {
    let mut out = String::new();
    let mut in_comment_block = false;
    for line in template.lines() {
        let t = line.trim();
        if t == "{{#if comment}}" {
            in_comment_block = true;
            continue;
        }
        if t == "{{/if}}" {
            in_comment_block = false;
            continue;
        }
        if in_comment_block && comment.is_none() {
            continue;
        }
        out.push_str(
            &line
                .replace("{{categories}}", categories)
                .replace("{{comment}}", comment.unwrap_or_default())
                .replace("{{exec}}", exec)
                .replace("{{icon}}", icon),
        );
        out.push('\n');
    }
    out
}

/// The pixel size encoded in an icon file name such as `alkahest-128.png`.
fn icon_size(file_name: &str) -> Option<u32> {
    file_name
        .strip_suffix(".png")?
        .rsplit('-')
        .next()?
        .parse()
        .ok()
}

fn str_field<'a>(packager: &'a Value, key: &str) -> Result<&'a str> {
    packager[key]
        .as_str()
        .ok_or_else(|| anyhow!("[package.metadata.packager] has no `{key}`"))
}

/// Builds the rpm for `flavour` from `bin` and returns its path.
pub fn build(ctx: &Context, flavour: Flavour, bin: &Path) -> Result<PathBuf> {
    let p = &ctx.packager;
    let arch = rpm_arch(&ctx.triple)?;
    let (version, release) = split_version(&ctx.version);
    let name = flavour.package_name();
    let lib = "/usr/lib/alkahest";

    // The long description names the desktop app; the headless package says what it lacks.
    let long = str_field(p, "long-description")?;
    let base = long.split(" This package contains").next().unwrap_or(long);
    let description = match flavour {
        Flavour::Desktop => long.to_owned(),
        Flavour::Headless => format!(
            "{base} This package contains the CLI, the server and a CPU llama.cpp engine, \
             without the desktop app and its graphics libraries."
        ),
    };

    let mut config = BuildConfig::v4().compression(CompressionType::Gzip);
    if let Some(t) = ctx.source_date {
        config = config.source_date(u32::try_from(t).context("SOURCE_DATE_EPOCH exceeds u32")?);
    }
    let mut b = PackageBuilder::new(name, &version, "MIT", arch, str_field(p, "description")?);
    b.using_config(config)
        .release(release)
        .description(description)
        .url(str_field(p, "homepage")?)
        .vendor(str_field(p, "publisher")?)
        .packager(
            p["authors"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(Value::as_str)
                .unwrap_or("Digio"),
        )
        // A fixed host keeps the header identical between builds on different machines.
        .build_host("alkahest-build")
        .conflicts(Dependency::any(flavour.conflicts_with()));

    b.with_file(
        bin,
        FileOptions::new("/usr/bin/alkahest").permissions(0o755),
    )?;
    b.with_dir_entry(FileOptions::dir(lib))?;
    b.with_dir(
        ctx.bundle_dir.join("engines"),
        format!("{lib}/engines"),
        |o| o,
    )?;
    b.with_dir(
        ctx.root.join("packaging/licenses"),
        format!("{lib}/licenses"),
        |o| o,
    )?;
    b.with_dir_entry(FileOptions::dir(format!("{lib}/systemd")))?;
    for (src, dst) in [
        (
            "packaging/systemd/alkahest.service",
            "systemd/alkahest.service",
        ),
        (
            "packaging/systemd/alkahest.user.service",
            "systemd/alkahest.user.service",
        ),
        ("NOTICE", "NOTICE"),
        ("THIRD_PARTY_LICENSES", "THIRD_PARTY_LICENSES"),
    ] {
        b.with_file(
            ctx.root.join(src),
            FileOptions::new(format!("{lib}/{dst}")).permissions(0o644),
        )?;
    }
    b.with_dir_entry(FileOptions::dir(format!("/usr/share/licenses/{name}")))?;
    b.with_file(
        ctx.root.join("LICENSE"),
        FileOptions::new(format!("/usr/share/licenses/{name}/LICENSE"))
            .permissions(0o644)
            .license(),
    )?;

    if flavour == Flavour::Desktop {
        for dep in DESKTOP_REQUIRES {
            b.requires(Dependency::any(*dep));
        }
        b.recommends(Dependency::any("mesa-vulkan-drivers"));
        let template = fs::read_to_string(ctx.root.join("packaging/linux/alkahest.desktop"))
            .context("reading packaging/linux/alkahest.desktop")?;
        let entry = render_desktop_entry(
            &template,
            "Utility;",
            p["description"].as_str(),
            "alkahest",
            "alkahest",
        );
        b.with_file_contents(
            entry,
            FileOptions::new("/usr/share/applications/alkahest.desktop").permissions(0o644),
        )?;
        let app_dir = ctx.root.join("crates/alkahest");
        for icon in p["icons"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            let path = app_dir.join(icon);
            let file = path
                .file_name()
                .and_then(|f| f.to_str())
                .ok_or_else(|| anyhow!("bad icon path {icon}"))?;
            let Some(px) = icon_size(file) else { continue };
            b.with_file(
                &path,
                FileOptions::new(format!(
                    "/usr/share/icons/hicolor/{px}x{px}/apps/alkahest.png"
                ))
                .permissions(0o644),
            )?;
        }
    }

    let pkg = b.build().map_err(|e| anyhow!("building the rpm: {e}"))?;
    fs::create_dir_all(&ctx.out_dir)?;
    pkg.write_to(&ctx.out_dir)
        .map_err(|e| anyhow!("writing the rpm: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
    }

    fn packager() -> Value {
        let text = fs::read_to_string(repo_root().join("crates/alkahest/Cargo.toml")).unwrap();
        let manifest: toml::Value = toml::from_str(&text).unwrap();
        serde_json::to_value(&manifest["package"]["metadata"]["packager"]).unwrap()
    }

    /// A context with a tiny engine tree, in `tmp`.
    fn context(tmp: &Path, version: &str) -> (Context, PathBuf) {
        let bundle = tmp.join("bundle");
        let eng = bundle.join("engines/llama-cpp/b1-cpu");
        fs::create_dir_all(&eng).unwrap();
        fs::write(eng.join("llama-server"), b"engine").unwrap();
        let bin = tmp.join("alkahest");
        fs::write(&bin, b"binary").unwrap();
        let ctx = Context {
            root: repo_root(),
            bundle_dir: bundle,
            out_dir: tmp.join("out"),
            version: version.to_owned(),
            triple: "x86_64-unknown-linux-gnu".to_owned(),
            source_date: Some(1_700_000_000),
            packager: packager(),
        };
        (ctx, bin)
    }

    fn paths(pkg: &rpm::Package) -> Vec<String> {
        pkg.metadata
            .get_file_paths()
            .unwrap()
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn versions_split_into_version_and_release() {
        assert_eq!(split_version("0.1.0"), ("0.1.0".into(), "1".into()));
        assert_eq!(
            split_version("1.2.0-rc.1"),
            ("1.2.0".into(), "0.rc.1".into())
        );
        assert_eq!(split_version("1.2.0+7"), ("1.2.0".into(), "1".into()));
        assert_eq!(rpm_arch("aarch64-unknown-linux-gnu").unwrap(), "aarch64");
        assert!(rpm_arch("riscv64-unknown-linux-gnu").is_err());
    }

    #[test]
    fn icon_sizes_come_from_the_file_name() {
        assert_eq!(icon_size("alkahest-128.png"), Some(128));
        assert_eq!(icon_size("alkahest.icns"), None);
        assert_eq!(icon_size("tray-light-22.png"), Some(22));
    }

    #[test]
    fn the_desktop_entry_matches_what_the_deb_gets() {
        let template =
            fs::read_to_string(repo_root().join("packaging/linux/alkahest.desktop")).unwrap();
        let with = render_desktop_entry(
            &template,
            "Utility;",
            Some("A server"),
            "alkahest",
            "alkahest",
        );
        assert!(with.contains("Exec=alkahest desktop\n"));
        assert!(with.contains("Icon=alkahest\n"));
        assert!(with.contains("Categories=Utility;\n"));
        assert!(with.contains("Comment=A server\n"));
        assert!(!with.contains("{{"));
        let without = render_desktop_entry(&template, "Utility;", None, "alkahest", "alkahest");
        assert!(!without.contains("Comment="));
        assert!(!without.contains("{{"));
    }

    #[test]
    fn desktop_rpm_has_the_deb_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, bin) = context(tmp.path(), "0.1.0");
        let path = build(&ctx, Flavour::Desktop, &bin).unwrap();
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            "alkahest-0.1.0-1.x86_64.rpm"
        );
        let pkg = rpm::Package::open(&path).unwrap();
        pkg.verify_digests().unwrap();
        let m = &pkg.metadata;
        assert_eq!(m.get_name().unwrap(), "alkahest");
        assert_eq!(m.get_license().unwrap(), "MIT");
        assert_eq!(m.get_arch().unwrap(), "x86_64");
        let files = paths(&pkg);
        for want in [
            "/usr/bin/alkahest",
            "/usr/lib/alkahest/engines/llama-cpp/b1-cpu/llama-server",
            "/usr/lib/alkahest/licenses/llama-cpp.txt",
            "/usr/lib/alkahest/systemd/alkahest.service",
            "/usr/lib/alkahest/NOTICE",
            "/usr/lib/alkahest/THIRD_PARTY_LICENSES",
            "/usr/share/licenses/alkahest/LICENSE",
            "/usr/share/applications/alkahest.desktop",
            "/usr/share/icons/hicolor/128x128/apps/alkahest.png",
            "/usr/share/icons/hicolor/512x512/apps/alkahest.png",
        ] {
            assert!(
                files.contains(&want.to_owned()),
                "missing {want} in {files:?}"
            );
        }
        let requires: Vec<String> = m
            .get_requires()
            .unwrap()
            .iter()
            .map(|d| d.name.clone())
            .collect();
        assert!(
            requires.contains(&"vulkan-loader".to_owned()),
            "{requires:?}"
        );
        let conflicts: Vec<String> = m
            .get_conflicts()
            .unwrap()
            .iter()
            .map(|d| d.name.clone())
            .collect();
        assert_eq!(conflicts, ["alkahest-headless"]);
        // No scriptlets: nothing runs at install time.
        assert!(m.get_pre_install_script().is_err());
        assert!(m.get_post_install_script().is_err());
        let engine = m
            .find_file_entry(Path::new("/usr/bin/alkahest"))
            .unwrap()
            .unwrap();
        assert_eq!(engine.mode().permissions() & 0o777, 0o755);
    }

    #[test]
    fn headless_rpm_has_no_gui_pieces_and_a_prerelease_release() {
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, bin) = context(tmp.path(), "1.0.0-rc.2");
        let path = build(&ctx, Flavour::Headless, &bin).unwrap();
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            "alkahest-headless-1.0.0-0.rc.2.x86_64.rpm"
        );
        let pkg = rpm::Package::open(&path).unwrap();
        pkg.verify_digests().unwrap();
        let files = paths(&pkg);
        assert!(files.contains(&"/usr/bin/alkahest".to_owned()));
        assert!(
            !files
                .iter()
                .any(|f| f.contains("/applications/") || f.contains("/icons/")),
            "{files:?}"
        );
        assert!(
            pkg.metadata
                .get_requires()
                .unwrap()
                .iter()
                .all(|d| d.name != "vulkan-loader")
        );
        let conflicts: Vec<String> = pkg
            .metadata
            .get_conflicts()
            .unwrap()
            .iter()
            .map(|d| d.name.clone())
            .collect();
        assert_eq!(conflicts, ["alkahest"]);
        assert!(
            pkg.metadata
                .get_description()
                .unwrap()
                .contains("without the desktop app")
        );
    }

    #[test]
    fn two_builds_of_the_same_inputs_are_identical() {
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, bin) = context(tmp.path(), "0.1.0");
        let a = fs::read(build(&ctx, Flavour::Headless, &bin).unwrap()).unwrap();
        let b = fs::read(build(&ctx, Flavour::Headless, &bin).unwrap()).unwrap();
        assert_eq!(a, b);
    }
}
