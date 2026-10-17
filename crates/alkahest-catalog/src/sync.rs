//! One catalog sync (ALK-SP-0011.R8, R10, R11, R14): resolve the ref to a commit, fetch only
//! what changed (raw files guided by `index.toml`, or the commit archive), validate the whole
//! snapshot, swap it in. A sync downloads and parses TOML only: it never runs anything and
//! never downloads an asset, dependency or model file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use alkahest_core::config::{CatalogLimits, CatalogSource, is_commit_id};
use alkahest_fetch::FetchConfig;
use serde::Serialize;

use crate::archive::extract_catalog;
use crate::content::{CatalogContent, ValidateOptions, validate};
use crate::error::{CatalogError, Result};
use crate::hosts::{HostKind, archive_url, raw_url, redact_url};
use crate::http::{AuthStyle, Credential, CredentialSource, HttpClient, KeyringCredentials};
use crate::layout;
use crate::manifest::{Index, sha256_hex};
use crate::refs;
use crate::snapshot::{
    self, SnapshotMeta, SyncLock, catalogs_root, now_rfc3339, read_files, read_meta,
};

/// Size bounds of a snapshot (ALK-SP-0011.R14), from `[catalog_limits]` in the configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Whole snapshot (default 16 MiB).
    pub max_total_bytes: u64,
    /// One catalog file (default 1 MiB).
    pub max_file_bytes: u64,
    /// The compressed commit archive of the fallback transport (default 256 MiB): the archive
    /// holds the whole repository, not just the catalog.
    pub max_archive_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self::from_config(&CatalogLimits::default())
    }
}

impl Limits {
    /// The limits configured in `[catalog_limits]`.
    pub fn from_config(c: &CatalogLimits) -> Self {
        Self {
            max_total_bytes: c.max_snapshot_bytes,
            max_file_bytes: c.max_file_bytes,
            max_archive_bytes: c.max_archive_bytes,
        }
    }

    /// Bound of the decompressed archive stream: four times the compressed bound (text
    /// compresses well, a decompression bomb far better).
    pub fn max_unpacked_bytes(&self) -> u64 {
        self.max_archive_bytes.saturating_mul(4)
    }
}

/// Everything a sync needs besides the source.
#[derive(Clone)]
pub struct SyncContext {
    /// `<data_dir>`; snapshots go to `<data_dir>/catalogs/`.
    pub data_dir: PathBuf,
    /// HTTP client.
    pub http: HttpClient,
    /// Size bounds.
    pub limits: Limits,
    /// Token lookup; consulted only after the host answered 401 or 403.
    pub credentials: Arc<dyn CredentialSource>,
    /// Version checked against `min_alkahest`.
    pub alkahest_version: semver::Version,
    /// Overall deadline of one sync; a sync cut short records the timeout like any failure.
    pub deadline: Option<Duration>,
    /// Receives the steps of a sync (the admin API turns them into job progress).
    pub progress: Option<SyncProgress>,
    /// Checked once the catalog lock is held: `false` means the source was removed or its
    /// URL or folder changed meanwhile, and the sync stops without writing anything.
    pub source_check: Option<SourceCheck>,
}

/// See [`SyncContext::source_check`].
pub type SourceCheck = Arc<dyn Fn(&CatalogSource) -> bool + Send + Sync>;

/// One step of a sync, reported through [`SyncContext::progress`] (desktop screen 16b shows
/// it as `Syncing…` with the step).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum SyncStep {
    /// Resolving the configured ref to a commit over git smart HTTP.
    ResolvingRef,
    /// Reading a `file://` catalog directory.
    ReadingDirectory,
    /// Downloading changed files (`index.toml` included) or the commit archive (one file).
    Downloading {
        /// Files downloaded so far.
        done: usize,
        /// Files to download.
        total: usize,
    },
    /// Extracting the catalog folder from the commit archive.
    ExtractingArchive,
    /// Validating the new snapshot (all-or-nothing) and swapping it in.
    Validating,
}

