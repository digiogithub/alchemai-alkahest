//! `cargo xtask bundle`: release artefacts for Linux (ALK-US-0074, ALK-US-0105), macOS
//! (ALK-US-0075) and Windows (ALK-US-0076). The host decides the platform; `--target` names
//! another one (used to check the Windows installer on a Linux machine, see below).
//!
//! ```text
//! cargo xtask bundle [--flavour all|headless|desktop] [--formats <list>]
//!                    [--target <triple>] [--no-build] [--refresh-engine]
//! ```
//!
//! Produces in `target/packager/`:
//!
//! - the headless archive `alkahest-<version>-<target triple>.tar.gz` (`.zip` on Windows): the
//!   CLI/server binary built with `--no-default-features`, `engines/` with the bundled CPU
//!   llama.cpp (Metal included on macOS), the systemd unit templates (Linux), the licence files
//!   and `THIRD_PARTY_LICENSES`. This is the archive that `scripts/setup/install.sh` and
//!   `install.ps1` download.
//! - the desktop packages, from the binary built with the `desktop` feature and packaged by
//!   `cargo-packager` from `[package.metadata.packager]` in `crates/alkahest/Cargo.toml` with the
//!   same resources. `--formats` defaults per platform: Linux `deb,appimage,rpm`, macOS
//!   `app,dmg`, Windows `nsis`. `rpm` is produced by the `rpm` module (cargo-packager has no rpm
//!   target) for both flavours.
//!
//! Signing is opt-in through the environment and never needed locally: `APPLE_SIGNING_IDENTITY`
//! (plus the `APPLE_*` notarisation variables cargo-packager reads) on macOS and
//! `ALKAHEST_WINDOWS_SIGN_COMMAND` (a command with `%1` for the file) or
//! `ALKAHEST_WINDOWS_CERT_THUMBPRINT` (certificate in the store, `signtool` run by the packager)
//! on Windows.
//!
//! Cross check: `--target x86_64-pc-windows-gnullvm --no-build` packages an existing
//! `target/<triple>/release/alkahest.exe` plus a staged `target/bundle/engines`, which is how the
//! NSIS template is compiled with `makensis` on Linux without a Windows machine.
//!
//! The bundled engine is fetched by the product itself: the headless binary just built runs
//! `alkahest engines install llama-cpp --variant cpu` into a scratch `ALKAHEST_HOME`
//! (`target/bundle/home`), so the download, SHA-256 check and install marker are exactly those of
//! a user install, and the result is copied to `target/bundle/engines/` (idempotent; the copy is
//! kept until `--refresh-engine`). The packages never run maintainer scripts: the service is not
//! installed at package time; `sudo alkahest service install` does it on request
//! (ALK-SP-0009.R2), using the unit templates shipped under `<lib>/alkahest/systemd/`. The
//! Windows installer is the one exception by design: an optional, unticked-by-default component
//! calls `alkahest service install` (ALK-US-0076).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use walkdir::WalkDir;

mod archive;
mod rpm;

/// The package that holds the binary and the packager metadata.
const APP_PACKAGE: &str = "alkahest";

/// Engine recipe bundled into the packages.
const ENGINE_ID: &str = "llama-cpp";

/// macOS icon set, rendered by `cargo xtask icons`; relative to the crate directory.
const MACOS_ICON: &str = "../../packaging/icons/alkahest.icns";

/// Windows icon set, rendered by `cargo xtask icons`; relative to the crate directory.
const WINDOWS_ICON: &str = "../../packaging/icons/alkahest.ico";

/// The operating system a bundle is built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Os {
    Linux,
    Macos,
    Windows,
}

impl Os {
    /// The platform this xtask is running on.
    fn host() -> Os {
        if cfg!(target_os = "macos") {
            Os::Macos
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Linux
        }
    }

