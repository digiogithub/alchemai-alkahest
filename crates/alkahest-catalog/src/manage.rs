//! Catalog administration shared by the CLI (`alkahest catalog ...`) and the admin API
//! (`/alkahest/v1/catalogs`, ALK-US-0124): the listing document, adding, removing and editing
//! sources, private catalog tokens in the OS keyring, and the live set of sources a running
//! server reads.
//!
//! Tokens (ALK-SP-0011.R14) live only in the OS keyring (service `alkahest`, user
//! `catalog:<name>`) or in `ALKAHEST_CATALOG_TOKEN_<NAME>`. Nothing here returns, logs or
//! formats a token: the listing says only whether one is present and where it comes from.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use alkahest_core::config::{CatalogLimits, CatalogSource, OFFICIAL_CATALOG_NAME};
use serde::{Deserialize, Serialize};

use crate::edit::{Removal, SourcePatch, add_source, remove_source, update_source};
use crate::error::{CatalogError, Result};
use crate::http::{CredentialSource, KEYRING_SERVICE, KeyringCredentials, Token, token_env_var};
use crate::layers::CatalogState;
use crate::snapshot::{SyncLock, catalogs_root, snapshot_dir, sync_in_progress};

/// Environment switch: `0`, `false`, `no` or `off` turns the automatic catalog refresh off (CI,
/// air-gapped machines); explicit syncs still work.
pub const AUTO_SYNC_ENV: &str = "ALKAHEST_CATALOG_AUTO_SYNC";

/// Whether the automatic refresh is on, given the value of [`AUTO_SYNC_ENV`].
pub fn auto_sync_enabled(value: Option<&str>) -> bool {
    !value.is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        )
    })
}

/// Keyring user of catalog `name`'s token: `catalog:<name>`.
pub fn keyring_user(name: &str) -> String {
    format!("catalog:{name}")
}

/// Where private catalog tokens are stored. The real one is [`KeyringTokenStore`]; tests and
/// keyring-less embeddings use [`MemoryTokenStore`].
pub trait CatalogTokenStore: Send + Sync {
    /// The stored token of catalog `name`, if any.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Keyring`] when the store cannot be read.
    fn get(&self, name: &str) -> Result<Option<Token>>;
    /// Stores (replaces) the token of catalog `name`.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Keyring`] when the store cannot be written.
    fn set(&self, name: &str, token: &Token) -> Result<()>;
    /// Removes the token of catalog `name`; absent is fine.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Keyring`] when the store cannot be written.
    fn delete(&self, name: &str) -> Result<()>;
}

/// The OS keyring: service `alkahest`, user `catalog:<name>`.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringTokenStore;

impl KeyringTokenStore {
    fn entry(name: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(KEYRING_SERVICE, &keyring_user(name))
            .map_err(|e| CatalogError::Keyring(e.to_string()))
    }
}

impl CatalogTokenStore for KeyringTokenStore {
    fn get(&self, name: &str) -> Result<Option<Token>> {
        match Self::entry(name)?.get_password() {
            Ok(v) => Ok(Token::new(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(CatalogError::Keyring(e.to_string())),
        }
    }

    fn set(&self, name: &str, token: &Token) -> Result<()> {
        Self::entry(name)?
            .set_password(token.expose())
            .map_err(|e| CatalogError::Keyring(e.to_string()))
    }

    fn delete(&self, name: &str) -> Result<()> {
        match Self::entry(name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(CatalogError::Keyring(e.to_string())),
        }
    }
}

/// Tokens kept in memory (tests, machines without a keyring).
#[derive(Debug, Default)]
pub struct MemoryTokenStore(Mutex<BTreeMap<String, Token>>);

impl CatalogTokenStore for MemoryTokenStore {
    fn get(&self, name: &str) -> Result<Option<Token>> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .cloned())
    }

    fn set(&self, name: &str, token: &Token) -> Result<()> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.to_string(), token.clone());
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<()> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(name);
        Ok(())
    }
}

