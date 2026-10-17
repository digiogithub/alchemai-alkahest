//! API key format, generation and hashing.
//!
//! A key looks like `alk_<id>_<secret>`: `id` is 8 base32 characters (40 bits, public, used
//! to find the record in O(1) and shown by `alkahest keys ls`), `secret` is 52 base32
//! characters (260 bits from the operating system CSPRNG). Only `SHA-256(salt || key)` with a
//! per-key 16-byte salt is stored.
//!
//! Why SHA-256 and not a password hash such as Argon2: the secret is 260 random bits, so an
//! offline guessing attack is infeasible whatever the hash speed; a deliberately slow hash
//! would only make every request slower and turn the auth path into a CPU amplifier for
//! anyone sending made-up keys. ALK-SP-0006.R3 asks for salted hashes, which this is.

use std::fmt;

use rand::TryRng;
use rand::rngs::SysRng;
use sha2::{Digest, Sha256};

use super::KeyError;

/// Literal prefix of every Alkahest API key.
pub const KEY_PREFIX: &str = "alk_";
/// Length of the public key id.
pub const ID_LEN: usize = 8;
/// Length of the secret part.
pub const SECRET_LEN: usize = 52;
// Each base32 character carries 5 bits: the secret must keep at least 256 bits of entropy.
const _: () = assert!(SECRET_LEN * 5 >= 256);

/// Salt length in bytes.
pub const SALT_LEN: usize = 16;

/// Lower-case RFC 4648 base32 alphabet: 32 symbols, so a random byte masked to 5 bits maps
/// to a symbol without bias.
const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// Total key length: `alk_` + id + `_` + secret.
const KEY_LEN: usize = KEY_PREFIX.len() + ID_LEN + 1 + SECRET_LEN;

/// A full API key. `Debug` and `Display` never print the secret.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKeySecret(String);

impl ApiKeySecret {
    /// The complete key, for showing it to the user exactly once.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The public id embedded in the key.
    pub fn id(&self) -> &str {
        &self.0[KEY_PREFIX.len()..KEY_PREFIX.len() + ID_LEN]
    }
}

impl fmt::Debug for ApiKeySecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ApiKeySecret({}{}_…)", KEY_PREFIX, self.id())
    }
}

/// Fills `buf` from the operating system CSPRNG.
pub(crate) fn os_random(buf: &mut [u8]) -> Result<(), KeyError> {
    SysRng
        .try_fill_bytes(buf)
        .map_err(|e| KeyError::Random(e.to_string()))
}

/// `len` random base32 characters.
fn random_base32(len: usize) -> Result<String, KeyError> {
    let mut bytes = vec![0u8; len];
    os_random(&mut bytes)?;
    Ok(bytes
        .iter()
        .map(|b| char::from(ALPHABET[usize::from(b & 0x1f)]))
        .collect())
}

/// Generates a random public key id.
pub(crate) fn generate_id() -> Result<String, KeyError> {
    random_base32(ID_LEN)
}

/// Generates a new key with the given id.
pub(crate) fn generate_key(id: &str) -> Result<ApiKeySecret, KeyError> {
    let secret = random_base32(SECRET_LEN)?;
    Ok(ApiKeySecret(format!("{KEY_PREFIX}{id}_{secret}")))
}

/// Generates a fresh random salt.
pub(crate) fn generate_salt() -> Result<[u8; SALT_LEN], KeyError> {
    let mut salt = [0u8; SALT_LEN];
    os_random(&mut salt)?;
    Ok(salt)
}

/// Parses a presented key, returning it when it has the exact Alkahest shape.
///
/// Shape checks run before any hashing so garbage costs nothing.
pub fn parse_key(presented: &str) -> Option<ApiKeySecret> {
    let bytes = presented.as_bytes();
    if bytes.len() != KEY_LEN || !presented.starts_with(KEY_PREFIX) {
        return None;
    }
    let sep = KEY_PREFIX.len() + ID_LEN;
    if bytes[sep] != b'_' {
        return None;
    }
    let body_ok = bytes[KEY_PREFIX.len()..]
        .iter()
        .enumerate()
        .all(|(i, b)| i + KEY_PREFIX.len() == sep || ALPHABET.contains(b));
    body_ok.then(|| ApiKeySecret(presented.to_string()))
}

/// `SHA-256(salt || key)`, hex encoded.
pub(crate) fn hash_key(salt: &[u8], key: &ApiKeySecret) -> String {
    let mut h = Sha256::new();
    h.update(salt);
    h.update(key.0.as_bytes());
    hex::encode(h.finalize())
}

/// Compares two byte strings in time that depends only on their lengths.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(diff) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_keys_have_the_documented_shape() {
        let id = generate_id().unwrap();
        let key = generate_key(&id).unwrap();
        assert_eq!(key.expose().len(), KEY_LEN);
        assert!(key.expose().starts_with("alk_"));
        assert_eq!(key.id(), id);
        assert_eq!(parse_key(key.expose()), Some(key.clone()));
    }

    #[test]
    fn keys_are_unique() {
        let a = generate_key("aaaaaaaa").unwrap();
        let b = generate_key("aaaaaaaa").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn malformed_keys_are_rejected_before_hashing() {
        let good = generate_key("abcdefgh").unwrap();
        let s = good.expose();
        assert!(parse_key("").is_none());
        assert!(parse_key(&s[..s.len() - 1]).is_none());
        assert!(parse_key(&format!("{s}a")).is_none());
        assert!(parse_key(&s.replacen("alk_", "sk-_", 1)).is_none());
        assert!(parse_key(&s.to_uppercase()).is_none());
        let mut bad_sep = s.to_string();
        bad_sep.replace_range(12..13, "a");
        assert!(parse_key(&bad_sep).is_none());
        // Multi-byte characters must not slip past the length check.
        let mut uni = s[..s.len() - 2].to_string();
        uni.push('é');
        assert!(parse_key(&uni).is_none());
    }

    #[test]
    fn debug_never_prints_the_secret() {
        let key = generate_key("abcdefgh").unwrap();
        let dbg = format!("{key:?}");
        assert!(!dbg.contains(&key.expose()[13..]));
        assert!(dbg.contains("alk_abcdefgh"));
    }

    #[test]
    fn hash_depends_on_salt_and_key() {
        let key = generate_key("abcdefgh").unwrap();
        let other = generate_key("abcdefgh").unwrap();
        let s1 = [1u8; SALT_LEN];
        let s2 = [2u8; SALT_LEN];
        assert_eq!(hash_key(&s1, &key), hash_key(&s1, &key));
        assert_ne!(hash_key(&s1, &key), hash_key(&s2, &key));
        assert_ne!(hash_key(&s1, &key), hash_key(&s1, &other));
        assert_eq!(hash_key(&s1, &key).len(), 64);
    }

    #[test]
    fn constant_time_eq_matches_slice_eq() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abx"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
