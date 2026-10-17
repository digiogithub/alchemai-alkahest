//! `alkahest update`: replace the running binary with a GitHub release (ALK-US-0112).
//!
//! Flow: detect the install channel and refuse package-manager installs (unless `--force`),
//! look the release up ([`release::select_latest`] for the newest stable one that has an
//! archive for this platform, or the explicit version), compare it with the running version,
//! confirm, download the archive into a staging directory next to the binary, verify its size
//! and SHA-256 against the release `SHA256SUMS` (mandatory, no opt-out), unpack it, then swap
//! the binary ([`apply::replace_file`]) and the companion files that already sit next to it
//! (`engines/`, `licenses/`, licence texts). Nothing installed is touched before the archive
//! is verified.
//!
//! Exit codes: 0 done or up to date, 1 failure, 3 refused (package-manager install, no
//! terminal without `--yes`, cancelled), 4 verification failed, 10 `--check` found an update.

pub mod apply;
pub mod channel;
pub mod release;

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use alkahest_core::config::Loaded;
use alkahest_fetch::{DownloadRequest, Downloader, FetchConfig, FetchError};
use alkahest_recipes::ArchiveFormat;
use anyhow::{Context, Result, anyhow};
use semver::Version;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::cli::UpdateArgs;
use channel::{Channel, ChannelEnv};
use release::{Release, ReleaseClient, Source};

/// Exit code: the update was refused (channel, no terminal, cancelled).
pub const EXIT_REFUSED: u8 = 3;
/// Exit code: the download did not match the published size or checksum.
pub const EXIT_VERIFY: u8 = 4;
/// Exit code of `--check` when a different release would be installed.
pub const EXIT_AVAILABLE: u8 = 10;

/// Directories of the release archive that are refreshed when they exist next to the binary.
pub const COMPANION_DIRS: &[&str] = &["engines", "licenses"];
/// Files of the release archive that are refreshed when they exist next to the binary.
const COMPANION_FILES: &[&str] = &["LICENSE", "NOTICE", "THIRD_PARTY_LICENSES"];
/// Archives larger than this are refused before downloading.
const MAX_ARCHIVE_BYTES: u64 = 2 << 30;

/// What `alkahest update --json` prints.
#[derive(Debug, Default, Serialize)]
struct Report {
    /// Running version.
    current: String,
    /// Release version considered, empty when none was found.
    latest: String,
    /// Release page.
    url: String,
    /// `up_to_date`, `available`, `updated`, `refused` or `failed`.
    action: &'static str,
    /// Human explanation of the action.
    reason: String,
    /// Install channel (see [`Channel::as_str`]).
    channel: &'static str,
    /// The archive that was (or would be) installed.
    #[serde(skip_serializing_if = "Option::is_none")]
    asset: Option<String>,
    /// Companion directories and files that were refreshed next to the binary.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    updated_files: Vec<String>,
    /// Non-fatal problems (`--force` on a package-manager install, engines left in place).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

/// One run of the command.
struct Updater<'a> {
    args: &'a UpdateArgs,
    json: bool,
    report: Report,
}

/// Runs `alkahest update`.
pub fn run(args: &UpdateArgs, loaded: &Loaded, json: bool) -> Result<ExitCode> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    let mut u = Updater {
        args,
        json,
        report: Report {
            current: env!("CARGO_PKG_VERSION").to_owned(),
            ..Report::default()
        },
    };
    let fetch = FetchConfig {
        max_bytes_per_sec: loaded.config.pull.bandwidth_cap(),
        ..FetchConfig::default()
    };
    let (action, reason, code) = match runtime.block_on(u.execute(fetch)) {
        Ok(done) => done,
        Err(Failure { code, message }) => {
            let action = if code == EXIT_REFUSED {
                "refused"
            } else {
                "failed"
            };
            if !json {
                eprintln!("error: {message}");
            }
            (action, message, code)
        }
    };
    u.report.action = action;
    u.report.reason = reason;
    if json {
        println!("{}", serde_json::to_string_pretty(&u.report)?);
    }
    Ok(ExitCode::from(code))
}

/// A failed or refused run with its exit code.
struct Failure {
    code: u8,
    message: String,
}

