//! An operator-supplied API key taken from the environment (containers, CI, servers whose
//! secrets come from an orchestrator rather than `alkahest keys create`).
//!
//! `ALKAHEST_API_KEY` holds the key itself; `ALKAHEST_API_KEY_FILE` names a file holding it
//! (Docker and Kubernetes secrets mount as files, which keeps the value out of
//! `docker inspect`). `ALKAHEST_API_KEY_SCOPES` (`inference` by default, or
//! `inference,admin`) sets what the key may do. The value is any string of at least
//! [`MIN_LEN`] characters; it does not need the `alk_<id>_<secret>` shape, so an operator can
//! use `openssl rand -hex 32`.
//!
//! [`EnvKeyVerifier`] checks the static key first and falls back to the key store, so keys
//! created with `alkahest keys create` keep working next to it. Only the SHA-256 digest of the
//! key is kept in memory, and the comparison is over digests, so its timing reveals neither
//! the key nor its length.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::token::constant_time_eq;
use super::{KeyError, KeyRejection, KeyVerifier, Principal, Scope};

/// Variable holding the key.
pub const API_KEY_ENV: &str = "ALKAHEST_API_KEY";
/// Variable naming a file that holds the key.
pub const API_KEY_FILE_ENV: &str = "ALKAHEST_API_KEY_FILE";
/// Variable listing the key's scopes, comma separated.
pub const API_KEY_SCOPES_ENV: &str = "ALKAHEST_API_KEY_SCOPES";
/// Every variable above; engine subprocesses are started without them.
pub const SECRET_ENV_VARS: [&str; 2] = [API_KEY_ENV, API_KEY_FILE_ENV];

/// Shortest accepted key, so a placeholder such as `changeme` cannot protect a server.
pub const MIN_LEN: usize = 32;

/// Key id reported for the environment key in audit and usage records.
pub const ENV_KEY_ID: &str = "env";

/// Why the environment key could not be configured. The server refuses to start: an
/// operator who set a key expects it to be enforced.
#[derive(Debug, thiserror::Error)]
pub enum EnvKeyError {
    /// Both variables are set; which one wins would be a guess.
    #[error("set either {API_KEY_ENV} or {API_KEY_FILE_ENV}, not both")]
    Ambiguous,
    /// The key file could not be read.
    #[error("{API_KEY_FILE_ENV}={}: {source}", path.display())]
    File {
        /// File named by the variable.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// The key is shorter than [`MIN_LEN`].
    #[error("the API key from the environment is shorter than {MIN_LEN} characters")]
    TooShort,
    /// The key contains whitespace or control characters, which no client sends in a header.
    #[error("the API key from the environment contains whitespace or control characters")]
    InvalidCharacters,
    /// The scope list does not parse.
    #[error("{API_KEY_SCOPES_ENV}: {0}")]
    Scopes(#[source] KeyError),
}

/// The static key: its digest and the principal it authenticates as.
#[derive(Clone)]
pub struct EnvKey {
    digest: [u8; 32],
    principal: Principal,
}

impl fmt::Debug for EnvKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnvKey")
            .field("scopes", &self.principal.scopes)
            .finish_non_exhaustive()
    }
}

impl EnvKey {
    /// Reads the key from the process environment. `Ok(None)` when no key variable is set.
    pub fn from_env() -> Result<Option<Self>, EnvKeyError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Like [`Self::from_env`], with the variables looked up through `get` (tests).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, EnvKeyError> {
        let inline = get(API_KEY_ENV)
            .filter(|v| !v.is_empty())
            .map(Zeroizing::new);
        let file = get(API_KEY_FILE_ENV).filter(|v| !v.is_empty());
        let key = match (inline, file) {
            (None, None) => return Ok(None),
            (Some(_), Some(_)) => return Err(EnvKeyError::Ambiguous),
            (Some(k), None) => k,
            (None, Some(path)) => read_key_file(Path::new(&path))?,
        };
        let scopes = match get(API_KEY_SCOPES_ENV).filter(|v| !v.trim().is_empty()) {
            Some(list) => parse_scopes(&list).map_err(EnvKeyError::Scopes)?,
            None => vec![Scope::Inference],
        };
        Self::new(&key, scopes).map(Some)
    }

    /// A static key with the given scopes.
    pub fn new(key: &str, scopes: Vec<Scope>) -> Result<Self, EnvKeyError> {
        if key.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(EnvKeyError::InvalidCharacters);
        }
        if key.chars().count() < MIN_LEN {
            return Err(EnvKeyError::TooShort);
        }
        Ok(Self {
            digest: digest(key),
            principal: Principal {
                key_id: ENV_KEY_ID.to_owned(),
                name: API_KEY_ENV.to_owned(),
                scopes,
                models: None,
            },
        })
    }

    /// True when `presented` is this key.
    fn matches(&self, presented: &str) -> bool {
        constant_time_eq(&digest(presented), &self.digest)
    }
}

/// Reads a key file, dropping one trailing newline (`echo` and most editors add it).
fn read_key_file(path: &Path) -> Result<Zeroizing<String>, EnvKeyError> {
    let raw =
        Zeroizing::new(
            std::fs::read_to_string(path).map_err(|source| EnvKeyError::File {
                path: path.to_owned(),
                source,
            })?,
        );
    let trimmed = raw.strip_suffix('\n').unwrap_or(&raw);
    let trimmed = trimmed.strip_suffix('\r').unwrap_or(trimmed);
    Ok(Zeroizing::new(trimmed.to_owned()))
}

