//! The key store file: hashed key records, atomic owner-only writes.
//!
//! Format: JSON `{"version": 1, "keys": [KeyRecord, ...]}` at `ResolvedPaths::key_store`
//! (default `<data_dir>/keys.db`). Writers (the CLI today, the admin API later) take a lock
//! file, read, modify, write a temporary file with mode 0600, fsync it and rename it over
//! the store, so a reader never sees a half-written file. The server only reads it.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::token::{self, ApiKeySecret, KEY_PREFIX};
use super::{KeyError, Scope};

/// Current file format version.
pub const FORMAT_VERSION: u32 = 1;

/// Longest accepted key name, in characters.
pub const MAX_NAME_LEN: usize = 64;

/// One stored key. Holds no secret: only the salt and the salted hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyRecord {
    /// Public id (the 8 characters after `alk_`).
    pub id: String,
    /// Human label, unique among active keys.
    pub name: String,
    /// Per-key salt, hex.
    pub salt: String,
    /// `SHA-256(salt || key)`, hex.
    pub hash: String,
    /// Creation time.
    #[serde(with = "time::serde::rfc3339")]
    pub created: OffsetDateTime,
    /// Granted scopes.
    pub scopes: Vec<Scope>,
    /// Models the key may use; `None` means every model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,
    /// Expiry; `None` means the key does not expire.
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub expires: Option<OffsetDateTime>,
    /// Revocation time; a revoked key never authenticates again.
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub revoked: Option<OffsetDateTime>,
}

impl KeyRecord {
    /// Non-secret display form, `alk_<id>_…`.
    pub fn display_prefix(&self) -> String {
        format!("{KEY_PREFIX}{}_…", self.id)
    }

    /// True when the key is neither revoked nor expired at `now`.
    pub fn is_active(&self, now: OffsetDateTime) -> bool {
        self.revoked.is_none() && self.expires.is_none_or(|e| e > now)
    }

    /// Lifecycle state at `now`, for listings.
    pub fn status(&self, now: OffsetDateTime) -> &'static str {
        if self.revoked.is_some() {
            "revoked"
        } else if self.expires.is_some_and(|e| e <= now) {
            "expired"
        } else {
            "active"
        }
    }

    /// Checks `key` against the stored salted hash in constant time.
    pub(crate) fn matches(&self, key: &ApiKeySecret) -> bool {
        let Ok(salt) = hex::decode(&self.salt) else {
            return false;
        };
        let computed = token::hash_key(&salt, key);
        token::constant_time_eq(computed.as_bytes(), self.hash.as_bytes())
    }
}

/// What to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewKey {
    /// Label.
    pub name: String,
    /// Scopes; empty means `inference`.
    pub scopes: Vec<Scope>,
    /// Optional model allowlist.
    pub models: Option<Vec<String>>,
    /// Optional expiry.
    pub expires: Option<OffsetDateTime>,
}

