//! `cargo xtask bundle`: Linux release artefacts (ALK-US-0074).
//!
//! ```text
//! cargo xtask bundle [--flavour all|headless|desktop] [--formats deb,appimage]
//!                    [--no-build] [--refresh-engine]
//! ```
//!
//! Produces in `target/packager/`:
//!
//! - `alkahest-<version>-<target triple>.tar.gz` (headless): the CLI/server binary built with
//!   `--no-default-features`, `engines/` with the bundled CPU llama.cpp, the systemd unit
//!   templates, the licence files and `THIRD_PARTY_LICENSES`. This is the archive that
//!   `scripts/setup/install.sh` downloads.
//! - `alkahest_<version>_<arch>.deb` and `.AppImage` (desktop): the binary built with the
//!   `desktop` feature, packaged by `cargo-packager` from `[package.metadata.packager]` in
//!   `crates/alkahest/Cargo.toml`, with the same resources.
//!
//! The bundled engine is fetched by the product itself: the headless binary just built runs
//! `alkahest engines install llama-cpp --variant cpu` into a scratch `ALKAHEST_HOME`
//! (`target/bundle/home`), so the download, SHA-256 check and install marker are exactly those of
//! a user install, and the result is copied to `target/bundle/engines/` (idempotent; the copy is
//! kept until `--refresh-engine`). The packages never run maintainer scripts: the service is not
//! installed at package time; `sudo alkahest service install` does it on request
//! (ALK-SP-0009.R2), using the unit templates shipped under `<lib>/alkahest/systemd/`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use walkdir::WalkDir;

/// The package that holds the binary and the packager metadata.
const APP_PACKAGE: &str = "alkahest";

/// Engine recipe bundled into the packages.
const ENGINE_ID: &str = "llama-cpp";

/// Which artefacts to build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flavour {
    All,
    Headless,
    Desktop,
}

/// Parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Options {
    flavour: Flavour,
    formats: String,
    build: bool,
    refresh_engine: bool,
}

fn parse_args(args: &[String]) -> Result<Options> {
    let mut o = Options {
        flavour: Flavour::All,
        formats: "deb,appimage".to_owned(),
        build: true,
        refresh_engine: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--no-build" => o.build = false,
            "--refresh-engine" => o.refresh_engine = true,
            "--formats" => o.formats = it.next().cloned().context("--formats needs a value")?,
            "--flavour" => {
                o.flavour = match it.next().map(String::as_str) {
                    Some("all") => Flavour::All,
                    Some("headless") => Flavour::Headless,
                    Some("desktop") => Flavour::Desktop,
                    other => bail!("--flavour expects all|headless|desktop, got {other:?}"),
                }
            }
            other => bail!("unknown bundle option `{other}`"),
        }
    }
    Ok(o)
}

/// Converts the kebab-case keys of `[package.metadata.packager]` to the camelCase of the
/// cargo-packager JSON schema.
fn camelize(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.into_iter()
                .map(|(k, v)| (kebab_to_camel(&k), camelize(v)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.into_iter().map(camelize).collect()),
        other => other,
    }
}

