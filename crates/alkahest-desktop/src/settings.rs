//! Desktop settings: the saved server connections, which one is active and the theme, plus
//! one API key per connection in the OS keyring (ALK-US-0144).
//!
//! The settings file is `desktop.toml` in the Alkahest configuration directory (the binary
//! passes the path). It never holds an API key: those live in the OS keyring (service
//! `alkahest`, user `desktop-api-key:<connection>`), the same store the Hugging Face token
//! uses. The built-in [`LOCAL`] connection is the server of the Alkahest configuration; it is
//! not stored and cannot be deleted. Only it may be started, stopped or restarted by the app:
//! a saved connection is somebody else's server even when its URL is loopback (an SSH tunnel).
//!
//! Files written before connections existed hold a single `server_url` / `ca_cert` and one
//! keyring entry (user `desktop-api-key`); [`DesktopSettings::load`] turns them into a
//! connection named [`MIGRATED`] and [`migrate_legacy_key`] moves the key to it.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::theme::ThemePreference;

/// Keyring service shared with the rest of Alkahest.
pub const KEYRING_SERVICE: &str = "alkahest";
/// Keyring user of the single API key of files written before connections existed; per
/// connection keys append `:<connection name>`.
pub const KEYRING_USER: &str = "desktop-api-key";
/// Name of the built-in connection to the configured (local) server.
pub const LOCAL: &str = "local";
/// Name given to the `server_url` of an older settings file.
pub const MIGRATED: &str = "remote";
/// Longest connection name.
pub const NAME_MAX_CHARS: usize = 40;

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

/// A saved connection to a server other than the configured one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedConnection {
    /// Unique name shown in Settings and used for its keyring entry.
    pub name: String,
    /// Base URL, `https://gpu-box:11435`.
    pub url: String,
    /// CA certificate file (PEM) trusted for an `https` server with a private CA.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_cert: Option<String>,
}

/// The connection the app talks to, resolved against the configured server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveConnection {
    /// Connection name ([`LOCAL`] for the configured server).
    pub name: String,
    /// Base URL.
    pub url: String,
    /// CA file saved with the connection, if any.
    pub ca_cert: Option<String>,
    /// Whether this is the built-in local connection (the only one the app may start).
    pub local: bool,
}

/// The contents of `desktop.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DesktopSettings {
    /// Saved connections besides [`LOCAL`], in the order they were added.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub connections: Vec<SavedConnection>,
    /// Name of the active connection; `None` (or an unknown name): [`LOCAL`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    /// Theme preference.
    pub theme: ThemePreference,
    /// Legacy single server URL, read only to migrate it ([`DesktopSettings::load`]).
    #[serde(skip_serializing)]
    server_url: Option<String>,
    /// Legacy CA file of `server_url`, read only to migrate it.
    #[serde(skip_serializing)]
    ca_cert: Option<String>,
    /// `load` turned a legacy `server_url` into the [`MIGRATED`] connection.
    #[serde(skip)]
    migrated: bool,
}

impl DesktopSettings {
    /// Reads `path`; a missing file gives the defaults. A legacy `server_url` becomes the
    /// active connection [`MIGRATED`] (see [`DesktopSettings::migrated`]).
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
        let mut s: Self = toml::from_str(&text).map_err(|e| SettingsError::Parse {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        s.migrate();
        Ok(s)
    }

    /// Moves a legacy `server_url` / `ca_cert` into a saved connection.
    fn migrate(&mut self) {
        let url = self.server_url.take();
        let ca = self.ca_cert.take();
        let Some(url) = url.map(|u| u.trim().to_string()).filter(|u| !u.is_empty()) else {
            return;
        };
        let mut name = MIGRATED.to_string();
        let mut n = 2;
        while self.find(&name).is_some() {
            name = format!("{MIGRATED}-{n}");
            n += 1;
        }
        self.connections.push(SavedConnection {
            name: name.clone(),
            url,
            ca_cert: ca.filter(|c| !c.trim().is_empty()),
        });
        if self.active.is_none() {
            self.active = Some(name);
        }
        self.migrated = true;
    }

    /// The connection a legacy `server_url` was migrated to by [`DesktopSettings::load`], so
    /// the caller can move the legacy key ([`migrate_legacy_key`]) and save the file.
    pub fn migrated(&self) -> Option<&str> {
        self.migrated
            .then(|| self.connections.last().map(|c| c.name.as_str()))
            .flatten()
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

    /// The saved connection called `name` (case-insensitive).
    pub fn find(&self, name: &str) -> Option<&SavedConnection> {
        self.connections
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name.trim()))
    }

    /// Name of the active connection: a saved one that exists, else [`LOCAL`].
    pub fn active_name(&self) -> &str {
        self.active
            .as_deref()
            .and_then(|n| self.find(n))
            .map_or(LOCAL, |c| c.name.as_str())
    }