impl NewKey {
    /// An inference key with no allowlist and no expiry.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            scopes: vec![Scope::Inference],
            models: None,
            expires: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct KeyFile {
    version: u32,
    keys: Vec<KeyRecord>,
}

/// Handle on the key store file.
#[derive(Debug, Clone)]
pub struct KeyStore {
    path: PathBuf,
}

impl KeyStore {
    /// A store at `path`; nothing is read or created until used.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The store file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads every record. A missing file is an empty store.
    pub fn load(&self) -> Result<Vec<KeyRecord>, KeyError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(KeyError::io(&self.path, e)),
        };
        warn_if_permissive(&self.path);
        let file: KeyFile = serde_json::from_str(&text).map_err(|e| KeyError::Corrupt {
            path: self.path.clone(),
            message: e.to_string(),
        })?;
        if file.version != FORMAT_VERSION {
            return Err(KeyError::UnsupportedVersion {
                path: self.path.clone(),
                version: file.version,
            });
        }
        Ok(file.keys)
    }

    /// Creates a key, stores its salted hash and returns the record plus the secret, which
    /// the caller must show once and then drop.
    pub fn create(
        &self,
        spec: NewKey,
        now: OffsetDateTime,
    ) -> Result<(KeyRecord, ApiKeySecret), KeyError> {
        let name = validate_name(&spec.name)?;
        let _lock = StoreLock::acquire(&self.path)?;
        let mut keys = self.load()?;
        if keys.iter().any(|k| k.is_active(now) && k.name == name) {
            return Err(KeyError::DuplicateName(name));
        }
        let id = loop {
            let id = token::generate_id()?;
            if !keys.iter().any(|k| k.id == id) {
                break id;
            }
        };
        let secret = token::generate_key(&id)?;
        let salt = token::generate_salt()?;
        let mut scopes = if spec.scopes.is_empty() {
            vec![Scope::Inference]
        } else {
            spec.scopes
        };
        scopes.sort();
        scopes.dedup();
        let record = KeyRecord {
            id,
            name,
            hash: token::hash_key(&salt, &secret),
            salt: hex::encode(salt),
            created: now,
            scopes,
            models: spec.models.filter(|m| !m.is_empty()),
            expires: spec.expires,
            revoked: None,
        };
        keys.push(record.clone());
        self.save(&keys)?;
        Ok((record, secret))
    }

    /// Revokes the key named by id (`abcd1234` or `alk_abcd1234`) or by active name.
    pub fn revoke(&self, id_or_name: &str, now: OffsetDateTime) -> Result<KeyRecord, KeyError> {
        let _lock = StoreLock::acquire(&self.path)?;
        let mut keys = self.load()?;
        let wanted = id_or_name.trim();
        let id = wanted.strip_prefix(KEY_PREFIX).unwrap_or(wanted);
        let id = id.trim_end_matches(['_', '…']);
        let index = keys
            .iter()
            .position(|k| k.id == id)
            .or_else(|| {
                keys.iter()
                    .position(|k| k.revoked.is_none() && k.name == wanted)
            })
            .ok_or_else(|| KeyError::NotFound(wanted.to_string()))?;
        let record = &mut keys[index];
        if record.revoked.is_some() {
            return Err(KeyError::AlreadyRevoked(record.id.clone()));
        }
        record.revoked = Some(now);
        let revoked = record.clone();
        self.save(&keys)?;
        Ok(revoked)
    }

    /// Atomically replaces the store with `keys`.
    fn save(&self, keys: &[KeyRecord]) -> Result<(), KeyError> {
        let file = KeyFile {
            version: FORMAT_VERSION,
            keys: keys.to_vec(),
        };
        let mut text = serde_json::to_string_pretty(&file).map_err(|e| KeyError::Corrupt {
            path: self.path.clone(),
            message: e.to_string(),
        })?;
        text.push('\n');
        write_private_atomic(&self.path, text.as_bytes())
    }
}

/// Validates and trims a key name.
fn validate_name(name: &str) -> Result<String, KeyError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_LEN || name.chars().any(char::is_control)
    {
        return Err(KeyError::InvalidName(format!(
            "names must be 1 to {MAX_NAME_LEN} printable characters"
        )));
    }
    Ok(name.to_string())
}

/// Writes `bytes` to a sibling temporary file readable only by the owner, syncs it and
/// renames it over `path`.
pub(crate) fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<(), KeyError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir).map_err(|e| KeyError::io(dir, e))?;
    let mut suffix = [0u8; 6];
    token::os_random(&mut suffix)?;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "keys".into());
    let tmp = dir.join(format!(".{file_name}.{}.tmp", hex::encode(suffix)));
    let result = (|| {
        let mut f = private_create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(KeyError::io(path, e));
    }
    Ok(())
}

/// Creates a new file with owner-only permissions (0600 on Unix, a protected owner-only DACL
/// on Windows; see [`crate::private_file`]).
fn private_create(path: &Path) -> io::Result<File> {
    crate::private_file::create_new(path)
}

#[cfg(unix)]
fn warn_if_permissive(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = fs::metadata(path)
        && meta.permissions().mode() & 0o077 != 0
    {
        tracing::warn!(
            path = %path.display(),
            "key store is readable by other users; run `chmod 600` on it"
        );
    }
}

#[cfg(not(unix))]
fn warn_if_permissive(_path: &Path) {}

/// Exclusive writer lock (`<store>.lock`, created with `create_new`). A lock older than
/// [`STALE_LOCK`] is assumed to belong to a crashed writer and is broken.
struct StoreLock {
    path: PathBuf,
}

const STALE_LOCK: Duration = Duration::from_secs(30);