/// Environment lookup (`ALKAHEST_CATALOG_TOKEN_<NAME>`); injectable for tests.
pub type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Where the token of a catalog comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenSource {
    /// `ALKAHEST_CATALOG_TOKEN_<NAME>` (wins over the keyring).
    Env,
    /// The token store (OS keyring).
    Keyring,
    /// No token: the catalog is read anonymously.
    None,
}

/// Where the token of catalog `name` comes from: the environment first, then `store`. A store
/// that cannot be read counts as "no token".
pub fn token_source(name: &str, store: &dyn CatalogTokenStore, env: &EnvLookup) -> TokenSource {
    if env(&token_env_var(name)).is_some_and(|v| !v.trim().is_empty()) {
        return TokenSource::Env;
    }
    match store.get(name) {
        Ok(Some(_)) => TokenSource::Keyring,
        _ => TokenSource::None,
    }
}

/// [`CredentialSource`] over a [`CatalogTokenStore`]: `ALKAHEST_CATALOG_TOKEN_<NAME>`, then the
/// store; host tokens as [`KeyringCredentials`] reads them.
pub struct StoreCredentials {
    store: Arc<dyn CatalogTokenStore>,
    env: EnvLookup,
}

impl StoreCredentials {
    /// Credentials from `store` with `env` consulted first.
    pub fn new(store: Arc<dyn CatalogTokenStore>, env: EnvLookup) -> Self {
        Self { store, env }
    }
}

impl std::fmt::Debug for StoreCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreCredentials").finish_non_exhaustive()
    }
}

impl CredentialSource for StoreCredentials {
    fn catalog_token(&self, name: &str) -> Option<Token> {
        if let Some(t) = (self.env)(&token_env_var(name)).and_then(Token::new) {
            return Some(t);
        }
        self.store.get(name).ok().flatten()
    }

    fn host_token(&self, host: &str) -> Option<Token> {
        KeyringCredentials.host_token(host)
    }
}

/// One catalog as `alkahest catalog ls --json` and `GET /alkahest/v1/catalogs` list it: the
/// configured source, the snapshot state and whether a token is present (never the token).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogInfo {
    /// Layer position, 1 = lowest (later catalogs override earlier ones).
    pub position: usize,
    /// Catalog name.
    pub name: String,
    /// Repository URL (`https://`) or `file://` directory.
    pub url: String,
    /// Branch or tag followed.
    #[serde(rename = "ref")]
    pub r#ref: String,
    /// Folder inside the repository.
    pub subdir: String,
    /// Commit the catalog is frozen at.
    pub pin: Option<String>,
    /// Whether the catalog contributes entries.
    pub enabled: bool,
    /// Refresh interval (`24h`).
    pub refresh_interval: String,
    /// The official catalog (cannot be removed, only disabled).
    pub official: bool,
    /// Commit of the snapshot in use.
    pub commit: Option<String>,
    /// When the content was fetched (RFC 3339).
    pub fetched_at: Option<String>,
    /// When the remote was last checked (RFC 3339).
    pub checked_at: Option<String>,
    /// When the last sync failed (RFC 3339).
    pub error_at: Option<String>,
    /// The last sync error; the previous snapshot stays in use.
    pub last_error: Option<String>,
    /// Whether a command already showed `last_error`.
    pub error_reported: bool,
    /// Why the snapshot on disk cannot be used.
    pub load_error: Option<String>,
    /// Engine recipe variants of the snapshot.
    pub engines: Option<usize>,
    /// Model entries of the snapshot.
    pub models: Option<usize>,
    /// Validation warnings of the snapshot.
    pub warnings: Vec<String>,
    /// One-line status (`ok`, `pinned`, `not fetched`, `disabled`, plus failures).
    pub status: String,
    /// A token is available for this catalog (keyring or environment).
    pub token_present: bool,
    /// Where the token comes from.
    pub token_source: TokenSource,
    /// A sync of this catalog runs now (in this or another process).
    pub syncing: bool,
}