impl Failure {
    fn new(code: u8, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn failed(err: anyhow::Error) -> Self {
        Self::new(1, format!("{err:#}"))
    }
}

type Outcome = std::result::Result<(&'static str, String, u8), Failure>;

impl Updater<'_> {
    /// Prints a human line; with `--json` it goes to stderr so stdout stays one JSON document.
    fn say(&self, line: &str) {
        if self.json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }

    fn up_to_date(&self, line: String) -> Outcome {
        self.say(&line);
        Ok(("up_to_date", line, 0))
    }

    async fn execute(&mut self, fetch: FetchConfig) -> Outcome {
        let explicit = self.args.version.is_some();
        let exe = current_exe().map_err(Failure::failed)?;
        apply::cleanup_old(&exe);

        let channel = channel::detect(&exe, &ChannelEnv::for_exe(&exe));
        self.report.channel = channel.as_str();
        if let Some(hint) = channel.hint() {
            let allowed = channel == Channel::Source && explicit;
            if self.args.force {
                self.warn(format!(
                    "--force replaces the binary of a {} install; {hint}",
                    channel.as_str()
                ));
            } else if !allowed {
                return Err(Failure::new(
                    EXIT_REFUSED,
                    format!("{hint} (or pass --force to replace the binary anyway)"),
                ));
            }
        }

        let target = release::host_target().ok_or_else(|| {
            Failure::new(
                1,
                format!(
                    "no release archive is published for {}/{}",
                    std::env::consts::OS,
                    std::env::consts::ARCH
                ),
            )
        })?;
        let client = ReleaseClient::new(Source::from_env()).map_err(Failure::failed)?;
        let (rel, latest) = self.find_release(&client, &target).await?;
        self.report.latest = latest.to_string();
        self.report.url = rel.html_url.clone();

        let current = Version::parse(env!("CARGO_PKG_VERSION"))
            .map_err(|e| Failure::failed(anyhow!("running version: {e}")))?;
        let ordering = latest.cmp(&current);
        let (downgrade, reinstall) = match ordering {
            std::cmp::Ordering::Greater => (false, false),
            std::cmp::Ordering::Equal if !explicit => {
                return self.up_to_date(format!("alkahest {current} is up to date."));
            }
            std::cmp::Ordering::Equal => (false, true),
            std::cmp::Ordering::Less if !explicit => {
                return self.up_to_date(format!(
                    "alkahest {current} is newer than the latest release {latest}; up to date."
                ));
            }
            std::cmp::Ordering::Less => (true, false),
        };
        let asset_name = release::archive_name(&latest, &target);
        self.report.asset = Some(asset_name.clone());
        let verb = if downgrade {
            "Downgrade"
        } else if reinstall {
            "Reinstall"
        } else {
            "Update"
        };

        if self.args.check {
            self.say(&format!(
                "{verb} available: alkahest {current} -> {latest} ({asset_name})"
            ));
            if !rel.html_url.is_empty() {
                self.say(&format!("Release notes: {}", rel.html_url));
            }
            let how = if explicit {
                format!("alkahest update {latest}")
            } else {
                "alkahest update".to_owned()
            };
            self.say(&format!("Run `{how}` to install it."));
            return Ok((
                "available",
                format!("{verb} to {latest} available"),
                EXIT_AVAILABLE,
            ));
        }

        if !self.args.yes {
            self.confirm(verb, &current, &latest)?;
        }
        self.install(&client, &rel, &asset_name, &exe, fetch)
            .await?;
        self.say(&format!("Updated alkahest {current} -> {latest}."));
        self.say(
            "A running `alkahest serve` or service still runs the old binary; restart it to use \
             the new version.",
        );
        Ok(("updated", format!("updated to {latest}"), 0))
    }

    /// The release to install: the explicit version, or the newest eligible one.
    async fn find_release(
        &self,
        client: &ReleaseClient,
        target: &str,
    ) -> std::result::Result<(Release, Version), Failure> {
        if let Some(v) = &self.args.version {
            let rel = client
                .by_version(v)
                .await
                .map_err(Failure::failed)?
                .ok_or_else(|| Failure::new(1, format!("release {v} not found")))?;
            let version = rel
                .version()
                .ok_or_else(|| Failure::new(1, format!("tag {} is not a version", rel.tag_name)))?;
            return Ok((rel, version));
        }
        let releases = client.list().await.map_err(Failure::failed)?;
        let (rel, version) = release::select_latest(&releases, self.args.prerelease, target)
            .ok_or_else(|| {
                let kind = if self.args.prerelease {
                    "release"
                } else {
                    "stable release"
                };
                Failure::new(
                    1,
                    format!(
                        "no published {kind} of {} has an archive for {target}",
                        client.source().repo
                    ),
                )
            })?;
        Ok((rel.clone(), version))
    }

    /// Asks on the terminal; refuses off a terminal.
    fn confirm(
        &self,
        verb: &str,
        current: &Version,
        latest: &Version,
    ) -> std::result::Result<(), Failure> {
        let stdin = std::io::stdin();
        if !stdin.is_terminal() {
            return Err(Failure::new(
                EXIT_REFUSED,
                "not a terminal: pass --yes to update without asking, or --check to only look",
            ));
        }
        let shout = if verb == "Downgrade" {
            "DOWNGRADE"
        } else {
            verb
        };
        eprint!("{shout} alkahest {current} -> {latest}? [y/N] ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        let _ = stdin.lock().read_line(&mut line);
        match line.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => Ok(()),
            _ => Err(Failure::new(EXIT_REFUSED, "cancelled")),
        }
    }

    /// Downloads, verifies, unpacks and swaps.
    async fn install(
        &mut self,
        client: &ReleaseClient,
        rel: &Release,
        asset_name: &str,
        exe: &Path,
        fetch: FetchConfig,
    ) -> std::result::Result<(), Failure> {
        let asset = rel.asset(asset_name).ok_or_else(|| {
            Failure::new(1, format!("release {} has no {asset_name}", rel.tag_name))
        })?;
        if asset.size == 0 || asset.size > MAX_ARCHIVE_BYTES {
            return Err(Failure::new(
                EXIT_VERIFY,
                format!(
                    "{asset_name} has an unacceptable size of {} bytes",
                    asset.size
                ),
            ));
        }
        client.check_asset_url(asset).map_err(Failure::failed)?;
        let sums = rel.asset(release::CHECKSUMS_NAME).ok_or_else(|| {
            Failure::new(
                EXIT_VERIFY,
                format!(
                    "release {} has no {}; refusing to install an unverified archive",
                    rel.tag_name,
                    release::CHECKSUMS_NAME
                ),
            )
        })?;
        let manifest = client
            .fetch_checksums(sums)
            .await
            .map_err(Failure::failed)?;
        let want = release::lookup_checksum(&manifest, asset_name).ok_or_else(|| {
            Failure::new(
                EXIT_VERIFY,
                format!("{asset_name} is not listed in {}", release::CHECKSUMS_NAME),
            )
        })?;

        let dir = exe
            .parent()
            .ok_or_else(|| Failure::new(1, format!("{} has no parent directory", exe.display())))?;
        let stage = tempfile::Builder::new()
            .prefix(".alkahest-update-")
            .tempdir_in(dir)
            .map_err(|e| {
                Failure::new(
                    1,
                    format!(
                        "cannot stage the update next to {}: {e}{}",
                        exe.display(),
                        permission_hint(e.kind())
                    ),
                )
            })?;

        self.say(&format!(
            "Downloading {asset_name} ({:.1} MB) ...",
            asset.size as f64 / 1_000_000.0
        ));
        let archive = stage.path().join(asset_name);
        let req = DownloadRequest::new(&asset.browser_download_url, &archive)
            .with_sha256(&want)
            .with_size(asset.size);
        let downloader = Downloader::new(fetch).map_err(|e| Failure::failed(e.into()))?;
        if let Err(e) = downloader.download(&req, &CancellationToken::new()).await {
            let integrity = matches!(
                e,
                FetchError::DigestMismatch { .. } | FetchError::SizeMismatch { .. }
            );
            let code = if integrity { EXIT_VERIFY } else { 1 };
            let prefix = if integrity {
                "verification failed, the installed binary was not touched: "
            } else {
                ""
            };
            return Err(Failure::new(code, format!("{prefix}{asset_name}: {e}")));
        }
        self.say("Checksum verified.");

        let unpacked = stage.path().join("unpacked");
        let format = if asset_name.ends_with(".zip") {
            ArchiveFormat::Zip
        } else {
            ArchiveFormat::TarGz
        };
        alkahest_recipes::extract(&archive, format, 0, &unpacked)
            .map_err(|e| Failure::failed(anyhow!("unpacking {asset_name}: {e}")))?;
        let bin_name = exe_name();
        let root = archive_root(&unpacked, bin_name)
            .ok_or_else(|| Failure::new(1, format!("{asset_name} contains no {bin_name}")))?;

        apply::replace_file(&root.join(bin_name), exe)
            .map_err(|e| Failure::new(1, format!("{e}{}", permission_hint(e.source.kind()))))?;

        // The binary is in place; companions are refreshed best effort so a locked engine
        // directory (a running server on Windows) does not undo the update.
        for name in COMPANION_DIRS {
            let (src, dst) = (root.join(name), dir.join(name));
            if src.is_dir() && dst.is_dir() {
                match apply::replace_dir(&src, &dst) {
                    Ok(()) => self.report.updated_files.push(format!("{name}/")),
                    Err(e) => self.warn(format!(
                        "{name}/ was not updated ({e}); stop `alkahest serve` and run \
                         `alkahest update {} --yes` again",
                        self.report.latest
                    )),
                }
            }
        }
        for name in COMPANION_FILES {
            let (src, dst) = (root.join(name), dir.join(name));
            if src.is_file() && dst.is_file() {
                match apply::replace_file(&src, &dst) {
                    Ok(()) => self.report.updated_files.push((*name).to_owned()),
                    Err(e) => self.warn(format!("{name} was not updated: {e}")),
                }
            }
        }
        if !self.report.updated_files.is_empty() {
            self.say(&format!(
                "Also updated next to the binary: {}",
                self.report.updated_files.join(", ")
            ));
        }
        Ok(())
    }

    fn warn(&mut self, message: String) {
        self.say(&format!("warning: {message}"));
        self.report.warnings.push(message);
    }
}

/// The running executable with symlinks resolved: the file the update replaces.
fn current_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("locating the running binary")?;
    fs_canonicalize(&exe)
}

