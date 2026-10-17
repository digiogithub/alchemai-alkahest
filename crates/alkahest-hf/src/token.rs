//! Hub token handling: resolution order, OS keyring storage, redaction.

use std::fmt;

use zeroize::Zeroizing;

use alkahest_core::config::HfConfig;
use alkahest_core::paths::EnvFn;

use crate::error::{HfError, Result};

/// Keyring service name.
pub const KEYRING_SERVICE: &str = "alkahest";
/// Keyring user name of the Hub token.
pub const KEYRING_USER: &str = "hf-token";

/// A Hub access token. `Debug` and `Display` never show the secret; its memory is wiped on
/// drop.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(Zeroizing<String>);

impl Token {
    /// Wraps a token string; surrounding whitespace is trimmed and an empty string yields `None`.
    pub fn new(value: impl AsRef<str>) -> Option<Self> {
        let v = value.as_ref().trim();
        (!v.is_empty()).then(|| Self(Zeroizing::new(v.to_owned())))
    }

    /// The secret, for the `Authorization` header only.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Where a persistent token lives. The real implementation is [`KeyringStore`]; tests inject
/// an in-memory one.
pub trait TokenStore {
    /// The stored token, if any.
    fn get(&self) -> Result<Option<Token>>;
    /// Stores (replaces) the token.
    fn set(&self, token: &Token) -> Result<()>;
    /// Removes the stored token; absent is fine.
    fn delete(&self) -> Result<()>;
}

/// The OS keyring (service `alkahest`, user `hf-token`).
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringStore;

impl KeyringStore {
    fn entry() -> Result<keyring::Entry> {
        keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
            .map_err(|e| HfError::Keyring(e.to_string()))
    }
}

impl TokenStore for KeyringStore {
    fn get(&self) -> Result<Option<Token>> {
        match Self::entry()?.get_password() {
            Ok(v) => Ok(Token::new(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(HfError::Keyring(e.to_string())),
        }
    }

    fn set(&self, token: &Token) -> Result<()> {
        Self::entry()?
            .set_password(token.expose())
            .map_err(|e| HfError::Keyring(e.to_string()))
    }

    fn delete(&self) -> Result<()> {
        match Self::entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(HfError::Keyring(e.to_string())),
        }
    }
}

/// Resolves the token: `HF_TOKEN` / `HUGGING_FACE_HUB_TOKEN` ([`HfConfig::token`]) first, then
/// `store`. A keyring that is unavailable (headless machine without a secret service) counts as
/// "no token": public repositories must keep working anonymously.
pub fn resolve_token(env: EnvFn<'_>, store: &dyn TokenStore) -> Option<Token> {
    HfConfig::token(env)
        .and_then(Token::new)
        .or_else(|| store.get().ok().flatten())
}

/// The Hub endpoint: `ALKAHEST_HF_ENDPOINT` / the configuration file (already merged into
/// `config.endpoint` by the loader), else `HF_ENDPOINT` (the variable the Python tooling
/// uses), else the public Hub. A trailing slash is removed.
pub fn resolve_endpoint(config: &HfConfig, env: EnvFn<'_>) -> String {
    let default = HfConfig::default().endpoint;
    let endpoint = if config.endpoint != default {
        config.endpoint.clone()
    } else {
        env("HF_ENDPOINT").unwrap_or(default)
    };
    endpoint.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[derive(Default)]
    struct Memory(RefCell<Option<Token>>);

    impl TokenStore for Memory {
        fn get(&self) -> Result<Option<Token>> {
            Ok(self.0.borrow().clone())
        }
        fn set(&self, token: &Token) -> Result<()> {
            *self.0.borrow_mut() = Some(token.clone());
            Ok(())
        }
        fn delete(&self) -> Result<()> {
            *self.0.borrow_mut() = None;
            Ok(())
        }
    }

    struct Broken;
    impl TokenStore for Broken {
        fn get(&self) -> Result<Option<Token>> {
            Err(HfError::Keyring("no secret service".into()))
        }
        fn set(&self, _: &Token) -> Result<()> {
            Err(HfError::Keyring("no secret service".into()))
        }
        fn delete(&self) -> Result<()> {
            Ok(())
        }
    }

    fn env_with(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn debug_and_display_redact_the_secret() {
        let t = Token::new("hf_supersecret").unwrap();
        assert!(!format!("{t:?}").contains("supersecret"));
        assert!(!format!("{t}").contains("supersecret"));
        assert_eq!(t.expose(), "hf_supersecret");
        assert!(Token::new("   ").is_none());
    }

    #[test]
    fn env_beats_keyring() {
        let store = Memory::default();
        store.set(&Token::new("from_keyring").unwrap()).unwrap();
        let env = env_with(&[("HF_TOKEN", "from_env")]);
        assert_eq!(resolve_token(&env, &store).unwrap().expose(), "from_env");
    }

    #[test]
    fn keyring_is_used_when_env_is_empty() {
        let store = Memory::default();
        store.set(&Token::new("from_keyring").unwrap()).unwrap();
        let env = env_with(&[]);
        assert_eq!(
            resolve_token(&env, &store).unwrap().expose(),
            "from_keyring"
        );
    }

    #[test]
    fn no_token_anywhere_is_anonymous_and_a_broken_keyring_is_not_fatal() {
        let env = env_with(&[]);
        assert!(resolve_token(&env, &Memory::default()).is_none());
        assert!(resolve_token(&env, &Broken).is_none());
    }

    #[test]
    fn endpoint_precedence() {
        let none = env_with(&[]);
        let hf = env_with(&[("HF_ENDPOINT", "https://mirror.example/")]);
        let default = HfConfig::default();
        assert_eq!(resolve_endpoint(&default, &none), "https://huggingface.co");
        assert_eq!(resolve_endpoint(&default, &hf), "https://mirror.example");
        let configured = HfConfig {
            endpoint: "https://corp.example".into(),
        };
        assert_eq!(resolve_endpoint(&configured, &hf), "https://corp.example");
    }
}