fn kebab_to_camel(s: &str) -> String {
    let mut out = String::new();
    let mut up = false;
    for c in s.chars() {
        if c == '-' {
            up = true;
        } else if up {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// The `host:` triple reported by `rustc -vV`.
fn host_triple() -> Result<String> {
    let out = Command::new("rustc")
        .arg("-vV")
        .output()
        .context("running rustc -vV")?;
    parse_host(&String::from_utf8_lossy(&out.stdout))
}

fn parse_host(rustc_vv: &str) -> Result<String> {
    rustc_vv
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(|h| h.trim().to_owned())
        .ok_or_else(|| anyhow!("rustc -vV printed no host triple"))
}

fn run_status(cmd: &mut Command) -> Result<()> {
    let status = cmd.status().with_context(|| format!("running {cmd:?}"))?;
    if !status.success() {
        bail!("{cmd:?} failed with {status}");
    }
    Ok(())
}

/// Entry point.
pub fn run(root: &Path, args: &[String]) -> Result<bool> {
    let opts = parse_args(args)?;
    if !cfg!(target_os = "linux") {
        bail!("bundle builds the Linux packages and must run on Linux");
    }
    let meta = cargo_metadata::MetadataCommand::new().no_deps().exec()?;
    let pkg = meta
        .packages
        .iter()
        .find(|p| p.name.as_str() == APP_PACKAGE)
        .context("package alkahest not found")?;
    let version = pkg.version.to_string();
    let triple = host_triple()?;
    let bundle_dir = root.join("target/bundle");
    let out_dir = root.join("target/packager");
    fs::create_dir_all(&bundle_dir)?;
    fs::create_dir_all(&out_dir)?;
    let release_bin = root.join("target/release/alkahest");

    // The headless binary is built first: it fetches the engine and becomes the tarball
    // payload. The desktop build later overwrites `target/release/alkahest`.
    let headless_bin = bundle_dir.join("alkahest-headless");
    if opts.build || !headless_bin.exists() {
        if opts.build {
            run_status(Command::new("cargo").current_dir(root).args([
                "build",
                "--release",
                "--locked",
                "-p",
                APP_PACKAGE,
                "--no-default-features",
            ]))?;
        }
        fs::copy(&release_bin, &headless_bin)
            .with_context(|| format!("copying {}", release_bin.display()))?;
    }
    ensure_engine(root, &headless_bin, &bundle_dir, opts.refresh_engine)?;

    if matches!(opts.flavour, Flavour::All | Flavour::Headless) {
        let tar = make_tarball(
            root,
            &headless_bin,
            &bundle_dir,
            &out_dir,
            &version,
            &triple,
        )?;
        println!("headless archive: {}", tar.display());
    }

    if matches!(opts.flavour, Flavour::All | Flavour::Desktop) {
        if opts.build {
            run_status(Command::new("cargo").current_dir(root).args([
                "build",
                "--release",
                "--locked",
                "-p",
                APP_PACKAGE,
                "--features",
                "desktop",
            ]))?;
        }
        let packager = pkg
            .metadata
            .get("packager")
            .cloned()
            .context("missing [package.metadata.packager] in crates/alkahest/Cargo.toml")?;
        let mut config = camelize(packager);
        config["version"] = json!(version);
        config["formats"] = json!(opts.formats.split(',').map(str::trim).collect::<Vec<_>>());
        let app_dir = pkg
            .manifest_path
            .parent()
            .context("manifest has no parent")?
            .as_std_path()
            .to_path_buf();
        // A raw JSON string: a config file path would also become the packager's working
        // directory, and the metadata paths are relative to the crate directory.
        run_status(
            Command::new("cargo")
                .current_dir(&app_dir)
                .args(["packager", "--release", "-c"])
                .arg(config.to_string()),
        )?;
    }
    println!("artefacts in {}", out_dir.display());
    Ok(true)
}

/// Installs the bundled CPU llama.cpp with the product's own installer and stages it under
/// `<bundle_dir>/engines/`.
fn ensure_engine(root: &Path, bin: &Path, bundle_dir: &Path, refresh: bool) -> Result<()> {
    let dest = bundle_dir.join("engines").join(ENGINE_ID);
    if dest.is_dir() && !refresh {
        println!("bundled engine: reusing {}", dest.display());
        return Ok(());
    }
    let home = bundle_dir.join("home");
    if home.exists() {
        fs::remove_dir_all(&home)?;
    }
    fs::create_dir_all(&home)?;
    run_status(
        Command::new(bin)
            .current_dir(root)
            .env("ALKAHEST_HOME", &home)
            .args(["engines", "install", ENGINE_ID, "--variant", "cpu"]),
    )?;
    let installed = WalkDir::new(&home)
        .into_iter()
        .filter_map(|e| e.ok())
        .find(|e| {
            e.file_type().is_dir()
                && e.file_name() == ENGINE_ID
                && e.path().parent().is_some_and(|p| p.ends_with("engines"))
        })
        .map(|e| e.into_path())
        .ok_or_else(|| {
            anyhow!(
                "engine install left no `engines/{ENGINE_ID}` below {}",
                home.display()
            )
        })?;
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    copy_dir(&installed, &dest)?;
    fs::remove_dir_all(&home)?;
    println!("bundled engine: staged {}", dest.display());
    Ok(())
}

/// Recursively copies a directory, keeping file modes and symlinks.
fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    for entry in WalkDir::new(from) {
        let entry = entry?;
        let rel = entry.path().strip_prefix(from)?;
        let target = to.join(rel);
        let ft = entry.file_type();
        if ft.is_dir() {
            fs::create_dir_all(&target)?;
        } else if ft.is_symlink() {
            let link = fs::read_link(entry.path())?;
            std::os::unix::fs::symlink(link, &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Files copied into the tarball root and into the packages, as `(source, name in package)`.
const DOCS: &[(&str, &str)] = &[
    ("LICENSE", "LICENSE"),
    ("NOTICE", "NOTICE"),
    ("THIRD_PARTY_LICENSES", "THIRD_PARTY_LICENSES"),
    (
        "packaging/systemd/alkahest.service",
        "systemd/alkahest.service",
    ),
    (
        "packaging/systemd/alkahest.user.service",
        "systemd/alkahest.user.service",
    ),
];

/// Stages and compresses the headless archive; returns its path.
fn make_tarball(
    root: &Path,
    bin: &Path,
    bundle_dir: &Path,
    out_dir: &Path,
    version: &str,
    triple: &str,
) -> Result<PathBuf> {
    let name = format!("alkahest-{version}-{triple}");
    let stage = bundle_dir.join(&name);
    if stage.exists() {
        fs::remove_dir_all(&stage)?;
    }
    fs::create_dir_all(&stage)?;
    fs::copy(bin, stage.join("alkahest"))?;
    copy_dir(&bundle_dir.join("engines"), &stage.join("engines"))?;
    copy_dir(&root.join("packaging/licenses"), &stage.join("licenses"))?;
    for (src, dst) in DOCS {
        let to = stage.join(dst);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(root.join(src), &to).with_context(|| format!("copying {src}"))?;
    }
    let tar = out_dir.join(format!("{name}.tar.gz"));
    // Reproducible archive: sorted names, root ownership, fixed mtime (the commit date when
    // SOURCE_DATE_EPOCH is set).
    let mtime = std::env::var("SOURCE_DATE_EPOCH").unwrap_or_else(|_| "0".to_owned());
    run_status(
        Command::new("tar")
            .current_dir(bundle_dir)
            .args(["--sort=name", "--owner=0", "--group=0", "--numeric-owner"])
            .arg(format!("--mtime=@{mtime}"))
            .arg("-czf")
            .arg(&tar)
            .arg(&name),
    )?;
    Ok(tar)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camelizes_keys() {
        let v = camelize(
            json!({"product-name": "x", "deb": {"desktop-template": 1}, "a": [{"b-c": 2}]}),
        );
        assert_eq!(
            v,
            json!({"productName": "x", "deb": {"desktopTemplate": 1}, "a": [{"bC": 2}]})
        );
    }

    #[test]
    fn parses_options() {
        let o = parse_args(&["--flavour".into(), "headless".into(), "--no-build".into()]).unwrap();
        assert_eq!(o.flavour, Flavour::Headless);
        assert!(!o.build);
        assert_eq!(o.formats, "deb,appimage");
        assert!(parse_args(&["--bogus".into()]).is_err());
        assert!(parse_args(&["--flavour".into(), "x".into()]).is_err());
    }

    #[test]
    fn finds_the_host_triple() {
        let text =
            "rustc 1.98.0\nbinary: rustc\nhost: aarch64-unknown-linux-gnu\nrelease: 1.98.0\n";
        assert_eq!(parse_host(text).unwrap(), "aarch64-unknown-linux-gnu");
        assert!(parse_host("nothing").is_err());
    }

    #[test]
    fn tarball_has_the_expected_layout() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let tmp = tempfile::tempdir().unwrap();
        let bundle = tmp.path().join("bundle");
        let eng = bundle.join("engines/llama-cpp/b1-cpu");
        fs::create_dir_all(&eng).unwrap();
        fs::write(eng.join("llama-server"), b"x").unwrap();
        let bin = tmp.path().join("alkahest");
        fs::write(&bin, b"bin").unwrap();
        let out = tmp.path().join("out");
        fs::create_dir_all(&out).unwrap();
        let tar = make_tarball(
            &root,
            &bin,
            &bundle,
            &out,
            "9.9.9",
            "x86_64-unknown-linux-gnu",
        )
        .unwrap();
        let list = Command::new("tar").arg("-tzf").arg(&tar).output().unwrap();
        let list = String::from_utf8_lossy(&list.stdout);
        for want in [
            "alkahest-9.9.9-x86_64-unknown-linux-gnu/alkahest",
            "alkahest-9.9.9-x86_64-unknown-linux-gnu/engines/llama-cpp/b1-cpu/llama-server",
            "alkahest-9.9.9-x86_64-unknown-linux-gnu/systemd/alkahest.service",
            "alkahest-9.9.9-x86_64-unknown-linux-gnu/THIRD_PARTY_LICENSES",
            "alkahest-9.9.9-x86_64-unknown-linux-gnu/licenses/llama-cpp.txt",
        ] {
            assert!(list.lines().any(|l| l == want), "missing {want} in\n{list}");
        }
    }
}
