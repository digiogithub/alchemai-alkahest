//! Desktop settings: which server to talk to and the theme, plus the API key in the OS keyring.
//!
//! The settings file is `desktop.toml` in the Alkahest configuration directory (the binary
//! passes the path). It never holds the API key: that lives in the OS keyring (service
//! `alkahest`, user `desktop-api-key`), the same store the Hugging Face token uses. Without a
//! `server_url` the app talks to the server of the Alkahest configuration (local by default).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::theme::ThemePreference;

/// Keyring service shared with the rest of Alkahest.
pub const KEYRING_SERVICE: &str = "alkahest";
/// Keyring user of the desktop app's API key.
pub const KEYRING_USER: &str = "desktop-api-key";

/// Settings failures.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    /// The settings file could not be read or written.
    #[error("settings file {path}: {source}")]
    Io {
        /// The file.
        path: String,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
    /// The settings file is not valid TOML for these settings.
    #[error("settings file {path}: {message}")]
    Parse {
        /// The file.
        path: String,
        /// What is wrong.
        message: String,
    },
    /// The OS keyring failed.
    #[error("OS keyring: {0}")]
    Keyring(String),
}

/// The contents of `desktop.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DesktopSettings {
    /// A remote (or non-default) server, `http://gpu-box:11435`. `None`: the configured local
    /// server.
    pub server_url: Option<String>,
    /// CA certificate file (PEM) trusted for an `https` server with a private CA. `None`: the
    /// system roots, `ALKAHEST_CA_CERT`, or the CA generated for the local server.
    pub ca_cert: Option<String>,
    /// Theme preference.
    pub theme: ThemePreference,
}

impl DesktopSettings {
    /// Reads `path`; a missing file gives the defaults.
    pub fn load(path: &Path) -> Result<Self, SettingsError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(SettingsError::Io {
                    path: path.display().to_string(),
                    source,
                });
            }
        };
        toml::from_str(&text).map_err(|e| SettingsError::Parse {
            path: path.display().to_string(),
            message: e.to_string(),
        })
    }

    /// Writes `path`, creating its directory.
    pub fn save(&self, path: &Path) -> Result<(), SettingsError> {
        let io = |source| SettingsError::Io {
            path: path.display().to_string(),
            source,
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| SettingsError::Parse {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        std::fs::write(path, text).map_err(io)
    }

    /// The server URL to use: this file's `server_url`, else `configured`.
    pub fn effective_server_url<'a>(&'a self, configured: &'a str) -> &'a str {
        self.server_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .unwrap_or(configured)
    }
}

/// Where the API key lives. [`KeyringKeyStore`] is the real one; tests use memory.
pub trait ApiKeyStore {
    /// The stored key, if any.
    fn get(&self) -> Result<Option<String>, SettingsError>;
    /// Stores (replaces) the key.
    fn set(&self, key: &str) -> Result<(), SettingsError>;
    /// Removes the key; absent is fine.
    fn delete(&self) -> Result<(), SettingsError>;
}

/// The OS keyring.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringKeyStore;

impl KeyringKeyStore {
    fn entry() -> Result<keyring::Entry, SettingsError> {
        keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
            .map_err(|e| SettingsError::Keyring(e.to_string()))
    }
}

impl ApiKeyStore for KeyringKeyStore {
    fn get(&self) -> Result<Option<String>, SettingsError> {
        match Self::entry()?.get_password() {
            Ok(v) if v.is_empty() => Ok(None),
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SettingsError::Keyring(e.to_string())),
        }
    }

    fn set(&self, key: &str) -> Result<(), SettingsError> {
        Self::entry()?
            .set_password(key)
            .map_err(|e| SettingsError::Keyring(e.to_string()))
    }

    fn delete(&self) -> Result<(), SettingsError> {
        match Self::entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SettingsError::Keyring(e.to_string())),
        }
    }
}

/// The API key to send: `env` (the CLI's `ALKAHEST_API_KEY`) first, then `store`. A broken
/// keyring (headless machine without a secret service) counts as "no key", so a local server
/// keeps working.
pub fn resolve_api_key(env: Option<String>, store: &dyn ApiKeyStore) -> Option<String> {
    env.filter(|k| !k.is_empty())
        .or_else(|| store.get().ok().flatten())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[derive(Default)]
    struct Memory(RefCell<Option<String>>);

    impl ApiKeyStore for Memory {
        fn get(&self) -> Result<Option<String>, SettingsError> {
            Ok(self.0.borrow().clone())
        }
        fn set(&self, key: &str) -> Result<(), SettingsError> {
            *self.0.borrow_mut() = Some(key.to_string());
            Ok(())
        }
        fn delete(&self) -> Result<(), SettingsError> {
            *self.0.borrow_mut() = None;
            Ok(())
        }
    }

    struct Broken;

    impl ApiKeyStore for Broken {
        fn get(&self) -> Result<Option<String>, SettingsError> {
            Err(SettingsError::Keyring("no secret service".into()))
        }
        fn set(&self, _: &str) -> Result<(), SettingsError> {
            Err(SettingsError::Keyring("no secret service".into()))
        }
        fn delete(&self) -> Result<(), SettingsError> {
            Ok(())
        }
    }

    #[test]
    fn missing_file_gives_defaults_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg").join("desktop.toml");
        assert_eq!(
            DesktopSettings::load(&path).unwrap(),
            DesktopSettings::default()
        );
        let s = DesktopSettings {
            server_url: Some("https://gpu-box:11435".into()),
            ca_cert: Some("/etc/alkahest/ca.pem".into()),
            theme: ThemePreference::System,
        };
        s.save(&path).unwrap();
        assert_eq!(DesktopSettings::load(&path).unwrap(), s);
        assert!(!std::fs::read_to_string(&path).unwrap().contains("key"));
    }

    #[test]
    fn unknown_fields_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("desktop.toml");
        std::fs::write(&path, "api_key = \"alk_secret\"\n").unwrap();
        assert!(matches!(
            DesktopSettings::load(&path),
            Err(SettingsError::Parse { .. })
        ));
    }

    #[test]
    fn remote_url_overrides_the_configured_server() {
        let local = "http://127.0.0.1:11435";
        assert_eq!(
            DesktopSettings::default().effective_server_url(local),
            local
        );
        let blank = DesktopSettings {
            server_url: Some("  ".into()),
            ..DesktopSettings::default()
        };
        assert_eq!(blank.effective_server_url(local), local);
        let remote = DesktopSettings {
            server_url: Some("http://gpu-box:11435".into()),
            ..DesktopSettings::default()
        };
        assert_eq!(remote.effective_server_url(local), "http://gpu-box:11435");
    }

    #[test]
    fn api_key_comes_from_env_then_keyring() {
        let store = Memory::default();
        assert_eq!(resolve_api_key(None, &store), None);
        store.set("alk_from_keyring").unwrap();
        assert_eq!(
            resolve_api_key(None, &store).as_deref(),
            Some("alk_from_keyring")
        );
        assert_eq!(
            resolve_api_key(Some("alk_env".into()), &store).as_deref(),
            Some("alk_env")
        );
        assert_eq!(
            resolve_api_key(Some(String::new()), &store).as_deref(),
            Some("alk_from_keyring")
        );
        store.delete().unwrap();
        assert_eq!(resolve_api_key(None, &Broken), None);
    }
}
