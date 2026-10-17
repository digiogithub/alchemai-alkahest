//! Forward-compatible parsing (ALK-SP-0011.R3).
//!
//! Every catalog file type has a key schema here. In lenient mode (files of a git catalog)
//! keys the schema does not know are removed from the parsed TOML and reported as warnings
//! before the document reaches the strict serde types, so a catalog written for a newer minor
//! version keeps loading. In strict mode nothing is removed and the serde types reject the
//! unknown key, as `cargo xtask catalog-check` and the local recipe layers require.
//!
//! The recipe schema mirrors the strict types of `alkahest-recipes` (`RecipeFile`,
//! `VariantDef`, `Asset`, `Companion`, `Dependency`, `SourceBuild`, `VenvBuild`). A key added there must be
//! added here too, or git catalogs will drop it with a warning.

/// The shape of a TOML value.
#[derive(Debug)]
pub(crate) enum Shape {
    /// Any value, contents unchecked (free-form tables such as model `params`).
    Any,
    /// A scalar or a list of scalars; the serde types check the type.
    Leaf,
    /// A table with a fixed set of keys.
    Table(&'static [(&'static str, Shape)]),
    /// A table whose keys are names (variants) and whose values share one shape.
    Map(&'static Shape),
    /// An array of tables sharing one shape.
    Array(&'static Shape),
}

const ASSET: Shape = Shape::Table(&[
    ("os", Shape::Leaf),
    ("arch", Shape::Leaf),
    ("url", Shape::Leaf),
    ("sha256", Shape::Leaf),
    ("tofu", Shape::Leaf),
    ("size", Shape::Leaf),
    ("format", Shape::Leaf),
    ("strip_components", Shape::Leaf),
]);

const COMPANION: Shape = Shape::Table(&[
    ("name", Shape::Leaf),
    ("os", Shape::Leaf),
    ("arch", Shape::Leaf),
    ("url", Shape::Leaf),
    ("sha256", Shape::Leaf),
    ("size", Shape::Leaf),
    ("format", Shape::Leaf),
    ("strip_components", Shape::Leaf),
]);

const DEPENDENCY: Shape = Shape::Table(&[
    ("id", Shape::Leaf),
    ("os", Shape::Leaf),
    ("arch", Shape::Leaf),
    ("url", Shape::Leaf),
    ("sha256", Shape::Leaf),
    ("size", Shape::Leaf),
    ("format", Shape::Leaf),
    ("strip_components", Shape::Leaf),
    ("dest", Shape::Leaf),
    ("optional", Shape::Leaf),
]);

const SOURCE: Shape = Shape::Table(&[
    ("git", Shape::Leaf),
    ("rev", Shape::Leaf),
    ("submodules", Shape::Leaf),
    ("cmake_flags", Shape::Leaf),
    ("targets", Shape::Leaf),
    ("tools", Shape::Leaf),
    ("bin_dir", Shape::Leaf),
]);

const VENV: Shape = Shape::Table(&[
    ("python", Shape::Leaf),
    ("packages", Shape::Leaf),
    ("index_url", Shape::Leaf),
    ("extra_index_urls", Shape::Leaf),
    ("os", Shape::Leaf),
    ("arch", Shape::Leaf),
]);

const VARIANT: Shape = Shape::Table(&[
    ("accelerator", Shape::Leaf),
    ("cuda", Shape::Leaf),
    ("notes", Shape::Leaf),
    ("assets", Shape::Array(&ASSET)),
    ("companions", Shape::Array(&COMPANION)),
    ("dependencies", Shape::Array(&DEPENDENCY)),
    ("native", Shape::Leaf),
    ("source", SOURCE),
    ("venv", VENV),
]);

/// `engines/<id>.toml` (ALK-SP-0003.R3 format).
pub(crate) const RECIPE: Shape = Shape::Table(&[
    ("id", Shape::Leaf),
    ("version", Shape::Leaf),
    ("description", Shape::Leaf),
    ("license", Shape::Leaf),
    ("homepage", Shape::Leaf),
    ("executable", Shape::Leaf),
    ("library_dirs", Shape::Leaf),
    ("variants", Shape::Map(&VARIANT)),
]);

const MODEL_FILE: Shape = Shape::Table(&[
    ("kind", Shape::Leaf),
    ("url", Shape::Leaf),
    ("urls", Shape::Leaf),
    ("sha256", Shape::Leaf),
    ("size", Shape::Leaf),
    ("filename", Shape::Leaf),
    ("auth", Shape::Leaf),
]);

const MODEL_VARIANT: Shape = Shape::Table(&[
    ("tag", Shape::Leaf),
    ("format", Shape::Leaf),
    ("quant", Shape::Leaf),
    ("engine", Shape::Leaf),
    ("template", Shape::Leaf),
    ("template_text", Shape::Leaf),
    ("template_format", Shape::Leaf),
    ("params", Shape::Any),
    ("files", Shape::Array(&MODEL_FILE)),
]);

/// `models/<namespace>/<model>.toml` (ALK-SP-0011.R7).
pub(crate) const MODEL: Shape = Shape::Table(&[
    ("name", Shape::Leaf),
    ("description", Shape::Leaf),
    ("license", Shape::Leaf),
    ("homepage", Shape::Leaf),
    ("tags", Shape::Leaf),
    ("variants", Shape::Array(&MODEL_VARIANT)),
]);

/// `catalog.toml` (ALK-SP-0011.R2).
pub(crate) const MANIFEST: Shape = Shape::Table(&[
    ("schema_version", Shape::Leaf),
    ("name", Shape::Leaf),
    ("description", Shape::Leaf),
    ("min_alkahest", Shape::Leaf),
    ("homepage", Shape::Leaf),
    ("maintainers", Shape::Leaf),
]);

const INDEX_FILE: Shape = Shape::Table(&[
    ("path", Shape::Leaf),
    ("sha256", Shape::Leaf),
    ("size", Shape::Leaf),
]);

/// `index.toml` (ALK-SP-0011.R8).
pub(crate) const INDEX: Shape = Shape::Table(&[
    ("schema_version", Shape::Leaf),
    ("files", Shape::Array(&INDEX_FILE)),
]);

/// Removes the keys `shape` does not know from `value`, returning their dotted paths
/// (`variants.cpu.experimental_flag`). Values of the wrong type are left for serde to reject.
pub(crate) fn strip_unknown(value: &mut toml::Value, shape: &Shape) -> Vec<String> {
    let mut unknown = Vec::new();
    walk(value, shape, "", &mut unknown);
    unknown
}

fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

fn walk(value: &mut toml::Value, shape: &Shape, prefix: &str, unknown: &mut Vec<String>) {
    match (shape, value) {
        (Shape::Table(fields), toml::Value::Table(table)) => {
            let extra: Vec<String> = table
                .keys()
                .filter(|k| !fields.iter().any(|(name, _)| name == k))
                .cloned()
                .collect();
            for key in extra {
                table.remove(&key);
                unknown.push(join(prefix, &key));
            }
            for (name, inner) in *fields {
                if let Some(v) = table.get_mut(*name) {
                    walk(v, inner, &join(prefix, name), unknown);
                }
            }
        }
        (Shape::Map(inner), toml::Value::Table(table)) => {
            for (key, v) in table.iter_mut() {
                walk(v, inner, &join(prefix, key), unknown);
            }
        }
        (Shape::Array(inner), toml::Value::Array(items)) => {
            for (i, v) in items.iter_mut().enumerate() {
                walk(v, inner, &format!("{prefix}[{i}]"), unknown);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_keys_are_removed_and_reported() {
        let mut v: toml::Value = toml::from_str(
            r#"
            id = "x"
            future = 1
            [variants.cpu]
            accelerator = "cpu"
            experimental_flag = true
            [[variants.cpu.assets]]
            os = "linux"
            mirror = "y"
            "#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let unknown = strip_unknown(&mut v, &RECIPE);
        assert_eq!(
            unknown,
            vec![
                "future".to_string(),
                "variants.cpu.experimental_flag".to_string(),
                "variants.cpu.assets[0].mirror".to_string(),
            ]
        );
        let cpu = &v["variants"]["cpu"];
        assert!(cpu.get("experimental_flag").is_none());
        assert_eq!(cpu["accelerator"].as_str(), Some("cpu"));
    }

    #[test]
    fn free_form_params_are_kept() {
        let mut v: toml::Value = toml::from_str(
            "name = \"a/b\"\n[[variants]]\ntag = \"q\"\nparams = { anything = 1 }\n",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(strip_unknown(&mut v, &MODEL).is_empty());
    }
}