/// The one-line status of a catalog.
pub fn status_text(state: &CatalogState) -> String {
    if !state.source.enabled {
        return "disabled".into();
    }
    let mut parts = Vec::new();
    if let Some(e) = &state.load_error {
        parts.push(format!("unusable snapshot: {e}"));
    } else if state.content.is_none() {
        parts.push("not fetched".into());
    } else if state.source.pin.is_some() {
        parts.push("pinned".into());
    } else {
        parts.push("ok".into());
    }
    if let Some(e) = state.meta.as_ref().and_then(|m| m.last_error.as_deref()) {
        parts.push(format!("last sync failed: {e}"));
    }
    if let Some(c) = &state.content
        && !c.warnings.is_empty()
    {
        parts.push(format!("{} warning(s)", c.warnings.len()));
    }
    parts.join("; ")
}

/// The listing of `states` (in configured order). `token_of` says where each catalog's token
/// comes from; `data_dir` is used to see whether a sync runs.
pub fn describe(
    data_dir: &Path,
    states: &[CatalogState],
    token_of: impl Fn(&str) -> TokenSource,
) -> Vec<CatalogInfo> {
    let time = |t: Option<&String>| t.filter(|t| !t.is_empty()).cloned();
    states
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let meta = s.meta.as_ref();
            let token = token_of(&s.source.name);
            CatalogInfo {
                position: i + 1,
                name: s.source.name.clone(),
                url: s.source.url.clone(),
                r#ref: s.source.r#ref.clone(),
                subdir: s.source.subdir.clone(),
                pin: s.source.pin.clone(),
                enabled: s.source.enabled,
                refresh_interval: s.source.refresh_interval.clone(),
                official: s.source.name == OFFICIAL_CATALOG_NAME,
                commit: s.commit().map(str::to_string),
                fetched_at: time(meta.map(|m| &m.fetched_at)),
                checked_at: time(meta.map(|m| &m.checked_at)),
                error_at: meta.and_then(|m| m.error_at.clone()),
                last_error: meta.and_then(|m| m.last_error.clone()),
                error_reported: meta.is_some_and(|m| m.error_reported),
                load_error: s.load_error.clone(),
                engines: s.content.as_ref().map(|c| c.recipe_count()),
                models: s.content.as_ref().map(|c| c.models.len()),
                warnings: s
                    .content
                    .as_ref()
                    .map(|c| c.warnings.iter().map(ToString::to_string).collect())
                    .unwrap_or_default(),
                status: status_text(s),
                token_present: token != TokenSource::None,
                token_source: token,
                syncing: sync_in_progress(data_dir, &s.source.name),
            }
        })
        .collect()
}

/// The catalog sources and limits a running server works with. Edits through the admin API
/// replace them, so every reader (engine listing, catalog search, pulls, the background
/// refresh) sees the change at once, without a restart. Cheap to clone.
#[derive(Debug, Clone)]
pub struct CatalogSet {
    inner: Arc<RwLock<(Vec<CatalogSource>, CatalogLimits)>>,
}

impl CatalogSet {
    /// A set holding `sources` and `limits`.
    pub fn new(sources: Vec<CatalogSource>, limits: CatalogLimits) -> Self {
        Self {
            inner: Arc::new(RwLock::new((sources, limits))),
        }
    }

    /// The sources, in layer order.
    pub fn sources(&self) -> Vec<CatalogSource> {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .0
            .clone()
    }

    /// The snapshot limits.
    pub fn limits(&self) -> CatalogLimits {
        self.inner.read().unwrap_or_else(PoisonError::into_inner).1
    }

    /// Replaces the sources.
    pub fn set_sources(&self, sources: Vec<CatalogSource>) {
        self.inner.write().unwrap_or_else(PoisonError::into_inner).0 = sources;
    }

    /// Replaces the limits.
    pub fn set_limits(&self, limits: CatalogLimits) {
        self.inner.write().unwrap_or_else(PoisonError::into_inner).1 = limits;
    }
}

/// Adds `source` last in the layer order of the configuration file `file` (comments kept)
/// and returns the sources now in force.
///
/// # Errors
///
/// [`CatalogError::Config`] for an invalid source or a name in use.
pub fn add_catalog(
    file: &Path,
    source: &CatalogSource,
    effective: &[CatalogSource],
) -> Result<Vec<CatalogSource>> {
    add_source(file, source, effective)?;
    let mut next = effective.to_vec();
    next.push(source.clone());
    Ok(next)
}