    /// The platform a target triple names.
    fn of_triple(triple: &str) -> Result<Os> {
        if triple.contains("-linux-") {
            Ok(Os::Linux)
        } else if triple.contains("-apple-darwin") {
            Ok(Os::Macos)
        } else if triple.contains("-windows-") {
            Ok(Os::Windows)
        } else {
            bail!("cannot tell the platform of target `{triple}`")
        }
    }

    /// Package formats built when `--formats` is not given.
    fn default_formats(self) -> &'static str {
        match self {
            Os::Linux => "deb,appimage,rpm",
            Os::Macos => "app,dmg",
            Os::Windows => "nsis",
        }
    }

    /// Suffix of executables.
    fn exe_suffix(self) -> &'static str {
        if self == Os::Windows { ".exe" } else { "" }
    }
}

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
    /// `None`: the platform default.
    formats: Option<String>,
    /// `None`: the host triple.
    target: Option<String>,
    build: bool,
    refresh_engine: bool,
}

fn parse_args(args: &[String]) -> Result<Options> {
    let mut o = Options {
        flavour: Flavour::All,
        formats: None,
        target: None,
        build: true,
        refresh_engine: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--no-build" => o.build = false,
            "--refresh-engine" => o.refresh_engine = true,
            "--formats" => {
                o.formats = Some(it.next().cloned().context("--formats needs a value")?);
            }
            "--target" => o.target = Some(it.next().cloned().context("--target needs a value")?),
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

/// Splits a `--formats` list into the formats cargo-packager builds and whether `rpm` (built
/// here) was asked for.
fn split_formats(list: &str) -> (Vec<String>, bool) {
    let mut packager = Vec::new();
    let mut rpm = false;
    for f in list.split(',').map(str::trim).filter(|f| !f.is_empty()) {
        if f == "rpm" {
            rpm = true;
        } else {
            packager.push(f.to_owned());
        }
    }
    (packager, rpm)
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

/// Signing inputs read from the environment (empty locally; set by the release workflow).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Signing {
    /// `Developer ID Application: ...`; cargo-packager reads the certificate and the
    /// notarisation credentials (`APPLE_CERTIFICATE`, `APPLE_ID`, ...) from the environment itself.
    apple_identity: Option<String>,
    /// Command that signs `%1` (for example `osslsigncode` or `signtool` with its arguments).
    /// Wins over the thumbprint.
    windows_sign_command: Option<String>,
    /// SHA-1 thumbprint of a code signing certificate in the Windows certificate store; the
    /// packager then runs `signtool` itself with SHA-256 and an RFC 3161 timestamp.
    windows_thumbprint: Option<String>,
    /// Timestamp server used with the thumbprint.
    windows_timestamp_url: String,
}

impl Signing {
    fn from_env() -> Signing {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        Signing {
            apple_identity: get("APPLE_SIGNING_IDENTITY"),
            windows_sign_command: get("ALKAHEST_WINDOWS_SIGN_COMMAND"),
            windows_thumbprint: get("ALKAHEST_WINDOWS_CERT_THUMBPRINT"),
            windows_timestamp_url: get("ALKAHEST_WINDOWS_TIMESTAMP_URL")
                .unwrap_or_else(|| "http://timestamp.digicert.com".to_owned()),
        }
    }
}

/// Resources that only exist on one platform, as `(os, source, target)`; paths are relative to
/// the crate directory like the rest of `[package.metadata.packager]`.
const OS_RESOURCES: &[(Os, &str, &str)] = &[
    (
        Os::Windows,
        "../../packaging/windows/path.ps1",
        "tools/path.ps1",
    ),
    (Os::Windows, WINDOWS_ICON, "alkahest.ico"),
];

/// Turns the shared packager metadata (already camel-cased) into the configuration for `os`:
/// version and formats, platform icons, no systemd units outside Linux, platform-only
/// resources, and signing when the environment provides it.
fn platform_config(
    os: Os,
    mut config: Value,
    version: &str,
    formats: &[String],
    signing: &Signing,
) -> Value {
    config["version"] = json!(version);
    config["formats"] = json!(formats);

    let mut resources: Vec<Value> = config["resources"].as_array().cloned().unwrap_or_default();
    if os != Os::Linux {
        resources.retain(|r| {
            !r["target"]
                .as_str()
                .is_some_and(|t| t.starts_with("systemd/"))
        });
    }
    for (_, src, target) in OS_RESOURCES.iter().filter(|(o, _, _)| *o == os) {
        resources.push(json!({"src": src, "target": target}));
    }
    config["resources"] = json!(resources);

    match os {
        Os::Linux => {}
        Os::Macos => {
            // `Alkahest.app` and `Alkahest_<v>_<arch>.dmg`; the Linux names stay lower-case.
            config["productName"] = json!("Alkahest");
            config["icons"] = json!([MACOS_ICON]);
            if let Some(id) = &signing.apple_identity {
                config["macos"]["signingIdentity"] = json!(id);
            }
        }
        Os::Windows => {
            // The install folder, Start menu entry and Add/Remove Programs name.
            config["productName"] = json!("Alkahest");
            config["icons"] = json!([WINDOWS_ICON]);
            config["nsis"]["installerIcon"] = json!(WINDOWS_ICON);
            if let Some(cmd) = &signing.windows_sign_command {
                config["windows"]["signCommand"] = json!(cmd);
            } else if let Some(thumb) = &signing.windows_thumbprint {
                config["windows"]["certificateThumbprint"] = json!(thumb);
                config["windows"]["digestAlgorithm"] = json!("sha256");
                config["windows"]["timestampUrl"] = json!(signing.windows_timestamp_url);
                config["windows"]["tsp"] = json!(true);
            }
        }
    }
    config
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
    let host = host_triple()?;
    let triple = opts.target.clone().unwrap_or_else(|| host.clone());
    let os = Os::of_triple(&triple)?;
    let cross = triple != host;
    if !cross && os != Os::host() {
        bail!("the host triple {host} does not match this machine's platform");
    }
    let meta = cargo_metadata::MetadataCommand::new().no_deps().exec()?;
    let pkg = meta
        .packages
        .iter()
        .find(|p| p.name.as_str() == APP_PACKAGE)
        .context("package alkahest not found")?;
    let version = pkg.version.to_string();
    let bundle_dir = root.join("target/bundle");
    let out_dir = root.join("target/packager");
    fs::create_dir_all(&bundle_dir)?;
    fs::create_dir_all(&out_dir)?;
    let (packager_formats, want_rpm) = split_formats(
        opts.formats
            .as_deref()
            .unwrap_or_else(|| os.default_formats()),
    );
    if want_rpm && os != Os::Linux {
        bail!("rpm packages are built for Linux only");
    }
    // `cargo build --target` writes to `target/<triple>/release`; a native build does not.
    let release_dir = if opts.target.is_some() {
        root.join("target").join(&triple).join("release")
    } else {
        root.join("target/release")
    };
    let exe_name = format!("alkahest{}", os.exe_suffix());
    let release_bin = release_dir.join(&exe_name);
    let cargo_build = |features: Option<&str>| -> Result<()> {
        let mut cmd = Command::new("cargo");
        cmd.current_dir(root)
            .args(["build", "--release", "--locked", "-p", APP_PACKAGE]);
        if let Some(t) = &opts.target {
            cmd.args(["--target", t]);
        }
        match features {
            Some(f) => cmd.args(["--features", f]),
            None => cmd.arg("--no-default-features"),
        };
        run_status(&mut cmd)
    };

    // The headless binary is built first: it fetches the engine and becomes the archive
    // payload. The desktop build later overwrites `release_bin`.
    let headless_bin = bundle_dir.join(format!("alkahest-headless{}", os.exe_suffix()));
    if opts.build || !headless_bin.exists() {
        if opts.build {
            cargo_build(None)?;
        }
        fs::copy(&release_bin, &headless_bin)
            .with_context(|| format!("copying {}", release_bin.display()))?;
    }
    ensure_engine(root, &headless_bin, &bundle_dir, opts.refresh_engine, cross)?;

    let source_date = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    let packager_metadata = pkg
        .metadata
        .get("packager")
        .cloned()
        .context("missing [package.metadata.packager] in crates/alkahest/Cargo.toml")?;
    let rpm_ctx = rpm::Context {
        root: root.to_path_buf(),
        bundle_dir: bundle_dir.clone(),
        out_dir: out_dir.clone(),
        version: version.clone(),
        triple: triple.clone(),
        source_date,
        packager: packager_metadata.clone(),
    };

    if matches!(opts.flavour, Flavour::All | Flavour::Headless) {
        let name = format!("alkahest-{version}-{triple}");
        let stage = stage_tree(root, &headless_bin, &bundle_dir, &name, os)?;
        let archive = if os == Os::Windows {
            archive::write_zip(&stage, &out_dir.join(format!("{name}.zip")), source_date)?
        } else {
            archive::write_tar_gz(&stage, &out_dir.join(format!("{name}.tar.gz")), source_date)?
        };
        println!("headless archive: {}", archive.display());
        if want_rpm {
            let rpm = rpm::build(&rpm_ctx, rpm::Flavour::Headless, &headless_bin)?;
            println!("headless rpm: {}", rpm.display());
        }
    }

    if matches!(opts.flavour, Flavour::All | Flavour::Desktop) {
        if opts.build {
            cargo_build(Some("desktop"))?;
        }
        if !packager_formats.is_empty() {
            let mut config = platform_config(
                os,
                camelize(packager_metadata),
                &version,
                &packager_formats,
                &Signing::from_env(),
            );
            if opts.target.is_some() {
                config["binariesDir"] = json!(release_dir);
                config["targetTriple"] = json!(triple);
            }
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
        if want_rpm {
            let rpm = rpm::build(&rpm_ctx, rpm::Flavour::Desktop, &release_bin)?;
            println!("desktop rpm: {}", rpm.display());
        }
    }
    println!("artefacts in {}", out_dir.display());
    Ok(true)
}

/// Installs the bundled CPU llama.cpp with the product's own installer and stages it under
/// `<bundle_dir>/engines/`. A cross build cannot run the binary it just built, so it needs the
/// engines staged beforehand.
fn ensure_engine(
    root: &Path,
    bin: &Path,
    bundle_dir: &Path,
    refresh: bool,
    cross: bool,
) -> Result<()> {
    let dest = bundle_dir.join("engines").join(ENGINE_ID);
    if engine_staged(&dest) && !refresh {
        println!("bundled engine: reusing {}", dest.display());
        return Ok(());
    }
    if cross {
        bail!(
            "a cross build cannot install the engine: stage it in {} first",
            dest.display()
        );
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

/// Whether `dest` holds a staged engine, not merely its directory. A CI cache that prunes
/// files under `target/` (Swatinem/rust-cache) restores the tree as empty directories, and
/// reusing that skeleton ships an app without its engine.
fn engine_staged(dest: &Path) -> bool {
    WalkDir::new(dest)
        .into_iter()
        .filter_map(|e| e.ok())
        .any(|e| {
            e.file_type().is_file()
                && matches!(
                    e.file_name().to_str(),
                    Some("llama-server" | "llama-server.exe")
                )
        })
}

/// Recursively copies a directory, keeping file modes and (on Unix) symlinks.
fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    for entry in WalkDir::new(from) {
        let entry = entry?;
        let rel = entry.path().strip_prefix(from)?;
        let target = to.join(rel);
        let ft = entry.file_type();
        if ft.is_dir() {
            fs::create_dir_all(&target)?;
        } else if ft.is_symlink() {
            #[cfg(unix)]
            {
                let link = fs::read_link(entry.path())?;
                std::os::unix::fs::symlink(link, &target)?;
            }
            #[cfg(not(unix))]
            {
                fs::copy(entry.path(), &target)?;
            }
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Files copied into the archive root and into the packages, as `(source, name in package)`.
const DOCS: &[(&str, &str)] = &[
    ("LICENSE", "LICENSE"),
    ("NOTICE", "NOTICE"),
    ("THIRD_PARTY_LICENSES", "THIRD_PARTY_LICENSES"),
];

/// systemd unit templates, shipped on Linux only.
const SYSTEMD_UNITS: &[(&str, &str)] = &[
    (
        "packaging/systemd/alkahest.service",
        "systemd/alkahest.service",
    ),
    (
        "packaging/systemd/alkahest.user.service",
        "systemd/alkahest.user.service",
    ),
];

/// Stages the headless archive tree `<bundle_dir>/<name>/` and returns its path.
fn stage_tree(root: &Path, bin: &Path, bundle_dir: &Path, name: &str, os: Os) -> Result<PathBuf> {
    let stage = bundle_dir.join(name);
    if stage.exists() {
        fs::remove_dir_all(&stage)?;
    }
    fs::create_dir_all(&stage)?;
    fs::copy(bin, stage.join(format!("alkahest{}", os.exe_suffix())))?;
    copy_dir(&bundle_dir.join("engines"), &stage.join("engines"))?;
    copy_dir(&root.join("packaging/licenses"), &stage.join("licenses"))?;
    let units: &[(&str, &str)] = if os == Os::Linux { SYSTEMD_UNITS } else { &[] };
    for (src, dst) in DOCS.iter().chain(units) {
        let to = stage.join(dst);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(root.join(src), &to).with_context(|| format!("copying {src}"))?;
    }
    Ok(stage)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
    }

    /// The packager metadata of `crates/alkahest/Cargo.toml`, camel-cased.
    fn packager_metadata() -> Value {
        let text = fs::read_to_string(repo_root().join("crates/alkahest/Cargo.toml")).unwrap();
        let manifest: toml::Value = toml::from_str(&text).unwrap();
        let json = serde_json::to_value(&manifest["package"]["metadata"]["packager"]).unwrap();
        camelize(json)
    }

    fn resource_targets(config: &Value) -> Vec<String> {
        config["resources"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["target"].as_str().map(str::to_owned))
            .collect()
    }

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
        assert_eq!(o.formats, None);
        let o = parse_args(&[
            "--target".into(),
            "x86_64-pc-windows-gnullvm".into(),
            "--formats".into(),
            "nsis".into(),
        ])
        .unwrap();
        assert_eq!(o.target.as_deref(), Some("x86_64-pc-windows-gnullvm"));
        assert_eq!(o.formats.as_deref(), Some("nsis"));
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
    fn the_platform_follows_the_triple() {
        for (t, os) in [
            ("x86_64-unknown-linux-gnu", Os::Linux),
            ("aarch64-apple-darwin", Os::Macos),
            ("x86_64-apple-darwin", Os::Macos),
            ("x86_64-pc-windows-msvc", Os::Windows),
            ("x86_64-pc-windows-gnullvm", Os::Windows),
        ] {
            assert_eq!(Os::of_triple(t).unwrap(), os, "{t}");
        }
        assert!(Os::of_triple("wasm32-unknown-unknown").is_err());
    }

    #[test]
    fn rpm_is_split_from_the_packager_formats() {
        let (f, rpm) = split_formats("deb, appimage,rpm");
        assert_eq!(f, ["deb", "appimage"]);
        assert!(rpm);
        let (f, rpm) = split_formats("nsis");
        assert_eq!(f, ["nsis"]);
        assert!(!rpm);
    }

    #[test]
    fn macos_config_has_the_icns_the_plist_and_no_systemd_units() {
        let signing = Signing {
            apple_identity: Some("Developer ID Application: Digio (TEAM)".into()),
            ..Signing::default()
        };
        let c = platform_config(
            Os::Macos,
            packager_metadata(),
            "1.2.3",
            &["app".into(), "dmg".into()],
            &signing,
        );
        assert_eq!(c["version"], "1.2.3");
        assert_eq!(c["formats"], json!(["app", "dmg"]));
        assert_eq!(c["icons"], json!([MACOS_ICON]));
        assert_eq!(c["identifier"], "es.digio.alkahest");
        assert_eq!(c["productName"], "Alkahest");
        assert_eq!(
            c["macos"]["signingIdentity"],
            "Developer ID Application: Digio (TEAM)"
        );
        for key in ["entitlements", "infoPlistPath"] {
            let rel = c["macos"][key].as_str().unwrap();
            assert!(
                repo_root().join("crates/alkahest").join(rel).is_file(),
                "{key}: {rel} is missing"
            );
        }
        let targets = resource_targets(&c);
        assert!(targets.contains(&"engines".to_owned()));
        assert!(!targets.iter().any(|t| t.starts_with("systemd/")));
        // Unsigned by default: no identity key without the environment.
        let unsigned = platform_config(
            Os::Macos,
            packager_metadata(),
            "1.2.3",
            &["dmg".into()],
            &Signing::default(),
        );
        assert!(unsigned["macos"]["signingIdentity"].is_null());
    }

    #[test]
    fn windows_config_has_the_installer_pieces() {
        let signing = Signing {
            windows_sign_command: Some("osslsigncode sign -in %1".into()),
            ..Signing::default()
        };
        let c = platform_config(
            Os::Windows,
            packager_metadata(),
            "1.2.3",
            &["nsis".into()],
            &signing,
        );
        assert_eq!(c["productName"], "Alkahest");
        assert_eq!(c["nsis"]["installMode"], "perMachine");
        assert_eq!(c["nsis"]["installerIcon"], WINDOWS_ICON);
        assert_eq!(c["windows"]["signCommand"], "osslsigncode sign -in %1");
        let targets = resource_targets(&c);
        for want in [
            "engines",
            "tools/path.ps1",
            "alkahest.ico",
            "THIRD_PARTY_LICENSES",
        ] {
            assert!(targets.contains(&want.to_owned()), "missing {want}");
        }
        assert!(!targets.iter().any(|t| t.starts_with("systemd/")));
        // Every resource source exists, except the engines staged by `ensure_engine`.
        for r in c["resources"].as_array().unwrap() {
            let src = r["src"].as_str().unwrap();
            if !src.contains("target/bundle") {
                let path = repo_root().join("crates/alkahest").join(src);
                assert!(path.exists(), "{} is missing", path.display());
            }
        }
        let template = c["nsis"]["template"].as_str().unwrap();
        assert!(repo_root().join("crates/alkahest").join(template).is_file());
        let by_thumbprint = platform_config(
            Os::Windows,
            packager_metadata(),
            "1.2.3",
            &["nsis".into()],
            &Signing {
                windows_thumbprint: Some("ABCDEF".into()),
                windows_timestamp_url: "http://ts.example".into(),
                ..Signing::default()
            },
        );
        assert_eq!(by_thumbprint["windows"]["certificateThumbprint"], "ABCDEF");
        assert_eq!(by_thumbprint["windows"]["digestAlgorithm"], "sha256");
        assert_eq!(
            by_thumbprint["windows"]["timestampUrl"],
            "http://ts.example"
        );
        let unsigned = platform_config(
            Os::Windows,
            packager_metadata(),
            "1.2.3",
            &["nsis".into()],
            &Signing::default(),
        );
        assert!(unsigned["windows"]["signCommand"].is_null());
        assert!(unsigned["windows"]["certificateThumbprint"].is_null());
    }

    #[test]
    fn linux_config_keeps_systemd_units() {
        let c = platform_config(
            Os::Linux,
            packager_metadata(),
            "1.2.3",
            &["deb".into()],
            &Signing::default(),
        );
        assert!(resource_targets(&c).contains(&"systemd/alkahest.service".to_owned()));
        assert_eq!(c["icons"].as_array().unwrap().len(), 4);
    }

    /// Creates a tiny engines tree and binary under `dir` like `ensure_engine` leaves them.
    fn fake_bundle(dir: &Path) -> PathBuf {
        let bundle = dir.join("bundle");
        let eng = bundle.join("engines/llama-cpp/b1-cpu");
        fs::create_dir_all(&eng).unwrap();
        fs::write(eng.join("llama-server"), b"x").unwrap();
        fs::write(dir.join("alkahest"), b"bin").unwrap();
        bundle
    }

    #[test]
    fn archive_has_the_expected_layout() {
        let root = repo_root();
        let tmp = tempfile::tempdir().unwrap();
        let bundle = fake_bundle(tmp.path());
        let bin = tmp.path().join("alkahest");
        let name = "alkahest-9.9.9-x86_64-unknown-linux-gnu";
        let stage = stage_tree(&root, &bin, &bundle, name, Os::Linux).unwrap();
        let tar = archive::write_tar_gz(&stage, &tmp.path().join("a.tar.gz"), Some(0)).unwrap();
        let names = archive::list_tar_gz(&tar).unwrap();
        for want in [
            "alkahest",
            "engines/llama-cpp/b1-cpu/llama-server",
            "systemd/alkahest.service",
            "THIRD_PARTY_LICENSES",
            "licenses/llama-cpp.txt",
        ] {
            let full = format!("{name}/{want}");
            assert!(names.contains(&full), "missing {full} in {names:?}");
        }
    }

    #[test]
    fn windows_archive_is_a_zip_without_systemd_units() {
        let root = repo_root();
        let tmp = tempfile::tempdir().unwrap();
        let bundle = fake_bundle(tmp.path());
        let bin = tmp.path().join("alkahest");
        let name = "alkahest-9.9.9-x86_64-pc-windows-msvc";
        let stage = stage_tree(&root, &bin, &bundle, name, Os::Windows).unwrap();
        assert!(stage.join("alkahest.exe").is_file());
        assert!(!stage.join("systemd").exists());
        let zip = archive::write_zip(&stage, &tmp.path().join("a.zip"), Some(0)).unwrap();
        let names = archive::list_zip(&zip).unwrap();
        for want in [
            "alkahest.exe",
            "engines/llama-cpp/b1-cpu/llama-server",
            "NOTICE",
        ] {
            let full = format!("{name}/{want}");
            assert!(names.contains(&full), "missing {full} in {names:?}");
        }
    }

    #[test]
    fn an_empty_engine_skeleton_is_not_staged() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("bundle/engines/llama-cpp");
        fs::create_dir_all(dest.join("b1-cpu")).unwrap();
        assert!(!engine_staged(&dest));
        // A cross build therefore refuses the skeleton instead of bundling it.
        let err = ensure_engine(
            tmp.path(),
            Path::new("x"),
            &tmp.path().join("bundle"),
            false,
            true,
        )
        .unwrap_err();
        assert!(err.to_string().contains("stage it"), "{err}");
        let bundle = fake_bundle(tmp.path());
        assert!(engine_staged(&bundle.join("engines/llama-cpp")));
    }

    #[test]
    fn a_cross_build_demands_staged_engines() {
        let tmp = tempfile::tempdir().unwrap();
        let err = ensure_engine(tmp.path(), Path::new("x"), tmp.path(), false, true).unwrap_err();
        assert!(err.to_string().contains("stage it"), "{err}");
    }
}