    /// The active connection; [`LOCAL`] talks to `configured` (the server of the Alkahest
    /// configuration).
    pub fn active_connection(&self, configured: &str) -> ActiveConnection {
        match self.active.as_deref().and_then(|n| self.find(n)) {
            Some(c) => ActiveConnection {
                name: c.name.clone(),
                url: c.url.clone(),
                ca_cert: c.ca_cert.clone(),
                local: false,
            },
            None => ActiveConnection {
                name: LOCAL.to_string(),
                url: configured.to_string(),
                ca_cert: None,
                local: true,
            },
        }
    }

    /// Makes `name` the active connection ([`LOCAL`] or a saved one).
    pub fn select(&mut self, name: &str) -> Result<(), String> {
        if name.trim().eq_ignore_ascii_case(LOCAL) {
            self.active = None;
            return Ok(());
        }
        let found = self
            .find(name)
            .map(|c| c.name.clone())
            .ok_or_else(|| format!("no connection named {name:?}"))?;
        self.active = Some(found);
        Ok(())
    }

    /// Adds `conn`, or replaces the saved connection with the same name. The name must be
    /// valid ([`validate_name`]).
    pub fn upsert(&mut self, conn: SavedConnection) -> Result<(), String> {
        validate_name(&conn.name)?;
        match self
            .connections
            .iter_mut()
            .find(|c| c.name.eq_ignore_ascii_case(&conn.name))
        {
            Some(c) => *c = conn,
            None => self.connections.push(conn),
        }
        Ok(())
    }

    /// Removes the saved connection `name`; the active one falls back to [`LOCAL`]. Returns
    /// whether it existed. [`LOCAL`] cannot be removed.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.connections.len();
        self.connections
            .retain(|c| !c.name.eq_ignore_ascii_case(name.trim()));
        if self
            .active
            .as_deref()
            .is_some_and(|a| a.eq_ignore_ascii_case(name.trim()))
        {
            self.active = None;
        }
        self.connections.len() != before
    }
}

/// Whether `name` can name a saved connection: 1 to [`NAME_MAX_CHARS`] letters, digits,
/// spaces, `-`, `_` or `.`, and not [`LOCAL`].
pub fn validate_name(name: &str) -> Result<(), String> {
    let n = name.trim();
    if n.is_empty() {
        return Err("give the connection a name".into());
    }
    if n.chars().count() > NAME_MAX_CHARS {
        return Err(format!("a name has at most {NAME_MAX_CHARS} characters"));
    }
    if n.eq_ignore_ascii_case(LOCAL) {
        return Err(format!(
            "{LOCAL:?} is the built-in connection to this machine"
        ));
    }
    if !n
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
    {
        return Err("use letters, digits, spaces, - _ or .".into());
    }
    Ok(())
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

/// One entry of the OS keyring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyringKeyStore {
    user: String,
}

impl KeyringKeyStore {
    /// The key of connection `name` (user `desktop-api-key:<name>`).
    pub fn for_connection(name: &str) -> Self {
        Self {
            user: format!("{KEYRING_USER}:{}", name.trim().to_lowercase()),
        }
    }

    /// The single key of settings files written before connections existed.
    pub fn legacy() -> Self {
        Self {
            user: KEYRING_USER.to_string(),
        }
    }

    fn entry(&self) -> Result<keyring::Entry, SettingsError> {
        keyring::Entry::new(KEYRING_SERVICE, &self.user)
            .map_err(|e| SettingsError::Keyring(e.to_string()))
    }
}

