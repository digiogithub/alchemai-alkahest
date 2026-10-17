//! Model entries of a catalog (ALK-SP-0011.R7): parsed, validated and stored in snapshots.
//! Pulling them (`alkahest pull catalog:...`) is ALK-US-0120, which reads these types.

use serde::{Deserialize, Serialize};

use alkahest_core::config::is_catalog_name;

/// File format of a model variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelFormat {
    /// GGUF (llama.cpp family).
    #[default]
    Gguf,
    /// Safetensors (vLLM).
    Safetensors,
}

/// Template language of `template_text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TemplateFormat {
    /// Jinja2 (Hugging Face chat templates).
    Jinja,
    /// Go `text/template` (Ollama templates).
    Go,
}

/// Role of a file in a model variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelFileKind {
    /// The weights; exactly one per variant.
    Model,
    /// Multimodal projector.
    Projector,
    /// LoRA adapter.
    Adapter,
    /// Image VAE.
    Vae,
    /// Text encoder (image models).
    TextEncoder,
    /// Tokenizer.
    Tokenizer,
    /// Configuration file.
    Config,
}

/// One downloadable file of a model variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelFile {
    /// Role of the file.
    pub kind: ModelFileKind,
    /// Single download URL (https).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Mirrors tried in order (https); exclusive with `url`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub urls: Vec<String>,
    /// Lower-case hex SHA-256 (mandatory).
    pub sha256: String,
    /// Size in bytes (mandatory).
    pub size: u64,
    /// Name inside the store; the last URL segment when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// Host whose keyring token authenticates the download (ALK-SP-0011.R14).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
}

impl ModelFile {
    /// The URLs to try, in order.
    pub fn sources(&self) -> Vec<&str> {
        self.url
            .as_deref()
            .into_iter()
            .chain(self.urls.iter().map(String::as_str))
            .collect()
    }

