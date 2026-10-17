//! `alkahest catalog add|rm|enable|disable|edit` and the admin API: edit `[[catalogs]]` in a
//! configuration file with `toml_edit`, keeping comments and every other key as they are
//! (ALK-SP-0011.R9).

use std::path::Path;

use alkahest_core::Config;
use alkahest_core::config::{CatalogSource, OFFICIAL_CATALOG_NAME};
use serde::{Deserialize, Deserializer};
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Removal {
    /// The source was removed.
    Removed,
    /// The official catalog (or the last source, replaced by the official one) is kept but
    /// disabled, because a configuration without `[[catalogs]]` gets the official catalog
    /// back.
    Disabled,
}

/// Removes the source `name` from `[[catalogs]]` of the file at `path`. The official catalog
/// is never removed: it is disabled (`enabled = false`) instead.
///
/// # Errors
///
/// [`CatalogError::Unknown`] when no source has that name.
pub fn remove_source(path: &Path, name: &str, effective: &[CatalogSource]) -> Result<Removal> {
    if name == OFFICIAL_CATALOG_NAME {
        let patch = SourcePatch {
            enabled: Some(false),
            ..SourcePatch::default()
        };
        update_source(path, name, &patch, effective)?;
        return Ok(Removal::Disabled);
    }
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

/// A change to one catalog source (`PATCH /alkahest/v1/catalogs/{name}`,
/// `alkahest catalog enable|disable|edit`). Absent fields stay as they are; `pin: null`
/// removes the pin. The URL and the name never change (remove and add instead).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SourcePatch {
    /// Turn the catalog on or off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Branch or tag to follow.
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    /// Folder of the catalog inside the repository.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subdir: Option<String>,
    /// `Some(Some(commit))` pins, `Some(None)` (JSON `null`) unpins, `None` leaves it.
    #[serde(
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub pin: Option<Option<String>>,
    /// Refresh interval (`24h`, `30m`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_interval: Option<String>,
}

/// Distinguishes an explicit `null` (`Some(None)`) from an absent field (`None`, through
/// `#[serde(default)]`).
fn double_option<'de, D: Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

impl SourcePatch {
    /// Whether the patch changes nothing.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// `source` with the patch applied (not validated).
    pub fn apply(&self, source: &CatalogSource) -> CatalogSource {
        let mut s = source.clone();
        if let Some(e) = self.enabled {
            s.enabled = e;
        }
        if let Some(r) = &self.r#ref {
            s.r#ref = r.trim().to_string();
        }
        if let Some(d) = &self.subdir {
            let d = d.trim();
            s.subdir = if d.is_empty() { ".".into() } else { d.into() };
        }
        if let Some(p) = &self.pin {
            s.pin = p
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_ascii_lowercase);
        }
        if let Some(i) = &self.refresh_interval {
            s.refresh_interval = i.trim().to_string();
        }
        s
    }
}

/// Applies `patch` to the source `name` of `[[catalogs]]` in the file at `path`, key by key
/// (comments inside the table are kept), and returns the updated source.
///
/// # Errors
///
/// [`CatalogError::Unknown`] when no source has that name, [`CatalogError::Config`] when the
/// result breaks a rule of ALK-SP-0011.R9.
pub fn update_source(
    path: &Path,
    name: &str,
    patch: &SourcePatch,
    effective: &[CatalogSource],
) -> Result<CatalogSource> {
    let current = effective
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| CatalogError::Unknown(name.to_string()))?;
    let next = patch.apply(current);
    next.validate()
        .map_err(|e| CatalogError::Config(e.to_string()))?;
    let mut doc = read_doc(path)?;
    let arr = catalogs(&mut doc, effective)?;
    let Some(t) = arr.iter_mut().find(|t| name_of(t) == Some(name)) else {
        return Err(CatalogError::Unknown(name.to_string()));
    };
    let mut set = |key: &str, v: Option<Item>| match v {
        Some(v) => {
            t.insert(key, v);
        }
        None => {
            t.remove(key);
        }
    };
    if patch.enabled.is_some() {
        set("enabled", (!next.enabled).then(|| value(false)));
    }
    if patch.r#ref.is_some() {
        set("ref", Some(value(next.r#ref.as_str())));
    }
    if patch.subdir.is_some() {
        set(
            "subdir",
            (next.subdir != ".").then(|| value(next.subdir.as_str())),
        );
    }
    if patch.pin.is_some() {
        set("pin", next.pin.as_deref().map(value));
    }
    if patch.refresh_interval.is_some() {
        set(
            "refresh_interval",
            (next.refresh_interval != "24h").then(|| value(next.refresh_interval.as_str())),
        );
    }
    write_doc(path, &doc)?;
    Ok(next)
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
