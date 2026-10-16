//! SHA-256 content digests, the identity of every blob in both stores.
//!
//! A digest has two spellings: manifests and registries write `sha256:<hex>`, blob file names
//! use `sha256-<hex>` (a colon is not portable in file names). [`Digest`] parses both and
//! formats each on request, so no caller ever converts by string replacement.

use std::fmt;
use std::str::FromStr;

/// Number of bytes in a SHA-256 digest.
const LEN: usize = 32;

/// A SHA-256 digest.
///
/// Displays as `sha256:<64 lowercase hex>`; [`Digest::filename`] gives the blob file name
/// `sha256-<hex>`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest([u8; LEN]);

/// Why a string is not a digest.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid sha256 digest {input:?}: expected sha256:<64 hex> or sha256-<64 hex>")]
pub struct DigestParseError {
    /// The rejected input.
    pub input: String,
}

impl Digest {
    /// Wraps raw digest bytes (for example the output of a SHA-256 hasher).
    pub const fn from_bytes(bytes: [u8; LEN]) -> Self {
        Self(bytes)
    }

    /// The raw digest bytes.
    pub const fn as_bytes(&self) -> &[u8; LEN] {
        &self.0
    }

    /// The 64-character lowercase hex form, without algorithm prefix.
    pub fn hex(&self) -> String {
        hex::encode(self.0)
    }

    /// The blob file name, `sha256-<hex>`.
    pub fn filename(&self) -> String {
        format!("sha256-{}", self.hex())
    }

    /// The first 12 hex characters, the short id shown by `ls` (as Ollama does).
    pub fn short(&self) -> String {
        let mut h = self.hex();
        h.truncate(12);
        h
    }

    /// Parses a blob file name (`sha256-<hex>` only). Used when scanning `blobs/`, where any
    /// other spelling is a stray file rather than a blob.
    pub fn from_filename(name: &str) -> Option<Self> {
        name.strip_prefix("sha256-").and_then(parse_hex)
    }
}

/// Decodes exactly 64 hex characters (either case).
fn parse_hex(s: &str) -> Option<Digest> {
    if s.len() != LEN * 2 {
        return None;
    }
    let mut out = [0u8; LEN];
    hex::decode_to_slice(s, &mut out).ok()?;
    Some(Digest(out))
}

impl FromStr for Digest {
    type Err = DigestParseError;

    /// Accepts `sha256:<hex>` and `sha256-<hex>`, hex in either case; the algorithm name is
    /// case-insensitive.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || DigestParseError {
            input: s.to_string(),
        };
        let (algo, hex) = s
            .split_once(':')
            .or_else(|| s.split_once('-'))
            .ok_or_else(err)?;
        if !algo.eq_ignore_ascii_case("sha256") {
            return Err(err());
        }
        parse_hex(hex).ok_or_else(err)
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", self.hex())
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "d93c393a9bd5139a4b5cfe24d31ef553c5a497bfb8afec178a354ecbf508f062";

    #[test]
    fn parses_both_spellings_and_case() {
        let a: Digest = format!("sha256:{HEX}").parse().unwrap();
        let b: Digest = format!("sha256-{HEX}").parse().unwrap();
        let c: Digest = format!("SHA256:{}", HEX.to_uppercase()).parse().unwrap();
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert_eq!(a.to_string(), format!("sha256:{HEX}"));
        assert_eq!(a.filename(), format!("sha256-{HEX}"));
        assert_eq!(a.short(), "d93c393a9bd5");
        assert_eq!(Digest::from_filename(&a.filename()), Some(a));
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            "",
            "sha256:",
            "sha256:abc",
            "md5:d41d8cd98f00b204e9800998ecf8427e",
            &format!("sha512:{HEX}"),
            &format!("sha256:{HEX}0"),
            &format!("sha256:{}g", &HEX[..63]),
            HEX,
        ] {
            assert!(bad.parse::<Digest>().is_err(), "{bad:?} accepted");
        }
        assert_eq!(Digest::from_filename(&format!("sha256:{HEX}")), None);
    }
}
