//! Request-time key verification.
//!
//! [`KeyVerifier`] is the trait the authentication middleware calls; [`FileKeyVerifier`]
//! implements it over the key store file. It keeps the parsed store in memory and re-reads
//! it whenever the file's identity changes (modification time, length or inode), checked
//! with one `stat` per verification. Writers replace the file by rename, so a revocation is
//! seen by the very next request (ALK-SP-0006.R3 "revocation") without a cache to
//! invalidate. Verification itself is one SHA-256 over ~70 bytes, so no result cache is
//! needed either (see `token.rs` for why the hash is not Argon2).

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

use parking_lot::Mutex;
use time::OffsetDateTime;

use super::store::{KeyRecord, KeyStore};
use super::token::{self, SALT_LEN};
use super::{KeyError, Scope};

/// The authenticated identity behind a request that presented a valid key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// Public key id.
    pub key_id: String,
    /// Key name.
    pub name: String,
    /// Granted scopes.
    pub scopes: Vec<Scope>,
    /// Model allowlist; `None` means every model.
    pub models: Option<Vec<String>>,
}

impl Principal {
    /// True when the key grants `scope`. `admin` includes `inference`.
    pub fn has_scope(&self, scope: Scope) -> bool {
        self.scopes.contains(&scope) || self.scopes.contains(&Scope::Admin)
    }

    /// True when the key may use `model`. Handlers call this once they know the model.
    pub fn allows_model(&self, model: &str) -> bool {
        self.models
            .as_ref()
            .is_none_or(|list| list.iter().any(|m| m == model))
    }

    fn from_record(r: &KeyRecord) -> Self {
        Self {
            key_id: r.id.clone(),
            name: r.name.clone(),
            scopes: r.scopes.clone(),
            models: r.models.clone(),
        }
    }
}

/// Why a presented key was not accepted. Logged (never with the key); clients only ever
/// see a generic 401 so the reason cannot be used as an oracle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyRejection {
    /// No key was presented.
    Missing,
    /// The value does not have the `alk_<id>_<secret>` shape.
    Malformed,
    /// No key with that id exists.
    Unknown,
    /// The id exists but the secret does not match.
    Mismatch,
    /// The key was revoked.
    Revoked,
    /// The key has expired.
    Expired,
    /// The key store could not be read; every key is refused (fail closed).
    StoreUnavailable,
}

impl fmt::Display for KeyRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Missing => "missing",
            Self::Malformed => "malformed",
            Self::Unknown => "unknown",
            Self::Mismatch => "mismatch",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
            Self::StoreUnavailable => "store unavailable",
        })
    }
}

/// Verifies presented API keys.
pub trait KeyVerifier: Send + Sync + 'static {
    /// Checks a presented key (the raw header value after `Bearer `).
    fn verify(&self, presented: &str) -> Result<Principal, KeyRejection>;

    /// Number of currently usable keys (used by the exposure check at startup).
    fn active_keys(&self) -> Result<usize, KeyError>;
}

/// File identity used to detect replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
    ino: u64,
}

impl Stamp {
    fn of(path: &Path) -> Option<Self> {
        let meta = fs::metadata(path).ok()?;
        #[cfg(unix)]
        let ino = std::os::unix::fs::MetadataExt::ino(&meta);
        #[cfg(not(unix))]
        let ino = 0;
        Some(Self {
            modified: meta.modified().ok(),
            len: meta.len(),
            ino,
        })
    }
}

#[derive(Debug)]
struct Snapshot {
    stamp: Option<Stamp>,
    keys: Result<HashMap<String, KeyRecord>, String>,
}

/// [`KeyVerifier`] backed by a [`KeyStore`] file, reloaded when the file changes.
#[derive(Debug)]
pub struct FileKeyVerifier {
    store: KeyStore,
    snapshot: Mutex<Option<Snapshot>>,
    dummy_salt: [u8; SALT_LEN],
}

impl FileKeyVerifier {
    /// A verifier over `store`. The file is read lazily, on first use.
    pub fn new(store: KeyStore) -> Self {
        Self {
            store,
            snapshot: Mutex::new(None),
            // Fixed per process; only used to spend the same work on unknown ids.
            dummy_salt: [0x5a; SALT_LEN],
        }
    }

    /// The underlying store.
    pub fn store(&self) -> &KeyStore {
        &self.store
    }

    /// Runs `f` on the current records, reloading them first if the file changed.
    fn with_keys<T>(&self, f: impl FnOnce(&HashMap<String, KeyRecord>) -> T) -> Result<T, String> {
        let stamp = Stamp::of(self.store.path());
        let mut guard = self.snapshot.lock();
        let stale = guard.as_ref().is_none_or(|s| s.stamp != stamp);
        if stale {
            let keys = self
                .store
                .load()
                .map(|list| list.into_iter().map(|k| (k.id.clone(), k)).collect())
                .map_err(|e| e.to_string());
            if let Err(err) = &keys {
                tracing::error!(%err, "cannot read the key store; refusing all keys");
            }
            *guard = Some(Snapshot { stamp, keys });
        }
        match guard.as_ref().map(|s| &s.keys) {
            Some(Ok(keys)) => Ok(f(keys)),
            Some(Err(e)) => Err(e.clone()),
            None => Err("key store not loaded".into()),
        }
    }