    /// The file name in the store: `filename`, else the last segment of the first URL.
    pub fn store_name(&self) -> Option<String> {
        self.filename.clone().or_else(|| {
            self.sources()
                .first()
                .and_then(|u| u.split(['?', '#']).next())
                .and_then(|u| u.rsplit('/').next())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
    }
}

/// One tag of a model entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelVariant {
    /// Tag, unique within the entry.
    pub tag: String,
    /// File format.
    #[serde(default)]
    pub format: ModelFormat,
    /// Quantisation label (`Q4_K_M`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quant: Option<String>,
    /// Engine id; ALK-SP-0003.R2 selection applies when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// Named built-in template (`chatml`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// Inline template text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_text: Option<String>,
    /// Language of `template_text`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_format: Option<TemplateFormat>,
    /// Ollama-style parameters (`num_ctx`, `temperature`, `stop`, ...).
    #[serde(default, skip_serializing_if = "toml::Table::is_empty")]
    pub params: toml::Table,
    /// Files to download.
    pub files: Vec<ModelFile>,
}

impl ModelVariant {
    /// Sum of the file sizes in bytes.
    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// A model entry: `models/<namespace>/<model>.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    /// `<namespace>/<model>`, equal to the file path.
    pub name: String,
    /// One-line description.
    pub description: String,
    /// SPDX licence id or `other`.
    pub license: String,
    /// Home page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// Search words.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Tags; the first is the default.
    pub variants: Vec<ModelVariant>,
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl ModelEntry {
    /// The default tag (first variant).
    pub fn default_tag(&self) -> Option<&str> {
        self.variants.first().map(|v| v.tag.as_str())
    }

    /// The variant with `tag`, or the default one for `None`.
    pub fn variant(&self, tag: Option<&str>) -> Option<&ModelVariant> {
        match tag {
            Some(t) => self.variants.iter().find(|v| v.tag == t),
            None => self.variants.first(),
        }
    }

    /// Checks every rule of ALK-SP-0011.R7; `expected_name` is the name implied by the path.
    /// Returns the first violation, naming the entry, the tag and the file.
    pub fn validate(&self, expected_name: &str) -> Result<(), String> {
        if self.name != expected_name {
            return Err(format!(
                "name `{}` does not match the path (expected `{expected_name}`)",
                self.name
            ));
        }
        if self.variants.is_empty() {
            return Err(format!("{}: no variants", self.name));
        }
        let mut tags = std::collections::BTreeSet::new();
        for v in &self.variants {
            let at = |reason: String| format!("{}:{}: {reason}", self.name, v.tag);
            if !is_catalog_name(&v.tag) {
                return Err(at(
                    "tag must use lower-case letters, digits, `-`, `_`, `.`".into()
                ));
            }
            if !tags.insert(v.tag.as_str()) {
                return Err(at("tag is used twice".into()));
            }
            if let Some(e) = &v.engine
                && !is_catalog_name(e)
            {
                return Err(at(format!("engine `{e}` is not an engine id")));
            }
            match (&v.template, &v.template_text, &v.template_format) {
                (Some(_), None, None) | (None, None, None) | (None, Some(_), Some(_)) => {}
                (Some(_), Some(_), _) => {
                    return Err(at("set template or template_text, not both".into()));
                }
                (_, Some(_), None) => {
                    return Err(at(
                        "template_text needs template_format (jinja or go)".into()
                    ));
                }
                (_, None, Some(_)) => {
                    return Err(at("template_format without template_text".into()));
                }
            }
            if v.files
                .iter()
                .filter(|f| f.kind == ModelFileKind::Model)
                .count()
                != 1
            {
                return Err(at("exactly one file must have kind = \"model\"".into()));
            }
            for (i, f) in v.files.iter().enumerate() {
                let name = f.store_name().unwrap_or_else(|| format!("files[{i}]"));
                let fat = |reason: &str| at(format!("file {name}: {reason}"));
                match (&f.url, f.urls.is_empty()) {
                    (Some(_), false) => return Err(fat("set url or urls, not both")),
                    (None, true) => return Err(fat("needs url or urls")),
                    _ => {}
                }
                if f.sources().iter().any(|u| !u.starts_with("https://")) {
                    return Err(fat("url must be https"));
                }
                if !is_sha256(&f.sha256) {
                    return Err(fat("sha256 must be 64 lower-case hex digits"));
                }
                if let Some(n) = &f.filename
                    && (n.is_empty() || n.contains(['/', '\\']) || n == "." || n == "..")
                {
                    return Err(fat("filename must be a plain file name"));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> ModelEntry {
        toml::from_str(text).unwrap_or_else(|e| panic!("{e}"))
    }

    const BASE: &str = r#"
name = "acme/assistant"
description = "d"
license = "mit"
[[variants]]
tag = "q4_k_m"
template = "chatml"
[[variants.files]]
kind = "model"
url = "https://example.org/a.gguf"
sha256 = "a4e5e156ddec27e286f75328784d7106b60a4eb1d246e950a001a3f944fbda99"
size = 24
"#;

    #[test]
    fn a_valid_entry_passes() {
        let e = parse(BASE);
        assert_eq!(e.validate("acme/assistant"), Ok(()));
        assert_eq!(e.default_tag(), Some("q4_k_m"));
        assert_eq!(
            e.variants[0].files[0].store_name().as_deref(),
            Some("a.gguf")
        );
    }

    #[test]
    fn missing_sha256_refused() {
        let text = BASE.replace(
            "sha256 = \"a4e5e156ddec27e286f75328784d7106b60a4eb1d246e950a001a3f944fbda99\"\n",
            "",
        );
        let err = toml::from_str::<ModelEntry>(&text)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("sha256"), "{err}");
    }

    #[test]
    fn url_and_urls_exclusive() {
        let text = BASE.replace("size = 24", "size = 24\nurls = [\"https://m/a.gguf\"]");
        let err = parse(&text)
            .validate("acme/assistant")
            .err()
            .unwrap_or_default();
        assert!(err.contains("set url or urls, not both"), "{err}");
    }

    #[test]
    fn path_mismatch_refused() {
        assert!(parse(BASE).validate("acme/other").is_err());
    }
}