impl std::fmt::Display for SyncStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ResolvingRef => f.write_str("resolving ref"),
            Self::ReadingDirectory => f.write_str("reading directory"),
            Self::Downloading { done, total } => {
                write!(f, "downloading changed files {done} / {total}")
            }
            Self::ExtractingArchive => f.write_str("extracting archive"),
            Self::Validating => f.write_str("validating"),
        }
    }
}

/// Receiver of [`SyncStep`]s.
pub type SyncProgress = Arc<dyn Fn(SyncStep) + Send + Sync>;

impl std::fmt::Debug for SyncContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncContext")
            .field("data_dir", &self.data_dir)
            .field("limits", &self.limits)
            .field("deadline", &self.deadline)
            .finish_non_exhaustive()
    }
}

impl SyncContext {
    /// The production context: the `alkahest-fetch` client settings, keyring credentials,
    /// default limits, no deadline and this build's version.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Http`] when the TLS backend cannot start.
    pub fn new(data_dir: &Path) -> Result<Self> {
        Ok(Self {
            data_dir: data_dir.to_path_buf(),
            http: HttpClient::new(&FetchConfig::default())?,
            limits: Limits::default(),
            credentials: Arc::new(KeyringCredentials),
            alkahest_version: crate::running_version(),
            deadline: None,
            progress: None,
            source_check: None,
        })
    }

    /// Stops a sync whose source no longer passes `check` once the lock is held (a server
    /// passes "still configured with the same URL and folder").
    pub fn with_source_check(mut self, check: SourceCheck) -> Self {
        self.source_check = Some(check);
        self
    }

    /// Uses `credentials` to find a token after a 401/403 (the admin API passes its token
    /// store, so a token set through the API is used at once).
    pub fn with_credentials(mut self, credentials: Arc<dyn CredentialSource>) -> Self {
        self.credentials = credentials;
        self
    }

    /// Reports the steps of every sync to `progress`.
    pub fn with_progress(mut self, progress: SyncProgress) -> Self {
        self.progress = Some(progress);
        self
    }

    /// Reports `step` to the progress receiver, if any.
    fn step(&self, step: SyncStep) {
        if let Some(p) = &self.progress {
            p(step);
        }
    }

    /// Uses `limits`.
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Bounds every sync to `deadline`.
    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = Some(deadline);
        self
    }
}

/// What a sync did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum SyncOutcome {
    /// The ref still points at the snapshot commit; only `checked_at` changed.
    Unchanged {
        /// Commit.
        commit: String,
    },
    /// A pinned catalog already at its pin: no network request was made.
    Pinned {
        /// Commit.
        commit: String,
    },
    /// A new snapshot is in use.
    Updated {
        /// New commit.
        commit: String,
        /// Previous commit, if there was a snapshot.
        previous: Option<String>,
        /// Files downloaded (index included).
        downloaded: Vec<String>,
        /// Files reused from the previous snapshot.
        reused: Vec<String>,
        /// Transport used: `raw`, `archive` or `directory`.
        transport: String,
    },
}

/// The result of a successful sync.
#[derive(Debug, Clone, Serialize)]
pub struct SyncReport {
    /// Catalog name.
    pub catalog: String,
    /// What happened.
    pub outcome: SyncOutcome,
    /// Validation warnings of the snapshot in use.
    pub warnings: Vec<String>,
}

/// The previous snapshot: metadata and files.
type Previous = (SnapshotMeta, BTreeMap<String, Vec<u8>>);

/// Runs blocking work (parsing, hashing, filesystem, keyring) off the async workers, so a
/// deadline can fire and a server worker is never tied up.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| CatalogError::Config(format!("background task failed: {e}")))
}