/// What [`remove_catalog`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemoveOutcome {
    /// Removed, or (official) disabled.
    pub result: Removal,
    /// The snapshot directory was deleted.
    pub snapshot_deleted: bool,
    /// The keyring token was deleted.
    pub token_deleted: bool,
    /// The sources now in force.
    #[serde(skip)]
    pub sources: Vec<CatalogSource>,
}

/// Removes catalog `name` from the configuration file `file`, deletes its snapshot and, when
/// `tokens` is given, its keyring token. The official catalog is disabled instead, and its
/// snapshot kept so enabling it again needs no download.
///
/// # Errors
///
/// [`CatalogError::Unknown`], [`CatalogError::Busy`] while a sync of the catalog runs,
/// [`CatalogError::Io`], [`CatalogError::Keyring`].
pub fn remove_catalog(
    file: &Path,
    data_dir: &Path,
    name: &str,
    effective: &[CatalogSource],
    tokens: Option<&dyn CatalogTokenStore>,
) -> Result<RemoveOutcome> {
    if !effective.iter().any(|s| s.name == name) {
        return Err(CatalogError::Unknown(name.to_string()));
    }
    let official = name == OFFICIAL_CATALOG_NAME;
    // Held while the configuration and the snapshot change, so no sync writes a snapshot of
    // a catalog that is going away.
    let lock = SyncLock::acquire(&catalogs_root(data_dir), name)?;
    let result = remove_source(file, name, effective)?;
    let mut snapshot_deleted = false;
    if !official {
        let dir = snapshot_dir(data_dir, name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| CatalogError::io(&dir, e))?;
            snapshot_deleted = true;
        }
    }
    drop(lock);
    if !official {
        // The lock file of a catalog that no longer exists.
        let _ = std::fs::remove_file(catalogs_root(data_dir).join(format!(".lock-{name}")));
    }
    let mut token_deleted = false;
    if let Some(store) = tokens {
        store.delete(name)?;
        token_deleted = true;
    }
    let mut sources: Vec<CatalogSource> = effective.to_vec();
    if official {
        for s in sources.iter_mut().filter(|s| s.name == name) {
            s.enabled = false;
        }
    } else {
        sources.retain(|s| s.name != name);
        if sources.is_empty() {
            let mut o = CatalogSource::official();
            o.enabled = false;
            sources.push(o);
        }
    }
    Ok(RemoveOutcome {
        result,
        snapshot_deleted,
        token_deleted,
        sources,
    })
}

