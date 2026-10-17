//! Reading the configured catalogs and layering them (ALK-SP-0011.R10, R12): built-in recipes <
//! git catalogs in configured order < `recipes/` next to the binary < the user directory.
//! Model entries layer among git catalogs on `(name, tag)`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use alkahest_core::config::CatalogSource;
use alkahest_recipes::{Catalog, DiscoveryIssue, RecipeLayer};
use serde::Serialize;

use crate::content::{CatalogContent, ValidateOptions, validate};
use crate::model::{ModelEntry, ModelVariant};
use crate::snapshot::{self, SnapshotMeta, read_meta, read_snapshot};

/// After a failed sync, the next attempt is due this soon (or at the refresh interval, if
/// shorter), so one network hiccup does not leave a catalog stale for a day.
pub const RETRY_AFTER_ERROR: Duration = Duration::from_secs(15 * 60);

/// One configured catalog as found on disk.
#[derive(Debug, Clone, Serialize)]
pub struct CatalogState {
    /// The configured source.
    pub source: CatalogSource,
    /// `snapshot.toml`, when the catalog was ever synced (or failed to).
    pub meta: Option<SnapshotMeta>,
    /// The validated snapshot content; `None` when disabled, never fetched or unreadable.
    #[serde(skip)]
    pub content: Option<CatalogContent>,
    /// Why an existing snapshot could not be used.
    pub load_error: Option<String>,
}

impl CatalogState {
    /// The commit of the snapshot in use.
    pub fn commit(&self) -> Option<&str> {
        self.content
            .as_ref()
            .and(self.meta.as_ref())
            .map(|m| m.commit.as_str())
            .filter(|c| !c.is_empty())
    }

    /// `official@2d45ae95a500` style label of the layer.
    pub fn label(&self) -> String {
        match self.commit() {
            Some(c) => format!("{}@{}", self.source.name, &c[..c.len().min(12)]),
            None => self.source.name.clone(),
        }
    }

    /// Whether the catalog should be checked again: enabled, not pinned to the commit it
    /// already has, and last checked longer ago than its refresh interval (or never).
    pub fn is_due(&self) -> bool {
        if !self.source.enabled {
            return false;
        }
        if let (Some(pin), Some(commit)) = (&self.source.pin, self.commit())
            && pin == commit
        {
            return false;
        }
        let mut interval = self
            .source
            .refresh_duration()
            .unwrap_or(Duration::from_secs(86_400));
        if self.meta.as_ref().is_some_and(|m| m.last_error.is_some()) {
            interval = interval.min(RETRY_AFTER_ERROR);
        }
        match self.meta.as_ref().and_then(SnapshotMeta::checked_age) {
            Some(age) => age >= interval,
            None => true,
        }
    }

    /// A recorded sync error not shown to the user yet.
    pub fn unreported_error(&self) -> Option<&str> {
        self.meta
            .as_ref()
            .filter(|m| !m.error_reported)
            .and_then(|m| m.last_error.as_deref())
    }
}

/// Loads every configured catalog's snapshot from `<data_dir>/catalogs/`. Disabled catalogs
/// are listed with no content. Snapshots are re-validated leniently, so a snapshot written by
/// a newer Alkahest that this build cannot read contributes nothing instead of failing.
pub fn load_states(data_dir: &Path, sources: &[CatalogSource]) -> Vec<CatalogState> {
    sources
        .iter()
        .map(|source| {
            let dir = snapshot::snapshot_dir(data_dir, &source.name);
            let (meta, content, load_error) = match read_snapshot(&dir) {
                Some((meta, files)) if source.enabled => {
                    match validate(&files, &ValidateOptions::lenient(&source.name)) {
                        Ok(c) => (Some(meta), Some(c), None),
                        Err(e) => (Some(meta), None, Some(e.to_string())),
                    }
                }
                _ => (read_meta(&dir), None, None),
            };
            CatalogState {
                source: source.clone(),
                meta,
                content,
                load_error,
            }
        })
        .collect()
}

/// The layered recipe catalog: built-in, then each git catalog snapshot in configured order,
/// then `dirs` (the binary's `recipes/`, then the user's), later layers winning on
/// `(id, version, variant)`. Unusable local recipe files are returned as issues.
pub fn recipe_catalog(states: &[CatalogState], dirs: &[PathBuf]) -> (Catalog, Vec<DiscoveryIssue>) {
    let mut catalog = Catalog::builtin();
    for state in states {
        if let Some(content) = &state.content {
            catalog.extend_layer(
                content.recipes().cloned(),
                RecipeLayer::GitCatalog {
                    name: state.source.name.clone(),
                    commit: state.commit().unwrap_or_default().to_string(),
                },
            );
        }
    }
    let mut issues = Vec::new();
    for dir in dirs {
        let (next, found) = catalog.with_dir(dir);
        catalog = next;
        issues.extend(found);
    }
    (catalog, issues)
}

