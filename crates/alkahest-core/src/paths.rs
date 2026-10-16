//! Platform directories and Ollama store detection.
//!
//! Defaults follow each platform's conventions through the `directories` crate. Setting
//! `ALKAHEST_HOME` relocates everything under one root (containers, tests, portable installs).

use std::path::{Path, PathBuf};

use directories::{BaseDirs, ProjectDirs};

use crate::error::{Error, Result};

/// Environment lookup used wherever the process environment is consulted, so tests can inject
/// a fake one. Returns `None` for unset variables.
pub type EnvFn<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Reads the real process environment; empty values count as unset.
pub fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Resolved base directories of an Alkahest installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlkahestPaths {
    /// Directory of the user configuration file.
    pub config_dir: PathBuf,
    /// Directory of persistent data (own model store, engines, key store).
    pub data_dir: PathBuf,
    /// Directory of disposable caches (catalog and search caches).
    pub cache_dir: PathBuf,
    /// Directory where log files are written.
    pub log_dir: PathBuf,
    /// Machine-wide configuration file, read before the user file.
    pub system_config_file: PathBuf,
}

impl AlkahestPaths {
    /// Discovers the paths from the process environment (`ALKAHEST_HOME`) and the platform.
    pub fn discover() -> Result<Self> {
        Self::discover_with(&process_env)
    }

    /// Like [`AlkahestPaths::discover`] with an injected environment.
    pub fn discover_with(env: EnvFn<'_>) -> Result<Self> {
        if let Some(home) = env("ALKAHEST_HOME") {
            return Ok(Self::under(home));
        }
        let dirs = ProjectDirs::from("", "Digio", "Alkahest").ok_or(Error::NoHomeDir)?;
        let log_dir = match dirs.state_dir() {
            Some(state) => state.join("logs"),
            None => dirs.data_dir().join("logs"),
        };
        Ok(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
            log_dir,
            system_config_file: system_config_file(),
        })
    }

    /// Places every directory under `root` (`config/`, `data/`, `cache/`, `logs/`, `system/`).
    pub fn under(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            log_dir: root.join("logs"),
            system_config_file: root.join("system").join("config.toml"),
        }
    }

    /// The per-user configuration file (`<config_dir>/config.toml`).
    pub fn user_config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    /// Default root of Alkahest's own model store (`<data_dir>/models`).
    pub fn default_models_dir(&self) -> PathBuf {
        self.data_dir.join("models")
    }

    /// Default directory for installed engines (`<data_dir>/engines`).
    pub fn default_engines_dir(&self) -> PathBuf {
        self.data_dir.join("engines")
    }

    /// Default location of the API key store (`<data_dir>/keys.db`).
    pub fn default_key_store(&self) -> PathBuf {
        self.data_dir.join("keys.db")
    }
}

#[cfg(target_os = "macos")]
fn system_config_file() -> PathBuf {
    PathBuf::from("/Library/Application Support/Alkahest/config.toml")
}

#[cfg(windows)]
fn system_config_file() -> PathBuf {
    let base = std::env::var_os("ProgramData").unwrap_or_else(|| "C:\\ProgramData".into());
    PathBuf::from(base).join("Alkahest").join("config.toml")
}

#[cfg(not(any(target_os = "macos", windows)))]
fn system_config_file() -> PathBuf {
    PathBuf::from("/etc/alkahest/config.toml")
}

/// Finds the Ollama model store to reuse (read-only; Alkahest never writes to it).
///
/// Order: `OLLAMA_MODELS` (honoured as given, even if it does not exist yet, because the user
/// asked for it), then `~/.ollama/models`, then the Linux service location
/// `/usr/share/ollama/.ollama/models`. The fallbacks must exist to be returned.
pub fn detect_ollama_store(env: EnvFn<'_>) -> Option<PathBuf> {
    if let Some(p) = env("OLLAMA_MODELS") {
        return Some(PathBuf::from(p));
    }
    let home = env("HOME")
        .map(PathBuf::from)
        .or_else(|| BaseDirs::new().map(|b| b.home_dir().to_path_buf()));
    let mut candidates = Vec::new();
    if let Some(home) = home {
        candidates.push(home.join(".ollama").join("models"));
    }
    if cfg!(target_os = "linux") {
        candidates.push(PathBuf::from("/usr/share/ollama/.ollama/models"));
    }
    candidates.into_iter().find(|p| p.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn fake(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn alkahest_home_relocates_everything() {
        let paths = AlkahestPaths::discover_with(&fake(&[("ALKAHEST_HOME", "/srv/alk")])).unwrap();
        assert_eq!(paths.data_dir, Path::new("/srv/alk/data"));
        assert_eq!(
            paths.user_config_file(),
            Path::new("/srv/alk/config/config.toml")
        );
        assert_eq!(paths.log_dir, Path::new("/srv/alk/logs"));
        assert_eq!(
            paths.default_models_dir(),
            Path::new("/srv/alk/data/models")
        );
    }

    #[test]
    fn platform_defaults_are_namespaced() {
        let paths = AlkahestPaths::discover_with(&fake(&[])).unwrap();
        assert!(
            paths
                .config_dir
                .to_string_lossy()
                .to_lowercase()
                .contains("alkahest")
        );
        assert!(
            paths
                .log_dir
                .to_string_lossy()
                .to_lowercase()
                .contains("alkahest")
        );
    }

    #[test]
    fn ollama_models_env_is_honoured_even_when_missing() {
        let found = detect_ollama_store(&fake(&[("OLLAMA_MODELS", "/nonexistent/models")]));
        assert_eq!(found, Some(PathBuf::from("/nonexistent/models")));
    }

    #[test]
    fn ollama_store_in_home_is_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let models = tmp.path().join(".ollama").join("models");
        std::fs::create_dir_all(&models).unwrap();
        let home = tmp.path().to_string_lossy().into_owned();
        assert_eq!(detect_ollama_store(&fake(&[("HOME", &home)])), Some(models));
    }
}