/// `std::fs::canonicalize` without the `\\?\` prefix Windows adds, which the channel
/// detection and messages do not need.
fn fs_canonicalize(p: &Path) -> Result<PathBuf> {
    let c = std::fs::canonicalize(p).with_context(|| format!("resolving {}", p.display()))?;
    #[cfg(windows)]
    {
        let s = c.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\")
            && !rest.starts_with("UNC\\")
        {
            return Ok(PathBuf::from(rest));
        }
    }
    Ok(c)
}

/// The binary name inside the archive.
fn exe_name() -> &'static str {
    if cfg!(windows) {
        "alkahest.exe"
    } else {
        "alkahest"
    }
}

/// The directory of an unpacked archive that holds the binary: the archive root itself, or
/// its single wrapper directory (`alkahest-<version>-<target>/`).
fn archive_root(unpacked: &Path, bin: &str) -> Option<PathBuf> {
    if unpacked.join(bin).is_file() {
        return Some(unpacked.to_path_buf());
    }
    let mut dirs = std::fs::read_dir(unpacked)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir());
    let only = dirs.next()?;
    if dirs.next().is_some() {
        return None;
    }
    let root = only.path();
    root.join(bin).is_file().then_some(root)
}

/// The usual fix for an install directory this user cannot write to.
fn permission_hint(kind: std::io::ErrorKind) -> &'static str {
    if kind == std::io::ErrorKind::PermissionDenied {
        "\nhint: the install directory is not writable by this user; re-run with the privileges \
         that installed alkahest (for example with sudo); alkahest never escalates by itself"
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_root_finds_the_wrapper_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let wrap = tmp.path().join("alkahest-1.0.0-x");
        std::fs::create_dir_all(wrap.join("engines")).unwrap();
        std::fs::write(wrap.join(exe_name()), b"x").unwrap();
        assert_eq!(archive_root(tmp.path(), exe_name()), Some(wrap));
        let flat = tempfile::tempdir().unwrap();
        std::fs::write(flat.path().join(exe_name()), b"x").unwrap();
        assert_eq!(
            archive_root(flat.path(), exe_name()),
            Some(flat.path().to_path_buf())
        );
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(archive_root(empty.path(), exe_name()), None);
    }
}