/// Syncs `source` now (foreground). On failure the previous snapshot stays in use and the
/// error is recorded in its `snapshot.toml` (shown once by the next command).
///
/// # Errors
///
/// Every failure of resolution, transport, limits or validation, [`CatalogError::Busy`] when
/// another sync of the catalog runs, [`CatalogError::Timeout`] past the context's deadline.
pub async fn sync(source: &CatalogSource, ctx: &SyncContext) -> Result<SyncReport> {
    let root = catalogs_root(&ctx.data_dir);
    let lock = {
        let (root, name) = (root.clone(), source.name.clone());
        blocking(move || SyncLock::acquire(&root, &name)).await??
    };
    if let Some(check) = &ctx.source_check
        && !check(source)
    {
        // Removed or changed while waiting: nothing is written, not even an error record, so
        // no snapshot directory of a removed catalog is created again.
        drop(lock);
        return Err(CatalogError::Unknown(source.name.clone()));
    }
    let dir = root.join(&source.name);
    let previous = {
        let dir = dir.clone();
        blocking(move || {
            snapshot::cleanup(&dir);
            snapshot::read_snapshot(&dir)
        })
        .await?
    };
    let attempt = async {
        let first = run(source, ctx, &dir, previous.as_ref(), None).await;
        let denied = matches!(
            first,
            Err(CatalogError::Status {
                status: 401 | 403,
                ..
            })
        );
        if !denied || source.url.starts_with("file://") {
            return first;
        }
        // Only now, for a host that refused anonymous access, look for a token: a public
        // catalog never touches the keyring.
        let creds = ctx.credentials.clone();
        let src = source.clone();
        let Some(token) = blocking(move || creds.catalog_token(&src)).await? else {
            return first;
        };
        let kind = HostKind::detect(&source.url);
        let cred = Credential {
            token,
            hosts: kind.credential_hosts(&source.url),
            git_user: kind.git_user(),
        };
        run(source, ctx, &dir, previous.as_ref(), Some(&cred)).await
    };
    let result = match ctx.deadline {
        Some(d) => tokio::time::timeout(d, attempt)
            .await
            .unwrap_or(Err(CatalogError::Timeout(d.as_secs()))),
        None => attempt.await,
    };
    let result = result.map_err(|e| with_catalog(e, &source.name));
    if let Err(e) = &result {
        record_error(&dir, source, e);
    }
    drop(lock);
    result
}

/// Fills the catalog name into errors raised below the sync (HTTP size limits).
fn with_catalog(e: CatalogError, name: &str) -> CatalogError {
    match e {
        CatalogError::TooLarge {
            catalog,
            what,
            size,
            limit,
        } if catalog.is_empty() => CatalogError::TooLarge {
            catalog: name.to_string(),
            what,
            size,
            limit,
        },
        other => other,
    }
}

/// Records a failed sync in `snapshot.toml`, keeping the content as it is. Called with the
/// catalog lock held.
fn record_error(dir: &Path, source: &CatalogSource, e: &CatalogError) {
    let mut meta = read_meta(dir).unwrap_or_else(|| SnapshotMeta {
        url: source.url.clone(),
        r#ref: source.r#ref.clone(),
        subdir: source.subdir.clone(),
        ..SnapshotMeta::default()
    });
    let now = now_rfc3339();
    meta.checked_at = now.clone();
    meta.last_error = Some(e.to_string());
    meta.error_at = Some(now);
    meta.error_reported = false;
    if let Err(w) = snapshot::write_meta(dir, &meta) {
        tracing::warn!(error = %w, "could not record the catalog sync error");
    }
}

/// Marks the recorded error of `name` as shown, so it is shown only once. Skipped while a
/// sync of the catalog runs (it writes its own record).
pub fn mark_error_reported(data_dir: &Path, name: &str) {
    let Ok(_lock) = SyncLock::acquire(&catalogs_root(data_dir), name) else {
        return;
    };
    let dir = snapshot::snapshot_dir(data_dir, name);
    if let Some(mut meta) = read_meta(&dir)
        && !meta.error_reported
    {
        meta.error_reported = true;
        let _ = snapshot::write_meta(&dir, &meta);
    }
}

fn same_source(meta: &SnapshotMeta, source: &CatalogSource) -> bool {
    meta.url == source.url && meta.subdir == source.subdir
}

