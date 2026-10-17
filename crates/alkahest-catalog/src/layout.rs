//! Catalog layout rules (ALK-SP-0011.R1): which relative paths are catalog files, and what a
//! safe relative path is (R8, R14).

use alkahest_core::config::is_catalog_name;

/// The manifest file.
pub const MANIFEST: &str = "catalog.toml";
/// The generated file index.
pub const INDEX: &str = "index.toml";

/// What a catalog file holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileKind {
    /// `catalog.toml`.
    Manifest,
    /// `index.toml`.
    Index,
    /// `engines/<id>.toml`; carries the file stem, which must equal the recipe id.
    Engine(String),
    /// `models/<namespace>/<model>.toml`; carries `<namespace>/<model>`, which must equal the
    /// entry name.
    Model(String),
}

/// Classifies a relative, forward-slash path. Anything outside the layout (README, scripts,
/// nested folders) is `None`: never parsed, never written to a snapshot.
pub fn classify(path: &str) -> Option<FileKind> {
    let parts: Vec<&str> = path.split('/').collect();
    let stem = |file: &str| {
        file.strip_suffix(".toml")
            .filter(|s| is_catalog_name(s))
            .map(str::to_string)
    };
    match parts.as_slice() {
        [MANIFEST] => Some(FileKind::Manifest),
        [INDEX] => Some(FileKind::Index),
        ["engines", file] => stem(file).map(FileKind::Engine),
        ["models", ns, file] if is_catalog_name(ns) => {
            stem(file).map(|m| FileKind::Model(format!("{ns}/{m}")))
        }
        _ => None,
    }
}

/// Checks that `path` is relative, uses forward slashes and has no empty, `.` or `..`
/// component. Returns the reason when it is not.
pub fn check_relative(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("empty path".into());
    }
    if path.starts_with('/') || path.contains('\\') || path.contains(':') {
        return Err("path must be relative and use forward slashes".into());
    }
    if path
        .split('/')
        .any(|c| c.is_empty() || c == "." || c == "..")
    {
        return Err("path must not contain empty, `.` or `..` components".into());
    }
    Ok(())
}

/// Joins a repository subdirectory and a catalog path (`catalog` + `index.toml`).
pub fn repo_path(subdir: &str, path: &str) -> String {
    let sub = subdir.trim_matches('/');
    if sub.is_empty() || sub == "." {
        path.to_string()
    } else {
        format!("{sub}/{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_paths_are_classified() {
        assert_eq!(classify("catalog.toml"), Some(FileKind::Manifest));
        assert_eq!(classify("index.toml"), Some(FileKind::Index));
        assert_eq!(
            classify("engines/llama-cpp.toml"),
            Some(FileKind::Engine("llama-cpp".into()))
        );
        assert_eq!(
            classify("models/acme/assistant.toml"),
            Some(FileKind::Model("acme/assistant".into()))
        );
        for other in [
            "README.md",
            "scripts/update.sh",
            "engines/x/y.toml",
            "engines/Upper.toml",
            "models/acme.toml",
            "models/a/b/c.toml",
            "engines/.hidden.toml",
        ] {
            assert_eq!(classify(other), None, "{other}");
        }
    }

    #[test]
    fn unsafe_paths_are_refused() {
        for bad in [
            "/etc/passwd",
            "../x",
            "a/../b",
            "a//b",
            "a\\b",
            "C:/x",
            "./a",
        ] {
            assert!(check_relative(bad).is_err(), "{bad}");
        }
        assert!(check_relative("engines/llama-cpp.toml").is_ok());
    }

    #[test]
    fn repo_paths_join_the_subdir() {
        assert_eq!(repo_path(".", "index.toml"), "index.toml");
        assert_eq!(repo_path("catalog/", "index.toml"), "catalog/index.toml");
    }
}