fn parse_scopes(list: &str) -> Result<Vec<Scope>, KeyError> {
    let mut scopes = Vec::new();
    for s in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let scope = s.parse::<Scope>()?;
        if !scopes.contains(&scope) {
            scopes.push(scope);
        }
    }
    Ok(scopes)
}

fn digest(s: &str) -> [u8; 32] {
    Sha256::digest(s.as_bytes()).into()
}

/// [`KeyVerifier`] that accepts the environment key and otherwise defers to `inner`.
pub struct EnvKeyVerifier {
    key: EnvKey,
    inner: Arc<dyn KeyVerifier>,
}

impl fmt::Debug for EnvKeyVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnvKeyVerifier")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl EnvKeyVerifier {
    /// Puts `key` in front of `inner`.
    pub fn new(key: EnvKey, inner: Arc<dyn KeyVerifier>) -> Self {
        Self { key, inner }
    }
}

impl KeyVerifier for EnvKeyVerifier {
    fn verify(&self, presented: &str) -> Result<Principal, KeyRejection> {
        let presented = presented.trim();
        if self.key.matches(presented) {
            return Ok(self.key.principal.clone());
        }
        self.inner.verify(presented)
    }

    fn active_keys(&self) -> Result<usize, KeyError> {
        // A broken key store must not be hidden by the static key: the store is still read
        // and its error still stops the start.
        Ok(self.inner.active_keys()? + 1)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::keys::{FileKeyVerifier, KeyStore, NewKey};

    const KEY: &str = "0123456789abcdef0123456789abcdef";

    fn lookup(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name| map.get(name).cloned()
    }

    fn verifier(key: EnvKey) -> (tempfile::TempDir, KeyStore, EnvKeyVerifier) {
        let tmp = tempfile::tempdir().unwrap();
        let store = KeyStore::new(tmp.path().join("keys.db"));
        let inner = Arc::new(FileKeyVerifier::new(store.clone()));
        (tmp, store, EnvKeyVerifier::new(key, inner))
    }

    #[test]
    fn unset_means_no_key() {
        assert!(EnvKey::from_lookup(lookup(&[])).unwrap().is_none());
        assert!(
            EnvKey::from_lookup(lookup(&[(API_KEY_ENV, "")]))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn inline_key_defaults_to_inference() {
        let key = EnvKey::from_lookup(lookup(&[(API_KEY_ENV, KEY)]))
            .unwrap()
            .unwrap();
        let (_tmp, _store, v) = verifier(key);
        let p = v.verify(KEY).unwrap();
        assert_eq!(p.key_id, ENV_KEY_ID);
        assert!(p.has_scope(Scope::Inference));
        assert!(!p.has_scope(Scope::Admin));
        assert_eq!(
            v.verify("0123456789abcdef0123456789abcdeX"),
            Err(KeyRejection::Malformed)
        );
        assert_eq!(v.active_keys().unwrap(), 1);
    }

    #[test]
    fn scopes_from_env() {
        let key = EnvKey::from_lookup(lookup(&[
            (API_KEY_ENV, KEY),
            (API_KEY_SCOPES_ENV, "inference, admin"),
        ]))
        .unwrap()
        .unwrap();
        let (_tmp, _store, v) = verifier(key);
        assert!(v.verify(KEY).unwrap().has_scope(Scope::Admin));
        assert!(matches!(
            EnvKey::from_lookup(lookup(&[(API_KEY_ENV, KEY), (API_KEY_SCOPES_ENV, "root")])),
            Err(EnvKeyError::Scopes(_))
        ));
    }

    #[test]
    fn key_file_with_trailing_newline() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("key");
        std::fs::write(&path, format!("{KEY}\n")).unwrap();
        let key = EnvKey::from_lookup(lookup(&[(API_KEY_FILE_ENV, path.to_str().unwrap())]))
            .unwrap()
            .unwrap();
        let (_tmp, _store, v) = verifier(key);
        assert!(v.verify(KEY).is_ok());
    }

    #[test]
    fn missing_key_file_is_an_error() {
        assert!(matches!(
            EnvKey::from_lookup(lookup(&[(API_KEY_FILE_ENV, "/nonexistent/alkahest-key")])),
            Err(EnvKeyError::File { .. })
        ));
    }

    #[test]
    fn rejects_weak_or_ambiguous_configuration() {
        assert!(matches!(
            EnvKey::from_lookup(lookup(&[(API_KEY_ENV, "changeme")])),
            Err(EnvKeyError::TooShort)
        ));
        assert!(matches!(
            EnvKey::new("0123456789abcdef 0123456789abcdef", vec![Scope::Inference]),
            Err(EnvKeyError::InvalidCharacters)
        ));
        assert!(matches!(
            EnvKey::from_lookup(lookup(&[(API_KEY_ENV, KEY), (API_KEY_FILE_ENV, "/k")])),
            Err(EnvKeyError::Ambiguous)
        ));
    }

    #[test]
    fn store_keys_still_work_next_to_the_env_key() {
        let (_tmp, store, v) = verifier(EnvKey::new(KEY, vec![Scope::Inference]).unwrap());
        let (rec, secret) = store
            .create(NewKey::named("ci"), time::OffsetDateTime::now_utc())
            .unwrap();
        assert_eq!(v.verify(secret.expose()).unwrap().key_id, rec.id);
        assert_eq!(v.active_keys().unwrap(), 2);
    }

    #[test]
    fn debug_never_prints_the_key() {
        let key = EnvKey::new(KEY, vec![Scope::Inference]).unwrap();
        assert!(!format!("{key:?}").contains(KEY));
    }
}