/// A model variant with the catalog it comes from.
#[derive(Debug, Clone, Serialize)]
pub struct CatalogModel {
    /// Catalog name.
    pub catalog: String,
    /// Commit of the snapshot.
    pub commit: String,
    /// The whole entry as that catalog defines it.
    pub entry: ModelEntry,
    /// The tag this row is about.
    pub variant: ModelVariant,
}

/// Model entries of every catalog, layered on `(name, tag)`: a later catalog replaces the
/// variant of an earlier one. Sorted by name, then by the tag order of the winning entry.
pub fn layered_models(states: &[CatalogState]) -> Vec<CatalogModel> {
    let mut out: Vec<CatalogModel> = Vec::new();
    for state in states {
        let Some(content) = &state.content else {
            continue;
        };
        for entry in &content.models {
            for v in &entry.variants {
                out.retain(|m| !(m.entry.name == entry.name && m.variant.tag == v.tag));
                out.push(CatalogModel {
                    catalog: state.source.name.clone(),
                    commit: state.commit().unwrap_or_default().to_string(),
                    entry: entry.clone(),
                    variant: v.clone(),
                });
            }
        }
    }
    out.sort_by(|a, b| a.entry.name.cmp(&b.entry.name));
    out
}

/// Finds the model `name` (`<namespace>/<model>`) with `tag` (the default tag when `None`) in
/// catalog `source` (the highest layer that has the entry when `None`). This is the lookup
/// ALK-US-0120 builds `pull` and `search` on.
pub fn find_model(
    states: &[CatalogState],
    source: Option<&str>,
    name: &str,
    tag: Option<&str>,
) -> Option<CatalogModel> {
    let mut candidates = states
        .iter()
        .filter(|s| source.is_none_or(|want| s.source.name == want))
        .filter_map(|s| {
            let c = s.content.as_ref()?;
            let entry = c.models.iter().find(|m| m.name == name)?;
            Some((s, entry))
        });
    match tag {
        None => {
            let (state, entry) = candidates.next_back()?;
            let default = entry.default_tag()?.to_string();
            let layered = layered_models(states);
            layered
                .into_iter()
                .find(|m| {
                    m.entry.name == name
                        && m.variant.tag == default
                        && source.is_none_or(|w| m.catalog == w)
                })
                .or_else(|| {
                    Some(CatalogModel {
                        catalog: state.source.name.clone(),
                        commit: state.commit().unwrap_or_default().to_string(),
                        entry: entry.clone(),
                        variant: entry.variant(None)?.clone(),
                    })
                })
        }
        Some(t) => candidates
            .filter_map(|(s, e)| {
                Some(CatalogModel {
                    catalog: s.source.name.clone(),
                    commit: s.commit().unwrap_or_default().to_string(),
                    entry: e.clone(),
                    variant: e.variant(Some(t))?.clone(),
                })
            })
            .next_back(),
    }
}

/// Catalog models matching `query`, one row per `(name, tag)` after layering (R12): every
/// whitespace-separated word of `query` must appear, case-insensitively, in the entry name,
/// description, licence, tags, the variant tag or its quant. An empty query lists everything.
/// `catalog` keeps only rows whose winning entry comes from that catalog.
pub fn search_models(
    states: &[CatalogState],
    query: &str,
    catalog: Option<&str>,
) -> Vec<CatalogModel> {
    let words: Vec<String> = query
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect();
    let scope: Vec<CatalogState> = match catalog {
        // Layering only within the chosen catalog, so its own rows are shown even when a
        // later catalog overrides them.
        Some(c) => states
            .iter()
            .filter(|s| s.source.name == c)
            .cloned()
            .collect(),
        None => states.to_vec(),
    };
    layered_models(&scope)
        .into_iter()
        .filter(|m| {
            let hay = format!(
                "{} {} {} {} {} {}",
                m.entry.name,
                m.entry.description,
                m.entry.license,
                m.entry.tags.join(" "),
                m.variant.tag,
                m.variant.quant.as_deref().unwrap_or_default()
            )
            .to_ascii_lowercase();
            words.iter().all(|w| hay.contains(w.as_str()))
        })
        .collect()
}