    fn verify_at(&self, presented: &str, now: OffsetDateTime) -> Result<Principal, KeyRejection> {
        let presented = presented.trim();
        if presented.is_empty() {
            return Err(KeyRejection::Missing);
        }
        let key = token::parse_key(presented).ok_or(KeyRejection::Malformed)?;
        self.with_keys(|keys| match keys.get(key.id()) {
            None => {
                // Spend the same hashing work as a real check so response timing does not
                // reveal which ids exist.
                let h = token::hash_key(&self.dummy_salt, &key);
                let _ = token::constant_time_eq(h.as_bytes(), &[b'0'; 64]);
                Err(KeyRejection::Unknown)
            }
            Some(record) => {
                if !record.matches(&key) {
                    Err(KeyRejection::Mismatch)
                } else if record.revoked.is_some() {
                    Err(KeyRejection::Revoked)
                } else if record.expires.is_some_and(|e| e <= now) {
                    Err(KeyRejection::Expired)
                } else {
                    Ok(Principal::from_record(record))
                }
            }
        })
        .map_err(|_| KeyRejection::StoreUnavailable)?
    }
}

impl KeyVerifier for FileKeyVerifier {
    fn verify(&self, presented: &str) -> Result<Principal, KeyRejection> {
        self.verify_at(presented, OffsetDateTime::now_utc())
    }

    fn active_keys(&self) -> Result<usize, KeyError> {
        let now = OffsetDateTime::now_utc();
        // Read straight from the store so a corrupt file surfaces as a typed error.
        Ok(self
            .store
            .load()?
            .iter()
            .filter(|k| k.is_active(now))
            .count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::NewKey;

    fn setup() -> (tempfile::TempDir, FileKeyVerifier) {
        let tmp = tempfile::tempdir().unwrap();
        let v = FileKeyVerifier::new(KeyStore::new(tmp.path().join("keys.db")));
        (tmp, v)
    }

    #[test]
    fn valid_key_yields_its_principal() {
        let (_tmp, v) = setup();
        let mut spec = NewKey::named("ci");
        spec.models = Some(vec!["qwen3".into()]);
        let (rec, secret) = v.store().create(spec, OffsetDateTime::now_utc()).unwrap();
        let p = v.verify(secret.expose()).unwrap();
        assert_eq!(p.key_id, rec.id);
        assert!(p.has_scope(Scope::Inference));
        assert!(!p.has_scope(Scope::Admin));
        assert!(p.allows_model("qwen3"));
        assert!(!p.allows_model("llama3"));
        assert_eq!(v.active_keys().unwrap(), 1);
    }

    #[test]
    fn revocation() {
        // Scenario ALK-SP-0006.R3 "revocation": the next request with a revoked key fails.
        let (_tmp, v) = setup();
        let now = OffsetDateTime::now_utc();
        let (rec, secret) = v.store().create(NewKey::named("a"), now).unwrap();
        assert!(v.verify(secret.expose()).is_ok());
        v.store().revoke(&rec.id, now).unwrap();
        assert_eq!(v.verify(secret.expose()), Err(KeyRejection::Revoked));
        assert_eq!(v.active_keys().unwrap(), 0);
    }

    #[test]
    fn wrong_secret_unknown_id_and_garbage_are_refused() {
        let (_tmp, v) = setup();
        let now = OffsetDateTime::now_utc();
        let (_, secret) = v.store().create(NewKey::named("a"), now).unwrap();
        let mut forged = secret.expose().to_string();
        let last = forged.pop().unwrap();
        forged.push(if last == 'a' { 'b' } else { 'a' });
        assert_eq!(v.verify(&forged), Err(KeyRejection::Mismatch));
        let unknown = format!("alk_zzzzzzzz{}", &secret.expose()[12..]);
        assert_eq!(v.verify(&unknown), Err(KeyRejection::Unknown));
        assert_eq!(v.verify("sk-whatever"), Err(KeyRejection::Malformed));
        assert_eq!(v.verify(""), Err(KeyRejection::Missing));
    }

    #[test]
    fn expired_keys_are_refused() {
        let (_tmp, v) = setup();
        let now = OffsetDateTime::now_utc();
        let mut spec = NewKey::named("short");
        spec.expires = Some(now + time::Duration::minutes(1));
        let (_, secret) = v.store().create(spec, now).unwrap();
        assert!(v.verify_at(secret.expose(), now).is_ok());
        assert_eq!(
            v.verify_at(secret.expose(), now + time::Duration::minutes(2)),
            Err(KeyRejection::Expired)
        );
    }

    #[test]
    fn corrupt_store_fails_closed() {
        let (_tmp, v) = setup();
        let (_, secret) = v
            .store()
            .create(NewKey::named("a"), OffsetDateTime::now_utc())
            .unwrap();
        fs::write(v.store().path(), "{ broken").unwrap();
        assert_eq!(
            v.verify(secret.expose()),
            Err(KeyRejection::StoreUnavailable)
        );
        assert!(v.active_keys().is_err());
    }

    #[test]
    fn keys_created_after_start_are_picked_up() {
        let (_tmp, v) = setup();
        assert_eq!(v.verify("alk_aaaaaaaa_x"), Err(KeyRejection::Malformed));
        let (_, secret) = v
            .store()
            .create(NewKey::named("late"), OffsetDateTime::now_utc())
            .unwrap();
        assert!(v.verify(secret.expose()).is_ok());
    }
}