impl StoreLock {
    fn acquire(store: &Path) -> Result<Self, KeyError> {
        let mut name = store.as_os_str().to_os_string();
        name.push(".lock");
        let path = PathBuf::from(name);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| KeyError::io(dir, e))?;
        }
        for _ in 0..100 {
            match private_create(&path) {
                Ok(_) => return Ok(Self { path }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    let stale = fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| SystemTime::now().duration_since(t).ok())
                        .is_some_and(|age| age > STALE_LOCK);
                    if stale {
                        let _ = fs::remove_file(&path);
                    } else {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
                Err(e) => return Err(KeyError::io(&path, e)),
            }
        }
        Err(KeyError::Locked(path))
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap()
    }

    #[test]
    fn create_stores_only_a_salted_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KeyStore::new(tmp.path().join("keys.db"));
        let (record, secret) = store.create(NewKey::named("laptop"), now()).unwrap();
        let text = fs::read_to_string(store.path()).unwrap();
        assert!(!text.contains(secret.expose()));
        assert!(!text.contains(&secret.expose()[13..]));
        assert!(text.contains(&record.hash));
        assert!(record.matches(&secret));
        assert_eq!(record.scopes, vec![Scope::Inference]);
        assert_eq!(store.load().unwrap(), vec![record]);
    }

    #[cfg(unix)]
    #[test]
    fn store_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let store = KeyStore::new(tmp.path().join("data").join("keys.db"));
        store.create(NewKey::named("a"), now()).unwrap();
        let mode = fs::metadata(store.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        // No temporary or lock file is left behind.
        let names: Vec<_> = fs::read_dir(tmp.path().join("data"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
    }

    #[test]
    fn same_salt_never_repeats() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KeyStore::new(tmp.path().join("keys.db"));
        let (a, _) = store.create(NewKey::named("a"), now()).unwrap();
        let (b, _) = store.create(NewKey::named("b"), now()).unwrap();
        assert_ne!(a.salt, b.salt);
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn active_names_are_unique_but_reusable_after_revoke() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KeyStore::new(tmp.path().join("keys.db"));
        store.create(NewKey::named("ci"), now()).unwrap();
        assert!(matches!(
            store.create(NewKey::named("ci"), now()),
            Err(KeyError::DuplicateName(_))
        ));
        store.revoke("ci", now()).unwrap();
        store.create(NewKey::named("ci"), now()).unwrap();
    }

    #[test]
    fn revoke_by_id_prefix_or_name() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KeyStore::new(tmp.path().join("keys.db"));
        let (a, _) = store.create(NewKey::named("a"), now()).unwrap();
        let (b, _) = store.create(NewKey::named("b"), now()).unwrap();
        assert!(store.revoke(&a.id, now()).unwrap().revoked.is_some());
        assert!(
            store
                .revoke(&b.display_prefix(), now())
                .unwrap()
                .revoked
                .is_some()
        );
        assert!(matches!(
            store.revoke(&a.id, now()),
            Err(KeyError::AlreadyRevoked(_))
        ));
        assert!(matches!(
            store.revoke("nope", now()),
            Err(KeyError::NotFound(_))
        ));
        assert!(store.load().unwrap().iter().all(|k| !k.is_active(now())));
    }

    #[test]
    fn invalid_names_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KeyStore::new(tmp.path().join("keys.db"));
        for bad in ["", "   ", "a\nb", &"x".repeat(65)] {
            assert!(matches!(
                store.create(NewKey::named(bad), now()),
                Err(KeyError::InvalidName(_))
            ));
        }
    }

    #[test]
    fn expiry_and_status() {
        let mut spec = NewKey::named("tmp");
        spec.expires = Some(now() + time::Duration::hours(1));
        spec.scopes = vec![Scope::Admin, Scope::Inference, Scope::Admin];
        spec.models = Some(vec!["llama3".into()]);
        let tmp = tempfile::tempdir().unwrap();
        let store = KeyStore::new(tmp.path().join("keys.db"));
        let (rec, _) = store.create(spec, now()).unwrap();
        assert_eq!(rec.scopes, vec![Scope::Inference, Scope::Admin]);
        assert_eq!(rec.status(now()), "active");
        assert_eq!(rec.status(now() + time::Duration::hours(2)), "expired");
        assert_eq!(store.load().unwrap()[0], rec);
    }

    #[test]
    fn corrupt_and_future_files_are_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("keys.db");
        fs::write(&path, "not json").unwrap();
        assert!(matches!(
            KeyStore::new(&path).load(),
            Err(KeyError::Corrupt { .. })
        ));
        fs::write(&path, r#"{"version":99,"keys":[]}"#).unwrap();
        assert!(matches!(
            KeyStore::new(&path).load(),
            Err(KeyError::UnsupportedVersion { .. })
        ));
    }

    #[test]
    fn stale_lock_is_broken() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KeyStore::new(tmp.path().join("keys.db"));
        let lock = tmp.path().join("keys.db.lock");
        fs::write(&lock, "").unwrap();
        let old = SystemTime::now() - Duration::from_secs(120);
        File::options()
            .write(true)
            .open(&lock)
            .unwrap()
            .set_modified(old)
            .unwrap();
        store.create(NewKey::named("a"), now()).unwrap();
        assert!(!lock.exists());
    }
}
