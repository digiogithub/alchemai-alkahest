//! `alkahest config set|unset`: edits the configuration file from the command line
//! (ALK-US-0133), for example `alkahest config set server.port=11500 server.ollama_compat=true`.
//!
//! The file edited is the `--config` file when one is given, else the user configuration
//! file: the same file the running server's admin API saves to. Comments and unrelated keys
//! are kept (`toml_edit`), the result must load and validate before it replaces the file, and
//! the replacement is atomic (temp file + rename). Changes apply on the next server start.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::str::FromStr;

use alkahest_core::Config;
use alkahest_core::Source;
use alkahest_core::config::Loaded;
use anyhow::{Context, Result, bail};
use serde_json::json;

/// The file `config set` writes: the `--config` file, else the user configuration file.
pub fn target_file(loaded: &Loaded) -> PathBuf {
    loaded
        .sources
        .iter()
        .find_map(|s| match s {
            Source::ExplicitFile(p) => Some(p.clone()),
            _ => None,
        })
        .unwrap_or_else(|| loaded.paths.user_config_file())
}

/// `alkahest config set KEY=VALUE...`.
pub fn set(assignments: &[String], loaded: &Loaded, json_out: bool) -> Result<ExitCode> {
    let pairs = assignments
        .iter()
        .map(|a| parse_assignment(a))
        .collect::<Result<Vec<_>>>()?;
    let file = target_file(loaded);
    edit_file(&file, |doc| {
        for (key, value) in &pairs {
            set_key(doc, key, value.clone())?;
        }
        Ok(())
    })?;
    report(&file, assignments, "set", json_out)
}

/// `alkahest config unset KEY...`: removes keys so their defaults apply again.
pub fn unset(keys: &[String], loaded: &Loaded, json_out: bool) -> Result<ExitCode> {
    let parts = keys
        .iter()
        .map(|k| key_path(k))
        .collect::<Result<Vec<_>>>()?;
    let file = target_file(loaded);
    edit_file(&file, |doc| {
        for path in &parts {
            remove_key(doc, path);
        }
        Ok(())
    })?;
    report(&file, keys, "unset", json_out)
}

fn report(file: &Path, what: &[String], verb: &str, json_out: bool) -> Result<ExitCode> {
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "file": file, verb: what }))?
        );
    } else {
        for w in what {
            println!("{verb} {w}");
        }
        println!(
            "saved to {}; restart the server to apply (`alkahest service restart` or stop and \
             start `alkahest serve`)",
            file.display()
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// `server.port=11500` -> (`["server", "port"]`, 11500). The value is read as a TOML value
/// (`true`, `8`, `"text"`, `["a", "b"]`); anything that is not one is taken as a string, so
/// `server.host=0.0.0.0` and `logging.level=debug` need no quotes.
fn parse_assignment(a: &str) -> Result<(Vec<String>, toml_edit::Value)> {
    let Some((key, raw)) = a.split_once('=') else {
        bail!("`{a}`: expected KEY=VALUE, for example `server.port=11500`");
    };
    let path = key_path(key)?;
    let raw = raw.trim();
    let value = toml_edit::Value::from_str(raw).unwrap_or_else(|_| raw.into());
    Ok((path, value))
}

/// `server.tls.enabled` -> `["server", "tls", "enabled"]`.
fn key_path(key: &str) -> Result<Vec<String>> {
    let parts: Vec<String> = key
        .trim()
        .split('.')
        .map(|p| p.trim().to_string())
        .collect();
    if parts.iter().any(|p| {
        p.is_empty()
            || !p
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }) {
        bail!("`{key}`: expected a dotted key such as `server.port`");
    }
    Ok(parts)
}

fn set_key(
    doc: &mut toml_edit::DocumentMut,
    path: &[String],
    value: toml_edit::Value,
) -> Result<()> {
    let (last, tables) = path.split_last().context("empty key")?;
    let mut item = doc.as_item_mut();
    for t in tables {
        // A standard `[table]`, not an inline one: the file is also edited by hand.
        if item.get(t).is_none_or(|i| !i.is_table_like()) {
            item[t.as_str()] = toml_edit::table();
        }
        item = &mut item[t.as_str()];
    }
    item[last.as_str()] = toml_edit::Item::Value(value);
    Ok(())
}

fn remove_key(doc: &mut toml_edit::DocumentMut, path: &[String]) {
    let Some((last, tables)) = path.split_last() else {
        return;
    };
    let mut item = doc.as_item_mut();
    for t in tables {
        match item.get_mut(t) {
            Some(next) => item = next,
            None => return,
        }
    }
    if let Some(t) = item.as_table_like_mut() {
        t.remove(last);
    }
}

/// Edits the TOML file at `path` (missing = empty), refuses a result that would not load, and
/// replaces the file atomically.
fn edit_file(
    path: &Path,
    edit: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<()>,
) -> Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("{} is not valid TOML; fix it by hand first", path.display()))?;
    edit(&mut doc)?;
    let out = doc.to_string();
    Config::from_toml_str(&out).context("the configuration would not load; nothing was saved")?;
    let dir = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = dir.join(format!(".config.toml.tmp-{}", std::process::id()));
    std::fs::write(&tmp, out.as_bytes())
        .and_then(|()| std::fs::rename(&tmp, path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
        .with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(text: &str, assignments: &[&str]) -> Result<String> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, text)?;
        let pairs = assignments
            .iter()
            .map(|a| parse_assignment(a))
            .collect::<Result<Vec<_>>>()?;
        edit_file(&path, |doc| {
            for (k, v) in &pairs {
                set_key(doc, k, v.clone())?;
            }
            Ok(())
        })?;
        Ok(std::fs::read_to_string(&path)?)
    }

    #[test]
    fn set_port_and_ollama_compat_keeps_comments() {
        let out = apply(
            "# mine\n[server]\nhost = \"127.0.0.1\" # loopback\n",
            &["server.port=11500", "server.ollama_compat=true"],
        )
        .unwrap();
        assert!(
            out.contains("# mine") && out.contains("# loopback"),
            "{out}"
        );
        let c = Config::from_toml_str(&out).unwrap();
        assert_eq!(c.server.port, 11500);
        assert!(c.server.ollama_compat);
    }

    #[test]
    fn bare_words_are_strings() {
        let out = apply("", &["server.host=0.0.0.0", "logging.level=debug"]).unwrap();
        let c = Config::from_toml_str(&out).unwrap();
        assert_eq!(c.server.host, "0.0.0.0");
        assert_eq!(c.logging.level, "debug");
    }

    #[test]
    fn invalid_settings_are_refused_and_nothing_is_written() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        for bad in ["server.port=99999", "server.nope=1", "server.port=abc"] {
            let (k, v) = parse_assignment(bad).unwrap();
            assert!(
                edit_file(&path, |doc| set_key(doc, &k, v.clone())).is_err(),
                "{bad}"
            );
        }
        assert!(!path.exists());
        assert!(parse_assignment("server.port").is_err());
        assert!(parse_assignment("server..port=1").is_err());
    }

    #[test]
    fn unset_removes_the_key() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "[server]\nport = 11500\n").unwrap();
        edit_file(&path, |doc| {
            remove_key(doc, &key_path("server.port")?);
            Ok(())
        })
        .unwrap();
        let c = Config::from_toml_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(c.server.port, alkahest_core::config::DEFAULT_PORT);
    }
}