async fn run(
    source: &CatalogSource,
    ctx: &SyncContext,
    dir: &Path,
    previous: Option<&Previous>,
    cred: Option<&Credential>,
) -> Result<SyncReport> {
    let prev_meta = previous.map(|p| &p.0);
    let name = source.name.as_str();
    let opts = ValidateOptions {
        catalog: name.to_string(),
        strict: false,
        alkahest_version: ctx.alkahest_version.clone(),
    };
    let finish = |prev: &SnapshotMeta| -> Result<SyncReport> {
        let mut meta = prev.clone();
        meta.checked_at = now_rfc3339();
        meta.r#ref = source.r#ref.clone();
        meta.last_error = None;
        meta.error_at = None;
        meta.error_reported = false;
        snapshot::write_meta(dir, &meta)?;
        Ok(SyncReport {
            catalog: name.to_string(),
            outcome: SyncOutcome::Unchanged {
                commit: meta.commit.clone(),
            },
            warnings: meta.warnings,
        })
    };

    if let Some(path) = source.url.strip_prefix("file://") {
        ctx.step(SyncStep::ReadingDirectory);
        let path = PathBuf::from(path);
        let (files, commit) = blocking(move || {
            let files = read_files(&path);
            let listing: String = files
                .iter()
                .map(|(p, b)| format!("{p} {}\n", sha256_hex(b)))
                .collect();
            let commit = format!("dir-{}", &sha256_hex(listing.as_bytes())[..16]);
            (files, commit)
        })
        .await?;
        if let Some(prev) = prev_meta.filter(|p| p.commit == commit && same_source(p, source)) {
            return finish(prev);
        }
        check_total(name, &files, &ctx.limits)?;
        let downloaded = files.keys().cloned().collect();
        return commit_snapshot(
            source,
            ctx,
            dir,
            prev_meta,
            files,
            &opts,
            &commit,
            downloaded,
            Vec::new(),
            "directory",
        )
        .await;
    }

    let kind = HostKind::detect(&source.url);
    let commit = match &source.pin {
        Some(pin) => {
            if let Some(prev) = prev_meta.filter(|p| p.commit == *pin && same_source(p, source)) {
                let mut report = finish(prev)?;
                report.outcome = SyncOutcome::Pinned {
                    commit: pin.clone(),
                };
                return Ok(report);
            }
            pin.clone()
        }
        None => {
            ctx.step(SyncStep::ResolvingRef);
            refs::resolve(&ctx.http, name, &source.url, &source.r#ref, cred).await?
        }
    };
    if !is_commit_id(&commit) {
        return Err(CatalogError::Protocol {
            url: redact_url(&source.url),
            reason: format!("`{commit}` is not a commit id"),
        });
    }
    if let Some(prev) = prev_meta.filter(|p| p.commit == commit && same_source(p, source)) {
        return finish(prev);
    }

    // Raw files guided by the index.
    if let Some(index_url) = raw_url(source, &kind, &commit, layout::INDEX) {
        ctx.step(SyncStep::Downloading { done: 0, total: 1 });
        match ctx
            .http
            .get(
                &index_url,
                cred,
                AuthStyle::Bearer,
                ctx.limits.max_file_bytes,
            )
            .await
        {
            Ok(index_bytes) => {
                return fetch_raw(
                    source,
                    ctx,
                    dir,
                    previous,
                    &kind,
                    cred,
                    &commit,
                    index_bytes,
                    &opts,
                )
                .await;
            }
            Err(e) if e.is_not_found() => {
                tracing::debug!(catalog = name, "no index.toml; using the commit archive");
            }
            Err(e) => return Err(e),
        }
    }

    let Some(url) = archive_url(source, &kind, &commit) else {
        return Err(CatalogError::Config(format!(
            "catalog {name}: the host of {} has no known raw or archive endpoint; set raw_url \
             and archive_url in its [[catalogs]] entry",
            redact_url(&source.url)
        )));
    };
    ctx.step(SyncStep::Downloading { done: 0, total: 1 });
    let data = ctx
        .http
        .get(&url, cred, AuthStyle::Bearer, ctx.limits.max_archive_bytes)
        .await?;
    ctx.step(SyncStep::Downloading { done: 1, total: 1 });
    ctx.step(SyncStep::ExtractingArchive);
    // Built-in hosts write the commit into the archive (pax `comment`); a custom
    // `archive_url` host may not, which is only a warning.
    let require_commit = source.archive_url.is_none() && kind != HostKind::Other;
    let extracted = {
        let (name, subdir, commit, limits) = (
            name.to_string(),
            source.subdir.clone(),
            commit.clone(),
            ctx.limits,
        );
        blocking(move || extract_catalog(&name, &data, &subdir, &commit, &limits, require_commit))
            .await??
    };
    let downloaded = extracted.files.keys().cloned().collect();
    let mut report = commit_snapshot(
        source,
        ctx,
        dir,
        prev_meta,
        extracted.files,
        &opts,
        &commit,
        downloaded,
        Vec::new(),
        "archive",
    )
    .await?;
    if !extracted.commit_checked {
        report.warnings.push(format!(
            "the archive does not record its commit; its content was not checked against {commit}"
        ));
    }
    Ok(report)
}

fn check_total(name: &str, files: &BTreeMap<String, Vec<u8>>, limits: &Limits) -> Result<()> {
    for (p, b) in files {
        if b.len() as u64 > limits.max_file_bytes {
            return Err(CatalogError::TooLarge {
                catalog: name.to_string(),
                what: p.clone(),
                size: b.len() as u64,
                limit: limits.max_file_bytes,
            });
        }
    }
    let total = files
        .values()
        .try_fold(0u64, |acc, b| acc.checked_add(b.len() as u64))
        .unwrap_or(u64::MAX);
    if total > limits.max_total_bytes {
        return Err(CatalogError::TooLarge {
            catalog: name.to_string(),
            what: "the snapshot".into(),
            size: total,
            limit: limits.max_total_bytes,
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn fetch_raw(
    source: &CatalogSource,
    ctx: &SyncContext,
    dir: &Path,
    previous: Option<&Previous>,
    kind: &HostKind,
    cred: Option<&Credential>,
    commit: &str,
    index_bytes: Vec<u8>,
    opts: &ValidateOptions,
) -> Result<SyncReport> {
    let name = source.name.as_str();
    let invalid = |reason: String| CatalogError::Invalid {
        catalog: name.to_string(),
        issues: vec![crate::error::FileIssue::new(layout::INDEX, reason)],
    };
    let text = std::str::from_utf8(&index_bytes).map_err(|_| invalid("not UTF-8 text".into()))?;
    let mut value: toml::Value =
        toml::from_str(text).map_err(|e| invalid(e.message().to_string()))?;
    crate::schema::strip_unknown(&mut value, &crate::schema::INDEX);
    let index: Index = value
        .try_into()
        .map_err(|e: toml::de::Error| invalid(e.message().to_string()))?;
    let issues = index.check();
    if !issues.is_empty() {
        // A path outside the layout (absolute, `..`) fails before anything is fetched.
        return Err(CatalogError::Invalid {
            catalog: name.to_string(),
            issues,
        });
    }
    // Each file first, then the sum without overflow: sizes come from the remote.
    for f in &index.files {
        if f.size > ctx.limits.max_file_bytes {
            return Err(CatalogError::TooLarge {
                catalog: name.to_string(),
                what: f.path.clone(),
                size: f.size,
                limit: ctx.limits.max_file_bytes,
            });
        }
    }
    let total = index
        .files
        .iter()
        .try_fold(index_bytes.len() as u64, |acc, f| acc.checked_add(f.size))
        .unwrap_or(u64::MAX);
    if total > ctx.limits.max_total_bytes {
        return Err(CatalogError::TooLarge {
            catalog: name.to_string(),
            what: "the snapshot".into(),
            size: total,
            limit: ctx.limits.max_total_bytes,
        });
    }

    let empty = BTreeMap::new();
    let old_files = match previous {
        Some((m, f)) if same_source(m, source) => f,
        _ => &empty,
    };
    let unchanged = |f: &crate::manifest::IndexEntry| {
        old_files
            .get(&f.path)
            .is_some_and(|old| sha256_hex(old) == f.sha256 && old.len() as u64 == f.size)
    };
    // The index plus every changed file.
    let to_fetch = 1 + index.files.iter().filter(|f| !unchanged(f)).count();
    ctx.step(SyncStep::Downloading {
        done: 1,
        total: to_fetch,
    });
    let mut files = BTreeMap::new();
    let mut downloaded = vec![layout::INDEX.to_string()];
    let mut reused = Vec::new();
    for f in &index.files {
        if let Some(old) = old_files.get(&f.path)
            && sha256_hex(old) == f.sha256
            && old.len() as u64 == f.size
        {
            files.insert(f.path.clone(), old.clone());
            reused.push(f.path.clone());
            continue;
        }
        let Some(url) = raw_url(source, kind, commit, &f.path) else {
            return Err(CatalogError::Config(format!("catalog {name}: no raw URL")));
        };
        let bytes = ctx
            .http
            .get(
                &url,
                cred,
                AuthStyle::Bearer,
                f.size.min(ctx.limits.max_file_bytes),
            )
            .await
            .map_err(|e| with_catalog(e, name))?;
        let actual = sha256_hex(&bytes);
        if actual != f.sha256 || bytes.len() as u64 != f.size {
            return Err(CatalogError::DigestMismatch {
                catalog: name.to_string(),
                path: f.path.clone(),
                expected: f.sha256.clone(),
                actual,
                expected_size: f.size,
                actual_size: bytes.len() as u64,
            });
        }
        files.insert(f.path.clone(), bytes);
        downloaded.push(f.path.clone());
        ctx.step(SyncStep::Downloading {
            done: downloaded.len(),
            total: to_fetch,
        });
    }
    files.insert(layout::INDEX.to_string(), index_bytes);
    commit_snapshot(
        source,
        ctx,
        dir,
        previous.map(|p| &p.0),
        files,
        opts,
        commit,
        downloaded,
        reused,
        "raw",
    )
    .await
}

/// Validates `files` (all-or-nothing), stages them in a new version directory (off the
/// async workers) and makes it current with one rename of `snapshot.toml` (under the lock, on
/// the caller's task, so a cancelled sync never activates anything).
#[allow(clippy::too_many_arguments)]
async fn commit_snapshot(
    source: &CatalogSource,
    ctx: &SyncContext,
    dir: &Path,
    previous: Option<&SnapshotMeta>,
    files: BTreeMap<String, Vec<u8>>,
    opts: &ValidateOptions,
    commit: &str,
    downloaded: Vec<String>,
    reused: Vec<String>,
    transport: &str,
) -> Result<SyncReport> {
    ctx.step(SyncStep::Validating);
    let (version, warnings) = {
        let (dir, name, commit, opts) = (
            dir.to_path_buf(),
            source.name.clone(),
            commit.to_string(),
            opts.clone(),
        );
        blocking(move || -> Result<(String, Vec<String>)> {
            let content: CatalogContent = validate(&files, &opts)?;
            let warnings = content.warnings.iter().map(ToString::to_string).collect();
            let version = snapshot::stage(&dir, &name, &commit, &files)?;
            Ok((version, warnings))
        })
        .await??
    };
    let now = now_rfc3339();
    let meta = SnapshotMeta {
        version,
        url: source.url.clone(),
        r#ref: source.r#ref.clone(),
        subdir: source.subdir.clone(),
        commit: commit.to_string(),
        fetched_at: now.clone(),
        checked_at: now,
        last_error: None,
        error_at: None,
        error_reported: false,
        warnings: warnings.clone(),
    };
    snapshot::activate(dir, &meta)?;
    Ok(SyncReport {
        catalog: source.name.clone(),
        outcome: SyncOutcome::Updated {
            commit: commit.to_string(),
            previous: previous.map(|p| p.commit.clone()).filter(|c| !c.is_empty()),
            downloaded,
            reused,
            transport: transport.to_string(),
        },
        warnings,
    })
}