impl ApiKeyStore for KeyringKeyStore {
    fn get(&self) -> Result<Option<String>, SettingsError> {
        match self.entry()?.get_password() {
            Ok(v) if v.is_empty() => Ok(None),
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SettingsError::Keyring(e.to_string())),
        }
    }

    fn set(&self, key: &str) -> Result<(), SettingsError> {
        self.entry()?
            .set_password(key)
            .map_err(|e| SettingsError::Keyring(e.to_string()))
    }

    fn delete(&self) -> Result<(), SettingsError> {
        match self.entry()?.delete_credential() {
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

/// Moves the legacy single key (`legacy`) to the migrated connection (`to`) unless that one
/// already has a key. The legacy entry is removed once copied. Keyring failures are left for
/// the next start: the app must open without a secret service.
pub fn migrate_legacy_key(legacy: &dyn ApiKeyStore, to: &dyn ApiKeyStore) {
    let Ok(Some(key)) = legacy.get() else { return };
    if matches!(to.get(), Ok(None)) && to.set(&key).is_ok() {
        let _ = legacy.delete();
    }
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

    fn gpu_box() -> SavedConnection {
        SavedConnection {
            name: "gpu-box".into(),
            url: "https://gpu-box:11435".into(),
            ca_cert: Some("/etc/alkahest/ca.pem".into()),
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
        let mut s = DesktopSettings {
            theme: ThemePreference::System,
            ..DesktopSettings::default()
        };
        s.upsert(gpu_box()).unwrap();
        s.select("GPU-box").unwrap();
        s.save(&path).unwrap();
        assert_eq!(DesktopSettings::load(&path).unwrap(), s);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("key"), "{text}");
        assert!(!text.contains("server_url"), "{text}");
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
    fn the_local_connection_is_the_configured_server() {
        let local = "http://127.0.0.1:11435";
        let s = DesktopSettings::default();
        let a = s.active_connection(local);
        assert_eq!(
            (a.name.as_str(), a.url.as_str(), a.local),
            (LOCAL, local, true)
        );
        // An active name that no longer exists falls back to the local server.
        let stale = DesktopSettings {
            active: Some("gone".into()),
            ..DesktopSettings::default()
        };
        assert!(stale.active_connection(local).local);
        assert_eq!(stale.active_name(), LOCAL);
    }

    #[test]
    fn a_loopback_tunnel_is_still_a_remote_connection() {
        let mut s = DesktopSettings::default();
        s.upsert(SavedConnection {
            name: "dgpu01".into(),
            url: "https://127.0.0.1:21435".into(),
            ca_cert: None,
        })
        .unwrap();
        s.select("dgpu01").unwrap();
        let a = s.active_connection("http://127.0.0.1:11435");
        assert!(!a.local, "only the built-in connection may start a server");
        assert_eq!(a.url, "https://127.0.0.1:21435");
    }

    #[test]
    fn connections_switch_replace_and_delete() {
        let mut s = DesktopSettings::default();
        s.upsert(gpu_box()).unwrap();
        s.upsert(SavedConnection {
            name: "lab".into(),
            url: "http://lab:11435".into(),
            ca_cert: None,
        })
        .unwrap();
        s.select("lab").unwrap();
        assert_eq!(s.active_name(), "lab");
        assert!(s.select("nope").is_err());
        // Same name (any case) replaces.
        s.upsert(SavedConnection {
            name: "LAB".into(),
            url: "http://lab2:11435".into(),
            ca_cert: None,
        })
        .unwrap();
        assert_eq!(s.connections.len(), 2);
        assert_eq!(s.find("lab").unwrap().url, "http://lab2:11435");
        // Deleting the active one goes back to local.
        assert!(s.remove("lab"));
        assert_eq!(s.active_name(), LOCAL);
        assert!(!s.remove("lab"));
        assert!(!s.remove(LOCAL));
        s.select(LOCAL).unwrap();
        assert_eq!(s.active, None);
    }

    #[test]
    fn connection_names_are_validated() {
        assert!(validate_name("gpu box-01_a.b").is_ok());
        assert!(validate_name("  ").is_err());
        assert!(validate_name("Local").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name(&"x".repeat(NAME_MAX_CHARS + 1)).is_err());
        let mut s = DesktopSettings::default();
        assert!(
            s.upsert(SavedConnection {
                name: "local".into(),
                url: "http://x:1".into(),
                ca_cert: None,
            })
            .is_err()
        );
    }

    #[test]
    fn a_legacy_server_url_becomes_the_active_remote_connection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("desktop.toml");
        std::fs::write(
            &path,
            "server_url = \"https://127.0.0.1:21435\"\nca_cert = \"/x/ca.pem\"\ntheme = \"dark\"\n",
        )
        .unwrap();
        let s = DesktopSettings::load(&path).unwrap();
        assert_eq!(s.migrated(), Some(MIGRATED));
        let a = s.active_connection("http://127.0.0.1:11435");
        assert_eq!(a.name, MIGRATED);
        assert_eq!(a.url, "https://127.0.0.1:21435");
        assert_eq!(a.ca_cert.as_deref(), Some("/x/ca.pem"));
        assert!(!a.local);
        s.save(&path).unwrap();
        let again = DesktopSettings::load(&path).unwrap();
        assert_eq!(again.migrated(), None, "saved in the new form");
        assert_eq!(again.connections, s.connections);
        // A blank legacy URL migrates to nothing.
        std::fs::write(&path, "server_url = \" \"\n").unwrap();
        let blank = DesktopSettings::load(&path).unwrap();
        assert!(blank.connections.is_empty());
        assert_eq!(blank.migrated(), None);
    }

    #[test]
    fn the_legacy_key_moves_to_the_migrated_connection_once() {
        let legacy = Memory::default();
        let to = Memory::default();
        migrate_legacy_key(&legacy, &to);
        assert_eq!(to.get().unwrap(), None);
        legacy.set("alk_old").unwrap();
        migrate_legacy_key(&legacy, &to);
        assert_eq!(to.get().unwrap().as_deref(), Some("alk_old"));
        assert_eq!(legacy.get().unwrap(), None);
        // A connection that already has a key keeps it.
        legacy.set("alk_older").unwrap();
        migrate_legacy_key(&legacy, &to);
        assert_eq!(to.get().unwrap().as_deref(), Some("alk_old"));
        assert_eq!(legacy.get().unwrap().as_deref(), Some("alk_older"));
        // A broken keyring changes nothing.
        migrate_legacy_key(&Broken, &to);
        assert_eq!(
            KeyringKeyStore::for_connection("GPU-Box"),
            KeyringKeyStore {
                user: "desktop-api-key:gpu-box".into()
            }
        );
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
