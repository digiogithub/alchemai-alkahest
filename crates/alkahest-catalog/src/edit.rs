//! `alkahest catalog add|rm`: edits `[[catalogs]]` in a configuration file with `toml_edit`,
//! keeping comments and every other key as they are (ALK-SP-0011.R9).

use std::path::Path;

use alkahest_core::Config;
use alkahest_core::config::CatalogSource;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

use crate::error::{CatalogError, Result};

fn read_doc(path: &Path) -> Result<DocumentMut> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(CatalogError::io(path, e)),
    };
    text.parse::<DocumentMut>()
        .map_err(|e| CatalogError::Config(format!("{}: {e}", path.display())))
}

fn write_doc(path: &Path, doc: &DocumentMut) -> Result<()> {
    let text = doc.to_string();
    // The edited file must still be a valid configuration.
    Config::from_toml_str(&text)
        .map_err(|e| CatalogError::Config(format!("{}: {e}", path.display())))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| CatalogError::io(dir, e))?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(|e| CatalogError::io(&tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| CatalogError::io(path, e))
}

fn table_of(source: &CatalogSource) -> Table {
    let mut t = Table::new();
    t.insert("name", value(source.name.as_str()));
    t.insert("url", value(source.url.as_str()));
    t.insert("ref", value(source.r#ref.as_str()));
    if source.subdir != "." {
        t.insert("subdir", value(source.subdir.as_str()));
    }
    if let Some(p) = &source.pin {
        t.insert("pin", value(p.as_str()));
    }
    if !source.enabled {
        t.insert("enabled", value(false));
    }
    if source.refresh_interval != "24h" {
        t.insert("refresh_interval", value(source.refresh_interval.as_str()));
    }
    if let Some(r) = &source.raw_url {
        t.insert("raw_url", value(r.as_str()));
    }
    if let Some(a) = &source.archive_url {
        t.insert("archive_url", value(a.as_str()));
    }
    t
}

/// The `[[catalogs]]` array of `doc`, created from `effective` (the sources in force, the
/// official one by default) when the file has none, so adding a source never drops the ones
/// the user already gets.
fn catalogs<'a>(
    doc: &'a mut DocumentMut,
    effective: &[CatalogSource],
) -> Result<&'a mut ArrayOfTables> {
    if doc.get("catalogs").is_none() {
        let mut arr = ArrayOfTables::new();
        for s in effective {
            arr.push(table_of(s));
        }
        doc.insert("catalogs", Item::ArrayOfTables(arr));
    }
    doc.get_mut("catalogs")
        .and_then(Item::as_array_of_tables_mut)
        .ok_or_else(|| {
            CatalogError::Config("`catalogs` must be written as [[catalogs]] tables".into())
        })
}

fn name_of(t: &Table) -> Option<&str> {
    t.get("name").and_then(Item::as_str)
}

/// Appends `source` to `[[catalogs]]` of the file at `path` (created when missing), after the
/// sources in `effective`.
///
/// # Errors
///
/// [`CatalogError::Config`] for an invalid source or a name already in use.
pub fn add_source(path: &Path, source: &CatalogSource, effective: &[CatalogSource]) -> Result<()> {
    source
        .validate()
        .map_err(|e| CatalogError::Config(e.to_string()))?;
    let mut doc = read_doc(path)?;
    let arr = catalogs(&mut doc, effective)?;
    if arr.iter().any(|t| name_of(t) == Some(source.name.as_str())) {
        return Err(CatalogError::Config(format!(
            "a catalog named `{}` already exists",
            source.name
        )));
    }
    arr.push(table_of(source));
    write_doc(path, &doc)
}

/// What [`remove_source`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removal {
    /// The source was removed.
    Removed,
    /// It was the last source: the official catalog is kept but disabled, because a
    /// configuration without `[[catalogs]]` gets the official catalog back.
    Disabled,
}

/// Removes the source `name` from `[[catalogs]]` of the file at `path`.
///
/// # Errors
///
/// [`CatalogError::Unknown`] when no source has that name.
pub fn remove_source(path: &Path, name: &str, effective: &[CatalogSource]) -> Result<Removal> {
    let mut doc = read_doc(path)?;
    let arr = catalogs(&mut doc, effective)?;
    let Some(i) = arr.iter().position(|t| name_of(t) == Some(name)) else {
        return Err(CatalogError::Unknown(name.to_string()));
    };
    arr.remove(i);
    let outcome = if arr.is_empty() {
        let mut official = CatalogSource::official();
        official.enabled = false;
        arr.push(table_of(&official));
        Removal::Disabled
    } else {
        Removal::Removed
    };
    write_doc(path, &doc)?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_remove_a_catalog() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# my settings\n[server]\nport = 1234 # keep\n").unwrap_or_default();
        let effective = vec![CatalogSource::official()];
        let mut acme = CatalogSource::new("acme", "https://git.example.org/acme/catalog");
        acme.r#ref = "stable".into();
        add_source(&path, &acme, &effective).unwrap_or_else(|e| panic!("{e}"));
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            text.contains("# my settings") && text.contains("port = 1234 # keep"),
            "{text}"
        );
        let cfg = Config::from_toml_str(&text).unwrap_or_else(|e| panic!("{e}"));
        let names: Vec<&str> = cfg.catalogs.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["official", "acme"]);
        assert_eq!(cfg.catalogs[1].r#ref, "stable");
        assert!(add_source(&path, &acme, &effective).is_err());

        assert_eq!(
            remove_source(&path, "acme", &effective).ok(),
            Some(Removal::Removed)
        );
        let cfg = Config::from_toml_str(&std::fs::read_to_string(&path).unwrap_or_default())
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(cfg.catalogs, vec![CatalogSource::official()]);
        assert_eq!(
            remove_source(&path, "official", &effective).ok(),
            Some(Removal::Disabled)
        );
        assert!(matches!(
            remove_source(&path, "nope", &effective),
            Err(CatalogError::Unknown(_))
        ));
    }
}