/// Applies `patch` to catalog `name` in the configuration file `file` and returns the updated
/// source and the sources now in force.
///
/// # Errors
///
/// [`CatalogError::Unknown`], [`CatalogError::Config`] for an empty patch or a result that
/// breaks a rule of ALK-SP-0011.R9.
pub fn edit_catalog(
    file: &Path,
    name: &str,
    patch: &SourcePatch,
    effective: &[CatalogSource],
) -> Result<(CatalogSource, Vec<CatalogSource>)> {
    if patch.is_empty() {
        return Err(CatalogError::Config(
            "nothing to change: set enabled, ref, subdir, pin or refresh_interval".into(),
        ));
    }
    let updated = update_source(file, name, patch, effective)?;
    let sources = effective
        .iter()
        .map(|s| {
            if s.name == name {
                updated.clone()
            } else {
                s.clone()
            }
        })
        .collect();
    Ok((updated, sources))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alkahest_core::Config;

    fn read(path: &Path) -> Config {
        Config::from_toml_str(&std::fs::read_to_string(path).unwrap_or_default())
            .unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn add_edit_and_remove_keep_comments_and_the_official_catalog() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let file = dir.path().join("config.toml");
        let data = dir.path().join("data");
        std::fs::write(&file, "# mine\n[server]\nport = 1 # keep\n").unwrap_or_default();
        let effective = vec![CatalogSource::official()];
        let acme = CatalogSource::new("acme", "https://git.example.org/acme/catalog");
        let effective = add_catalog(&file, &acme, &effective).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(effective.len(), 2);

        let patch: SourcePatch = serde_json::from_str(
            r#"{"enabled": false, "ref": "stable", "refresh_interval": "1h",
                "pin": "2d45ae95a500e22fa79fcd1c3c6c4ecdc4ea5a29"}"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let (updated, effective) =
            edit_catalog(&file, "acme", &patch, &effective).unwrap_or_else(|e| panic!("{e}"));
        assert!(!updated.enabled);
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        assert!(
            text.contains("# mine") && text.contains("port = 1 # keep"),
            "{text}"
        );
        let cfg = read(&file);
        assert_eq!(cfg.catalogs[1], updated);
        assert_eq!(cfg.catalogs[1].r#ref, "stable");

        // `pin: null` unpins; an absent field stays.
        let unpin: SourcePatch =
            serde_json::from_str(r#"{"pin": null}"#).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(unpin.pin, Some(None));
        let (updated, effective) =
            edit_catalog(&file, "acme", &unpin, &effective).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(updated.pin, None);
        assert_eq!(updated.r#ref, "stable");
        assert_eq!(read(&file).catalogs[1].pin, None);

        for bad in [
            r#"{"pin": "main"}"#,
            r#"{"subdir": "../x"}"#,
            r#"{"refresh_interval": "soon"}"#,
            r#"{"ref": "-x"}"#,
        ] {
            let p: SourcePatch = serde_json::from_str(bad).unwrap_or_else(|e| panic!("{e}"));
            assert!(
                edit_catalog(&file, "acme", &p, &effective).is_err(),
                "{bad}"
            );
        }
        assert!(edit_catalog(&file, "acme", &SourcePatch::default(), &effective).is_err());
        assert!(serde_json::from_str::<SourcePatch>(r#"{"url": "https://x"}"#).is_err());

        // The official catalog is disabled, never removed; its snapshot stays.
        std::fs::create_dir_all(snapshot_dir(&data, "official")).unwrap_or_default();
        std::fs::create_dir_all(snapshot_dir(&data, "acme")).unwrap_or_default();
        let tokens = MemoryTokenStore::default();
        tokens
            .set(
                "acme",
                &Token::new("s3cr3t").unwrap_or_else(|| panic!("token")),
            )
            .unwrap_or_else(|e| panic!("{e}"));
        let out = remove_catalog(&file, &data, "official", &effective, None)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(out.result, Removal::Disabled);
        assert!(!out.snapshot_deleted && snapshot_dir(&data, "official").exists());
        assert!(!read(&file).catalogs[0].enabled);
        let effective = out.sources;
        let out = remove_catalog(&file, &data, "acme", &effective, Some(&tokens))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(out.result, Removal::Removed);
        assert!(out.snapshot_deleted && out.token_deleted);
        assert!(!snapshot_dir(&data, "acme").exists());
        assert!(tokens.get("acme").unwrap_or_default().is_none());
        assert_eq!(out.sources.len(), 1);
        assert!(matches!(
            remove_catalog(&file, &data, "acme", &out.sources, None),
            Err(CatalogError::Unknown(_))
        ));
    }

    #[test]
    fn token_source_prefers_the_environment() {
        let store = MemoryTokenStore::default();
        let none: EnvLookup = Arc::new(|_| None);
        assert_eq!(token_source("acme", &store, &none), TokenSource::None);
        store
            .set("acme", &Token::new("t").unwrap_or_else(|| panic!("token")))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(token_source("acme", &store, &none), TokenSource::Keyring);
        let env: EnvLookup =
            Arc::new(|k| (k == "ALKAHEST_CATALOG_TOKEN_ACME").then(|| "e".to_string()));
        assert_eq!(token_source("acme", &store, &env), TokenSource::Env);
        let creds = StoreCredentials::new(Arc::new(store), none);
        assert!(creds.catalog_token("acme").is_some());
        assert!(creds.catalog_token("other").is_none());
    }
}
